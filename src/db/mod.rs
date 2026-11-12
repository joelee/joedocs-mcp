//! SQLite access: connection setup, embedded migrations versioned with
//! `PRAGMA user_version`, and queries for projects, API keys, documents and
//! revisions.
//!
//! Queries are free functions over a [`Connection`], so callers can run
//! them inside a transaction (which dereferences to a connection). See
//! `docs/plan.md` section 10.

mod admin;
mod documents;
mod files;
mod keys;
mod projects;
mod search;
mod users;

use std::path::Path;

pub use admin::{
    AdminSecrets, AdminTokenRow, SessionKind, SessionRow, admin_secrets, find_admin_token_by_id,
    find_admin_token_by_name, find_session, insert_admin_token, insert_session, list_admin_tokens,
    record_failed_login, record_login, revoke_admin_token, revoke_session, touch_session,
};
pub use documents::{
    DocumentRow, ListQuery, RevisionRow, Sort, archived_documents, document_counts, find_by_title,
    find_files, get_document, get_revision, insert_document, insert_revision, last_change_at,
    list_documents, list_revisions, live_documents, live_documents_in, path_taken, save_document,
};
pub use files::{
    FileChange, FileRow, RevisionFileRow, count_live_files, file_by_id, files_of, find_live_file,
    insert_file, insert_revision_file, live_files, revision_files, save_file,
};
pub use keys::{
    KeyRow, find_key_by_id, find_key_by_name, insert_key, key_secret, list_keys, remove_grant,
    revoke_key, set_grant, set_key_secret, touch_key,
};
pub use projects::{
    ProjectRow, allocate_seq, find_project, find_project_by_id, insert_project, list_projects,
    raise_next_seq,
};
pub use rusqlite::{Connection, Error as SqlError};
use rusqlite::{OptionalExtension as _, params};
pub use search::{
    SearchQuery, SearchRow, clear_documents, file_fts_rowid, fts_rowid, index_document, index_file,
    search_documents, unindex_document, unindex_file,
};
pub use users::{
    DeviceRow, THROTTLE_CAP, ThrottleRow, UserRow, UserSecrets, UserSessionRow, clear_totp,
    confirm_totp, devices_of, end_setup, find_device, find_user_by_id, find_user_by_name,
    find_user_session, insert_device, insert_user, insert_user_session, list_users,
    record_device_failure, record_user_login, rehash_password, revoke_all_of, revoke_device,
    revoke_others, revoke_user_session, set_initial_password, set_password, set_user_disabled,
    set_user_removed, start_setup, throttle_clear, throttle_failure, throttle_of, throttle_rows,
    touch_device, touch_user_session, user_secrets,
};

/// Schema migrations, applied in order. The schema version
/// (`PRAGMA user_version`) is the number applied.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_core.sql"),
    include_str!("migrations/0002_audit.sql"),
    include_str!("migrations/0003_search.sql"),
    include_str!("migrations/0004_admin.sql"),
    include_str!("migrations/0005_files.sql"),
    include_str!("migrations/0006_web_sessions.sql"),
    include_str!("migrations/0007_users.sql"),
];

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

/// One audited tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry<'a> {
    /// Time of the call.
    pub at: &'a str,
    /// API key ID.
    pub api_key_id: &'a str,
    /// API key name.
    pub key_name: &'a str,
    /// MCP client name.
    pub client: &'a str,
    /// Tool name.
    pub tool: &'a str,
    /// Document the call targeted, if any.
    pub document_id: Option<&'a str>,
    /// `ok` or an error code.
    pub outcome: &'a str,
}

/// Appends an audit entry.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn insert_audit(conn: &Connection, entry: &AuditEntry<'_>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit_log (at, api_key_id, key_name, client, tool, document_id, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            entry.at,
            entry.api_key_id,
            entry.key_name,
            entry.client,
            entry.tool,
            entry.document_id,
            entry.outcome
        ],
    )?;
    Ok(())
}

/// Appends an audit entry for an admin API or web interface call, naming
/// the signed-in user when there is one (migration 7). MCP calls use
/// [`insert_audit`], which leaves the user columns empty.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn insert_admin_audit(
    conn: &Connection,
    entry: &AuditEntry<'_>,
    user: Option<(&str, &str)>,
) -> rusqlite::Result<()> {
    let (user_id, user_name) = user.unzip();
    conn.execute(
        "INSERT INTO audit_log (at, api_key_id, key_name, client, tool, document_id, outcome,
             user_id, user_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            entry.at,
            entry.api_key_id,
            entry.key_name,
            entry.client,
            entry.tool,
            entry.document_id,
            entry.outcome,
            user_id,
            user_name
        ],
    )?;
    Ok(())
}

/// Conditions of an audit log read; `None` means any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery<'a> {
    /// Only entries at or after this time (`YYYY-MM-DDTHH:MM:SSZ`).
    pub since: Option<&'a str>,
    /// Only entries before this time.
    pub until: Option<&'a str>,
    /// Only this key name.
    pub key_name: Option<&'a str>,
    /// Only this tool.
    pub tool: Option<&'a str>,
    /// Only tools that start with this text (matched exactly, case
    /// included).
    pub tool_prefix: Option<&'a str>,
    /// Only this document.
    pub document_id: Option<&'a str>,
    /// Only this outcome (`ok` or an error code).
    pub outcome: Option<&'a str>,
    /// Every outcome but this one (`ok` for the refusals).
    pub outcome_not: Option<&'a str>,
    /// Entries to skip.
    pub offset: u64,
    /// Most entries to return.
    pub limit: u64,
}

/// One stored audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// Time of the call.
    pub at: String,
    /// API key or admin token ID.
    pub api_key_id: String,
    /// Key or token name.
    pub key_name: String,
    /// Client name.
    pub client: String,
    /// Tool name, `admin:<operation>` for the admin API.
    pub tool: String,
    /// Document the call targeted, if any.
    pub document_id: Option<String>,
    /// `ok` or an error code.
    pub outcome: String,
    /// The signed-in user who made an admin call, if one did (0.5.1).
    pub user_name: Option<String>,
}

/// A page of audit entries, newest first, and how many match.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_audit(
    conn: &Connection,
    query: &AuditQuery<'_>,
) -> rusqlite::Result<(Vec<AuditRow>, u64)> {
    let filter = "WHERE (?1 IS NULL OR at >= ?1) AND (?2 IS NULL OR at < ?2)
        AND (?3 IS NULL OR key_name = ?3) AND (?4 IS NULL OR tool = ?4)
        AND (?5 IS NULL OR substr(tool, 1, length(?5)) = ?5)
        AND (?6 IS NULL OR document_id = ?6) AND (?7 IS NULL OR outcome = ?7)
        AND (?8 IS NULL OR outcome <> ?8)";
    let conditions = params![
        query.since,
        query.until,
        query.key_name,
        query.tool,
        query.tool_prefix,
        query.document_id,
        query.outcome,
        query.outcome_not,
    ];
    let total = conn.query_row(
        &format!("SELECT count(*) FROM audit_log {filter}"),
        conditions,
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT at, api_key_id, key_name, client, tool, document_id, outcome, user_name
         FROM audit_log {filter} ORDER BY at DESC, rowid DESC LIMIT ?9 OFFSET ?10"
    ))?;
    let rows = stmt
        .query_map(
            params![
                query.since,
                query.until,
                query.key_name,
                query.tool,
                query.tool_prefix,
                query.document_id,
                query.outcome,
                query.outcome_not,
                query.limit,
                query.offset,
            ],
            |row| {
                Ok(AuditRow {
                    at: row.get(0)?,
                    api_key_id: row.get(1)?,
                    key_name: row.get(2)?,
                    client: row.get(3)?,
                    tool: row.get(4)?,
                    document_id: row.get(5)?,
                    outcome: row.get(6)?,
                    user_name: row.get(7)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
}

/// Meta key holding the history commit the index was last updated to.
const HISTORY_HEAD: &str = "history_head";

/// Meta key holding the [`INDEX_FORMAT`] the index was last rebuilt with.
const INDEX_FORMAT_KEY: &str = "index_format";

/// The layout of the rebuildable index (documents, revisions and search).
/// A database recording another value is rebuilt by `joedocs reindex`,
/// which `serve` runs on start. Bump it when the index gains data that
/// only a rebuild can fill in.
pub const INDEX_FORMAT: &str = "2";

fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// The history commit the index reflects, or `None` before the first.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn history_head(conn: &Connection) -> rusqlite::Result<Option<String>> {
    meta(conn, HISTORY_HEAD)
}

/// Records the history commit the index now reflects.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_history_head(conn: &Connection, commit: &str) -> rusqlite::Result<()> {
    set_meta(conn, HISTORY_HEAD, commit)
}

/// Forgets the history commit, for an index rebuilt from an empty history.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn clear_history_head(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [HISTORY_HEAD])?;
    Ok(())
}

/// Whether the index was built with this build's [`INDEX_FORMAT`].
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn index_format_current(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(meta(conn, INDEX_FORMAT_KEY)?.as_deref() == Some(INDEX_FORMAT))
}

/// Records that the index matches this build's [`INDEX_FORMAT`].
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn set_index_format(conn: &Connection) -> rusqlite::Result<()> {
    set_meta(conn, INDEX_FORMAT_KEY, INDEX_FORMAT)
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
    fn a_version_4_database_migrates_to_multi_file_documents_with_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joedocs.db");
        let mut conn = Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        for sql in &MIGRATIONS[..4] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 4).unwrap();
        tx.execute_batch(
            "INSERT INTO projects (id, name, key, title, created_at)
                 VALUES (1, 'joedocs', 'JD', 'JoeDocs', 't');
             INSERT INTO documents (id, project_id, seq, type, title, path, status, revision,
                 created_at, created_by, updated_at, updated_by, client, content_sha256)
                 VALUES ('JD-1', 1, 1, 'plans', 'Plan', 'joedocs/plans/Plan.md', 'draft', 3,
                 't', 'laptop', 'u', 'laptop', 'test', x'00');
             INSERT INTO doc_fields (document_id, key, value) VALUES ('JD-1', 'status', 'draft');",
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let row = get_document(&conn, "JD-1").unwrap().unwrap();
        assert_eq!(
            (row.revision, row.main_revision, row.multifile),
            (3, 3, false)
        );
        let fields: (String, Option<i64>) = conn
            .query_row("SELECT value, file_id FROM doc_fields", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(fields, ("draft".to_owned(), None));
        assert_eq!(live_files(&conn, "JD-1").unwrap(), []);
        assert!(!index_format_current(&conn).unwrap(), "a reindex follows");
    }

    #[test]
    fn a_version_5_database_gains_session_kinds_with_existing_sessions_as_api() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joedocs.db");
        let mut conn = Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        for sql in &MIGRATIONS[..5] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 5).unwrap();
        tx.execute_batch(
            "INSERT INTO admin_tokens (id, name, secret_sha256, totp_secret, created_at)
                 VALUES ('a1', 'laptop', x'00', x'00', 't');
             INSERT INTO admin_sessions (id, admin_token_id, secret_sha256, created_at, expires_at)
                 VALUES ('s1', 'a1', x'00', 't', 'u');",
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let session = find_session(&conn, "s1").unwrap().unwrap();
        assert_eq!(session.kind, SessionKind::Api);
        assert!(
            conn.execute(
                "UPDATE admin_sessions SET kind = 'other' WHERE id = 's1'",
                []
            )
            .is_err(),
            "kind is api or web"
        );
    }

    #[test]
    fn a_version_6_database_gains_users_with_its_tokens_sessions_keys_and_audit_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("joedocs.db");
        let mut conn = Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        for sql in &MIGRATIONS[..6] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 6).unwrap();
        tx.execute_batch(
            "INSERT INTO projects (id, name, key, title, created_at)
                 VALUES (1, 'joedocs', 'JD', 'JoeDocs', 't');
             INSERT INTO api_keys (id, name, secret_sha256, created_at)
                 VALUES ('k1', 'laptop', x'01', 't');
             INSERT INTO grants (api_key_id, project_id, permission) VALUES ('k1', 1, 'write');
             INSERT INTO admin_tokens (id, name, secret_sha256, totp_secret, created_at)
                 VALUES ('a1', 'ops', x'00', x'00', 't');
             INSERT INTO admin_sessions (id, admin_token_id, secret_sha256, created_at, expires_at, kind)
                 VALUES ('s1', 'a1', x'00', 't', 'u', 'api'), ('w1', 'a1', x'00', 't', 'u', 'web');
             INSERT INTO audit_log (at, api_key_id, key_name, client, tool, document_id, outcome)
                 VALUES ('t', 'k1', 'laptop', 'c', 'new_document', 'JD-1', 'ok');",
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 7);
        assert_eq!(find_key_by_name(&conn, "laptop").unwrap().unwrap().id, "k1");
        assert_eq!(
            find_admin_token_by_name(&conn, "ops").unwrap().unwrap().id,
            "a1"
        );
        assert_eq!(
            find_session(&conn, "s1").unwrap().unwrap().kind,
            SessionKind::Api
        );
        assert_eq!(
            find_session(&conn, "w1").unwrap().unwrap().kind,
            SessionKind::Web
        );
        let audit: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT key_name, user_id, user_name FROM audit_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(audit, ("laptop".to_owned(), None, None));
        assert_eq!(list_users(&conn).unwrap(), []);
    }

    #[test]
    fn admin_audit_entries_name_the_user_and_mcp_entries_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("joedocs.db")).unwrap();
        let entry = |key: &'static str| AuditEntry {
            at: "t",
            api_key_id: key,
            key_name: key,
            client: "c",
            tool: "admin:status",
            document_id: None,
            outcome: "ok",
        };
        insert_audit(&conn, &entry("k1")).unwrap();
        insert_admin_audit(&conn, &entry("a1"), None).unwrap();
        insert_admin_audit(&conn, &entry("u1"), Some(("u1", "alice"))).unwrap();
        let mut stmt = conn
            .prepare("SELECT api_key_id, user_id, user_name FROM audit_log ORDER BY rowid")
            .unwrap();
        let rows: Vec<(String, Option<String>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                ("k1".to_owned(), None, None),
                ("a1".to_owned(), None, None),
                (
                    "u1".to_owned(),
                    Some("u1".to_owned()),
                    Some("alice".to_owned())
                ),
            ]
        );
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
    fn audit_entries_are_stored() {
        let conn = open_in_memory().unwrap();
        let entry = AuditEntry {
            at: "t",
            api_key_id: "k",
            key_name: "laptop",
            client: "c",
            tool: "update_document",
            document_id: Some("JD-1"),
            outcome: "conflict",
        };
        insert_audit(&conn, &entry).unwrap();
        let outcome: String = conn
            .query_row("SELECT outcome FROM audit_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outcome, "conflict");
    }

    /// An in-memory database with five audit entries over three days.
    fn audit_fixture() -> Connection {
        let conn = open_in_memory().unwrap();
        for (at, key, tool, doc, outcome) in [
            (
                "2026-11-09T10:00:00Z",
                "laptop",
                "new_document",
                Some("JD-1"),
                "ok",
            ),
            (
                "2026-11-10T00:00:00Z",
                "laptop",
                "update_document",
                Some("JD-1"),
                "conflict",
            ),
            ("2026-11-10T09:00:00Z", "admin", "admin:status", None, "ok"),
            ("2026-11-10T12:00:00Z", "ci", "admin:x", None, "ok"),
            (
                "2026-11-11T00:00:00Z",
                "laptop",
                "adm_n:y",
                Some("JD-2"),
                "ok",
            ),
        ] {
            let entry = AuditEntry {
                at,
                api_key_id: "id",
                key_name: key,
                client: "c",
                tool,
                document_id: doc,
                outcome,
            };
            insert_audit(&conn, &entry).unwrap();
        }
        conn
    }

    #[test]
    fn audit_entries_are_read_newest_first_with_filters() {
        let conn = audit_fixture();
        let read = |query: &AuditQuery<'_>| {
            let (rows, total) = list_audit(&conn, query).unwrap();
            (rows.into_iter().map(|r| r.tool).collect::<Vec<_>>(), total)
        };
        let all = AuditQuery {
            limit: 10,
            ..AuditQuery::default()
        };
        assert_eq!(
            read(&all),
            (
                vec![
                    "adm_n:y".into(),
                    "admin:x".into(),
                    "admin:status".into(),
                    "update_document".into(),
                    "new_document".into()
                ],
                5
            )
        );
        let day = AuditQuery {
            since: Some("2026-11-10T00:00:00Z"),
            until: Some("2026-11-11T00:00:00Z"),
            ..all.clone()
        };
        assert_eq!(read(&day).1, 3, "since is inclusive, until exclusive");
        let prefix = AuditQuery {
            tool_prefix: Some("admin:"),
            ..all.clone()
        };
        assert_eq!(read(&prefix).0, ["admin:x", "admin:status"]);
        let literal = AuditQuery {
            tool_prefix: Some("adm_n:"),
            ..all.clone()
        };
        assert_eq!(read(&literal).0, ["adm_n:y"], "`_` is literal");
        let percent = AuditQuery {
            tool_prefix: Some("%"),
            ..all.clone()
        };
        assert_eq!(read(&percent).1, 0, "`%` is literal");
        let upper = AuditQuery {
            tool_prefix: Some("ADMIN:"),
            ..all.clone()
        };
        assert_eq!(read(&upper).1, 0, "prefixes match case");
        let exact = AuditQuery {
            tool: Some("admin:status"),
            key_name: Some("admin"),
            outcome: Some("ok"),
            ..all.clone()
        };
        assert_eq!(read(&exact).0, ["admin:status"]);
        let by_document = AuditQuery {
            document_id: Some("JD-1"),
            ..all.clone()
        };
        assert_eq!(read(&by_document).0, ["update_document", "new_document"]);
        let refused = AuditQuery {
            outcome_not: Some("ok"),
            ..all.clone()
        };
        assert_eq!(
            read(&refused).0,
            ["update_document"],
            "every outcome but ok"
        );
        let page = AuditQuery {
            offset: 1,
            limit: 2,
            ..all
        };
        assert_eq!(
            read(&page),
            (vec!["admin:x".into(), "admin:status".into()], 5)
        );
        let (rows, _) = list_audit(&conn, &by_document).unwrap();
        assert_eq!(rows[0].document_id.as_deref(), Some("JD-1"));
        assert_eq!(rows[0].key_name, "laptop");
    }

    #[test]
    fn history_head_round_trips() {
        let conn = open_in_memory().unwrap();
        assert_eq!(history_head(&conn).unwrap(), None);
        set_history_head(&conn, "abc").unwrap();
        set_history_head(&conn, "def").unwrap();
        assert_eq!(history_head(&conn).unwrap().as_deref(), Some("def"));
        clear_history_head(&conn).unwrap();
        assert_eq!(history_head(&conn).unwrap(), None);
    }

    #[test]
    fn the_index_format_is_recorded() {
        let conn = open_in_memory().unwrap();
        assert!(!index_format_current(&conn).unwrap());
        set_index_format(&conn).unwrap();
        assert!(index_format_current(&conn).unwrap());
        conn.execute("UPDATE meta SET value = '0' WHERE key = 'index_format'", [])
            .unwrap();
        assert!(!index_format_current(&conn).unwrap());
    }
}
