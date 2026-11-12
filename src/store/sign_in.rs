//! Signing users in: the store's half of password sign-in, TOTP setup,
//! device renewal, user sessions and the per-name sign-in throttle.
//!
//! The store never sees a password. A sign-in runs in three phases so the
//! store lock is never held while a password is hashed (about 12 ms and
//! 19 MiB each, JD-36 C-04):
//!
//! 1. [`Store::sign_in_begin`], under the lock: the PHC string to check (the
//!    user's, or a fixed dummy for a name that is not a user) and whether
//!    the name is locked;
//! 2. the caller verifies the password against it, without the lock
//!    (`crate::password::Gate`);
//! 3. [`Store::sign_in_finish`], under the lock again: the user is read
//!    afresh, the password must still be the one checked, the code is
//!    checked, and a session, a device or a setup ticket issued.
//!
//! Every refusal of a sign-in is the same [`AdminRefusal::Unauthorized`],
//! and every failure counts against the submitted name, real or not, so a
//! reply never says whether a name exists or which part was wrong. Five
//! failures lock the name for 15 minutes; a lock stops new sign-ins and
//! nothing else (JD-36 D-03).

use secrecy::SecretString;

use super::admin_tokens::{LOCK_SECONDS, MAX_FAILED_LOGINS, later, seconds_until};
use super::{AdminRefusal, Error, Store};
use crate::auth::{
    DEVICE_PREFIX, RandomSource, SETUP_PREFIX, USER_SESSION_PREFIX, USER_WEB_SESSION_PREFIX,
    generate_prefixed, parse_prefixed, secret_for, verify_secret,
};
use crate::clock::format_timestamp;
use crate::db::{self, SessionKind, UserRow};
use crate::password::Verified;
use crate::totp;

/// Minutes a setup ticket, and the TOTP secret it shows, stay valid.
pub const SETUP_MINUTES: i64 = 10;
/// Bytes in a TOTP secret (160 bits, as RFC 4226 recommends).
const TOTP_SECRET_BYTES: usize = 20;
/// Longest name the throttle records; longer names are cut, so a stream of
/// long names cannot grow its rows.
const THROTTLE_NAME_CHARS: usize = 64;

/// What phase 1 of a sign-in found, for phase 3.
#[derive(Debug, Clone)]
pub struct SignInBegin {
    name: String,
    user: Option<UserRow>,
    phc: Option<String>,
    locked: Option<AdminRefusal>,
}

impl SignInBegin {
    /// The PHC string the password must be checked against: the user's.
    /// `None` for a name that is not a user: check it against
    /// `crate::password::dummy_hash()` instead, so the reply takes as long
    /// (the dummy is built by hashing once, so never under the store lock).
    #[must_use]
    pub fn phc(&self) -> Option<&str> {
        self.phc.as_deref()
    }

    /// The lock refusal when the name is locked now: reply with it at once,
    /// without checking the password.
    #[must_use]
    pub fn locked(&self) -> Option<&AdminRefusal> {
        self.locked.as_ref()
    }

    /// The user the name belongs to, if any (to name them in the audit log;
    /// never to tell the caller).
    #[must_use]
    pub fn user(&self) -> Option<&UserRow> {
        self.user.as_ref()
    }
}

/// What phase 3 of a sign-in, or setup, is asked to issue.
#[derive(Debug, Clone, Copy)]
pub struct SignInRequest<'a> {
    /// The TOTP code, if one was sent.
    pub code: Option<&'a str>,
    /// For the command line: issue a device credential with this name, and
    /// an API session. `None` for the web interface: a web session only.
    pub device: Option<&'a str>,
    /// How long the session lasts.
    pub session_hours: u32,
}

/// A device credential, shown to the client once.
#[derive(Debug)]
pub struct NewDevice {
    /// Its name, as stored (made unique among the user's devices).
    pub name: String,
    /// The full `jdd_<id>_<secret>` credential.
    pub credential: SecretString,
}

/// A TOTP secret being set up, shown to the user once.
#[derive(Debug)]
pub struct NewTotp {
    /// The secret in base32, to type into an authenticator.
    pub secret: SecretString,
    /// The `otpauth://` URI (it contains the secret).
    pub uri: SecretString,
}

/// What a successful sign-in (or setup) gives.
#[derive(Debug)]
pub enum SignedIn {
    /// A session, and for the command line a device credential.
    Session {
        /// The user.
        user: UserRow,
        /// The `jdu_...` or `jdv_...` session.
        session: SecretString,
        /// When it expires.
        expires_at: String,
        /// The device credential issued with it, for the command line.
        device: Option<NewDevice>,
    },
    /// Setup is due first: a ticket for [`Store::setup_finish`], with a new
    /// TOTP secret when no authenticator is set up yet (otherwise only a new
    /// password is due).
    Setup {
        /// The user.
        user: UserRow,
        /// The `jdt_...` ticket.
        ticket: SecretString,
        /// When the ticket lapses.
        expires_at: String,
        /// The TOTP secret to add to an authenticator, if one is due.
        totp: Option<NewTotp>,
    },
}

/// A checked user session.
#[derive(Debug, Clone)]
pub struct UserSession {
    /// The user.
    pub user: UserRow,
    /// When the session expires.
    pub expires_at: String,
    /// The device it belongs to (API sessions only).
    pub device_id: Option<String>,
    /// The session's ID.
    pub session_id: String,
}

/// What the web setup page shows for a ticket.
#[derive(Debug)]
pub struct SetupView {
    /// The user.
    pub user: UserRow,
    /// The TOTP secret being set up, if one is.
    pub totp: Option<NewTotp>,
    /// When the ticket lapses.
    pub expires_at: String,
}

/// What [`Store::setup_begin`] found: the user, and whether a TOTP code is
/// due as well as the new password.
#[derive(Debug, Clone)]
pub struct SetupBegin {
    /// The user.
    pub user: UserRow,
    /// Whether an authenticator is being set up (else only the password).
    pub totp: bool,
}

impl Store {
    /// Phase 1 of a sign-in, under the lock.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn sign_in_begin(&self, name: &str) -> Result<SignInBegin, Error> {
        let name = throttle_name(name);
        let locked = self.name_locked(&name)?;
        let user = db::find_user_by_name(&self.conn, &name)?.filter(|u| u.removed_at.is_none());
        let phc = match &user {
            Some(u) => db::user_secrets(&self.conn, &u.id)?.map(|s| s.password_hash),
            None => None,
        };
        Ok(SignInBegin {
            name,
            user,
            phc,
            locked,
        })
    }

    /// Phase 3 of a sign-in, under the lock: with the password checked
    /// (`verified`) and, when the stored hash is weak, its new hash
    /// (`rehash`), finishes the sign-in.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] ([`AdminRefusal::Unauthorized`] or
    /// [`AdminRefusal::Locked`]), and random or database errors.
    pub fn sign_in_finish(
        &mut self,
        begin: &SignInBegin,
        verified: Verified,
        rehash: Option<&str>,
        request: &SignInRequest<'_>,
        random: &dyn RandomSource,
    ) -> Result<SignedIn, Error> {
        if let Some(locked) = self.name_locked(&begin.name)? {
            return Err(locked.into());
        }
        let Some(user) = self.signable(begin, verified)? else {
            return self.refuse_sign_in(&begin.name);
        };
        let secrets = db::user_secrets(&self.conn, &user.id)?
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
        // The password was checked against what phase 1 read: if it was
        // reset or changed since, the check proves nothing.
        if begin.phc.as_deref() != Some(secrets.password_hash.as_str()) {
            return self.refuse_sign_in(&begin.name);
        }
        let Some(totp_secret) = secrets.totp_secret else {
            // No authenticator: only a one-time password gets here, and it
            // may only start setup.
            db::throttle_clear(&self.conn, &begin.name)?;
            return self.start_setup(user, true, random);
        };
        let now = self.clock.now();
        let step = request.code.and_then(|code| {
            totp::verify(&totp_secret, code, now.as_second(), secrets.last_totp_step)
        });
        let Some(step) = step else {
            return self.refuse_sign_in(&begin.name);
        };
        let now_text = format_timestamp(now);
        db::throttle_clear(&self.conn, &begin.name)?;
        db::record_user_login(&self.conn, &user.id, step, &now_text)?;
        if let Some(hash) = rehash {
            db::rehash_password(&self.conn, &user.id, hash)?;
        }
        if user.must_change_password {
            return self.start_setup(user, false, random);
        }
        self.open_session(&user, request, random)
    }

    /// The user a sign-in may continue for: the password matched, and the
    /// user (read again now) exists, is enabled, and, when its password is
    /// a one-time one, still within its time.
    fn signable(&self, begin: &SignInBegin, verified: Verified) -> Result<Option<UserRow>, Error> {
        if !verified.matches {
            return Ok(None);
        }
        let Some(id) = begin.user.as_ref().map(|u| u.id.as_str()) else {
            return Ok(None);
        };
        let now = self.now();
        Ok(db::find_user_by_id(&self.conn, id)?.filter(|u| {
            u.removed_at.is_none()
                && u.disabled_at.is_none()
                && (!u.must_change_password
                    || u.initial_password_until.as_ref().is_some_and(|t| *t > now))
        }))
    }

    /// Counts a failed sign-in for `name` and refuses it.
    fn refuse_sign_in<T>(&self, name: &str) -> Result<T, Error> {
        self.record_sign_in_failure(name)?;
        Err(AdminRefusal::Unauthorized.into())
    }

    /// Counts a failed sign-in (or a wrong current password) for `name`,
    /// locking it after [`MAX_FAILED_LOGINS`] in a row.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn record_sign_in_failure(&self, name: &str) -> Result<(), Error> {
        let name = throttle_name(name);
        let now = self.clock.now();
        let failures = db::throttle_of(&self.conn, &name)?.map_or(0, |t| t.failures);
        let lock = (failures + 1 >= MAX_FAILED_LOGINS).then(|| later(now, LOCK_SECONDS));
        db::throttle_failure(
            &self.conn,
            &name,
            lock.as_deref(),
            &format_timestamp(now),
            &later(now, -LOCK_SECONDS),
        )?;
        Ok(())
    }

    /// The lock refusal for `name`, when it is locked now.
    fn name_locked(&self, name: &str) -> Result<Option<AdminRefusal>, Error> {
        let now = self.clock.now();
        let now_text = format_timestamp(now);
        Ok(db::throttle_of(&self.conn, name)?
            .and_then(|t| t.locked_until)
            .filter(|until| *until > now_text)
            .map(|until| AdminRefusal::Locked {
                retry_after: seconds_until(now, &until),
                until,
            }))
    }

    /// Starts setup: a ticket, and a new TOTP secret when `totp`.
    fn start_setup(
        &self,
        user: UserRow,
        totp: bool,
        random: &dyn RandomSource,
    ) -> Result<SignedIn, Error> {
        let ticket = secret_for(SETUP_PREFIX, &user.id, random)?;
        let now = self.clock.now();
        let expires_at = later(now, SETUP_MINUTES * 60);
        let pending = if totp {
            let mut secret = [0_u8; TOTP_SECRET_BYTES];
            random.fill(&mut secret)?;
            Some(secret)
        } else {
            None
        };
        db::start_setup(
            &self.conn,
            &user.id,
            &ticket.secret_sha256,
            pending.as_ref().map(<[u8; TOTP_SECRET_BYTES]>::as_slice),
            &expires_at,
            &format_timestamp(now),
        )?;
        let totp = pending.map(|secret| {
            let base32 = totp::base32_encode(&secret);
            NewTotp {
                uri: SecretString::from(totp::otpauth_uri(&user.name, &base32)),
                secret: SecretString::from(base32),
            }
        });
        Ok(SignedIn::Setup {
            user,
            ticket: ticket.token,
            expires_at,
            totp,
        })
    }

    /// Opens a session for `user`: a web session, or for the command line
    /// a device credential and an API session.
    fn open_session(
        &self,
        user: &UserRow,
        request: &SignInRequest<'_>,
        random: &dyn RandomSource,
    ) -> Result<SignedIn, Error> {
        let now = self.clock.now();
        let now_text = format_timestamp(now);
        let expires_at = later(now, i64::from(request.session_hours) * 3600);
        let tx = self.conn.unchecked_transaction()?;
        let device = match request.device {
            Some(wanted) => {
                let new = generate_prefixed(DEVICE_PREFIX, random)?;
                let name = unique_device_name(&tx, &user.id, wanted)?;
                db::insert_device(&tx, &new.id, &user.id, &name, &new.secret_sha256, &now_text)?;
                Some((
                    new.id,
                    NewDevice {
                        name,
                        credential: new.token,
                    },
                ))
            }
            None => None,
        };
        let (prefix, kind) = if device.is_some() {
            (USER_SESSION_PREFIX, SessionKind::Api)
        } else {
            (USER_WEB_SESSION_PREFIX, SessionKind::Web)
        };
        let session = generate_prefixed(prefix, random)?;
        db::insert_user_session(
            &tx,
            &session.id,
            &user.id,
            device.as_ref().map(|(id, _)| id.as_str()),
            kind,
            &session.secret_sha256,
            &now_text,
            &expires_at,
        )?;
        let user = db::find_user_by_id(&tx, &user.id)?
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
        tx.commit()?;
        Ok(SignedIn::Session {
            user,
            session: session.token,
            expires_at,
            device: device.map(|(_, d)| d),
        })
    }

    /// Phase 1 of finishing setup, under the lock: the ticket's user, and
    /// whether a TOTP code is due. The caller then checks the new password
    /// against the rules and hashes it without the lock.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::Unauthorized`] for a ticket that is not
    /// valid, has lapsed or was used, and database errors.
    pub fn setup_begin(&self, ticket: &SecretString) -> Result<SetupBegin, Error> {
        let (user, totp) = self.ticket_user(ticket)?;
        Ok(SetupBegin { user, totp })
    }

    /// What the web setup page shows for a ticket: the user and, when an
    /// authenticator is being set up, its secret and URI again (the page
    /// can be reloaded within the ticket's ten minutes).
    ///
    /// # Errors
    ///
    /// As [`Store::setup_begin`].
    pub fn setup_view(&self, ticket: &SecretString) -> Result<SetupView, Error> {
        let (user, _) = self.ticket_user(ticket)?;
        let secrets = db::user_secrets(&self.conn, &user.id)?
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
        let totp = secrets.totp_pending_secret.map(|secret| {
            let base32 = totp::base32_encode(&secret);
            NewTotp {
                uri: SecretString::from(totp::otpauth_uri(&user.name, &base32)),
                secret: SecretString::from(base32),
            }
        });
        Ok(SetupView {
            user,
            totp,
            expires_at: secrets.totp_pending_until.unwrap_or_default(),
        })
    }

    fn ticket_user(&self, ticket: &SecretString) -> Result<(UserRow, bool), Error> {
        let unauthorized = || Error::from(AdminRefusal::Unauthorized);
        let (id, secret) = parse_prefixed(SETUP_PREFIX, ticket).ok_or_else(unauthorized)?;
        let user = db::find_user_by_id(&self.conn, &id)?
            .filter(|u| u.removed_at.is_none() && u.disabled_at.is_none())
            .ok_or_else(unauthorized)?;
        let secrets = db::user_secrets(&self.conn, &id)?.ok_or_else(unauthorized)?;
        let now = self.now();
        let valid = secrets
            .setup_ticket_sha256
            .as_ref()
            .is_some_and(|hash| verify_secret(&secret, hash))
            && secrets
                .totp_pending_until
                .as_ref()
                .is_some_and(|t| *t > now);
        if !valid {
            return Err(unauthorized());
        }
        Ok((user, secrets.totp_pending_secret.is_some()))
    }

    /// Phase 3 of setup, under the lock: confirms the pending TOTP secret
    /// with `request.code` (when one is being set up), stores the new
    /// password's hash, ends the ticket, and opens a session.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::Unauthorized`] for a ticket that is not
    /// valid or a wrong code, [`AdminRefusal::Locked`] for a locked name,
    /// and random or database errors.
    pub fn setup_finish(
        &mut self,
        ticket: &SecretString,
        new_hash: &str,
        request: &SignInRequest<'_>,
        random: &dyn RandomSource,
    ) -> Result<SignedIn, Error> {
        let (user, totp) = self.ticket_user(ticket)?;
        if let Some(locked) = self.name_locked(&user.name)? {
            return Err(locked.into());
        }
        let now = self.clock.now();
        let now_text = format_timestamp(now);
        if totp {
            let secrets = db::user_secrets(&self.conn, &user.id)?
                .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
            let pending = secrets.totp_pending_secret.unwrap_or_default();
            let step = request.code.and_then(|code| {
                totp::verify(&pending, code, now.as_second(), secrets.last_totp_step)
            });
            let Some(step) = step else {
                return self.refuse_sign_in(&user.name);
            };
            db::confirm_totp(&self.conn, &user.id, &pending, step, &now_text)?;
            db::record_user_login(&self.conn, &user.id, step, &now_text)?;
        }
        db::set_password(&self.conn, &user.id, new_hash, &now_text)?;
        db::end_setup(&self.conn, &user.id, &now_text)?;
        db::throttle_clear(&self.conn, &user.name)?;
        let user = db::find_user_by_id(&self.conn, &user.id)?
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
        self.open_session(&user, request, random)
    }

    /// Renews a command-line session with a device credential and a TOTP
    /// code. A device not used for `device_days` days lapses and is revoked.
    /// Wrong codes count against the device (not the name), five locking
    /// its renewals for 15 minutes.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::DeviceInvalid`] for a revoked or lapsed
    /// device or one whose user cannot sign in, [`AdminRefusal::Locked`],
    /// [`AdminRefusal::Unauthorized`] for a wrong code, and random or
    /// database errors.
    pub fn device_session(
        &mut self,
        credential: &SecretString,
        code: &str,
        request: &SignInRequest<'_>,
        device_days: u32,
        random: &dyn RandomSource,
    ) -> Result<SignedIn, Error> {
        let (device, user) = self.live_device(credential, device_days)?;
        let now = self.clock.now();
        let now_text = format_timestamp(now);
        if let Some(until) = device.locked_until.as_ref().filter(|u| **u > now_text) {
            return Err(AdminRefusal::Locked {
                until: until.clone(),
                retry_after: seconds_until(now, until),
            }
            .into());
        }
        let secrets = db::user_secrets(&self.conn, &user.id)?.ok_or(AdminRefusal::DeviceInvalid)?;
        let totp_secret = secrets.totp_secret.ok_or(AdminRefusal::DeviceInvalid)?;
        let Some(step) = totp::verify(&totp_secret, code, now.as_second(), secrets.last_totp_step)
        else {
            let lock =
                (device.failed_codes + 1 >= MAX_FAILED_LOGINS).then(|| later(now, LOCK_SECONDS));
            db::record_device_failure(&self.conn, &device.id, lock.as_deref())?;
            return Err(AdminRefusal::Unauthorized.into());
        };
        db::renew_device(&self.conn, &device.id, &now_text)?;
        db::record_user_login(&self.conn, &user.id, step, &now_text)?;
        let expires_at = later(now, i64::from(request.session_hours) * 3600);
        let session = generate_prefixed(USER_SESSION_PREFIX, random)?;
        db::insert_user_session(
            &self.conn,
            &session.id,
            &user.id,
            Some(&device.id),
            SessionKind::Api,
            &session.secret_sha256,
            &now_text,
            &expires_at,
        )?;
        Ok(SignedIn::Session {
            user,
            session: session.token,
            expires_at,
            device: None,
        })
    }

    /// The device `credential` names and proves, and its user, when both
    /// may still be used; a lapsed device is revoked here.
    fn live_device(
        &self,
        credential: &SecretString,
        device_days: u32,
    ) -> Result<(db::DeviceRow, UserRow), Error> {
        let invalid = || Error::from(AdminRefusal::DeviceInvalid);
        let (id, secret) = parse_prefixed(DEVICE_PREFIX, credential).ok_or_else(invalid)?;
        let device = db::find_device(&self.conn, &id)?
            .filter(|d| verify_secret(&secret, &d.secret_sha256))
            .ok_or_else(invalid)?;
        if device.revoked_at.is_some() {
            return Err(invalid());
        }
        let now = self.clock.now();
        let last_use = device.last_used_at.as_ref().unwrap_or(&device.created_at);
        let lapses = last_use
            .parse::<jiff::Timestamp>()
            .map(|t| later(t, i64::from(device_days) * 86_400))
            .unwrap_or_default();
        if lapses <= format_timestamp(now) {
            db::revoke_device(&self.conn, &device.id, &format_timestamp(now))?;
            return Err(invalid());
        }
        let user = db::find_user_by_id(&self.conn, &device.user_id)?
            .filter(can_hold_sessions)
            .ok_or_else(invalid)?;
        Ok((device, user))
    }

    /// The user a genuine device credential belongs to, whatever the state
    /// of the device or user (to name them in the audit log).
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn device_owner(&self, credential: &SecretString) -> Result<Option<UserRow>, Error> {
        let Some((id, secret)) = parse_prefixed(DEVICE_PREFIX, credential) else {
            return Ok(None);
        };
        let Some(device) =
            db::find_device(&self.conn, &id)?.filter(|d| verify_secret(&secret, &d.secret_sha256))
        else {
            return Ok(None);
        };
        Ok(db::find_user_by_id(&self.conn, &device.user_id)?)
    }

    /// Checks an API session of a device credential, and records its use
    /// (its renewal failures and lock stay).
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::DeviceInvalid`] for a device that may not be
    /// used, [`AdminRefusal::NoSession`] or [`AdminRefusal::SessionExpired`]
    /// for the session, and database errors.
    pub fn user_api_session(
        &self,
        credential: &SecretString,
        session: &SecretString,
        device_days: u32,
    ) -> Result<UserSession, Error> {
        let (device, _) = self.live_device(credential, device_days)?;
        let checked = self.user_session(USER_SESSION_PREFIX, SessionKind::Api, session)?;
        if checked.device_id.as_deref() != Some(device.id.as_str()) {
            return Err(AdminRefusal::NoSession.into());
        }
        db::touch_device(&self.conn, &device.id, &self.now())?;
        Ok(checked)
    }

    /// Checks a web session of a user, and records its use.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::NoSession`] or
    /// [`AdminRefusal::SessionExpired`], and database errors.
    pub fn user_web_session(&self, session: &SecretString) -> Result<UserSession, Error> {
        self.user_session(USER_WEB_SESSION_PREFIX, SessionKind::Web, session)
    }

    fn user_session(
        &self,
        prefix: &str,
        kind: SessionKind,
        session: &SecretString,
    ) -> Result<UserSession, Error> {
        let none = || Error::from(AdminRefusal::NoSession);
        let (id, secret) = parse_prefixed(prefix, session).ok_or_else(none)?;
        let row = db::find_user_session(&self.conn, &id)?
            .filter(|s| {
                s.kind == kind && s.revoked_at.is_none() && verify_secret(&secret, &s.secret_sha256)
            })
            .ok_or_else(none)?;
        let user = db::find_user_by_id(&self.conn, &row.user_id)?
            .filter(can_hold_sessions)
            .ok_or_else(none)?;
        let now = self.now();
        if row.expires_at <= now {
            return Err(AdminRefusal::SessionExpired.into());
        }
        db::touch_user_session(&self.conn, &row.id, &now)?;
        Ok(UserSession {
            user,
            expires_at: row.expires_at,
            device_id: row.device_id,
            session_id: row.id,
        })
    }

    /// Ends a user session (sign out); an expired one can still be ended.
    /// With `forget_device`, its device is revoked as well.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::NoSession`] when it is not a session of a
    /// user, and database errors.
    pub fn end_user_session(
        &self,
        session: &SecretString,
        forget_device: bool,
    ) -> Result<UserRow, Error> {
        let none = || Error::from(AdminRefusal::NoSession);
        let id = parse_prefixed(USER_SESSION_PREFIX, session)
            .or_else(|| parse_prefixed(USER_WEB_SESSION_PREFIX, session))
            .filter(|(id, secret)| {
                db::find_user_session(&self.conn, id)
                    .ok()
                    .flatten()
                    .is_some_and(|s| verify_secret(secret, &s.secret_sha256))
            })
            .map(|(id, _)| id)
            .ok_or_else(none)?;
        let row = db::find_user_session(&self.conn, &id)?.ok_or_else(none)?;
        let now = self.now();
        db::revoke_user_session(&self.conn, &row.id, &now)?;
        if forget_device && let Some(device) = &row.device_id {
            db::revoke_device(&self.conn, device, &now)?;
        }
        db::find_user_by_id(&self.conn, &row.user_id)?.ok_or_else(none)
    }

    /// The PHC string of `user`'s password, for a password change: the
    /// caller checks the current password against it without the lock.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UserNotFound`] and database errors.
    pub fn password_hash_of(&self, user: &UserRow) -> Result<String, Error> {
        db::user_secrets(&self.conn, &user.id)?
            .map(|s| s.password_hash)
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))
    }

    /// Changes a signed-in user's password, with the current password
    /// already checked against `verified_phc` and the new one hashed, once
    /// `code` is a valid TOTP code. Every other device and session of the
    /// user ends; the session `keep` (the one making the change) and its
    /// device stay.
    ///
    /// # Errors
    ///
    /// Returns [`AdminRefusal::Unauthorized`] for a wrong code, or when the
    /// stored password is no longer `verified_phc` (it was reset or changed
    /// while the current one was checked); both count against the user's
    /// name. Returns database errors too.
    pub fn change_password(
        &mut self,
        user: &UserRow,
        code: &str,
        verified_phc: &str,
        new_hash: &str,
        keep: &UserSession,
    ) -> Result<(), Error> {
        let secrets = db::user_secrets(&self.conn, &user.id)?
            .ok_or_else(|| Error::UserNotFound(user.name.clone()))?;
        if secrets.password_hash != verified_phc {
            return self.refuse_sign_in(&user.name);
        }
        let now = self.clock.now();
        let step = secrets
            .totp_secret
            .as_ref()
            .and_then(|secret| totp::verify(secret, code, now.as_second(), secrets.last_totp_step));
        let Some(step) = step else {
            return self.refuse_sign_in(&user.name);
        };
        let now_text = format_timestamp(now);
        let tx = self.conn.transaction()?;
        db::record_user_login(&tx, &user.id, step, &now_text)?;
        db::set_password(&tx, &user.id, new_hash, &now_text)?;
        db::revoke_others(
            &tx,
            &user.id,
            keep.device_id.as_deref(),
            &keep.session_id,
            &now_text,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A user's devices, revoked ones included.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn devices_of(&self, user: &UserRow) -> Result<Vec<db::DeviceRow>, Error> {
        Ok(db::devices_of(&self.conn, &user.id)?)
    }

    /// Revokes the user's device named `name`, and its sessions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when the user has no live device by that
    /// name, and database errors.
    pub fn revoke_device_of(&mut self, user: &UserRow, name: &str) -> Result<db::DeviceRow, Error> {
        let device = db::devices_of(&self.conn, &user.id)?
            .into_iter()
            .find(|d| d.name == name && d.revoked_at.is_none())
            .ok_or_else(|| Error::invalid("device", format!("you have no device named {name}")))?;
        db::revoke_device(&self.conn, &device.id, &self.now())?;
        Ok(db::find_device(&self.conn, &device.id)?.unwrap_or(device))
    }
}

/// Whether `user` may hold sessions and devices: not removed, not disabled,
/// set up, and not waiting for a new password.
fn can_hold_sessions(user: &UserRow) -> bool {
    user.removed_at.is_none()
        && user.disabled_at.is_none()
        && user.totp_enrolled_at.is_some()
        && !user.must_change_password
}

/// The throttle's key for a submitted name: trimmed and cut to
/// [`THROTTLE_NAME_CHARS`].
fn throttle_name(name: &str) -> String {
    name.trim().chars().take(THROTTLE_NAME_CHARS).collect()
}

/// `wanted` (cleaned of control characters, 1 to 64 characters, `device`
/// when empty), or with `-2`, `-3` ... when the user already has a live
/// device by that name.
fn unique_device_name(
    conn: &rusqlite::Connection,
    user_id: &str,
    wanted: &str,
) -> Result<String, Error> {
    let cleaned: String = wanted
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect::<String>()
        .trim()
        .to_owned();
    let base = if cleaned.is_empty() {
        "device".to_owned()
    } else {
        cleaned
    };
    let taken: Vec<String> = db::devices_of(conn, user_id)?
        .into_iter()
        .filter(|d| d.revoked_at.is_none())
        .map(|d| d.name)
        .collect();
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    Ok(name)
}
