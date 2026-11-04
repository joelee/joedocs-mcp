//! Document and revision queries.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

/// The index row of one document (its latest revision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRow {
    /// Document ID, e.g. `JD-42`.
    pub id: String,
    /// Project row ID.
    pub project_id: i64,
    /// Project name.
    pub project: String,
    /// Sequence number within the project.
    pub seq: u64,
    /// Document type.
    pub doc_type: String,
    /// Title.
    pub title: String,
    /// Vault-relative path with `/` separators.
    pub path: String,
    /// Status.
    pub status: String,
    /// Latest revision number.
    pub revision: u64,
    /// Creation time.
    pub created_at: String,
    /// Creating key name.
    pub created_by: String,
    /// Time of the latest revision.
    pub updated_at: String,
    /// Key name that wrote the latest revision.
    pub updated_by: String,
    /// MCP client that wrote the latest revision.
    pub client: String,
    /// SHA-256 of the file as last written.
    pub content_sha256: Vec<u8>,
    /// Archive time, if archived.
    pub archived_at: Option<String>,
    /// Key name that archived it.
    pub archived_by: Option<String>,
    /// Why it was archived.
    pub archive_reason: Option<String>,
}

/// The columns [`from_row`] reads, from `documents d` joined to `projects p`.
macro_rules! columns {
    () => {
        "d.id, d.project_id, p.name, d.seq, d.type, d.title, d.path, d.status, d.revision,
        d.created_at, d.created_by, d.updated_at, d.updated_by, d.client, d.content_sha256,
        d.archived_at, d.archived_by, d.archive_reason"
    };
}

/// The columns [`from_row`] reads, for other queries.
pub(super) const SELECT_COLUMNS: &str = columns!();

const SELECT: &str = concat!(
    "SELECT ",
    columns!(),
    " FROM documents d JOIN projects p ON p.id = d.project_id"
);

pub(super) fn from_row(row: &Row<'_>) -> rusqlite::Result<DocumentRow> {
    Ok(DocumentRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        project: row.get(2)?,
        seq: row.get(3)?,
        doc_type: row.get(4)?,
        title: row.get(5)?,
        path: row.get(6)?,
        status: row.get(7)?,
        revision: row.get(8)?,
        created_at: row.get(9)?,
        created_by: row.get(10)?,
        updated_at: row.get(11)?,
        updated_by: row.get(12)?,
        client: row.get(13)?,
        content_sha256: row.get(14)?,
        archived_at: row.get(15)?,
        archived_by: row.get(16)?,
        archive_reason: row.get(17)?,
    })
}

/// Inserts a new document row.
///
/// # Errors
///
/// Returns an error when the ID or live path is taken or the statement
/// fails.
pub fn insert_document(conn: &Connection, doc: &DocumentRow) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO documents (id, project_id, seq, type, title, path, status, revision,
            created_at, created_by, updated_at, updated_by, client, content_sha256,
            archived_at, archived_by, archive_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            doc.id,
            doc.project_id,
            doc.seq,
            doc.doc_type,
            doc.title,
            doc.path,
            doc.status,
            doc.revision,
            doc.created_at,
            doc.created_by,
            doc.updated_at,
            doc.updated_by,
            doc.client,
            doc.content_sha256,
            doc.archived_at,
            doc.archived_by,
            doc.archive_reason,
        ],
    )?;
    Ok(())
}

/// Writes every mutable column of an existing document row.
///
/// # Errors
///
/// Returns an error when the live path is taken or the statement fails.
pub fn save_document(conn: &Connection, doc: &DocumentRow) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE documents SET type = ?2, title = ?3, path = ?4, status = ?5, revision = ?6,
            updated_at = ?7, updated_by = ?8, client = ?9, content_sha256 = ?10,
            archived_at = ?11, archived_by = ?12, archive_reason = ?13
         WHERE id = ?1",
        params![
            doc.id,
            doc.doc_type,
            doc.title,
            doc.path,
            doc.status,
            doc.revision,
            doc.updated_at,
            doc.updated_by,
            doc.client,
            doc.content_sha256,
            doc.archived_at,
            doc.archived_by,
            doc.archive_reason,
        ],
    )?;
    Ok(())
}

/// The document with this ID, archived or not.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn get_document(conn: &Connection, id: &str) -> rusqlite::Result<Option<DocumentRow>> {
    conn.query_row(&format!("{SELECT} WHERE d.id = ?1"), [id], from_row)
        .optional()
}

/// Every live document, by ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn live_documents(conn: &Connection) -> rusqlite::Result<Vec<DocumentRow>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE d.archived_at IS NULL ORDER BY d.project_id, d.seq"
    ))?;
    stmt.query_map([], from_row)?.collect()
}

/// Whether a live document other than `except` uses `path`, compared
/// without ASCII case so that case-insensitive file systems cannot clash.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn path_taken(conn: &Connection, path: &str, except: Option<&str>) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM documents
         WHERE archived_at IS NULL AND path = ?1 COLLATE NOCASE AND id IS NOT ?2)",
        params![path, except],
        |row| row.get(0),
    )
}

/// Listing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Most recently updated first.
    #[default]
    UpdatedDesc,
    /// Least recently updated first.
    UpdatedAsc,
    /// Newest first.
    CreatedDesc,
    /// Oldest first.
    CreatedAsc,
    /// By title, A to Z.
    Title,
}

impl Sort {
    fn order_by(self) -> &'static str {
        match self {
            Self::UpdatedDesc => "d.updated_at DESC, d.seq DESC",
            Self::UpdatedAsc => "d.updated_at ASC, d.seq ASC",
            Self::CreatedDesc => "d.seq DESC",
            Self::CreatedAsc => "d.seq ASC",
            Self::Title => "d.title COLLATE NOCASE ASC, d.seq ASC",
        }
    }
}

/// Which live documents to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery<'a> {
    /// Project row ID.
    pub project_id: i64,
    /// Only this type, if set.
    pub doc_type: Option<&'a str>,
    /// Only this status, if set.
    pub status: Option<&'a str>,
    /// Order.
    pub sort: Sort,
    /// Rows to skip.
    pub offset: u64,
    /// Most rows to return.
    pub limit: u64,
}

/// One page of live documents and the total number matching.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_documents(
    conn: &Connection,
    query: &ListQuery<'_>,
) -> rusqlite::Result<(Vec<DocumentRow>, u64)> {
    let filter = "WHERE d.project_id = ?1 AND d.archived_at IS NULL
        AND (?2 IS NULL OR d.type = ?2) AND (?3 IS NULL OR d.status = ?3)";
    let total = conn.query_row(
        &format!("SELECT count(*) FROM documents d {filter}"),
        params![query.project_id, query.doc_type, query.status],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "{SELECT} {filter} ORDER BY {} LIMIT ?4 OFFSET ?5",
        query.sort.order_by()
    ))?;
    let rows = stmt
        .query_map(
            params![
                query.project_id,
                query.doc_type,
                query.status,
                query.limit,
                query.offset
            ],
            from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
}

/// One revision of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRow {
    /// Document ID.
    pub document_id: String,
    /// Revision number.
    pub revision: u64,
    /// History commit holding this revision.
    pub commit_id: String,
    /// Vault-relative path at this revision.
    pub path: String,
    /// What happened, e.g. `create` or `rename`.
    pub action: String,
    /// Key name.
    pub author: String,
    /// MCP client name.
    pub client: String,
    /// Commit subject.
    pub summary: String,
    /// Time.
    pub created_at: String,
}

/// Inserts a revision row.
///
/// # Errors
///
/// Returns an error when the revision exists or the statement fails.
pub fn insert_revision(conn: &Connection, rev: &RevisionRow) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO revisions (document_id, revision, commit_id, path, action, author, client,
            summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            rev.document_id,
            rev.revision,
            rev.commit_id,
            rev.path,
            rev.action,
            rev.author,
            rev.client,
            rev.summary,
            rev.created_at,
        ],
    )?;
    Ok(())
}

const REVISION_COLUMNS: &str = "document_id, revision, commit_id, path, action, author, client,
    summary, created_at";

fn revision_from_row(row: &Row<'_>) -> rusqlite::Result<RevisionRow> {
    Ok(RevisionRow {
        document_id: row.get(0)?,
        revision: row.get(1)?,
        commit_id: row.get(2)?,
        path: row.get(3)?,
        action: row.get(4)?,
        author: row.get(5)?,
        client: row.get(6)?,
        summary: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// A page of a document's revisions, newest first.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_revisions(
    conn: &Connection,
    document_id: &str,
    offset: u64,
    limit: u64,
) -> rusqlite::Result<Vec<RevisionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REVISION_COLUMNS} FROM revisions WHERE document_id = ?1
         ORDER BY revision DESC LIMIT ?2 OFFSET ?3"
    ))?;
    stmt.query_map(params![document_id, limit, offset], revision_from_row)?
        .collect()
}

/// Archived documents, most recently archived first, optionally of one
/// project.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn archived_documents(
    conn: &Connection,
    project_id: Option<i64>,
) -> rusqlite::Result<Vec<DocumentRow>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE d.archived_at IS NOT NULL AND (?1 IS NULL OR d.project_id = ?1)
         ORDER BY d.archived_at DESC, d.project_id, d.seq DESC"
    ))?;
    stmt.query_map([project_id], from_row)?.collect()
}

/// One revision of a document.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn get_revision(
    conn: &Connection,
    document_id: &str,
    revision: u64,
) -> rusqlite::Result<Option<RevisionRow>> {
    conn.query_row(
        &format!(
            "SELECT {REVISION_COLUMNS} FROM revisions WHERE document_id = ?1 AND revision = ?2"
        ),
        params![document_id, revision],
        revision_from_row,
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{insert_project, open_in_memory};

    fn doc(project_id: i64, seq: u64, path: &str, updated: &str) -> DocumentRow {
        DocumentRow {
            id: format!("JD-{seq}"),
            project_id,
            project: "joedocs".into(),
            seq,
            doc_type: "plans".into(),
            title: format!("Doc {seq}"),
            path: path.into(),
            status: "draft".into(),
            revision: 1,
            created_at: "t".into(),
            created_by: "laptop".into(),
            updated_at: updated.into(),
            updated_by: "laptop".into(),
            client: "test".into(),
            content_sha256: vec![0; 32],
            archived_at: None,
            archived_by: None,
            archive_reason: None,
        }
    }

    #[test]
    fn live_paths_are_unique_without_case_until_archived() {
        let conn = open_in_memory().unwrap();
        let p = insert_project(&conn, "joedocs", "JD", "JoeDocs", "t").unwrap();
        let mut first = doc(p, 1, "joedocs/plans/A.md", "1");
        insert_document(&conn, &first).unwrap();
        assert!(path_taken(&conn, "joedocs/plans/a.md", None).unwrap());
        assert!(!path_taken(&conn, "joedocs/plans/a.md", Some("JD-1")).unwrap());
        assert!(insert_document(&conn, &doc(p, 2, "joedocs/plans/a.MD", "1")).is_err());

        first.archived_at = Some("t".into());
        save_document(&conn, &first).unwrap();
        assert!(!path_taken(&conn, "joedocs/plans/A.md", None).unwrap());
        insert_document(&conn, &doc(p, 2, "joedocs/plans/A.md", "1")).unwrap();
        assert_eq!(get_document(&conn, "JD-1").unwrap(), Some(first));
        assert_eq!(get_document(&conn, "JD-9").unwrap(), None);
    }

    #[test]
    fn listing_filters_sorts_and_pages() {
        let conn = open_in_memory().unwrap();
        let p = insert_project(&conn, "joedocs", "JD", "JoeDocs", "t").unwrap();
        for (seq, updated) in [(1, "3"), (2, "1"), (3, "2")] {
            insert_document(&conn, &doc(p, seq, &format!("x/{seq}.md"), updated)).unwrap();
        }
        let mut other = doc(p, 4, "x/4.md", "9");
        other.doc_type = "ideas".into();
        other.status = "done".into();
        insert_document(&conn, &other).unwrap();

        let ids = |query: &ListQuery<'_>| {
            let (rows, total) = list_documents(&conn, query).unwrap();
            (rows.into_iter().map(|d| d.seq).collect::<Vec<_>>(), total)
        };
        let mut query = ListQuery {
            project_id: p,
            doc_type: Some("plans"),
            status: None,
            sort: Sort::UpdatedDesc,
            offset: 0,
            limit: 10,
        };
        assert_eq!(ids(&query), (vec![1, 3, 2], 3));
        query.sort = Sort::UpdatedAsc;
        assert_eq!(ids(&query), (vec![2, 3, 1], 3));
        query.sort = Sort::CreatedDesc;
        assert_eq!(ids(&query), (vec![3, 2, 1], 3));
        query.sort = Sort::Title;
        query.offset = 1;
        query.limit = 1;
        assert_eq!(ids(&query), (vec![2], 3));
        query.sort = Sort::CreatedAsc;
        query.doc_type = None;
        query.status = Some("done");
        query.offset = 0;
        assert_eq!(ids(&query), (vec![4], 1));
    }

    #[test]
    fn revisions_round_trip() {
        let conn = open_in_memory().unwrap();
        let p = insert_project(&conn, "joedocs", "JD", "JoeDocs", "t").unwrap();
        insert_document(&conn, &doc(p, 1, "x/1.md", "1")).unwrap();
        let rev = RevisionRow {
            document_id: "JD-1".into(),
            revision: 1,
            commit_id: "abc".into(),
            path: "x/1.md".into(),
            action: "create".into(),
            author: "laptop".into(),
            client: "test".into(),
            summary: "Create JD-1".into(),
            created_at: "t".into(),
        };
        insert_revision(&conn, &rev).unwrap();
        assert!(insert_revision(&conn, &rev).is_err());
        assert_eq!(get_revision(&conn, "JD-1", 1).unwrap(), Some(rev));
        assert_eq!(get_revision(&conn, "JD-1", 2).unwrap(), None);
    }
}
