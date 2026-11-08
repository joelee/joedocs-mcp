//! Admin token and session queries.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

/// A stored admin token (never its secret).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminTokenRow {
    /// Public ID.
    pub id: String,
    /// Name, recorded in the audit log for every call made with it.
    pub name: String,
    /// Creation time.
    pub created_at: String,
    /// Last successful login or call, if any.
    pub last_used_at: Option<String>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<String>,
    /// Consecutive failed logins since the last success or lockout.
    pub failed_logins: u32,
    /// Logins are refused until this time, if locked.
    pub locked_until: Option<String>,
}

/// What login needs to check a token and a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSecrets {
    /// SHA-256 of the token secret.
    pub secret_sha256: Vec<u8>,
    /// The TOTP shared secret.
    pub totp_secret: Vec<u8>,
    /// The last TOTP step accepted, if any.
    pub last_totp_step: Option<i64>,
}

/// A stored admin session (never its secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Public ID.
    pub id: String,
    /// The admin token it belongs to.
    pub admin_token_id: String,
    /// SHA-256 of the session secret.
    pub secret_sha256: Vec<u8>,
    /// When it stops working.
    pub expires_at: String,
    /// Revocation time (logout or token revocation), if any.
    pub revoked_at: Option<String>,
}

const SELECT: &str = "SELECT id, name, created_at, last_used_at, revoked_at, failed_logins, locked_until FROM admin_tokens";

fn from_row(row: &Row<'_>) -> rusqlite::Result<AdminTokenRow> {
    Ok(AdminTokenRow {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_used_at: row.get(3)?,
        revoked_at: row.get(4)?,
        failed_logins: row.get(5)?,
        locked_until: row.get(6)?,
    })
}

/// Inserts an admin token.
///
/// # Errors
///
/// Returns an error when the ID or name is taken or the statement fails.
pub fn insert_admin_token(
    conn: &Connection,
    id: &str,
    name: &str,
    secret_sha256: &[u8],
    totp_secret: &[u8],
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO admin_tokens (id, name, secret_sha256, totp_secret, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, name, secret_sha256, totp_secret, at],
    )?;
    Ok(())
}

/// Every admin token, by name.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_admin_tokens(conn: &Connection) -> rusqlite::Result<Vec<AdminTokenRow>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name"))?;
    stmt.query_map([], from_row)?.collect()
}

/// The admin token with this name.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_admin_token_by_name(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<Option<AdminTokenRow>> {
    conn.query_row(&format!("{SELECT} WHERE name = ?1"), [name], from_row)
        .optional()
}

/// The admin token with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_admin_token_by_id(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<AdminTokenRow>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), [id], from_row)
        .optional()
}

/// The secrets login checks, for the admin token with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn admin_secrets(conn: &Connection, id: &str) -> rusqlite::Result<Option<AdminSecrets>> {
    conn.query_row(
        "SELECT secret_sha256, totp_secret, last_totp_step FROM admin_tokens WHERE id = ?1",
        [id],
        |row| {
            Ok(AdminSecrets {
                secret_sha256: row.get(0)?,
                totp_secret: row.get(1)?,
                last_totp_step: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Records a failed login: counts it, and locks the token until
/// `lock_until` when given (resetting the count).
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn record_failed_login(
    conn: &Connection,
    id: &str,
    lock_until: Option<&str>,
) -> rusqlite::Result<()> {
    if let Some(until) = lock_until {
        conn.execute(
            "UPDATE admin_tokens SET failed_logins = 0, locked_until = ?2 WHERE id = ?1",
            params![id, until],
        )?;
    } else {
        conn.execute(
            "UPDATE admin_tokens SET failed_logins = failed_logins + 1 WHERE id = ?1",
            [id],
        )?;
    }
    Ok(())
}

/// Records a successful login at `step`: resets the failure count and the
/// lock, and remembers the step so its code cannot be used again.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn record_login(conn: &Connection, id: &str, step: i64, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE admin_tokens SET failed_logins = 0, locked_until = NULL, last_totp_step = ?2,
                last_used_at = ?3 WHERE id = ?1",
        params![id, step, at],
    )?;
    Ok(())
}

/// Revokes an admin token (if it is not already) and all its sessions.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn revoke_admin_token(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE admin_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    conn.execute(
        "UPDATE admin_sessions SET revoked_at = ?2 WHERE admin_token_id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    Ok(())
}

/// Inserts a session.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn insert_session(
    conn: &Connection,
    id: &str,
    admin_token_id: &str,
    secret_sha256: &[u8],
    at: &str,
    expires_at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO admin_sessions (id, admin_token_id, secret_sha256, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, admin_token_id, secret_sha256, at, expires_at],
    )?;
    Ok(())
}

/// The session with this ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_session(conn: &Connection, id: &str) -> rusqlite::Result<Option<SessionRow>> {
    conn.query_row(
        "SELECT id, admin_token_id, secret_sha256, expires_at, revoked_at FROM admin_sessions WHERE id = ?1",
        [id],
        |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                admin_token_id: row.get(1)?,
                secret_sha256: row.get(2)?,
                expires_at: row.get(3)?,
                revoked_at: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Records the use of a session and its admin token.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn touch_session(conn: &Connection, session: &SessionRow, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE admin_sessions SET last_used_at = ?2 WHERE id = ?1",
        params![session.id, at],
    )?;
    conn.execute(
        "UPDATE admin_tokens SET last_used_at = ?2 WHERE id = ?1",
        params![session.admin_token_id, at],
    )?;
    Ok(())
}

/// Ends a session.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn revoke_session(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE admin_sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    Ok(())
}
