//! The document library: one project's live documents, filtered by type and
//! status, sorted and paged as `joedocs list` does.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::{Chrome, Section};
use crate::cli::{DEFAULT_LIST_LIMIT, SortArg};
use crate::db::ProjectRow;
use crate::store::{DocumentInfo, ListRequest, Store};

/// The sort orders, with their labels.
const SORTS: [(SortArg, &str); 5] = [
    (SortArg::UpdatedDesc, "Recently updated"),
    (SortArg::UpdatedAsc, "Least recently updated"),
    (SortArg::CreatedDesc, "Newest"),
    (SortArg::CreatedAsc, "Oldest"),
    (SortArg::Title, "Title"),
];

/// One choice in a filter.
pub(crate) struct Choice {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) selected: bool,
}

#[derive(Template)]
#[template(path = "documents.html")]
struct DocumentsPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    project: Option<ProjectRow>,
    projects: Vec<Choice>,
    types: Vec<Choice>,
    statuses: Vec<Choice>,
    sorts: Vec<Choice>,
    items: Vec<DocumentInfo>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
}

/// What the page lists, from its query.
struct Filters {
    project: String,
    doc_type: Option<String>,
    status: Option<String>,
    sort: SortArg,
    start: u64,
    limit: u64,
}

impl Filters {
    /// This listing at `start`, as a link that keeps the other filters.
    fn url(&self, start: u64) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        query.append_pair("project", &self.project);
        if let Some(t) = &self.doc_type {
            query.append_pair("type", t);
        }
        if let Some(s) = &self.status {
            query.append_pair("status", s);
        }
        if self.sort != SortArg::default() {
            query.append_pair("sort", self.sort.api_name());
        }
        if start != 1 {
            query.append_pair("start", &start.to_string());
        }
        if self.limit != DEFAULT_LIST_LIMIT {
            query.append_pair("limit", &self.limit.to_string());
        }
        format!("/admin/documents?{}", query.finish())
    }
}

/// A positive number from the query, or `default`.
pub(crate) fn number(request: &WebRequest, key: &str, default: u64) -> Result<u64, WebError> {
    match request.query(key).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(value) => value.parse().map_err(|_| {
            WebError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("`{key}` must be a whole number, not \"{value}\""),
            )
        }),
    }
}

/// `GET /admin/documents`.
pub(crate) fn list(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let projects = store.projects().map_err(|e| WebError::from_store(&e))?;
    let wanted = request.query("project").filter(|p| !p.is_empty());
    let Some(name) = wanted
        .map(str::to_owned)
        .or_else(|| projects.first().map(|p| p.name.clone()))
    else {
        return Ok(empty(session));
    };
    let sort = match request.query("sort").filter(|s| !s.is_empty()) {
        None => SortArg::default(),
        Some(name) => SortArg::from_api_name(name).ok_or_else(|| {
            WebError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("`sort` cannot be \"{name}\""),
            )
        })?,
    };
    let filters = Filters {
        doc_type: request
            .query("type")
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        status: request
            .query("status")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        sort,
        start: number(request, "start", 1)?,
        limit: number(request, "limit", DEFAULT_LIST_LIMIT)?,
        project: name,
    };
    let listing = store
        .list(&ListRequest {
            project: filters.project.clone(),
            doc_type: filters.doc_type.clone(),
            status: filters.status.clone(),
            sort: filters.sort.sort(),
            start: filters.start,
            limit: filters.limit,
        })
        .map_err(|e| WebError::from_store(&e))?;
    let settings = store.settings();
    let choices = |values: &[String], current: Option<&str>, all: &str| {
        std::iter::once(Choice {
            value: String::new(),
            label: all.to_owned(),
            selected: current.is_none(),
        })
        .chain(values.iter().map(|v| Choice {
            value: v.clone(),
            label: v.clone(),
            selected: current == Some(v.as_str()),
        }))
        .collect()
    };
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        project: projects.iter().find(|p| p.name == filters.project).cloned(),
        projects: projects
            .iter()
            .map(|p| Choice {
                value: p.name.clone(),
                label: format!("{} ({})", p.name, p.key),
                selected: p.name == filters.project,
            })
            .collect(),
        types: choices(&settings.types, filters.doc_type.as_deref(), "All types"),
        statuses: choices(
            &settings.statuses,
            filters.status.as_deref(),
            "All statuses",
        ),
        sorts: SORTS
            .iter()
            .map(|(sort, label)| Choice {
                value: sort.api_name().to_owned(),
                label: (*label).to_owned(),
                selected: *sort == filters.sort,
            })
            .collect(),
        total: listing.total,
        start: filters.start,
        shown_to: filters.start + listing.items.len() as u64 - u64::from(!listing.items.is_empty()),
        previous: (filters.start > 1)
            .then(|| filters.url(filters.start.saturating_sub(filters.limit).max(1))),
        next: listing.next_start.map(|next| filters.url(next)),
        items: listing.items,
    };
    Ok(render(StatusCode::OK, &page))
}

/// The library before any project exists.
fn empty(session: &Session) -> Response {
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        project: None,
        projects: Vec::new(),
        types: Vec::new(),
        statuses: Vec::new(),
        sorts: Vec::new(),
        items: Vec::new(),
        total: 0,
        start: 1,
        shown_to: 0,
        previous: None,
        next: None,
    };
    render(StatusCode::OK, &page)
}
