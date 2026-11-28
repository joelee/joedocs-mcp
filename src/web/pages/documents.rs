//! The document library: the live documents of one project, several or
//! all, filtered by types and statuses with the Board's filter parts
//! (JD-81), paged as `joedocs list` does, and sorted from the View menu or
//! any column header across every page.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::filters::{self, Group};
use super::{Chrome, Section};
use crate::cli::DEFAULT_LIST_LIMIT;
use crate::db::{ProjectRow, Sort};
use crate::store::{DocumentInfo, ListAcross, Store};

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

    /// The column's name in the View menu.
    fn view_label(self) -> &'static str {
        match self {
            Self::Id => "Document ID",
            Self::Updated => "Last updated",
            other => other.label(),
        }
    }

    /// The column's name as its header shows it, without the time zone.
    fn label(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Title => "Title",
            Self::Type => "Type",
            Self::Status => "Status",
            Self::Revision => "Revision",
            Self::Updated => "Updated",
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

    /// What a screen reader hears when the results change in this order.
    fn announcement(order: Option<Self>) -> String {
        match order {
            None => "Documents updated. Most recently updated first.".to_owned(),
            Some(Self { column, descending }) => format!(
                "Documents updated. Sorted by {}, {}.",
                column.label(),
                if descending {
                    "descending"
                } else {
                    "ascending"
                }
            ),
        }
    }

    /// The store's order for this one, or the default.
    fn sort(order: Option<Self>) -> Sort {
        let Some(Self { column, descending }) = order else {
            return Sort::default();
        };
        match (column, descending) {
            (Column::Id, false) => Sort::IdAsc,
            (Column::Id, true) => Sort::IdDesc,
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
    panel: Panel,
    view: View,
    results: DocumentsResults,
}

/// The filter panel's choices: projects (a picker), types and statuses
/// (pills, or a picker when there are many).
struct Panel {
    projects: Group,
    types: Group,
    statuses: Group,
    /// How many groups the address narrows: the bar's count and chips.
    active: usize,
    /// Whether the panel is shut (`filters=closed`).
    closed: bool,
    /// No project exists yet.
    empty: bool,
}

/// The View menu: the sort field and the order.
struct View {
    /// The sorted column's name, `updated` by default.
    sort: &'static str,
    /// `desc` or `asc`.
    dir: &'static str,
    /// Every field the menu offers, as `(value, label, chosen)`, in its
    /// order.
    sorts: Vec<(&'static str, &'static str, bool)>,
    /// What the menu's button says, e.g. "Last updated ↓".
    summary: String,
}

impl View {
    /// The menu for `order` (the page's default when `None`: most
    /// recently updated first).
    fn of(order: Option<Order>) -> Self {
        let (column, descending) =
            order.map_or((Column::Updated, true), |o| (o.column, o.descending));
        let fields = [
            Column::Updated,
            Column::Id,
            Column::Title,
            Column::Type,
            Column::Status,
            Column::Revision,
        ];
        Self {
            sort: column.name(),
            dir: if descending { "desc" } else { "asc" },
            sorts: fields
                .iter()
                .map(|c| (c.name(), c.view_label(), *c == column))
                .collect(),
            summary: format!(
                "{} {}",
                column.view_label(),
                if descending { "↓" } else { "↑" }
            ),
        }
    }
}

/// The results region, also sent alone to htmx when a filter changes.
#[derive(Template)]
#[template(path = "documents_results.html")]
struct DocumentsResults {
    /// The project, when exactly one is in view.
    project: Option<ProjectRow>,
    /// Whether rows name their project (more than one in view).
    project_column: bool,
    headers: Vec<Header>,
    /// The order the results are in, `(column, descending)`, for the View
    /// menu to follow after a header's link reloads them.
    order: (&'static str, bool),
    /// What the region announces when htmx swaps it in: the order.
    announcement: String,
    items: Vec<DocumentInfo>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
}

/// What the page lists, from its query.
struct Filters {
    /// The projects, types and statuses the address names (none: all).
    projects: Vec<String>,
    types: Vec<String>,
    statuses: Vec<String>,
    /// The panel is shut (`filters=closed`), kept by every link.
    closed: bool,
    order: Option<Order>,
    start: u64,
    limit: u64,
}

impl Filters {
    /// This listing at `start`, as a link that keeps the other filters.
    fn url(&self, start: u64) -> String {
        self.url_with(self.order, start)
    }

    /// This listing in `order`, at `start`. Several values of a key are
    /// one comma-separated list, each value URL-encoded (the Board's
    /// address rule).
    fn url_with(&self, order: Option<Order>, start: u64) -> String {
        let encode =
            |value: &str| form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
        let mut pairs: Vec<String> = Vec::new();
        for (key, values) in [
            ("project", &self.projects),
            ("type", &self.types),
            ("status", &self.statuses),
        ] {
            if !values.is_empty() {
                let list: Vec<String> = values.iter().map(|v| encode(v)).collect();
                pairs.push(format!("{key}={}", list.join(",")));
            }
        }
        if self.closed {
            pairs.push("filters=closed".to_owned());
        }
        if let Some(order) = order {
            pairs.push(format!("sort={}", order.column.name()));
            if order.descending {
                pairs.push("dir=desc".to_owned());
            }
        }
        if start != 1 {
            pairs.push(format!("start={start}"));
        }
        if self.limit != DEFAULT_LIST_LIMIT {
            pairs.push(format!("limit={}", self.limit));
        }
        if pairs.is_empty() {
            "/admin/documents".to_owned()
        } else {
            format!("/admin/documents?{}", pairs.join("&"))
        }
    }

    /// The column headers, each linking to its next order from the first
    /// page; with `project_column`, a Project column after ID.
    fn headers(&self, project_column: bool) -> Vec<Header> {
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
        let plain = |label, class, key| Header {
            label,
            class,
            href: None,
            aria_sort: None,
            key,
        };
        let mut headers = vec![sortable("ID", Column::Id)];
        if project_column {
            headers.push(plain("Project", "", "project"));
        }
        headers.extend([
            sortable("Title", Column::Title),
            sortable("Type", Column::Type),
            sortable("Status", Column::Status),
            sortable("Revision", Column::Revision),
            plain("Files", "num", "files"),
            sortable("Updated (UTC)", Column::Updated),
        ]);
        headers
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
    if projects.is_empty() {
        return Ok(empty(session));
    }
    let settings = store.settings();
    let names: Vec<String> = projects.iter().map(|p| p.name.clone()).collect();
    let unknown = filters::unknown_values(
        request,
        &[
            ("project", &names),
            ("type", &settings.types),
            ("status", &settings.statuses),
        ],
        &[],
    );
    if !unknown.is_empty() {
        return Err(filters::unknown_refusal(
            &filters::Refusing {
                address: "The Documents page's address",
                path: "/admin/documents",
                reset: "Reset the Documents view",
                view: "documents",
            },
            &unknown,
        ));
    }
    let order = Order::parse(request.query("sort"), request.query("dir"))?;
    let filters = Filters {
        projects: filters::named(request, "project", &names),
        types: filters::named(request, "type", &settings.types),
        statuses: filters::named(request, "status", &settings.statuses),
        closed: request.query("filters") == Some("closed"),
        order,
        start: number(request, "start", 1)?,
        limit: number(request, "limit", DEFAULT_LIST_LIMIT)?,
    };
    // In view: the projects named, or every project.
    let in_view = if filters.projects.is_empty() {
        names.clone()
    } else {
        filters.projects.clone()
    };
    let listing = store
        .list_across(&ListAcross {
            projects: in_view.clone(),
            doc_types: filters.types.clone(),
            statuses: filters.statuses.clone(),
            sort: Order::sort(filters.order),
            start: filters.start,
            limit: filters.limit,
        })
        .map_err(|e| WebError::from_store(&e))?;
    let project_column = in_view.len() > 1;
    let view = View::of(filters.order);
    let results = DocumentsResults {
        project: match in_view.as_slice() {
            [only] => projects.iter().find(|p| p.name == *only).cloned(),
            _ => None,
        },
        project_column,
        headers: filters.headers(project_column),
        order: (view.sort, view.dir == "desc"),
        announcement: Order::announcement(filters.order),
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
    let panel = Panel {
        projects: filters::group(&projects, &names, &filters.projects, true),
        // Only projects have titles: a type or status named like a project
        // keeps its own name.
        types: filters::group(&[], &settings.types, &filters.types, false),
        statuses: filters::group(&[], &settings.statuses, &filters.statuses, false),
        active: usize::from(!filters.projects.is_empty())
            + usize::from(!filters.types.is_empty())
            + usize::from(!filters.statuses.is_empty()),
        closed: filters.closed,
        empty: false,
    };
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        panel,
        view,
        results,
    };
    Ok(render(StatusCode::OK, &page))
}

/// The library before any project exists.
fn empty(session: &Session) -> Response {
    let none = || filters::group(&[], &[], &[], false);
    let page = DocumentsPage {
        title: "Documents",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        panel: Panel {
            projects: none(),
            types: none(),
            statuses: none(),
            active: 0,
            closed: false,
            empty: true,
        },
        view: View::of(None),
        results: DocumentsResults {
            project: None,
            project_column: false,
            headers: Vec::new(),
            order: ("updated", true),
            announcement: Order::announcement(None),
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
