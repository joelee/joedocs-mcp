//! API key and grant queries.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

use crate::auth::{Grant, Permission};

/// A stored API key (never the secret itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    /// Public ID.
    pub id: String,
    /// Name, used as the author of every change made with the key.
    pub name: String,
    /// Creation time.
    pub created_at: String,
    /// Last successful authentication, if any.
    pub last_used_at: Option<String>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<String>,
    /// Project grants, by project name.
    pub grants: Vec<Grant>,
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<KeyRow> {
    Ok(KeyRow {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_used_at: row.get(3)?,
        revoked_at: row.get(4)?,
        grants: Vec::new(),
    })
}

const SELECT: &str = "SELECT id, name, created_at, last_used_at, revoked_at FROM api_keys";

fn with_grants(conn: &Connection, mut key: KeyRow) -> rusqlite::Result<KeyRow> {
    let mut stmt = conn.prepare(
        "SELECT p.name, g.permission FROM grants g JOIN projects p ON p.id = g.project_id
         WHERE g.api_key_id = ?1 ORDER BY p.name",
    )?;
    let rows = stmt.query_map([&key.id], |row| {
        let project: String = row.get(0)?;
        let permission: String = row.get(1)?;
        Ok((project, permission))
    })?;
    for row in rows {
        let (project, permission) = row?;
        let permission = if permission == "write" {
            Permission::Write
        } else {
            Permission::Read
        };
        key.grants.push(Grant {
            project,
            permission,
        });
    }
    Ok(key)
}

/// Inserts a key.
///
/// # Errors
///
/// Returns an error when the ID or name is taken or the statement fails.
pub fn insert_key(
    conn: &Connection,
    id: &str,
    name: &str,
    secret_sha256: &[u8],
    created_at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO api_keys (id, name, secret_sha256, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, secret_sha256, created_at],
    )?;
    Ok(())
}

/// Every key with its grants, by name.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_keys(conn: &Connection) -> rusqlite::Result<Vec<KeyRow>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name"))?;
    let keys = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    keys.into_iter().map(|key| with_grants(conn, key)).collect()
}

/// The key with this name, with its grants.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_key_by_name(conn: &Connection, name: &str) -> rusqlite::Result<Option<KeyRow>> {
    let key = conn
        .query_row(&format!("{SELECT} WHERE name = ?1"), [name], from_row)
        .optional()?;
    key.map(|key| with_grants(conn, key)).transpose()
}

/// The key with this public ID, with its grants.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_key_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<KeyRow>> {
    let key = conn
        .query_row(&format!("{SELECT} WHERE id = ?1"), [id], from_row)
        .optional()?;
    key.map(|key| with_grants(conn, key)).transpose()
}

/// The stored SHA-256 of a key's secret.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn key_secret(conn: &Connection, id: &str) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.query_row(
        "SELECT secret_sha256 FROM api_keys WHERE id = ?1",
        [id],
        |row| row.get(0),
    )
    .optional()
}

/// Records a successful authentication.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn touch_key(conn: &Connection, id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Grants `permission` on a project, replacing any earlier grant there.
///
/// # Errors
///
/// Returns an error when the key or project does not exist.
pub fn set_grant(
    conn: &Connection,
    key_id: &str,
    project_id: i64,
    permission: Permission,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO grants (api_key_id, project_id, permission) VALUES (?1, ?2, ?3)
         ON CONFLICT (api_key_id, project_id) DO UPDATE SET permission = excluded.permission",
        params![key_id, project_id, permission.as_str()],
    )?;
    Ok(())
}

/// Marks a key revoked at `at`, unless it already is.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn revoke_key(conn: &Connection, key_id: &str, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![key_id, at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{insert_project, open_in_memory};

    #[test]
    fn keys_carry_their_grants_and_can_be_revoked() {
        let conn = open_in_memory().unwrap();
        let jd = insert_project(&conn, "joedocs", "JD", "JoeDocs", "t").unwrap();
        let al = insert_project(&conn, "alpha", "AL", "Alpha", "t").unwrap();
        insert_key(&conn, "k1", "laptop", &[1; 32], "t0").unwrap();
        set_grant(&conn, "k1", jd, Permission::Read).unwrap();
        set_grant(&conn, "k1", jd, Permission::Write).unwrap();
        set_grant(&conn, "k1", al, Permission::Read).unwrap();
        assert!(set_grant(&conn, "nope", al, Permission::Read).is_err());

        let key = find_key_by_name(&conn, "laptop").unwrap().unwrap();
        let grants: Vec<_> = key.grants.iter().map(ToString::to_string).collect();
        assert_eq!(grants, ["alpha:read", "joedocs:write"]);

        assert_eq!(find_key_by_id(&conn, "k1").unwrap(), Some(key));
        assert_eq!(key_secret(&conn, "k1").unwrap(), Some(vec![1; 32]));
        assert_eq!(key_secret(&conn, "nope").unwrap(), None);
        touch_key(&conn, "k1", "t9").unwrap();
        revoke_key(&conn, "k1", "t1").unwrap();
        revoke_key(&conn, "k1", "t2").unwrap();
        let keys = list_keys(&conn).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].revoked_at.as_deref(), Some("t1"));
        assert_eq!(keys[0].last_used_at.as_deref(), Some("t9"));
        assert_eq!(find_key_by_name(&conn, "nope").unwrap(), None);
        assert!(insert_key(&conn, "k2", "laptop", &[2; 32], "t").is_err());
    }
}
