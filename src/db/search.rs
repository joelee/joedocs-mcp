//! The search index (`doc_fields` and `doc_fts`) and search queries.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params, params_from_iter};
use serde_json::Value;

use super::DocumentRow;
use super::documents::{SELECT_COLUMNS, from_row};
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

/// Replaces the document's search entries with `fields` (its whole front
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
    let mut title = row.title.clone();
    if let Some(Value::Array(aliases)) = fields.get("aliases") {
        for alias in aliases.iter().filter_map(Value::as_str) {
            title.push('\n');
            title.push_str(alias);
        }
    }
    conn.execute(
        "INSERT INTO doc_fts (rowid, title, body) VALUES (?1, ?2, ?3)",
        params![fts_rowid(row.project_id, row.seq), title, body],
    )?;
    Ok(())
}

/// Removes the document from the search index.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn unindex_document(conn: &Connection, row: &DocumentRow) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM doc_fields WHERE document_id = ?1", [&row.id])?;
    conn.execute(
        "DELETE FROM doc_fts WHERE rowid = ?1",
        [fts_rowid(row.project_id, row.seq)],
    )?;
    Ok(())
}

/// Deletes every document, revision and search entry, before a rebuild.
/// Projects, keys and the audit log are kept.
///
/// # Errors
///
/// Returns an error when a statement fails.
pub fn clear_documents(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM doc_fields; DELETE FROM doc_fts; DELETE FROM revisions; DELETE FROM documents;",
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
    /// Front matter conditions, all of which must hold.
    pub conditions: &'a [Condition],
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
    let mut filters = vec!["d.archived_at IS NULL".to_owned()];
    let from = if let Some(text) = query.text {
        filters.push("doc_fts MATCH ?".to_owned());
        params.push(SqlValue::Text(text.to_owned()));
        // The full-text match drives the query; each hit finds its document
        // by the (project_id, seq) its rowid encodes.
        "FROM doc_fts CROSS JOIN documents d \
         ON d.project_id = (doc_fts.rowid >> 32) AND d.seq = (doc_fts.rowid & 4294967295) \
         JOIN projects p ON p.id = d.project_id"
    } else {
        "FROM documents d JOIN projects p ON p.id = d.project_id"
    };
    let ids = vec!["?"; query.project_ids.len()].join(", ");
    filters.push(format!("d.project_id IN ({ids})"));
    params.extend(query.project_ids.iter().map(|id| SqlValue::Integer(*id)));
    if let Some(doc_type) = query.doc_type {
        filters.push("d.type = ?".to_owned());
        params.push(SqlValue::Text(doc_type.to_owned()));
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
            format!(", -{rank}, snippet(doc_fts, 1, '«', '»', '…', {SNIPPET_TOKENS})"),
            format!("{rank}, d.updated_at DESC, d.seq DESC"),
        )
    } else {
        (
            ", NULL, NULL".to_owned(),
            "d.updated_at DESC, d.seq DESC".to_owned(),
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
                score: row.get(18)?,
                snippet: row.get(19)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
}
