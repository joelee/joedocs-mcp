//! Admin tokens and the sessions they open: the credentials of the remote
//! admin API. A token is created only here, on the server; logging in needs
//! the token and a current TOTP code from the authenticator it was enrolled
//! in, and opens a session that expires after `admin.session_hours`.

use jiff::Timestamp;
use secrecy::SecretString;

use super::{Error, Store};
use crate::auth::{
    ADMIN_PREFIX, RandomSource, SESSION_PREFIX, WEB_SESSION_PREFIX, generate_prefixed,
    parse_prefixed, verify_secret,
};
use crate::clock::format_timestamp;
use crate::db::{self, AdminTokenRow, SessionKind};
use crate::document::is_slug;
use crate::totp;

/// Consecutive failed logins that lock a token.
pub const MAX_FAILED_LOGINS: u32 = 5;
/// How long a lock lasts, in seconds.
pub const LOCK_SECONDS: i64 = 15 * 60;
/// Bytes in a TOTP secret (160 bits, as RFC 4226 recommends).
const TOTP_SECRET_BYTES: usize = 20;

/// Why an admin login or session was refused. The messages never say which
/// part of a credential was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminRefusal {
    /// The token, or the code, is not valid (or the token is revoked).
    #[error("the admin token or code is not valid")]
    Unauthorized,
    /// Too many failed logins.
    #[error("sign-in is locked after {MAX_FAILED_LOGINS} failed attempts; try again after {until}")]
    Locked {
        /// When logins are accepted again.
        until: String,
        /// Seconds until then.
        retry_after: u64,
    },
    /// The session has expired.
    #[error("the admin session has expired; run `joedocs login`")]
    SessionExpired,
    /// The session is missing, ended, or belongs to another token.
    #[error("there is no valid admin session; run `joedocs login`")]
    NoSession,
    /// The device credential is revoked or has not been used for too long:
    /// sign in again with the password.
    #[error(
        "this device's sign-in has ended; sign in again with your password (`joedocs auth login`)"
    )]
    DeviceInvalid,
}

/// A newly created admin token. The token and the TOTP secret are shown to
/// the user once.
#[derive(Debug)]
pub struct CreatedAdminToken {
    /// The stored token.
    pub row: AdminTokenRow,
    /// The full `jda_<id>_<secret>` token.
    pub token: SecretString,
    /// The TOTP secret in base32, for the authenticator app.
    pub totp_secret: SecretString,
    /// The `otpauth://` enrolment URI (it contains the TOTP secret).
    pub uri: SecretString,
}

/// A session opened by a successful login.
#[derive(Debug)]
pub struct OpenedSession {
    /// The admin token that logged in.
    pub admin: AdminTokenRow,
    /// The full `jds_<id>_<secret>` session token.
    pub token: SecretString,
    /// When it expires.
    pub expires_at: String,
}

/// A current web session, as checked by [`Store::web_session`].
#[derive(Debug)]
pub struct WebSession {
    /// The admin token that opened it.
    pub admin: AdminTokenRow,
    /// When it expires.
    pub expires_at: String,
}

impl Store {
    /// Creates an admin token with a new TOTP secret.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad name, [`Error::AlreadyExists`],
    /// and random or database errors.
    pub fn create_admin_token(
        &mut self,
        name: &str,
        random: &dyn RandomSource,
    ) -> Result<CreatedAdminToken, Error> {
        if !is_slug(name) {
            return Err(Error::invalid(
                "name",
                format!(
                    "\"{name}\" must be lower-case letters, digits and '-', starting with a letter or digit"
                ),
            ));
        }
        let new = generate_prefixed(ADMIN_PREFIX, random)?;
        let mut secret = [0_u8; TOTP_SECRET_BYTES];
        random.fill(&mut secret)?;
        let now = self.now();
        let tx = self.conn.transaction()?;
        if db::find_admin_token_by_name(&tx, name)?.is_some() {
            return Err(Error::AlreadyExists(format!("admin token {name}")));
        }
        db::insert_admin_token(&tx, &new.id, name, &new.secret_sha256, &secret, &now)?;
        let row = db::find_admin_token_by_id(&tx, &new.id)?
            .ok_or_else(|| Error::invalid("name", format!("admin token {name} vanished")))?;
        tx.commit()?;
        let base32 = totp::base32_encode(&secret);
        Ok(CreatedAdminToken {
            row,
            token: new.token,
            uri: SecretString::from(totp::otpauth_uri(name, &base32)),
            totp_secret: SecretString::from(base32),
        })
    }

    /// Every admin token, by name.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn admin_tokens(&self) -> Result<Vec<AdminTokenRow>, Error> {
        Ok(db::list_admin_tokens(&self.conn)?)
    }

    /// Revokes an admin token and every session it opened, at once.
    /// Revoking a revoked token changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] for an unknown name, and database errors.
    pub fn revoke_admin_token(&mut self, name: &str) -> Result<AdminTokenRow, Error> {
        let now = self.now();
        let tx = self.conn.transaction()?;
        let row = db::find_admin_token_by_name(&tx, name)?
            .ok_or_else(|| Error::NotFound(format!("admin token {name}")))?;
        db::revoke_admin_token(&tx, &row.id, &now)?;
        let row = db::find_admin_token_by_id(&tx, &row.id)?
            .ok_or_else(|| Error::NotFound(format!("admin token {name}")))?;
        tx.commit()?;
        Ok(row)
    }

    /// The unrevoked admin token `token` names and proves, if any. Used to
    /// attribute audit entries; it changes nothing.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn admin_token_for(&self, token: &SecretString) -> Result<Option<AdminTokenRow>, Error> {
        Ok(self.verified_admin(token)?.map(|(row, _)| row))
    }

    fn verified_admin(
        &self,
        token: &SecretString,
    ) -> Result<Option<(AdminTokenRow, db::AdminSecrets)>, Error> {
        let Some((id, secret)) = parse_prefixed(ADMIN_PREFIX, token) else {
            return Ok(None);
        };
        let Some(secrets) = db::admin_secrets(&self.conn, &id)? else {
            return Ok(None);
        };
        if !verify_secret(&secret, &secrets.secret_sha256) {
            return Ok(None);
        }
        Ok(db::find_admin_token_by_id(&self.conn, &id)?
            .filter(|row| row.revoked_at.is_none())
            .map(|row| (row, secrets)))
    }

    /// Logs in with an admin token and a TOTP code, opening a session that
    /// lasts `session_hours`. A code's step is accepted only once; after
    /// [`MAX_FAILED_LOGINS`] wrong codes in a row the token is locked for
    /// [`LOCK_SECONDS`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] when the login is refused, and random
    /// or database errors.
    pub fn admin_login(
        &mut self,
        token: &SecretString,
        code: &str,
        session_hours: u32,
        random: &dyn RandomSource,
    ) -> Result<OpenedSession, Error> {
        self.login(token, code, session_hours, random, SessionKind::Api)
    }

    /// Logs in to the web interface: as [`Store::admin_login`], with the same
    /// checks and lockout, but the session is a web session (`jdw_...`),
    /// checked on its own by [`Store::web_session`] and refused by the admin
    /// API.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] when the login is refused, and random
    /// or database errors.
    pub fn web_login(
        &mut self,
        token: &SecretString,
        code: &str,
        session_hours: u32,
        random: &dyn RandomSource,
    ) -> Result<OpenedSession, Error> {
        self.login(token, code, session_hours, random, SessionKind::Web)
    }

    fn login(
        &mut self,
        token: &SecretString,
        code: &str,
        session_hours: u32,
        random: &dyn RandomSource,
        kind: SessionKind,
    ) -> Result<OpenedSession, Error> {
        let Some((admin, secrets)) = self.verified_admin(token)? else {
            return Err(AdminRefusal::Unauthorized.into());
        };
        let now = self.clock.now();
        let now_text = format_timestamp(now);
        if let Some(until) = admin.locked_until.as_ref().filter(|u| **u > now_text) {
            return Err(AdminRefusal::Locked {
                until: until.clone(),
                retry_after: seconds_until(now, until),
            }
            .into());
        }
        let Some(step) = totp::verify(
            &secrets.totp_secret,
            code,
            now.as_second(),
            secrets.last_totp_step,
        ) else {
            let lock =
                (admin.failed_logins + 1 >= MAX_FAILED_LOGINS).then(|| later(now, LOCK_SECONDS));
            db::record_failed_login(&self.conn, &admin.id, lock.as_deref())?;
            return Err(AdminRefusal::Unauthorized.into());
        };
        let prefix = match kind {
            SessionKind::Api => SESSION_PREFIX,
            SessionKind::Web => WEB_SESSION_PREFIX,
        };
        let session = generate_prefixed(prefix, random)?;
        let expires_at = later(now, i64::from(session_hours) * 3600);
        let tx = self.conn.transaction()?;
        db::record_login(&tx, &admin.id, step, &now_text)?;
        db::insert_session(
            &tx,
            &session.id,
            &admin.id,
            &session.secret_sha256,
            &now_text,
            &expires_at,
            kind,
        )?;
        let admin = db::find_admin_token_by_id(&tx, &admin.id)?.unwrap_or(admin);
        tx.commit()?;
        Ok(OpenedSession {
            admin,
            token: session.token,
            expires_at,
        })
    }

    /// Checks that `session` is a current session of the admin token
    /// `token`, and records its use.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] when either is not valid, and
    /// database errors.
    pub fn admin_session(
        &self,
        token: &SecretString,
        session: &SecretString,
    ) -> Result<AdminTokenRow, Error> {
        let (admin, row) = self.session_of(token, session)?;
        if row.expires_at <= self.now() {
            return Err(AdminRefusal::SessionExpired.into());
        }
        db::touch_session(&self.conn, &row, &self.now())?;
        Ok(admin)
    }

    /// Ends `session` (logout). An expired session can still be ended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] when the token or session is not
    /// valid, and database errors.
    pub fn end_admin_session(
        &self,
        token: &SecretString,
        session: &SecretString,
    ) -> Result<AdminTokenRow, Error> {
        let (admin, row) = self.session_of(token, session)?;
        db::revoke_session(&self.conn, &row.id, &self.now())?;
        Ok(admin)
    }

    /// Checks that `session` is a current web session whose admin token is
    /// not revoked, and records its use. The session alone is the
    /// credential: the web interface never holds the admin token.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] ([`AdminRefusal::NoSession`] or
    /// [`AdminRefusal::SessionExpired`]) when it is not valid, and database
    /// errors.
    pub fn web_session(&self, session: &SecretString) -> Result<WebSession, Error> {
        let (admin, row) = self.web_session_row(session)?;
        if row.expires_at <= self.now() {
            return Err(AdminRefusal::SessionExpired.into());
        }
        db::touch_session(&self.conn, &row, &self.now())?;
        Ok(WebSession {
            admin,
            expires_at: row.expires_at,
        })
    }

    /// Ends a web session (sign out). An expired session can still be
    /// ended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AdminRefused`] when it is not a live web session,
    /// and database errors.
    pub fn end_web_session(&self, session: &SecretString) -> Result<AdminTokenRow, Error> {
        let (admin, row) = self.web_session_row(session)?;
        db::revoke_session(&self.conn, &row.id, &self.now())?;
        Ok(admin)
    }

    fn web_session_row(
        &self,
        session: &SecretString,
    ) -> Result<(AdminTokenRow, db::SessionRow), Error> {
        let Some((id, secret)) = parse_prefixed(WEB_SESSION_PREFIX, session) else {
            return Err(AdminRefusal::NoSession.into());
        };
        let Some(row) = db::find_session(&self.conn, &id)? else {
            return Err(AdminRefusal::NoSession.into());
        };
        if row.kind != SessionKind::Web
            || row.revoked_at.is_some()
            || !verify_secret(&secret, &row.secret_sha256)
        {
            return Err(AdminRefusal::NoSession.into());
        }
        let admin = db::find_admin_token_by_id(&self.conn, &row.admin_token_id)?
            .filter(|admin| admin.revoked_at.is_none())
            .ok_or(AdminRefusal::NoSession)?;
        Ok((admin, row))
    }

    fn session_of(
        &self,
        token: &SecretString,
        session: &SecretString,
    ) -> Result<(AdminTokenRow, db::SessionRow), Error> {
        let Some((admin, _)) = self.verified_admin(token)? else {
            return Err(AdminRefusal::Unauthorized.into());
        };
        let found = parse_prefixed(SESSION_PREFIX, session)
            .map(|(id, secret)| db::find_session(&self.conn, &id).map(|row| (row, secret)))
            .transpose()?;
        match found {
            Some((Some(row), secret))
                if row.admin_token_id == admin.id
                    && row.kind == SessionKind::Api
                    && row.revoked_at.is_none()
                    && verify_secret(&secret, &row.secret_sha256) =>
            {
                Ok((admin, row))
            }
            _ => Err(AdminRefusal::NoSession.into()),
        }
    }
}

/// `now` plus `seconds`, formatted.
pub(super) fn later(now: Timestamp, seconds: i64) -> String {
    let at = Timestamp::from_second(now.as_second().saturating_add(seconds)).unwrap_or(now);
    format_timestamp(at)
}

/// Whole seconds from `now` until the formatted time `until` (at least 1).
pub(super) fn seconds_until(now: Timestamp, until: &str) -> u64 {
    until.parse::<Timestamp>().ok().map_or(1, |u| {
        u64::try_from(u.as_second() - now.as_second())
            .unwrap_or(1)
            .max(1)
    })
}

impl From<AdminRefusal> for Error {
    fn from(refusal: AdminRefusal) -> Self {
        Self::AdminRefused(refusal)
    }
}
