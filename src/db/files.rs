//! Queries for the files of multi-file documents: `document_files` (the
//! sub-files, live and archived) and `revision_files` (which files each
//! revision changed).

use rusqlite::{Connection, OptionalExtension as _, Row, params};

/// One sub-file of a multi-file document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Row ID; also keys the file's full-text entry. Ignored by
    /// [`insert_file`], which returns the new one.
    pub id: i64,
    /// The document's ID.
    pub document_id: String,
    /// Path inside the document folder, without `.md`.
    pub path: String,
    /// The document revision at which the file last changed.
    pub revision: u64,
    /// Creation time.
    pub created_at: String,
    /// Creating key name.
    pub created_by: String,
    /// Time of the file's latest change.
    pub updated_at: String,
    /// Key name of the file's latest change.
    pub updated_by: String,
    /// MCP client of the file's latest change.
    pub client: String,
    /// SHA-256 of the file as last written.
    pub content_sha256: Vec<u8>,
    /// Archive time, if archived.
    pub archived_at: Option<String>,
    /// Key name that archived it.
    pub archived_by: Option<String>,
}

/// How a revision changed one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    /// The file was created.
    Added,
    /// Its content or front matter changed.
    Modified,
    /// It moved from another path (and may have changed too).
    Renamed,
    /// It was archived (removed from the vault).
    Removed,
}

impl FileChange {
    /// The stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Removed => "removed",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "added" => Ok(Self::Added),
            "modified" => Ok(Self::Modified),
            "renamed" => Ok(Self::Renamed),
            "removed" => Ok(Self::Removed),
            other => Err(rusqlite::Error::InvalidColumnType(
                0,
                format!("file change {other}"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

/// One file changed by one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionFileRow {
    /// The document's ID.
    pub document_id: String,
    /// The document revision.
    pub revision: u64,
    /// The file's path after the change, without `.md`; for the main file,
    /// its stem at that revision.
    pub path: String,
    /// Whether this is the main file.
    pub main: bool,
    /// What happened to it.
    pub change: FileChange,
    /// The path before a rename.
    pub from_path: Option<String>,
}

const FILE_COLUMNS: &str = "id, document_id, path, revision, created_at, created_by, updated_at,
    updated_by, client, content_sha256, archived_at, archived_by";

fn file_from_row(row: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: row.get(0)?,
        document_id: row.get(1)?,
        path: row.get(2)?,
        revision: row.get(3)?,
        created_at: row.get(4)?,
        created_by: row.get(5)?,
        updated_at: row.get(6)?,
        updated_by: row.get(7)?,
        client: row.get(8)?,
        content_sha256: row.get(9)?,
        archived_at: row.get(10)?,
        archived_by: row.get(11)?,
    })
}

/// Inserts a sub-file and returns its row ID.
///
/// # Errors
///
/// Returns an error when a live file of the document already has the path
/// (ignoring ASCII case), or the statement fails.
pub fn insert_file(conn: &Connection, file: &FileRow) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO document_files (document_id, path, revision, created_at, created_by,
            updated_at, updated_by, client, content_sha256, archived_at, archived_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            file.document_id,
            file.path,
            file.revision,
            file.created_at,
            file.created_by,
            file.updated_at,
            file.updated_by,
            file.client,
            file.content_sha256,
            file.archived_at,
            file.archived_by,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Writes every mutable column of an existing sub-file row (by `id`).
///
/// # Errors
///
/// Returns an error when the live path is taken or the statement fails.
pub fn save_file(conn: &Connection, file: &FileRow) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE document_files SET document_id = ?2, path = ?3, revision = ?4, updated_at = ?5,
            updated_by = ?6, client = ?7, content_sha256 = ?8, archived_at = ?9,
            archived_by = ?10
         WHERE id = ?1",
        params![
            file.id,
            file.document_id,
            file.path,
            file.revision,
            file.updated_at,
            file.updated_by,
            file.client,
            file.content_sha256,
            file.archived_at,
            file.archived_by,
        ],
    )?;
    Ok(())
}

/// The live sub-files of a document, by path.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn live_files(conn: &Connection, document_id: &str) -> rusqlite::Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLUMNS} FROM document_files
         WHERE document_id = ?1 AND archived_at IS NULL ORDER BY path"
    ))?;
    stmt.query_map([document_id], file_from_row)?.collect()
}

/// Every sub-file a document has had, live and archived, oldest first.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn files_of(conn: &Connection, document_id: &str) -> rusqlite::Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLUMNS} FROM document_files WHERE document_id = ?1 ORDER BY id"
    ))?;
    stmt.query_map([document_id], file_from_row)?.collect()
}

/// The live sub-file of a document at `path`, compared without ASCII case.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_live_file(
    conn: &Connection,
    document_id: &str,
    path: &str,
) -> rusqlite::Result<Option<FileRow>> {
    conn.query_row(
        &format!(
            "SELECT {FILE_COLUMNS} FROM document_files
             WHERE document_id = ?1 AND path = ?2 COLLATE NOCASE AND archived_at IS NULL"
        ),
        params![document_id, path],
        file_from_row,
    )
    .optional()
}

/// The sub-file with this row ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn file_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<FileRow>> {
    conn.query_row(
        &format!("SELECT {FILE_COLUMNS} FROM document_files WHERE id = ?1"),
        [id],
        file_from_row,
    )
    .optional()
}

/// How many live sub-files a document has.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn count_live_files(conn: &Connection, document_id: &str) -> rusqlite::Result<u64> {
    conn.query_row(
        "SELECT count(*) FROM document_files WHERE document_id = ?1 AND archived_at IS NULL",
        [document_id],
        |row| row.get(0),
    )
}

/// Records one file changed by one revision.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn insert_revision_file(conn: &Connection, change: &RevisionFileRow) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO revision_files (document_id, revision, path, main, change, from_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            change.document_id,
            change.revision,
            change.path,
            change.main,
            change.change.as_str(),
            change.from_path,
        ],
    )?;
    Ok(())
}

/// The file changes of every revision of a document, oldest first, the
/// main file before sub-files within a revision.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn revision_files(
    conn: &Connection,
    document_id: &str,
) -> rusqlite::Result<Vec<RevisionFileRow>> {
    let mut stmt = conn.prepare(
        "SELECT document_id, revision, path, main, change, from_path FROM revision_files
         WHERE document_id = ?1 ORDER BY revision, main DESC, path",
    )?;
    stmt.query_map([document_id], |row| {
        Ok(RevisionFileRow {
            document_id: row.get(0)?,
            revision: row.get(1)?,
            path: row.get(2)?,
            main: row.get(3)?,
            change: FileChange::parse(&row.get::<_, String>(4)?)?,
            from_path: row.get(5)?,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DocumentRow, insert_document, open_in_memory};

    fn setup() -> Connection {
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, name, key, title, created_at)
             VALUES (1, 'joedocs', 'JD', 'JoeDocs', 't')",
        )
        .unwrap();
        insert_document(
            &conn,
            &DocumentRow {
                id: "JD-1".into(),
                project_id: 1,
                project: "joedocs".into(),
                seq: 1,
                doc_type: "plans".into(),
                title: "Roadmap".into(),
                path: "joedocs/plans/Roadmap/Roadmap.md".into(),
                status: "draft".into(),
                revision: 1,
                created_at: "t".into(),
                created_by: "laptop".into(),
                updated_at: "t".into(),
                updated_by: "laptop".into(),
                client: "test".into(),
                content_sha256: vec![0],
                archived_at: None,
                archived_by: None,
                archive_reason: None,
                multifile: true,
                main_revision: 1,
            },
        )
        .unwrap();
        conn
    }

    fn file(path: &str, revision: u64) -> FileRow {
        FileRow {
            id: 0,
            document_id: "JD-1".into(),
            path: path.into(),
            revision,
            created_at: "t".into(),
            created_by: "laptop".into(),
            updated_at: "t".into(),
            updated_by: "laptop".into(),
            client: "test".into(),
            content_sha256: vec![1],
            archived_at: None,
            archived_by: None,
        }
    }

    #[test]
    fn files_are_inserted_found_listed_and_saved() {
        let conn = setup();
        let b = insert_file(&conn, &file("backlog/b", 2)).unwrap();
        let a = insert_file(&conn, &file("a", 3)).unwrap();
        assert_ne!(a, b);
        let live = live_files(&conn, "JD-1").unwrap();
        assert_eq!(
            live.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["a", "backlog/b"]
        );
        assert_eq!(count_live_files(&conn, "JD-1").unwrap(), 2);
        let found = find_live_file(&conn, "JD-1", "BACKLOG/B").unwrap().unwrap();
        assert_eq!((found.id, found.revision), (b, 2));
        assert_eq!(file_by_id(&conn, a).unwrap().unwrap().path, "a");

        let mut moved = found;
        moved.path = "backlog/c".into();
        moved.revision = 4;
        save_file(&conn, &moved).unwrap();
        assert!(
            find_live_file(&conn, "JD-1", "backlog/b")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            find_live_file(&conn, "JD-1", "backlog/c")
                .unwrap()
                .unwrap()
                .revision,
            4
        );
    }

    #[test]
    fn live_paths_are_unique_ignoring_case_but_archived_ones_are_free() {
        let conn = setup();
        let id = insert_file(&conn, &file("Notes", 2)).unwrap();
        assert!(insert_file(&conn, &file("notes", 3)).is_err());
        let mut archived = file_by_id(&conn, id).unwrap().unwrap();
        archived.archived_at = Some("u".into());
        archived.archived_by = Some("laptop".into());
        save_file(&conn, &archived).unwrap();
        assert_eq!(count_live_files(&conn, "JD-1").unwrap(), 0);
        insert_file(&conn, &file("notes", 4)).unwrap();
        assert_eq!(files_of(&conn, "JD-1").unwrap().len(), 2);
    }

    #[test]
    fn revision_file_changes_are_recorded_in_order() {
        let conn = setup();
        let change = |revision, path: &str, main, change, from: Option<&str>| RevisionFileRow {
            document_id: "JD-1".into(),
            revision,
            path: path.into(),
            main,
            change,
            from_path: from.map(str::to_owned),
        };
        let rows = [
            change(2, "b", false, FileChange::Added, None),
            change(1, "Roadmap", true, FileChange::Added, None),
            change(3, "c", false, FileChange::Renamed, Some("b")),
            change(3, "Roadmap", true, FileChange::Modified, None),
            change(4, "c", false, FileChange::Removed, None),
        ];
        for row in &rows {
            insert_revision_file(&conn, row).unwrap();
        }
        let stored = revision_files(&conn, "JD-1").unwrap();
        assert_eq!(
            stored,
            [
                rows[1].clone(),
                rows[0].clone(),
                rows[3].clone(),
                rows[2].clone(),
                rows[4].clone()
            ]
        );
        for change in [
            FileChange::Added,
            FileChange::Modified,
            FileChange::Renamed,
            FileChange::Removed,
        ] {
            assert_eq!(FileChange::parse(change.as_str()).unwrap(), change);
        }
        assert!(FileChange::parse("moved").is_err());
    }
}
