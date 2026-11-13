//! The document library: one project's live documents, filtered by type and
//! status and paged as `joedocs list` does, and sorted by any column header
//! across every page.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::{Chrome, Section};
use crate::cli::DEFAULT_LIST_LIMIT;
use crate::db::{ProjectRow, Sort};
use crate::store::{DocumentInfo, ListRequest, Store};

/// A column the library sorts by, as its header and the `sort` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Id,
    Title,
    Type,
    Status,
    Revision,
    Updated,
}

impl Column {
    const ALL: [Self; 6] = [
        Self::Id,
        Self::Title,
        Self::Type,
        Self::Status,
        Self::Revision,
        Self::Updated,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Type => "type",
            Self::Status => "status",
            Self::Revision => "revision",
            Self::Updated => "updated",
        }
    }
}

/// An order picked with a column header. Without one, the library is most
/// recently updated first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Order {
    column: Column,
    descending: bool,
}

impl Order {
    /// The order a `sort` and `dir` ask for: a column name, or one of the
    /// orders `joedocs list --sort` names, as links from 0.5.1 carry.
    fn parse(sort: Option<&str>, dir: Option<&str>) -> Result<Option<Self>, WebError> {
        let invalid = |key: &str, value: &str| {
            WebError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("`{key}` cannot be \"{value}\""),
            )
        };
        let descending = match dir.unwrap_or("") {
            "" | "asc" => false,
            "desc" => true,
            other => return Err(invalid("dir", other)),
        };
        let order = |column, descending| Some(Self { column, descending });
        Ok(match sort.unwrap_or("") {
            "" => None,
            "updated_desc" => order(Column::Updated, true),
            "updated_asc" => order(Column::Updated, false),
            "created_desc" => order(Column::Id, true),
            "created_asc" => order(Column::Id, false),
            name => match Column::ALL.into_iter().find(|c| c.name() == name) {
                Some(column) => order(column, descending),
                None => return Err(invalid("sort", name)),
            },
        })
    }

    /// The store's order for this one, or the default.
    fn sort(order: Option<Self>) -> Sort {
        let Some(Self { column, descending }) = order else {
            return Sort::default();
        };
        match (column, descending) {
            (Column::Id, false) => Sort::CreatedAsc,
            (Column::Id, true) => Sort::CreatedDesc,
            (Column::Title, false) => Sort::Title,
            (Column::Title, true) => Sort::TitleDesc,
            (Column::Type, false) => Sort::TypeAsc,
            (Column::Type, true) => Sort::TypeDesc,
            (Column::Status, false) => Sort::StatusAsc,
            (Column::Status, true) => Sort::StatusDesc,
            (Column::Revision, false) => Sort::RevisionAsc,
            (Column::Revision, true) => Sort::RevisionDesc,
            (Column::Updated, false) => Sort::UpdatedAsc,
            (Column::Updated, true) => Sort::UpdatedDesc,
        }
    }
}

/// One column header of the library.
pub(crate) struct Header {
    pub(crate) label: &'static str,
    /// `num` for a right-aligned column, or empty.
    pub(crate) class: &'static str,
    /// The link to the next order (ascending, descending, then the
    /// default), for a sortable column.
    pub(crate) href: Option<String>,
    /// `ascending` or `descending` on the sorted column.
    pub(crate) aria_sort: Option<&'static str>,
    /// The key that brings focus back to this header after htmx swaps the
    /// results in.
    pub(crate) key: &'static str,
}

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
    projects: Vec<Choice>,
    types: Vec<Choice>,
    statuses: Vec<Choice>,
    results: DocumentsResults,
}

/// The results region, also sent alone to htmx when a filter changes.
#[derive(Template)]
#[template(path = "documents_results.html")]
struct DocumentsResults {
    project: Option<ProjectRow>,
    headers: Vec<Header>,
    /// The order, kept by the filter form through fields in this region.
    sort: Option<(&'static str, bool)>,
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
    order: Option<Order>,
    start: u64,
    limit: u64,
}

impl Filters {
    /// This listing at `start`, as a link that keeps the other filters.
    fn url(&self, start: u64) -> String {
        self.url_with(self.order, start)
    }

    /// This listing in `order`, at `start`.
    fn url_with(&self, order: Option<Order>, start: u64) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        query.append_pair("project", &self.project);
        if let Some(t) = &self.doc_type {
            query.append_pair("type", t);
        }
        if let Some(s) = &self.status {
            query.append_pair("status", s);
        }
        if let Some(order) = order {
            query.append_pair("sort", order.column.name());
            if order.descending {
                query.append_pair("dir", "desc");
            }
        }
        if start != 1 {
            query.append_pair("start", &start.to_string());
        }
        if self.limit != DEFAULT_LIST_LIMIT {
            query.append_pair("limit", &self.limit.to_string());
        }
        format!("/admin/documents?{}", query.finish())
    }

    /// The column headers, each linking to its next order from the first
    /// page.
    fn headers(&self) -> Vec<Header> {
        let sortable = |label, column: Column| {
            let current = self.order.filter(|o| o.column == column);
            let next = match current {
                None => Some(Order {
                    column,
                    descending: false,
                }),
                Some(Order {
                    descending: false, ..
                }) => Some(Order {
                    column,
                    descending: true,
                }),
                Some(_) => None,
            };
            Header {
                label,
                class: if column == Column::Revision {
                    "num"
                } else {
                    ""
                },
                href: Some(self.url_with(next, 1)),
                aria_sort: current.map(|o| {
                    if o.descending {
                        "descending"
                    } else {
                        "ascending"
                    }
                }),
                key: column.name(),
            }
        };
        vec![
            sortable("ID", Column::Id),
            sortable("Title", Column::Title),
            sortable("Type", Column::Type),
            sortable("Status", Column::Status),
            sortable("Revision", Column::Revision),
            Header {
                label: "Files",
                class: "num",
                href: None,
                aria_sort: None,
                key: "files",
            },
            sortable("Updated (UTC)", Column::Updated),
        ]
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
    let order = Order::parse(request.query("sort"), request.query("dir"))?;
    let filters = Filters {
        doc_type: request
            .query("type")
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        status: request
            .query("status")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        order,
        start: number(request, "start", 1)?,
        limit: number(request, "limit", DEFAULT_LIST_LIMIT)?,
        project: name,
    };
    let listing = store
        .list(&ListRequest {
            project: filters.project.clone(),
            doc_type: filters.doc_type.clone(),
            status: filters.status.clone(),
            sort: Order::sort(filters.order),
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
    let results = DocumentsResults {
        project: projects.iter().find(|p| p.name == filters.project).cloned(),
        headers: filters.headers(),
        sort: filters.order.map(|o| (o.column.name(), o.descending)),
        total: listing.total,
        start: filters.start,
        shown_to: filters.start + listing.items.len() as u64 - u64::from(!listing.items.is_empty()),
        previous: (filters.start > 1)
            .then(|| filters.url(filters.start.saturating_sub(filters.limit).max(1))),
        next: listing.next_start.map(|next| filters.url(next)),
        items: listing.items,
    };
    if request.wants_fragment() {
        return Ok(render(StatusCode::OK, &results));
    }
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
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
        results,
    };
    Ok(render(StatusCode::OK, &page))
}

/// The library before any project exists.
fn empty(session: &Session) -> Response {
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        projects: Vec::new(),
        types: Vec::new(),
        statuses: Vec::new(),
        results: DocumentsResults {
            project: None,
            headers: Vec::new(),
            sort: None,
            items: Vec::new(),
            total: 0,
            start: 1,
            shown_to: 0,
            previous: None,
            next: None,
        },
    };
    render(StatusCode::OK, &page)
}
