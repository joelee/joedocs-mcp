//! The pages: one module per workspace, each with its handlers and the
//! Askama structs of its templates.

pub(crate) mod login;

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::assets::{self, Urls};
use super::{Route, Session, WebError, WebRequest, WebState, render};
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

/// The page after signing in, until the Overview replaces it.
#[derive(Template)]
#[template(path = "home.html")]
struct HomePage<'a> {
    title: &'a str,
    assets: Urls,
    admin: &'a str,
    expires_at: &'a str,
    csrf: &'a str,
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
    _state: &WebState,
    route: &Route,
    _request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    match route {
        Route::Logout => Ok(login::logout(store, session)),
        Route::Home => Ok(render(
            StatusCode::OK,
            &HomePage {
                title: "Overview",
                assets: assets::urls(),
                admin: &session.admin.name,
                expires_at: &session.expires_at,
                csrf: &session.csrf,
            },
        )),
        _ => Err(WebError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such page",
        )),
    }
}
