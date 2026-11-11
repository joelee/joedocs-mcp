//! Signing in and out.

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use secrecy::{ExposeSecret as _, SecretString};

use super::super::assets::{self, Urls};
use super::super::session::{self, safe_next};
use super::super::{Session, WebError, WebRequest, WebState, redirect, render};
use crate::auth::OsRandom;
use crate::db::AdminTokenRow;
use crate::store::{AdminRefusal, Error, Store};

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    title: &'a str,
    assets: Urls,
    next: &'a str,
    error: Option<&'a str>,
}

/// The login form, with `error` shown above it.
pub(crate) fn form(
    _state: &WebState,
    request: &WebRequest,
    status: StatusCode,
    error: Option<&str>,
) -> Response {
    let next = safe_next(request.query("next").or_else(|| request.form("next")));
    render(
        status,
        &LoginPage {
            title: "Sign in",
            assets: assets::urls(),
            next: &next,
            error,
        },
    )
}

/// `POST /admin/login`: checks the token and code and opens a web session.
pub(crate) fn submit(
    store: &mut Store,
    state: &WebState,
    request: &WebRequest,
) -> (Option<AdminTokenRow>, Result<Response, WebError>) {
    if !session::same_origin(&request.headers, &state.allowed_origins) {
        let refused = WebError::forbidden("this form was not sent from this site");
        return (None, Err(refused));
    }
    let secure = session::is_https(&request.headers);
    if !secure && !session::is_loopback(&request.headers) {
        let message = "Signing in needs HTTPS: open this site with https://, or on the server itself with http://localhost.";
        let page = form(state, request, StatusCode::BAD_REQUEST, Some(message));
        let refused = WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", message);
        return (None, Err(refused.with_page(page)));
    }
    let token = SecretString::from(request.form("token").unwrap_or("").trim().to_owned());
    let who = store.admin_token_for(&token).ok().flatten();
    let code = request.form("code").unwrap_or("").trim();
    let hours = state.options.session_hours;
    match store.web_login(&token, code, hours, &OsRandom) {
        Ok(opened) => {
            let next = safe_next(request.form("next"));
            let mut reply = redirect(&next);
            let cookie = session::set_cookie(&opened.token, secure, u64::from(hours) * 3600);
            if let Ok(value) = HeaderValue::from_str(cookie.expose_secret()) {
                reply.headers_mut().insert(header::SET_COOKIE, value);
            }
            (Some(opened.admin), Ok(reply))
        }
        Err(error) => (who, Err(refused(state, request, &error))),
    }
}

/// The login form again, with the refusal's message and status.
fn refused(state: &WebState, request: &WebRequest, error: &Error) -> WebError {
    let refusal = WebError::from_store(error);
    let mut page = form(state, request, refusal.status, Some(&refusal.message));
    if let Error::AdminRefused(AdminRefusal::Locked { retry_after, .. }) = error
        && let Ok(value) = HeaderValue::from_str(&retry_after.to_string())
    {
        page.headers_mut().insert(header::RETRY_AFTER, value);
    }
    refusal.with_page(page)
}

/// `POST /admin/logout`: ends the session and clears the cookie.
pub(crate) fn logout(store: &Store, session: &Session) -> Response {
    if let Err(error) = store.end_web_session(&session.token) {
        tracing::warn!(%error, "cannot end a web session");
    }
    let mut reply = redirect("/admin/login");
    for clear in session::clear_cookies() {
        reply
            .headers_mut()
            .append(header::SET_COOKIE, HeaderValue::from_static(clear));
    }
    reply
}
