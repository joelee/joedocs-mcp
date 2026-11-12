//! Users: named accounts that sign in with a password and a TOTP code
//! (0.5.1). This part is user administration: creating users with a
//! one-time initial password, listing them, disabling, enabling, unlocking,
//! resetting and removing them.
//!
//! Passwords are hashed by the caller, never here: hashing takes about
//! 12 ms and 19 MiB, and the store's lock must not be held meanwhile
//! (`crate::password::Gate`). Functions take the finished PHC string.

use super::admin_tokens::later;
use super::{Error, Store};
use crate::auth::{RandomSource, hex};
use crate::db::{self, UserRow};
use crate::document::is_slug;

/// Hours a one-time initial password works before the account must be
/// reset.
pub const INITIAL_PASSWORD_HOURS: i64 = 72;
/// Bytes of randomness in a user's public ID.
const USER_ID_BYTES: usize = 6;

/// Where a user stands, for `joedocs user list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserState {
    /// Waiting for the user's first sign-in (or the one after a reset) to
    /// set up an authenticator or choose a password, until the one-time
    /// password lapses.
    SetupPending {
        /// When the one-time password stops working.
        until: String,
    },
    /// The one-time password lapsed before it was used: reset it.
    SetupExpired,
    /// Can sign in.
    Active,
    /// Sign-in is refused for now after failed attempts.
    SignInLocked {
        /// When attempts are accepted again.
        until: String,
    },
    /// Disabled by an admin.
    Disabled,
    /// Removed; the name stays taken.
    Removed {
        /// When.
        at: String,
    },
}

/// A user as `joedocs user list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserListing {
    /// The stored user.
    pub row: UserRow,
    /// Where it stands now.
    pub state: UserState,
    /// Device credentials that are not revoked.
    pub devices: usize,
}

/// A user created, or reset, with a one-time initial password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedUser {
    /// The stored user.
    pub row: UserRow,
    /// When the one-time password stops working.
    pub until: String,
}

impl Store {
    /// Creates `name` with the one-time initial password whose PHC string
    /// is `initial_hash`, valid for [`INITIAL_PASSWORD_HOURS`]. The user has
    /// no authenticator until the first sign-in sets one up.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad name, [`Error::AlreadyExists`]
    /// for a name in use or once used, and random or database errors.
    pub fn create_user(
        &mut self,
        name: &str,
        initial_hash: &str,
        random: &dyn RandomSource,
    ) -> Result<CreatedUser, Error> {
        if !is_slug(name) {
            return Err(Error::invalid(
                "name",
                format!(
                    "\"{name}\" must be lower-case letters, digits and '-', starting with a letter or digit"
                ),
            ));
        }
        let mut id = [0_u8; USER_ID_BYTES];
        random.fill(&mut id)?;
        let id = hex(&id);
        let now = self.now();
        let until = self.initial_until();
        let tx = self.conn.transaction()?;
        if db::find_user_by_name(&tx, name)?.is_some() {
            return Err(Error::AlreadyExists(format!(
                "user {name} (a removed user's name is not reused)"
            )));
        }
        db::insert_user(&tx, &id, name, initial_hash, &until, &now)?;
        let row = db::find_user_by_id(&tx, &id)?.ok_or_else(|| Error::UserNotFound(name.into()))?;
        tx.commit()?;
        Ok(CreatedUser { row, until })
    }

    /// Every user, removed ones included, by name, with where each stands.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn users(&self) -> Result<Vec<UserListing>, Error> {
        let now = self.now();
        let mut listed = Vec::new();
        for row in db::list_users(&self.conn)? {
            let locked = db::throttle_of(&self.conn, &row.id)?
                .and_then(|t| t.locked_until)
                .filter(|until| *until > now);
            let devices = db::devices_of(&self.conn, &row.id)?
                .iter()
                .filter(|d| d.revoked_at.is_none())
                .count();
            let state = state_of(&row, locked, &now);
            listed.push(UserListing {
                row,
                state,
                devices,
            });
        }
        Ok(listed)
    }

    /// The user with this name, if it exists and is not removed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UserNotFound`] and [`Error::Invalid`] for a removed
    /// user, and database errors.
    pub fn user(&self, name: &str) -> Result<UserRow, Error> {
        live_user(&self.conn, name)
    }

    /// Disables `name`: sign-in is refused, and their sessions end and
    /// their devices are revoked at once.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn disable_user(&mut self, name: &str) -> Result<UserRow, Error> {
        let now = self.now();
        let tx = self.conn.transaction()?;
        let row = live_user(&tx, name)?;
        db::set_user_disabled(&tx, &row.id, Some(&now), &now)?;
        db::revoke_all_of(&tx, &row.id, None, &now)?;
        let row = live_user(&tx, name)?;
        tx.commit()?;
        Ok(row)
    }

    /// Enables `name` again. Sessions and devices ended by disabling stay
    /// ended: the user signs in again.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn enable_user(&mut self, name: &str) -> Result<UserRow, Error> {
        let now = self.now();
        let tx = self.conn.transaction()?;
        let row = live_user(&tx, name)?;
        db::set_user_disabled(&tx, &row.id, None, &now)?;
        let row = live_user(&tx, name)?;
        tx.commit()?;
        Ok(row)
    }

    /// Clears the sign-in lock on `name` after failed attempts.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn unlock_user(&mut self, name: &str) -> Result<UserRow, Error> {
        let row = live_user(&self.conn, name)?;
        db::throttle_clear(&self.conn, &row.id)?;
        Ok(row)
    }

    /// Gives `name` a new one-time initial password (`initial_hash`), valid
    /// for [`INITIAL_PASSWORD_HOURS`]: they choose their own at the next
    /// sign-in. Their authenticator is kept; their sessions end and their
    /// devices are revoked.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn reset_password(&mut self, name: &str, initial_hash: &str) -> Result<CreatedUser, Error> {
        self.reset(name, initial_hash, false)
    }

    /// Clears `name`'s authenticator and gives them a new one-time initial
    /// password (`initial_hash`): the account is back where a new one
    /// starts, and the next sign-in sets up an authenticator again. Their
    /// sessions end and their devices are revoked.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn reset_totp(&mut self, name: &str, initial_hash: &str) -> Result<CreatedUser, Error> {
        self.reset(name, initial_hash, true)
    }

    fn reset(&mut self, name: &str, initial_hash: &str, totp: bool) -> Result<CreatedUser, Error> {
        let now = self.now();
        let until = self.initial_until();
        let tx = self.conn.transaction()?;
        let row = live_user(&tx, name)?;
        if totp {
            db::clear_totp(&tx, &row.id, &now)?;
        }
        db::set_initial_password(&tx, &row.id, initial_hash, &until, &now)?;
        db::revoke_all_of(&tx, &row.id, None, &now)?;
        let row = live_user(&tx, name)?;
        tx.commit()?;
        Ok(CreatedUser { row, until })
    }

    /// Removes `name`: sessions and devices are revoked, and the name stays
    /// taken so it is never reused. Run on the server only.
    ///
    /// # Errors
    ///
    /// As [`Store::user`].
    pub fn remove_user(&mut self, name: &str) -> Result<UserRow, Error> {
        let now = self.now();
        let tx = self.conn.transaction()?;
        let row = live_user(&tx, name)?;
        db::set_user_removed(&tx, &row.id, &now)?;
        db::revoke_all_of(&tx, &row.id, None, &now)?;
        db::throttle_clear(&tx, &row.id)?;
        let row = db::find_user_by_id(&tx, &row.id)?
            .ok_or_else(|| Error::UserNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(row)
    }

    /// Whether `name` is the last user who can sign in (enabled, not
    /// removed, with a confirmed authenticator). The admin API and the web
    /// interface refuse to disable or reset that user, so someone can
    /// always sign in; the server's command line still can.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn is_last_sign_in_user(&self, name: &str) -> Result<bool, Error> {
        let can = |u: &UserRow| {
            u.removed_at.is_none() && u.disabled_at.is_none() && u.totp_enrolled_at.is_some()
        };
        let users = db::list_users(&self.conn)?;
        let target = users.iter().any(|u| u.name == name && can(u));
        let others = users.iter().filter(|u| u.name != name && can(u)).count();
        Ok(target && others == 0)
    }

    /// When a one-time password set now stops working.
    fn initial_until(&self) -> String {
        later(self.clock.now(), INITIAL_PASSWORD_HOURS * 3600)
    }
}

/// The user `name`, refusing an unknown or removed one.
fn live_user(conn: &rusqlite::Connection, name: &str) -> Result<UserRow, Error> {
    let row = db::find_user_by_name(conn, name)?.ok_or_else(|| Error::UserNotFound(name.into()))?;
    if row.removed_at.is_some() {
        return Err(Error::invalid(
            "user",
            format!("user {name} was removed; its name stays taken"),
        ));
    }
    Ok(row)
}

fn state_of(row: &UserRow, locked_until: Option<String>, now: &str) -> UserState {
    if let Some(at) = &row.removed_at {
        return UserState::Removed { at: at.clone() };
    }
    if row.disabled_at.is_some() {
        return UserState::Disabled;
    }
    if row.must_change_password || row.totp_enrolled_at.is_none() {
        return match &row.initial_password_until {
            Some(until) if until.as_str() > now => UserState::SetupPending {
                until: until.clone(),
            },
            _ => UserState::SetupExpired,
        };
    }
    match locked_until {
        Some(until) => UserState::SignInLocked { until },
        None => UserState::Active,
    }
}
