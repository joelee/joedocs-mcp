//! The search index (`doc_fields` and `doc_fts`) and search queries.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params, params_from_iter};
use serde_json::Value;

use super::DocumentRow;
use super::documents::{SELECT_COLUMNS, from_row};
use super::files::FileRow;
use crate::document::front_matter::Fields;
use crate::search::{Condition, flatten};

/// Weight of a title match relative to a body match in the ranking.
const TITLE_WEIGHT: f64 = 10.0;

/// Words of context around each match in a snippet.
const SNIPPET_TOKENS: u32 = 16;

/// The `doc_fts` rowid of a document: `project_id * 2^32 + seq`.
#[must_use]
pub fn fts_rowid(project_id: i64, seq: u64) -> i64 {
    let seq = i64::try_from(seq & 0xffff_ffff).unwrap_or_default();
    (project_id << 32) | seq
}

/// The `doc_fts` rowid of a sub-file: minus its `document_files` row ID, so
/// it never meets a main file's rowid.
#[must_use]
pub fn file_fts_rowid(file_id: i64) -> i64 {
    -file_id
}

/// Replaces the main file's search entries with `fields` (its whole front
/// matter) and `body`.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn index_document(
    conn: &Connection,
    row: &DocumentRow,
    fields: &Fields,
    body: &str,
) -> rusqlite::Result<()> {
    unindex_document(conn, row)?;
    let mut insert = conn
        .prepare("INSERT INTO doc_fields (document_id, key, value, num) VALUES (?1, ?2, ?3, ?4)")?;
    for (key, value) in fields {
        for (text, num) in flatten(value) {
            insert.execute(params![row.id, key, text, num])?;
        }
    }
    conn.execute(
        "INSERT INTO doc_fts (rowid, title, body) VALUES (?1, ?2, ?3)",
        params![
            fts_rowid(row.project_id, row.seq),
            fts_title(&row.title, fields),
            body
        ],
    )?;
    Ok(())
}

/// Removes the main file from the search index; sub-files keep theirs.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn unindex_document(conn: &Connection, row: &DocumentRow) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM doc_fields WHERE document_id = ?1 AND file_id IS NULL",
        [&row.id],
    )?;
    conn.execute(
        "DELETE FROM doc_fts WHERE rowid = ?1",
        [fts_rowid(row.project_id, row.seq)],
    )?;
    Ok(())
}

/// Replaces a sub-file's search entries with `fields` (its whole front
/// matter) and `body`. Its full-text title is its path and aliases.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn index_file(
    conn: &Connection,
    file: &FileRow,
    fields: &Fields,
    body: &str,
) -> rusqlite::Result<()> {
    unindex_file(conn, file.id)?;
    let mut insert = conn.prepare(
        "INSERT INTO doc_fields (document_id, key, value, num, file_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (key, value) in fields {
        for (text, num) in flatten(value) {
            insert.execute(params![file.document_id, key, text, num, file.id])?;
        }
    }
    conn.execute(
        "INSERT INTO doc_fts (rowid, title, body) VALUES (?1, ?2, ?3)",
        params![file_fts_rowid(file.id), fts_title(&file.path, fields), body],
    )?;
    Ok(())
}

/// Removes a sub-file from the search index.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn unindex_file(conn: &Connection, file_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM doc_fields WHERE file_id = ?1", [file_id])?;
    conn.execute(
        "DELETE FROM doc_fts WHERE rowid = ?1",
        [file_fts_rowid(file_id)],
    )?;
    Ok(())
}

/// `name` and the `aliases` in `fields`, one per line, for the full-text
/// title column.
fn fts_title(name: &str, fields: &Fields) -> String {
    let mut title = name.to_owned();
    if let Some(Value::Array(aliases)) = fields.get("aliases") {
        for alias in aliases.iter().filter_map(Value::as_str) {
            title.push('\n');
            title.push_str(alias);
        }
    }
    title
}

/// Deletes every document, file, revision and search entry, before a
/// rebuild. Projects, keys and the audit log are kept.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn clear_documents(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM doc_fields; DELETE FROM doc_fts; DELETE FROM revision_files;
         DELETE FROM document_files; DELETE FROM revisions; DELETE FROM documents;",
    )
}

/// Which live documents to search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery<'a> {
    /// Projects to search (row IDs).
    pub project_ids: &'a [i64],
    /// Only this type, if set.
    pub doc_type: Option<&'a str>,
    /// FTS5 match expression, if there is text to find.
    pub text: Option<&'a str>,
    /// Front matter conditions, all of which must hold for the file that
    /// matched.
    pub conditions: &'a [Condition],
    /// Only this document's files (its main file included), if set.
    pub document_id: Option<&'a str>,
    /// Rows to skip.
    pub offset: u64,
    /// Most rows to return.
    pub limit: u64,
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRow {
    /// The document.
    pub document: DocumentRow,
    /// Relevance (higher is better), when there was text to find.
    pub score: Option<f64>,
    /// The best-matching part of the body, when there was text to find.
    pub snippet: Option<String>,
    /// The sub-file that matched, for a file of a multi-file document; the
    /// main file when `None`.
    pub file: Option<String>,
}

/// One page of results, best first, and the total number matching.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn search_documents(
    conn: &Connection,
    query: &SearchQuery<'_>,
) -> rusqlite::Result<(Vec<SearchRow>, u64)> {
    if query.project_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut params: Vec<SqlValue> = Vec::new();
    let mut filters = vec![
        "d.archived_at IS NULL".to_owned(),
        "df.archived_at IS NULL".to_owned(),
    ];
    // Every entry is a main file (no `df` row) or a live sub-file (`df`);
    // front matter conditions look at the entry's own `doc_fields` rows.
    let from = if let Some(text) = query.text {
        filters.push("doc_fts MATCH ?".to_owned());
        params.push(SqlValue::Text(text.to_owned()));
        // The full-text match drives the query. A main file's rowid encodes
        // its document's (project_id, seq); a sub-file's is minus its row ID.
        "FROM doc_fts \
         LEFT JOIN document_files df ON doc_fts.rowid < 0 AND df.id = -doc_fts.rowid \
         CROSS JOIN documents d ON d.id = CASE WHEN doc_fts.rowid < 0 THEN df.document_id \
             ELSE (SELECT m.id FROM documents m \
                   WHERE m.project_id = (doc_fts.rowid >> 32) AND m.seq = (doc_fts.rowid & 4294967295)) END \
         JOIN projects p ON p.id = d.project_id"
    } else {
        "FROM (SELECT id AS document_id, NULL AS file_id FROM documents \
               UNION ALL SELECT document_id, id FROM document_files WHERE archived_at IS NULL) e \
         JOIN documents d ON d.id = e.document_id \
         LEFT JOIN document_files df ON df.id = e.file_id \
         JOIN projects p ON p.id = d.project_id"
    };
    let ids = vec!["?"; query.project_ids.len()].join(", ");
    filters.push(format!("d.project_id IN ({ids})"));
    params.extend(query.project_ids.iter().map(|id| SqlValue::Integer(*id)));
    if let Some(doc_type) = query.doc_type {
        filters.push("d.type = ?".to_owned());
        params.push(SqlValue::Text(doc_type.to_owned()));
    }
    if let Some(id) = query.document_id {
        filters.push("d.id = ?".to_owned());
        params.push(SqlValue::Text(id.to_owned()));
    }
    for condition in query.conditions {
        filters.push(condition.to_sql(&mut params));
    }
    let filter = format!("{from} WHERE {}", filters.join(" AND "));

    let total = conn.query_row(
        &format!("SELECT count(*) {filter}"),
        params_from_iter(&params),
        |row| row.get(0),
    )?;
    let rank = format!("bm25(doc_fts, {TITLE_WEIGHT:.1}, 1.0)");
    let (extra, order) = if query.text.is_some() {
        (
            format!(", -{rank}, snippet(doc_fts, 1, '«', '»', '…', {SNIPPET_TOKENS}), df.path"),
            format!("{rank}, coalesce(df.updated_at, d.updated_at) DESC, d.seq DESC, df.path"),
        )
    } else {
        (
            ", NULL, NULL, df.path".to_owned(),
            "coalesce(df.updated_at, d.updated_at) DESC, d.seq DESC, df.path".to_owned(),
        )
    };
    params.push(SqlValue::Integer(
        i64::try_from(query.limit).unwrap_or(i64::MAX),
    ));
    params.push(SqlValue::Integer(
        i64::try_from(query.offset).unwrap_or(i64::MAX),
    ));
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS}{extra} {filter} ORDER BY {order} LIMIT ? OFFSET ?"
    ))?;
    let rows = stmt
        .query_map(params_from_iter(&params), |row| {
            Ok(SearchRow {
                document: from_row(row)?,
                score: row.get(20)?,
                snippet: row.get(21)?,
                file: row.get(22)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::{FileRow, get_document, insert_file, open_in_memory};

    fn fields(value: Value) -> Fields {
        match value {
            Value::Object(map) => map.into_iter().collect(),
            _ => Fields::new(),
        }
    }

    #[test]
    fn sub_file_entries_are_kept_apart_from_the_main_file() {
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, name, key, title, created_at)
                 VALUES (1, 'joedocs', 'JD', 'JoeDocs', 't');
             INSERT INTO documents (id, project_id, seq, type, title, path, status, revision,
                 created_at, created_by, updated_at, updated_by, client, content_sha256,
                 multifile, main_revision)
                 VALUES ('JD-1', 1, 1, 'plans', 'Roadmap', 'joedocs/plans/Roadmap/Roadmap.md',
                 'draft', 2, 't', 'laptop', 't', 'laptop', 'test', x'00', 1, 1);",
        )
        .unwrap();
        let row = get_document(&conn, "JD-1").unwrap().unwrap();
        index_document(
            &conn,
            &row,
            &fields(json!({"status": "draft"})),
            "main words",
        )
        .unwrap();
        let mut file = FileRow {
            id: 0,
            document_id: "JD-1".into(),
            path: "backlog/item".into(),
            revision: 2,
            created_at: "t".into(),
            created_by: "laptop".into(),
            updated_at: "t".into(),
            updated_by: "laptop".into(),
            client: "test".into(),
            content_sha256: vec![1],
            archived_at: None,
            archived_by: None,
        };
        file.id = insert_file(&conn, &file).unwrap();
        index_file(
            &conn,
            &file,
            &fields(json!({"status": "planned", "aliases": ["Item one"]})),
            "sub words",
        )
        .unwrap();

        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT count(*) FROM doc_fields WHERE file_id IS NOT NULL"),
            2
        );
        let title: String = conn
            .query_row(
                "SELECT title FROM doc_fts WHERE rowid = ?1",
                [file_fts_rowid(file.id)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "backlog/item\nItem one");

        // The sub-file is found by its own front matter and text.
        let Value::Object(filter) = json!({"status": "planned"}) else {
            unreachable!()
        };
        let planned = crate::search::filter::parse(&filter).unwrap();
        let query = SearchQuery {
            project_ids: &[1],
            doc_type: None,
            text: None,
            conditions: &planned,
            document_id: None,
            offset: 0,
            limit: 10,
        };
        let (rows, total) = search_documents(&conn, &query).unwrap();
        assert_eq!((total, rows[0].file.as_deref()), (1, Some("backlog/item")));
        let words = SearchQuery {
            text: Some("sub"),
            conditions: &[],
            ..query
        };
        let (rows, total) = search_documents(&conn, &words).unwrap();
        assert_eq!((total, rows[0].file.as_deref()), (1, Some("backlog/item")));

        // Re-indexing the main file leaves the sub-file's entries alone.
        unindex_document(&conn, &row).unwrap();
        assert_eq!(count("SELECT count(*) FROM doc_fields"), 2);
        unindex_file(&conn, file.id).unwrap();
        assert_eq!(count("SELECT count(*) FROM doc_fields"), 0);
        assert_eq!(count("SELECT count(*) FROM doc_fts"), 0);

        clear_documents(&conn).unwrap();
        assert_eq!(count("SELECT count(*) FROM document_files"), 0);
        assert_eq!(count("SELECT count(*) FROM documents"), 0);
    }
}
