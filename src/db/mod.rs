//! SQLite access: connection setup, embedded migrations versioned with
//! `PRAGMA user_version`, and queries for projects, API keys, documents and
//! revisions.
//!
//! Queries are free functions over a [`Connection`], so callers can run
//! them inside a transaction (which dereferences to a connection). See
//! `docs/plan.md` section 10.

mod documents;
mod keys;
mod projects;

use std::path::Path;

pub use documents::{
    DocumentRow, ListQuery, RevisionRow, Sort, get_document, get_revision, insert_document,
    insert_revision, list_documents, path_taken, save_document,
};
pub use keys::{KeyRow, find_key_by_name, insert_key, list_keys, revoke_key, set_grant};
pub use projects::{
    ProjectRow, allocate_seq, find_project, find_project_by_id, insert_project, list_projects,
};
pub use rusqlite::{Connection, Error as SqlError};
use rusqlite::{OptionalExtension as _, params};

/// Schema migrations, applied in order. The schema version
/// (`PRAGMA user_version`) is the number applied.
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_core.sql")];

/// The schema version this build expects.
pub const SCHEMA_VERSION: usize = MIGRATIONS.len();

/// A database operation failed.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a newer version of this program.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    NewerSchema {
        /// Version found in the file.
        found: usize,
        /// Latest version this build knows.
        supported: usize,
    },
}

/// Opens (creating if needed) the database at `path` and migrates it.
///
/// # Errors
///
/// Returns an error when SQLite cannot open or migrate the file.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    configure(conn)
}

/// Opens an in-memory database, migrated. For tests.
///
/// # Errors
///
/// Returns an error when SQLite fails.
pub fn open_in_memory() -> Result<Connection, DbError> {
    configure(Connection::open_in_memory()?)
}

fn configure(mut conn: Connection) -> Result<Connection, DbError> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let found = usize::try_from(found).unwrap_or(usize::MAX);
    if found > SCHEMA_VERSION {
        return Err(DbError::NewerSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Meta key holding the history commit the index was last updated to.
const HISTORY_HEAD: &str = "history_head";

/// The history commit the index reflects, or `None` before the first.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn history_head(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [HISTORY_HEAD],
        |row| row.get(0),
    )
    .optional()
}

/// Records the history commit the index now reflects.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_history_head(conn: &Connection, commit: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![HISTORY_HEAD, commit],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_set_the_schema_version_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joedocs.db");
        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(conn);
        open(&path).unwrap();
    }

    #[test]
    fn newer_schemas_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joedocs.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let err = open(&path).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "database schema version 99 is newer than this build supports ({SCHEMA_VERSION})"
            )
        );
    }

    #[test]
    fn unopenable_paths_are_sqlite_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = open(&dir.path().join("missing/joedocs.db")).unwrap_err();
        assert!(err.to_string().starts_with("database error: "), "{err}");
    }

    #[test]
    fn history_head_round_trips() {
        let conn = open_in_memory().unwrap();
        assert_eq!(history_head(&conn).unwrap(), None);
        set_history_head(&conn, "abc").unwrap();
        set_history_head(&conn, "def").unwrap();
        assert_eq!(history_head(&conn).unwrap().as_deref(), Some("def"));
    }
}
