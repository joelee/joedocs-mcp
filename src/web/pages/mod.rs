//! The pages: one module per workspace, each with its handlers and the
//! Askama structs of its templates.

pub(crate) mod login;
mod overview;

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
    _request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    match route {
        Route::Logout => Ok(login::logout(store, session)),
        Route::Home => overview::page(store, state, session),
        _ => Err(WebError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such page",
        )),
    }
}
