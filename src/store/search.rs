//! Searching documents by text and front matter.

use serde_json::{Map, Value};

use super::documents::{DocumentInfo, check_member, document_info};
use super::{Error, Store};
use crate::db::{self, SearchQuery};
use crate::search::{Query, filter};

/// Most results one search page may return.
pub const MAX_SEARCH_LIMIT: u64 = 200;

/// What to search for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Projects to search, already authorised. An empty list finds nothing.
    pub projects: Vec<String>,
    /// Only this type; `None` or `"all"` for every type.
    pub doc_type: Option<String>,
    /// Text to find, in the query grammar of [`crate::search::query`]. Empty
    /// for a pure front matter search.
    pub query: String,
    /// Front matter conditions, as described in [`crate::search::filter`].
    pub filters: Map<String, Value>,
    /// 1-based position of the first result.
    pub start: u64,
    /// Most results to return, 1 to [`MAX_SEARCH_LIMIT`].
    pub limit: u64,
    /// Only this document's files (its main file included), if set.
    pub document_id: Option<String>,
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The document.
    pub document: DocumentInfo,
    /// Relevance, higher is better; `None` without a text query.
    pub score: Option<f64>,
    /// The best-matching part of the body, with matches between `«` and
    /// `»`; `None` without a text query.
    pub snippet: Option<String>,
    /// The file that matched, for a file of a multi-file document (its path
    /// inside the folder); `None` for a main file.
    pub file: Option<String>,
}

/// One page of search results.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResults {
    /// Best first; without a text query, most recently updated first.
    pub items: Vec<SearchHit>,
    /// How many documents match in total.
    pub total: u64,
    /// `start` for the next page, if there is one.
    pub next_start: Option<u64>,
}

impl Store {
    /// Searches live documents by text and front matter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProjectNotFound`], [`Error::Invalid`] for a bad type,
    /// query, filter, start or limit, and database errors.
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResults, Error> {
        let doc_type = request.doc_type.as_deref().filter(|t| *t != "all");
        if let Some(doc_type) = doc_type {
            check_member("type", doc_type, &self.settings.types)?;
        }
        if request.start == 0 {
            return Err(Error::invalid("start", "must be 1 or more"));
        }
        if request.limit == 0 || request.limit > MAX_SEARCH_LIMIT {
            return Err(Error::invalid(
                "limit",
                format!("must be 1 to {MAX_SEARCH_LIMIT}"),
            ));
        }
        let text = Query::parse(&request.query)
            .map_err(|e| Error::invalid("query", e.0))?
            .map(|query| query.to_fts5());
        let conditions =
            filter::parse(&request.filters).map_err(|e| Error::invalid("filters", e.0))?;
        let project_ids = request
            .projects
            .iter()
            .map(|name| {
                db::find_project(&self.conn, name)?
                    .filter(|p| p.name == *name)
                    .map(|p| p.id)
                    .ok_or_else(|| Error::ProjectNotFound(name.clone()))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let document_id = request
            .document_id
            .as_deref()
            .map(|id| {
                id.parse::<crate::document::DocumentId>()
                    .map(|id| id.to_string())
                    .map_err(|e| Error::invalid("document_id", e.to_string()))
            })
            .transpose()?;
        let offset = request.start - 1;
        let (rows, total) = db::search_documents(
            &self.conn,
            &SearchQuery {
                project_ids: &project_ids,
                doc_type,
                text: text.as_deref(),
                conditions: &conditions,
                document_id: document_id.as_deref(),
                offset,
                limit: request.limit,
            },
        )?;
        let shown = offset + rows.len() as u64;
        // Each hit counts its document's files, as `get` and `list` do.
        let items = rows
            .into_iter()
            .map(|row| {
                Ok(SearchHit {
                    document: document_info(&self.conn, &row.document)?,
                    score: row.score,
                    snippet: row.snippet,
                    file: row.file,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(SearchResults {
            items,
            total,
            next_start: (shown < total).then_some(shown + 1),
        })
    }
}
