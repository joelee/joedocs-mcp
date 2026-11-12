//! User, device, user session and sign-in throttle queries (migration 7).
//!
//! Rows that callers pass around ([`UserRow`], [`DeviceRow`],
//! [`UserSessionRow`]) never hold a secret; what a sign-in checks is read
//! separately ([`UserSecrets`]), whose `Debug` output hides its values.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

use super::SessionKind;

/// Most rows the sign-in throttle keeps; the least recently touched go first.
pub const THROTTLE_CAP: usize = 10_000;

/// A stored user (never its password hash or TOTP secret).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserRow {
    /// Public ID.
    pub id: String,
    /// User name, a slug; never renamed and never reused.
    pub name: String,
    /// The password must be changed before a session is opened: it is a
    /// one-time initial password set by an admin.
    pub must_change_password: bool,
    /// When the one-time initial password stops working, if one is set.
    pub initial_password_until: Option<String>,
    /// When the authenticator was confirmed, if it is.
    pub totp_enrolled_at: Option<String>,
    /// Creation time.
    pub created_at: String,
    /// Last change to the row.
    pub updated_at: String,
    /// Last successful sign-in, if any.
    pub last_login_at: Option<String>,
    /// When an admin disabled the user, if disabled.
    pub disabled_at: Option<String>,
    /// When the user was removed (the row stays so the name is not reused).
    pub removed_at: Option<String>,
}

/// What a sign-in or TOTP setup checks, for one user.
#[derive(Clone, PartialEq, Eq)]
pub struct UserSecrets {
    /// The Argon2id PHC string of the password.
    pub password_hash: String,
    /// The confirmed TOTP secret, if any.
    pub totp_secret: Option<Vec<u8>>,
    /// A TOTP secret being set up, if any.
    pub totp_pending_secret: Option<Vec<u8>>,
    /// When the pending secret and its setup ticket lapse.
    pub totp_pending_until: Option<String>,
    /// SHA-256 of the current setup ticket's secret, if any.
    pub setup_ticket_sha256: Option<Vec<u8>>,
    /// The last TOTP step accepted, if any.
    pub last_totp_step: Option<i64>,
}

impl std::fmt::Debug for UserSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserSecrets")
            .field("password_hash", &"[REDACTED]")
            .field(
                "totp_secret",
                &self.totp_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "totp_pending_secret",
                &self.totp_pending_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("totp_pending_until", &self.totp_pending_until)
            .field("last_totp_step", &self.last_totp_step)
            .finish_non_exhaustive()
    }
}

/// A device credential issued by `joedocs auth login` (never its secret).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceRow {
    /// Public ID.
    pub id: String,
    /// The user it belongs to.
    pub user_id: String,
    /// Its name, unique among the user's devices.
    pub name: String,
    /// SHA-256 of its secret.
    #[serde(skip)]
    pub secret_sha256: Vec<u8>,
    /// Creation time.
    pub created_at: String,
    /// Last use (sign-in or renewal), if any.
    pub last_used_at: Option<String>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<String>,
    /// Consecutive wrong codes on renewal since the last success or lock.
    pub failed_codes: u32,
    /// Renewals are refused until this time, if locked.
    pub locked_until: Option<String>,
}

/// A stored user session (never its secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSessionRow {
    /// Public ID.
    pub id: String,
    /// The user it belongs to.
    pub user_id: String,
    /// The device that opened it, for an API session.
    pub device_id: Option<String>,
    /// Where it is accepted.
    pub kind: SessionKind,
    /// SHA-256 of its secret.
    pub secret_sha256: Vec<u8>,
    /// When it stops working.
    pub expires_at: String,
    /// Revocation time, if ended.
    pub revoked_at: Option<String>,
}

/// The sign-in throttle's record for one submitted name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleRow {
    /// Consecutive failures since the last success or lock.
    pub failures: u32,
    /// Sign-ins for the name are refused until this time, if locked.
    pub locked_until: Option<String>,
}

const USER_SELECT: &str = "SELECT id, name, must_change_password, initial_password_until, \
     totp_enrolled_at, created_at, updated_at, last_login_at, disabled_at, removed_at FROM users";

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        id: row.get(0)?,
        name: row.get(1)?,
        must_change_password: row.get::<_, i64>(2)? != 0,
        initial_password_until: row.get(3)?,
        totp_enrolled_at: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        last_login_at: row.get(7)?,
        disabled_at: row.get(8)?,
        removed_at: row.get(9)?,
    })
}

/// Inserts a user with a one-time initial password, valid until
/// `initial_until`, and no authenticator.
///
/// # Errors
///
/// Returns an error when the ID or name is taken or the statement fails.
pub fn insert_user(
    conn: &Connection,
    id: &str,
    name: &str,
    password_hash: &str,
    initial_until: &str,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO users (id, name, password_hash, must_change_password, initial_password_until,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?5)",
        params![id, name, password_hash, initial_until, at],
    )?;
    Ok(())
}

/// Every user, removed ones included, by name.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_users(conn: &Connection) -> rusqlite::Result<Vec<UserRow>> {
    let mut stmt = conn.prepare(&format!("{USER_SELECT} ORDER BY name"))?;
    stmt.query_map([], user_from_row)?.collect()
}

/// The user with this name, removed or not.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_user_by_name(conn: &Connection, name: &str) -> rusqlite::Result<Option<UserRow>> {
    conn.query_row(
        &format!("{USER_SELECT} WHERE name = ?1"),
        [name],
        user_from_row,
    )
    .optional()
}

/// The user with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_user_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<UserRow>> {
    conn.query_row(&format!("{USER_SELECT} WHERE id = ?1"), [id], user_from_row)
        .optional()
}

/// The secrets a sign-in checks, for the user with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn user_secrets(conn: &Connection, id: &str) -> rusqlite::Result<Option<UserSecrets>> {
    conn.query_row(
        "SELECT password_hash, totp_secret, totp_pending_secret, totp_pending_until,
                setup_ticket_sha256, last_totp_step
         FROM users WHERE id = ?1",
        [id],
        |row| {
            Ok(UserSecrets {
                password_hash: row.get(0)?,
                totp_secret: row.get(1)?,
                totp_pending_secret: row.get(2)?,
                totp_pending_until: row.get(3)?,
                setup_ticket_sha256: row.get(4)?,
                last_totp_step: row.get(5)?,
            })
        },
    )
    .optional()
}

/// Sets a one-time initial password (an admin's reset): it must be changed
/// at the next sign-in and works until `initial_until`. Any setup in
/// progress is dropped.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_initial_password(
    conn: &Connection,
    id: &str,
    password_hash: &str,
    initial_until: &str,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?2, must_change_password = 1,
                initial_password_until = ?3, totp_pending_secret = NULL,
                totp_pending_until = NULL, setup_ticket_sha256 = NULL, updated_at = ?4
         WHERE id = ?1",
        params![id, password_hash, initial_until, at],
    )?;
    Ok(())
}

/// Sets the user's own password: no change is due and no initial password
/// remains.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_password(
    conn: &Connection,
    id: &str,
    password_hash: &str,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?2, must_change_password = 0,
                initial_password_until = NULL, updated_at = ?3
         WHERE id = ?1",
        params![id, password_hash, at],
    )?;
    Ok(())
}

/// Replaces the stored hash of the same password (a rehash with newer
/// parameters), changing nothing else.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn rehash_password(conn: &Connection, id: &str, password_hash: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?2 WHERE id = ?1",
        params![id, password_hash],
    )?;
    Ok(())
}

/// Starts (or restarts) setup: a ticket valid until `until`, and, when
/// given, a pending TOTP secret.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn start_setup(
    conn: &Connection,
    id: &str,
    ticket_sha256: &[u8],
    pending_secret: Option<&[u8]>,
    until: &str,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET setup_ticket_sha256 = ?2, totp_pending_secret = ?3,
                totp_pending_until = ?4, updated_at = ?5
         WHERE id = ?1",
        params![id, ticket_sha256, pending_secret, until, at],
    )?;
    Ok(())
}

/// Ends setup: drops the ticket and any pending secret.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn end_setup(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET setup_ticket_sha256 = NULL, totp_pending_secret = NULL,
                totp_pending_until = NULL, updated_at = ?2
         WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Confirms the pending TOTP secret as the user's, with `step` used.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn confirm_totp(
    conn: &Connection,
    id: &str,
    secret: &[u8],
    step: i64,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET totp_secret = ?2, totp_enrolled_at = ?4, last_totp_step = ?3,
                totp_pending_secret = NULL, totp_pending_until = NULL, updated_at = ?4
         WHERE id = ?1",
        params![id, secret, step, at],
    )?;
    Ok(())
}

/// Clears the authenticator (an admin's `reset-totp`).
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn clear_totp(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET totp_secret = NULL, totp_enrolled_at = NULL, last_totp_step = NULL,
                totp_pending_secret = NULL, totp_pending_until = NULL,
                setup_ticket_sha256 = NULL, updated_at = ?2
         WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Records a successful sign-in at TOTP `step`.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn record_user_login(conn: &Connection, id: &str, step: i64, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET last_totp_step = ?2, last_login_at = ?3 WHERE id = ?1",
        params![id, step, at],
    )?;
    Ok(())
}

/// Disables (`Some(at)`) or enables (`None`) a user.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_user_disabled(
    conn: &Connection,
    id: &str,
    disabled_at: Option<&str>,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET disabled_at = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, disabled_at, at],
    )?;
    Ok(())
}

/// Removes a user: the row stays, marked removed, so the name is never
/// reused.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_user_removed(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET removed_at = ?2, updated_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
        params![id, at],
    )?;
    Ok(())
}

const DEVICE_SELECT: &str = "SELECT id, user_id, name, secret_sha256, created_at, last_used_at, \
     revoked_at, failed_codes, locked_until FROM devices";

fn device_from_row(row: &Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok(DeviceRow {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        secret_sha256: row.get(3)?,
        created_at: row.get(4)?,
        last_used_at: row.get(5)?,
        revoked_at: row.get(6)?,
        failed_codes: row.get(7)?,
        locked_until: row.get(8)?,
    })
}

/// Inserts a device credential, used now.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn insert_device(
    conn: &Connection,
    id: &str,
    user_id: &str,
    name: &str,
    secret_sha256: &[u8],
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO devices (id, user_id, name, secret_sha256, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, user_id, name, secret_sha256, at],
    )?;
    Ok(())
}

/// The device with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_device(conn: &Connection, id: &str) -> rusqlite::Result<Option<DeviceRow>> {
    conn.query_row(
        &format!("{DEVICE_SELECT} WHERE id = ?1"),
        [id],
        device_from_row,
    )
    .optional()
}

/// A user's devices, revoked ones included, oldest first.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn devices_of(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<DeviceRow>> {
    let mut stmt = conn.prepare(&format!(
        "{DEVICE_SELECT} WHERE user_id = ?1 ORDER BY created_at, name"
    ))?;
    stmt.query_map([user_id], device_from_row)?.collect()
}

/// Records a device's use. Its renewal failures and lock stay: only
/// [`renew_device`] clears them.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn touch_device(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE devices SET last_used_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Records a successful renewal of a device: its use, and a fresh count
/// of renewal failures with no lock.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn renew_device(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE devices SET last_used_at = ?2, failed_codes = 0, locked_until = NULL WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Records a wrong code on renewal: counts it, and locks renewals until
/// `lock_until` when given (resetting the count).
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn record_device_failure(
    conn: &Connection,
    id: &str,
    lock_until: Option<&str>,
) -> rusqlite::Result<()> {
    if let Some(until) = lock_until {
        conn.execute(
            "UPDATE devices SET failed_codes = 0, locked_until = ?2 WHERE id = ?1",
            params![id, until],
        )?;
    } else {
        conn.execute(
            "UPDATE devices SET failed_codes = failed_codes + 1 WHERE id = ?1",
            [id],
        )?;
    }
    Ok(())
}

/// Revokes one device and every session it opened.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn revoke_device(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    conn.execute(
        "UPDATE user_sessions SET revoked_at = ?2 WHERE device_id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    Ok(())
}

/// Revokes every device and session of a user, except the device `keep`
/// (and its sessions) when given.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn revoke_all_of(
    conn: &Connection,
    user_id: &str,
    keep: Option<&str>,
    at: &str,
) -> rusqlite::Result<()> {
    // With no kept device (`?3` NULL) every device and session goes,
    // browser sessions (no device) included.
    conn.execute(
        "UPDATE devices SET revoked_at = ?2
         WHERE user_id = ?1 AND revoked_at IS NULL AND (?3 IS NULL OR id <> ?3)",
        params![user_id, at, keep],
    )?;
    conn.execute(
        "UPDATE user_sessions SET revoked_at = ?2
         WHERE user_id = ?1 AND revoked_at IS NULL
           AND (?3 IS NULL OR device_id IS NULL OR device_id <> ?3)",
        params![user_id, at, keep],
    )?;
    Ok(())
}

/// Revokes every device and session of a user except the device
/// `keep_device` and the session `keep_session` (a password change made in
/// that session).
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn revoke_others(
    conn: &Connection,
    user_id: &str,
    keep_device: Option<&str>,
    keep_session: &str,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE devices SET revoked_at = ?2
         WHERE user_id = ?1 AND revoked_at IS NULL AND id <> ?3",
        params![user_id, at, keep_device.unwrap_or("")],
    )?;
    conn.execute(
        "UPDATE user_sessions SET revoked_at = ?2
         WHERE user_id = ?1 AND revoked_at IS NULL AND id <> ?3",
        params![user_id, at, keep_session],
    )?;
    Ok(())
}

/// Inserts a user session.
///
/// # Errors
///
/// Returns an error when the statement fails.
#[expect(clippy::too_many_arguments, reason = "one row's columns")]
pub fn insert_user_session(
    conn: &Connection,
    id: &str,
    user_id: &str,
    device_id: Option<&str>,
    kind: SessionKind,
    secret_sha256: &[u8],
    at: &str,
    expires_at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO user_sessions (id, user_id, device_id, kind, secret_sha256, created_at,
             expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            user_id,
            device_id,
            kind.as_str(),
            secret_sha256,
            at,
            expires_at
        ],
    )?;
    Ok(())
}

/// The user session with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_user_session(conn: &Connection, id: &str) -> rusqlite::Result<Option<UserSessionRow>> {
    conn.query_row(
        "SELECT id, user_id, device_id, kind, secret_sha256, expires_at, revoked_at
         FROM user_sessions WHERE id = ?1",
        [id],
        |row| {
            let kind: String = row.get(3)?;
            Ok(UserSessionRow {
                id: row.get(0)?,
                user_id: row.get(1)?,
                device_id: row.get(2)?,
                kind: if kind == "web" {
                    SessionKind::Web
                } else {
                    SessionKind::Api
                },
                secret_sha256: row.get(4)?,
                expires_at: row.get(5)?,
                revoked_at: row.get(6)?,
            })
        },
    )
    .optional()
}

/// Records the use of a user session.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn touch_user_session(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE user_sessions SET last_used_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Ends a user session.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn revoke_user_session(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE user_sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    Ok(())
}

/// The throttle's record for `name`, if any.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn throttle_of(conn: &Connection, name: &str) -> rusqlite::Result<Option<ThrottleRow>> {
    conn.query_row(
        "SELECT failures, locked_until FROM sign_in_throttle WHERE name = ?1",
        [name],
        |row| {
            Ok(ThrottleRow {
                failures: row.get(0)?,
                locked_until: row.get(1)?,
            })
        },
    )
    .optional()
}

/// Records a failed sign-in for `name`: counts it, and locks the name until
/// `lock_until` when given (resetting the count). Rows not touched since
/// `prune_before` are deleted first, and the table is kept to
/// [`THROTTLE_CAP`] rows.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn throttle_failure(
    conn: &Connection,
    name: &str,
    lock_until: Option<&str>,
    at: &str,
    prune_before: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM sign_in_throttle
         WHERE updated_at < ?1 AND (locked_until IS NULL OR locked_until < ?1)",
        [prune_before],
    )?;
    let (failures, locked) = match lock_until {
        Some(until) => (0, Some(until)),
        None => (
            throttle_of(conn, name)?.map_or(0, |row| row.failures) + 1,
            None,
        ),
    };
    conn.execute(
        "INSERT INTO sign_in_throttle (name, failures, locked_until, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (name) DO UPDATE SET failures = ?2, locked_until = ?3, updated_at = ?4",
        params![name, failures, locked, at],
    )?;
    let cap = i64::try_from(THROTTLE_CAP).unwrap_or(i64::MAX);
    conn.execute(
        "DELETE FROM sign_in_throttle WHERE name IN (
             SELECT name FROM sign_in_throttle ORDER BY updated_at DESC, name LIMIT -1 OFFSET ?1)",
        [cap],
    )?;
    Ok(())
}

/// Forgets the throttle's record for `name` (a success or an admin's
/// `unlock`).
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn throttle_clear(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM sign_in_throttle WHERE name = ?1", [name])?;
    Ok(())
}

/// How many rows the throttle holds.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn throttle_rows(conn: &Connection) -> rusqlite::Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM sign_in_throttle", [], |r| r.get(0))?;
    Ok(usize::try_from(count).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = super::super::open(&dir.path().join("joedocs.db")).unwrap();
        (dir, conn)
    }

    #[test]
    fn users_are_stored_with_unique_names_and_without_secrets_in_their_rows() {
        let (_dir, conn) = db();
        insert_user(
            &conn,
            "u1",
            "alice",
            "$argon2id$x",
            "2026-01-04T00:00:00Z",
            "t",
        )
        .unwrap();
        assert!(insert_user(&conn, "u2", "alice", "$argon2id$y", "u", "t").is_err());
        let row = find_user_by_name(&conn, "alice").unwrap().unwrap();
        assert_eq!(row.id, "u1");
        assert!(row.must_change_password);
        assert_eq!(
            row.initial_password_until.as_deref(),
            Some("2026-01-04T00:00:00Z")
        );
        assert_eq!(row.totp_enrolled_at, None);
        assert_eq!(find_user_by_id(&conn, "u1").unwrap().unwrap(), row);
        assert!(!format!("{row:?}").contains("argon2"));
        let secrets = user_secrets(&conn, "u1").unwrap().unwrap();
        assert_eq!(secrets.password_hash, "$argon2id$x");
        assert!(!format!("{secrets:?}").contains("argon2"));
        assert_eq!(list_users(&conn).unwrap().len(), 1);
    }

    #[test]
    fn setup_confirms_a_pending_secret_and_reset_clears_it() {
        let (_dir, conn) = db();
        insert_user(&conn, "u1", "alice", "h0", "u", "t").unwrap();
        start_setup(&conn, "u1", &[7; 32], Some(&[1, 2, 3]), "p", "t1").unwrap();
        let s = user_secrets(&conn, "u1").unwrap().unwrap();
        assert_eq!(s.totp_pending_secret.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(s.setup_ticket_sha256.as_deref(), Some(&[7; 32][..]));
        confirm_totp(&conn, "u1", &[1, 2, 3], 42, "t2").unwrap();
        set_password(&conn, "u1", "h1", "t2").unwrap();
        end_setup(&conn, "u1", "t2").unwrap();
        let s = user_secrets(&conn, "u1").unwrap().unwrap();
        assert_eq!(
            (
                s.totp_secret.as_deref(),
                s.totp_pending_secret,
                s.setup_ticket_sha256
            ),
            (Some(&[1, 2, 3][..]), None, None)
        );
        assert_eq!(
            (s.password_hash.as_str(), s.last_totp_step),
            ("h1", Some(42))
        );
        let row = find_user_by_id(&conn, "u1").unwrap().unwrap();
        assert!(!row.must_change_password);
        assert_eq!(row.initial_password_until, None);
        assert_eq!(row.totp_enrolled_at.as_deref(), Some("t2"));
        clear_totp(&conn, "u1", "t3").unwrap();
        set_initial_password(&conn, "u1", "h2", "u2", "t3").unwrap();
        let s = user_secrets(&conn, "u1").unwrap().unwrap();
        assert_eq!((s.totp_secret, s.last_totp_step), (None, None));
        assert!(
            find_user_by_id(&conn, "u1")
                .unwrap()
                .unwrap()
                .must_change_password
        );
    }

    #[test]
    fn removed_users_keep_their_names() {
        let (_dir, conn) = db();
        insert_user(&conn, "u1", "alice", "h", "u", "t").unwrap();
        set_user_disabled(&conn, "u1", Some("t1"), "t1").unwrap();
        assert_eq!(
            find_user_by_id(&conn, "u1")
                .unwrap()
                .unwrap()
                .disabled_at
                .as_deref(),
            Some("t1")
        );
        set_user_disabled(&conn, "u1", None, "t2").unwrap();
        set_user_removed(&conn, "u1", "t3").unwrap();
        assert_eq!(
            find_user_by_name(&conn, "alice")
                .unwrap()
                .unwrap()
                .removed_at
                .as_deref(),
            Some("t3")
        );
        assert!(insert_user(&conn, "u2", "alice", "h", "u", "t").is_err());
    }

    fn with_devices_and_sessions() -> (tempfile::TempDir, Connection) {
        let (dir, conn) = db();
        insert_user(&conn, "u1", "alice", "h", "u", "t").unwrap();
        insert_device(&conn, "d1", "u1", "laptop", &[1; 32], "t").unwrap();
        insert_device(&conn, "d2", "u1", "desktop", &[2; 32], "t").unwrap();
        for (id, device, kind) in [
            ("s1", Some("d1"), SessionKind::Api),
            ("s2", Some("d2"), SessionKind::Api),
            ("w1", None, SessionKind::Web),
        ] {
            insert_user_session(&conn, id, "u1", device, kind, &[3; 32], "t", "e").unwrap();
        }
        (dir, conn)
    }

    fn session_revoked(conn: &Connection, id: &str) -> bool {
        find_user_session(conn, id)
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some()
    }

    fn device_revoked(conn: &Connection, id: &str) -> bool {
        find_device(conn, id).unwrap().unwrap().revoked_at.is_some()
    }

    #[test]
    fn device_renewal_failures_count_and_lock() {
        let (_dir, conn) = with_devices_and_sessions();
        let s = find_user_session(&conn, "w1").unwrap().unwrap();
        assert_eq!((s.kind, s.device_id), (SessionKind::Web, None));
        record_device_failure(&conn, "d1", None).unwrap();
        assert_eq!(find_device(&conn, "d1").unwrap().unwrap().failed_codes, 1);
        record_device_failure(&conn, "d1", Some("later")).unwrap();
        let d = find_device(&conn, "d1").unwrap().unwrap();
        assert_eq!(
            (d.failed_codes, d.locked_until.as_deref()),
            (0, Some("later"))
        );
        touch_device(&conn, "d1", "t1").unwrap();
        let d = find_device(&conn, "d1").unwrap().unwrap();
        assert_eq!(
            (d.last_used_at.as_deref(), d.locked_until.as_deref()),
            (Some("t1"), Some("later")),
            "use alone keeps the lock"
        );
        renew_device(&conn, "d1", "t2").unwrap();
        let d = find_device(&conn, "d1").unwrap().unwrap();
        assert_eq!(
            (d.failed_codes, d.locked_until, d.last_used_at.as_deref()),
            (0, None, Some("t2"))
        );
    }

    #[test]
    fn devices_and_sessions_are_revoked_together() {
        let (_dir, conn) = with_devices_and_sessions();
        revoke_device(&conn, "d2", "t2").unwrap();
        assert!(device_revoked(&conn, "d2"));
        assert!(session_revoked(&conn, "s2"));
        assert!(!session_revoked(&conn, "s1"));

        revoke_all_of(&conn, "u1", Some("d1"), "t3").unwrap();
        assert!(session_revoked(&conn, "w1"));
        assert!(!session_revoked(&conn, "s1"));
        assert!(!device_revoked(&conn, "d1"));
        revoke_all_of(&conn, "u1", None, "t4").unwrap();
        assert!(device_revoked(&conn, "d1"));
        assert!(session_revoked(&conn, "s1"));
        assert_eq!(devices_of(&conn, "u1").unwrap().len(), 2);
    }

    #[test]
    fn revoking_all_without_a_kept_device_ends_browser_sessions() {
        let (_dir, conn) = with_devices_and_sessions();
        revoke_all_of(&conn, "u1", None, "t1").unwrap();
        for session in ["s1", "s2", "w1"] {
            assert!(session_revoked(&conn, session), "{session} is still live");
        }
        assert!(device_revoked(&conn, "d1"));
        assert!(device_revoked(&conn, "d2"));
    }

    #[test]
    fn the_throttle_counts_locks_prunes_and_is_capped() {
        let (_dir, conn) = db();
        throttle_failure(&conn, "alice", None, "2026-01-01T00:00:00Z", "2000").unwrap();
        throttle_failure(&conn, "alice", None, "2026-01-01T00:00:01Z", "2000").unwrap();
        assert_eq!(throttle_of(&conn, "alice").unwrap().unwrap().failures, 2);
        throttle_failure(
            &conn,
            "alice",
            Some("2026-01-01T00:15:00Z"),
            "2026-01-01T00:00:02Z",
            "2000",
        )
        .unwrap();
        let row = throttle_of(&conn, "alice").unwrap().unwrap();
        assert_eq!(
            (row.failures, row.locked_until.as_deref()),
            (0, Some("2026-01-01T00:15:00Z"))
        );
        throttle_failure(&conn, "nobody", None, "2026-01-01T00:00:03Z", "2000").unwrap();
        // A write at 00:20 prunes rows untouched since 00:05 whose lock has
        // ended: both.
        throttle_failure(
            &conn,
            "carol",
            None,
            "2026-01-01T00:20:00Z",
            "2026-01-01T00:16:00Z",
        )
        .unwrap();
        assert_eq!(throttle_of(&conn, "alice").unwrap(), None);
        assert_eq!(throttle_of(&conn, "nobody").unwrap(), None);
        assert_eq!(throttle_rows(&conn).unwrap(), 1);
        throttle_clear(&conn, "carol").unwrap();
        assert_eq!(throttle_rows(&conn).unwrap(), 0);

        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..THROTTLE_CAP {
            tx.execute(
                "INSERT INTO sign_in_throttle (name, failures, updated_at) VALUES (?1, 1, ?2)",
                params![format!("n{i}"), format!("2026-01-01T00:{:02}:00Z", i % 60)],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        throttle_failure(&conn, "newest", None, "2026-01-01T01:00:00Z", "2000").unwrap();
        assert_eq!(throttle_rows(&conn).unwrap(), THROTTLE_CAP);
        assert!(throttle_of(&conn, "newest").unwrap().is_some());
    }
}
