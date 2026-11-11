//! The pages: one module per workspace, each with its handlers and the
//! Askama structs of its templates.

mod archive;
mod audit;
mod document;
mod documents;
mod history;
mod keys;
pub(crate) mod login;
pub(crate) mod maintenance;
mod overview;
mod projects;
mod search;

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::assets::Urls;
use super::{Route, Session, WebError, WebRequest, WebState};
use crate::store::Store;

/// The error page.
#[derive(Template)]
#[template(path = "error.html")]
pub(crate) struct ErrorPage<'a> {
    pub(crate) title: &'a str,
    pub(crate) assets: Urls,
    pub(crate) status: u16,
    pub(crate) message: &'a str,
    /// A link to where the thing can be found instead.
    pub(crate) link: Option<(&'a str, &'a str)>,
}

/// A workspace in the navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Overview,
    Documents,
    Projects,
    Keys,
    Audit,
    Archive,
    Maintenance,
    /// The Search page, reached from the search box, not the navigation.
    Search,
}

impl Section {
    const ALL: [Self; 7] = [
        Self::Overview,
        Self::Documents,
        Self::Projects,
        Self::Keys,
        Self::Audit,
        Self::Archive,
        Self::Maintenance,
    ];

    fn href(self) -> &'static str {
        match self {
            Self::Overview => "/admin/",
            Self::Documents => "/admin/documents",
            Self::Projects => "/admin/projects",
            Self::Keys => "/admin/keys",
            Self::Audit => "/admin/audit",
            Self::Archive => "/admin/archive",
            Self::Maintenance => "/admin/maintenance",
            Self::Search => "/admin/search",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Documents => "Documents",
            Self::Projects => "Projects",
            Self::Keys => "API keys",
            Self::Audit => "Audit",
            Self::Archive => "Archive",
            Self::Maintenance => "Maintenance",
            Self::Search => "Search",
        }
    }
}

/// One navigation link.
pub(crate) struct NavItem {
    pub(crate) href: &'static str,
    pub(crate) label: &'static str,
    pub(crate) current: bool,
}

/// What the layout around every signed-in page shows: the session menu,
/// the search box and the navigation.
pub(crate) struct Chrome<'a> {
    pub(crate) admin: &'a str,
    pub(crate) expires_at: &'a str,
    pub(crate) csrf: &'a str,
    pub(crate) query: &'a str,
    pub(crate) nav: Vec<NavItem>,
}

impl<'a> Chrome<'a> {
    /// The layout for `session`, with `section` marked as the current page.
    pub(crate) fn new(session: &'a Session, section: Section, query: &'a str) -> Self {
        Self {
            admin: &session.admin.name,
            expires_at: &session.expires_at,
            csrf: &session.csrf,
            query,
            nav: Section::ALL
                .iter()
                .map(|s| NavItem {
                    href: s.href(),
                    label: s.label(),
                    current: *s == section,
                })
                .collect(),
        }
    }
}

/// `path` with the query `pairs` that have a non-empty value, in order.
pub(crate) fn link(path: &str, pairs: &[(&str, Option<&str>)]) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            query.append_pair(key, value);
        }
    }
    let query = query.finish();
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    }
}

/// The signed-in admin as the author of a change: the admin token's name,
/// from the web interface.
pub(crate) fn actor(session: &Session) -> crate::store::Actor {
    crate::store::Actor::new(&session.admin.name, super::CLIENT)
}

/// The refusal for a store error, shown with a link back to `href`.
pub(crate) fn refused_with_link(error: &crate::store::Error, href: &str, label: &str) -> WebError {
    let refusal = WebError::from_store(error);
    let page = ErrorPage {
        title: "Refused",
        assets: super::assets::urls(),
        status: refusal.status.as_u16(),
        message: &refusal.message,
        link: Some((href, label)),
    };
    let page = super::render(refusal.status, &page);
    refusal.with_page(page)
}

/// A route anyone may use.
pub(crate) fn public(
    state: &WebState,
    route: &Route,
    request: &WebRequest,
) -> Result<Response, WebError> {
    match route {
        Route::LoginPage => Ok(login::form(state, request, StatusCode::OK, None)),
        _ => Err(WebError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such page",
        )),
    }
}

/// A route for a signed-in admin.
pub(crate) fn page(
    store: &mut Store,
    state: &WebState,
    route: &Route,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    match route {
        Route::Logout => Ok(login::logout(store, session)),
        Route::Home => overview::page(store, state, session),
        Route::DocumentsList => documents::list(store, request, session),
        Route::Document(id) => document::view(store, state, request, session, id),
        Route::History(id) => history::history(store, request, session, id),
        Route::Diff(id) => history::diff(store, request, session, id),
        Route::Download(id) => document::download(store, request, id),
        Route::Search => search::page(store, request, session),
        Route::ProjectsList => projects::list(store, request, session),
        Route::ProjectsAdd => projects::add(store, request, session),
        Route::KeysList => keys::list(store, request, session),
        Route::KeysCreate => keys::create(store, request, session),
        Route::KeysGrant(name) => keys::grant(store, request, session, name),
        Route::KeysUngrant(name) => keys::ungrant(store, request, session, name),
        Route::KeysRotate(name) => keys::rotate(store, session, name),
        Route::KeysRevoke(name) => keys::revoke(store, session, name),
        Route::Audit => audit::page(store, request, session),
        Route::ArchiveList => archive::list(store, request, session),
        Route::ArchiveRestore(id) => archive::restore(store, session, id),
        Route::DocumentConvert(id) => document::convert(store, session, id),
        Route::FileMove(id) => document::move_file(store, request, session, id),
        Route::FileRestore(id) => document::restore_file(store, request, session, id),
        Route::Maintenance => maintenance::page(store, session),
        Route::Check => maintenance::check(store, session),
        Route::Reindex => maintenance::reindex(store, session),
        _ => Err(WebError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such page",
        )),
    }
}
