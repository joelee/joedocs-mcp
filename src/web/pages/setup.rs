//! Signing in as a user on the web (0.5.1): the sign-in form's post, and
//! the TOTP setup page shown at a user's first sign-in (or a password
//! change after a reset).
//!
//! Both posts hash a password, so they run in three phases outside the
//! page dispatcher's store lock (JD-36 C-04): read under the lock, hash
//! through the server-wide gate (at most two at once), then finish and
//! audit under the lock. The setup ticket travels in its own `HttpOnly`,
//! `SameSite=Strict` cookie, never in the page.

use std::sync::Arc;

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use secrecy::{ExposeSecret as _, SecretString};

use super::super::assets::{self, Urls};
use super::super::session::{self, safe_next};
use super::super::{Route, WebError, WebRequest, WebState, redirect, render, write_audit};
use super::login;
use crate::auth::OsRandom;
use crate::password::{self, Verified};
use crate::store::{Admin, AdminRefusal, Error, SETUP_MINUTES, SignInRequest, SignedIn, Store};

/// A TOTP secret on the setup page.
pub(crate) struct TotpView {
    pub(crate) secret: String,
    pub(crate) uri: String,
    /// The QR code of `uri`, drawn on the server as SVG.
    pub(crate) svg: String,
}

#[derive(Template)]
#[template(path = "setup.html")]
struct SetupPage<'a> {
    title: &'a str,
    assets: Urls,
    user: &'a str,
    expires_at: &'a str,
    next: &'a str,
    error: Option<&'a str>,
    totp: Option<TotpView>,
}

/// `GET /admin/setup`: the setup page for the ticket in the setup cookie.
/// Without a valid ticket, the sign-in form again.
pub(crate) fn page(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
) -> (Option<Admin>, Result<Response, WebError>) {
    let Some(ticket) = session::setup_cookie(&request.headers) else {
        return (None, Err(lapsed(state, request)));
    };
    match store.setup_view(&ticket) {
        Ok(view) => {
            let shown = setup_page(request, &view, None, StatusCode::OK);
            (Some(Admin::User(view.user)), Ok(shown))
        }
        Err(_) => (None, Err(lapsed(state, request))),
    }
}

/// The setup page for a ticket's `view`.
fn setup_page(
    request: &WebRequest,
    view: &crate::store::SetupView,
    error: Option<&str>,
    status: StatusCode,
) -> Response {
    let next = safe_next(request.query("next").or_else(|| request.form("next")));
    let totp = view.totp.as_ref().map(|t| {
        let uri = t.uri.expose_secret().to_owned();
        TotpView {
            secret: t.secret.expose_secret().to_owned(),
            svg: qr_svg(&uri),
            uri,
        }
    });
    render(
        status,
        &SetupPage {
            title: "Set up your sign-in",
            assets: assets::urls(),
            user: &view.user.name,
            expires_at: &view.expires_at,
            next: &next,
            error,
            totp,
        },
    )
}

/// The sign-in form, saying the setup has lapsed.
fn lapsed(state: &WebState, request: &WebRequest) -> WebError {
    let message =
        "This setup has ended (it lasts 10 minutes); sign in again with your one-time password.";
    let page = login::form(state, request, StatusCode::UNAUTHORIZED, Some(message));
    WebError::new(StatusCode::UNAUTHORIZED, "unauthorized", message).with_page(page)
}

/// `uri` as an SVG QR code for inline use: no XML declaration, sized by
/// the stylesheet.
fn qr_svg(uri: &str) -> String {
    use qrcode::render::svg;
    let Ok(code) = qrcode::QrCode::new(uri.as_bytes()) else {
        return String::new();
    };
    let drawn = code
        .render::<svg::Color<'_>>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    drawn
        .find("<svg")
        .map_or(drawn.clone(), |start| drawn[start..].to_owned())
}

/// Whether a sign-in post may go on: same origin, and HTTPS or loopback.
fn allowed(state: &WebState, request: &WebRequest) -> Result<bool, WebError> {
    if !session::same_origin(&request.headers, &state.allowed_origins) {
        return Err(WebError::forbidden("this form was not sent from this site"));
    }
    let secure = session::is_https(&request.headers);
    if !secure && !session::is_loopback(&request.headers) {
        let message = "Signing in needs HTTPS: open this site with https://, or on the server itself with http://localhost.";
        let page = login::form(state, request, StatusCode::BAD_REQUEST, Some(message));
        return Err(
            WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", message).with_page(page),
        );
    }
    Ok(secure)
}

/// `POST /admin/sign-in`: a user's name, password and code.
pub(crate) async fn sign_in(state: Arc<WebState>, request: WebRequest) -> Response {
    let tool = Route::SignIn.operation();
    let secure = match allowed(&state, &request) {
        Ok(secure) => secure,
        Err(refused) => return audited(&state, None, tool, refused).await,
    };
    let name = request.form("user").unwrap_or("").trim().to_owned();
    let password = SecretString::from(request.form("password").unwrap_or("").to_owned());
    let code = request
        .form("code")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    let begin_name = name.clone();
    let phase_one = under_lock(&state, move |store| store.sign_in_begin(&begin_name)).await;
    let begin = match phase_one {
        Some(Ok(found)) => found,
        Some(Err(error)) => return refused_form(&state, &request, None, tool, &error).await,
        None => return internal(),
    };
    let who = begin.user().cloned().map(Admin::User);
    if let Some(locked) = begin.locked() {
        let error = Error::AdminRefused(locked.clone());
        return refused_form(&state, &request, who, tool, &error).await;
    }
    let phc = begin.phc().map(str::to_owned);
    let hashed = state
        .shared
        .hashing()
        .run(move || {
            let stored = phc.as_deref().unwrap_or_else(|| password::dummy_hash());
            let verified = password::verify(&password, stored);
            let rehash = (verified.matches && verified.needs_rehash)
                .then(|| password::hash(&password, &OsRandom).ok())
                .flatten();
            (verified, rehash)
        })
        .await;
    let Ok((verified, rehash)) = hashed else {
        return busy(&state, &request, who, tool).await;
    };
    let hours = state.options.session_hours;
    let (state_for, request_for) = (Arc::clone(&state), request);
    let reply = tokio::task::spawn_blocking(move || {
        let Ok(mut store) = state_for.shared.store().lock() else {
            return internal();
        };
        let asked = SignInRequest {
            code: code.as_deref(),
            device: None,
            session_hours: hours,
        };
        let verified: Verified = verified;
        let result = store.sign_in_finish(&begin, verified, rehash.as_deref(), &asked, &OsRandom);
        finish(
            &store,
            &state_for,
            &request_for,
            who.as_ref(),
            tool,
            secure,
            result,
        )
    })
    .await;
    reply.unwrap_or_else(|_| internal())
}

/// Phase 3's reply, audited: a session cookie and the page asked for, or
/// the setup cookie and the setup page, or the form with the refusal.
fn finish(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
    who: Option<&Admin>,
    tool: &str,
    secure: bool,
    result: Result<SignedIn, Error>,
) -> Response {
    match result {
        Ok(SignedIn::Session {
            user,
            session: token,
            ..
        }) => {
            write_audit(store, Some(&Admin::User(user)), tool, "ok");
            let next = safe_next(request.form("next"));
            let mut reply = redirect(&next);
            let max_age = u64::from(state.options.session_hours) * 3600;
            set(&mut reply, &session::set_cookie(&token, secure, max_age));
            for clear in session::clear_setup_cookies() {
                reply
                    .headers_mut()
                    .append(header::SET_COOKIE, HeaderValue::from_static(clear));
            }
            reply
        }
        Ok(SignedIn::Setup { user, ticket, .. }) => {
            write_audit(store, Some(&Admin::User(user)), tool, "ok");
            let next = safe_next(request.form("next"));
            let mut to = "/admin/setup".to_owned();
            if next != "/admin/" {
                to.push_str("?next=");
                to.extend(form_urlencoded::byte_serialize(next.as_bytes()));
            }
            let mut reply = redirect(&to);
            let max_age = u64::try_from(SETUP_MINUTES * 60).unwrap_or(600);
            set(
                &mut reply,
                &session::set_setup_cookie(&ticket, secure, max_age),
            );
            reply
        }
        Err(error) => {
            let refused = refusal(state, request, &error);
            write_audit(store, who, tool, refused.code);
            refused.into_response()
        }
    }
}

/// `POST /admin/setup`: the code from the new authenticator (when one is
/// being set up) and the new password, typed twice.
pub(crate) async fn setup(state: Arc<WebState>, request: WebRequest) -> Response {
    let tool = Route::Setup.operation();
    let secure = match allowed(&state, &request) {
        Ok(secure) => secure,
        Err(refused) => return audited(&state, None, tool, refused).await,
    };
    let Some(ticket) = session::setup_cookie(&request.headers) else {
        let refused = lapsed(&state, &request);
        return audited(&state, None, tool, refused).await;
    };
    let new_password = SecretString::from(request.form("password").unwrap_or("").to_owned());
    let again = request.form("password_again").unwrap_or("");
    let code = request
        .form("code")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    let view_ticket = ticket.clone();
    let viewed = under_lock(&state, move |store| store.setup_view(&view_ticket)).await;
    let user = match viewed {
        Some(Ok(view)) => view.user,
        Some(Err(_)) => {
            let refused = lapsed(&state, &request);
            return audited(&state, None, tool, refused).await;
        }
        None => return internal(),
    };
    let who = Some(Admin::User(user.clone()));
    let problem = if new_password.expose_secret() == again {
        password::check_new(&user.name, &new_password)
            .err()
            .map(|refusal| refusal.to_string())
    } else {
        Some("The two passwords differ; type the same one twice.".to_owned())
    };
    if let Some(problem) = problem {
        return again_page(
            &state,
            &request,
            ticket,
            who,
            tool,
            &problem,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let hashed = state
        .shared
        .hashing()
        .run(move || password::hash(&new_password, &OsRandom))
        .await;
    let new_hash = match hashed {
        Ok(Ok(hash)) => hash,
        Ok(Err(error)) => {
            let error = Error::from(error);
            return refused_form(&state, &request, who, tool, &error).await;
        }
        Err(_) => return busy(&state, &request, who, tool).await,
    };
    let hours = state.options.session_hours;
    let (state_for, request_for) = (Arc::clone(&state), request);
    let reply = tokio::task::spawn_blocking(move || {
        let Ok(mut store) = state_for.shared.store().lock() else {
            return internal();
        };
        let asked = SignInRequest {
            code: code.as_deref(),
            device: None,
            session_hours: hours,
        };
        match store.setup_finish(&ticket, &new_hash, &asked, &OsRandom) {
            Err(Error::AdminRefused(AdminRefusal::Unauthorized)) if store.setup_view(&ticket).is_ok() => {
                write_audit(&store, who.as_ref(), tool, "unauthorized");
                let Ok(view) = store.setup_view(&ticket) else {
                    return internal();
                };
                let message = "That code was not accepted; check the time on your device and try the next one.";
                setup_page(&request_for, &view, Some(message), StatusCode::UNAUTHORIZED)
            }
            result => finish(&store, &state_for, &request_for, who.as_ref(), tool, secure, result),
        }
    })
    .await;
    reply.unwrap_or_else(|_| internal())
}

/// The setup page again with `problem`, audited.
async fn again_page(
    state: &Arc<WebState>,
    request: &WebRequest,
    ticket: SecretString,
    who: Option<Admin>,
    tool: &'static str,
    problem: &str,
    status: StatusCode,
) -> Response {
    let shared = Arc::clone(&state.shared);
    let request_copy = request.clone();
    let problem = problem.to_owned();
    let reply = tokio::task::spawn_blocking(move || {
        let Ok(store) = shared.store().lock() else {
            return internal();
        };
        write_audit(&store, who.as_ref(), tool, "invalid_argument");
        match store.setup_view(&ticket) {
            Ok(view) => setup_page(&request_copy, &view, Some(&problem), status),
            Err(_) => internal(),
        }
    })
    .await;
    reply.unwrap_or_else(|_| internal())
}

/// The sign-in form with the refusal of `error`, audited.
async fn refused_form(
    state: &Arc<WebState>,
    request: &WebRequest,
    who: Option<Admin>,
    tool: &'static str,
    error: &Error,
) -> Response {
    let refused = refusal(state, request, error);
    audited(state, who, tool, refused).await
}

/// `429 busy`: both hashing places stayed taken.
async fn busy(
    state: &Arc<WebState>,
    request: &WebRequest,
    who: Option<Admin>,
    tool: &'static str,
) -> Response {
    let message = password::Busy.to_string();
    let mut page = login::form(
        state,
        request,
        StatusCode::TOO_MANY_REQUESTS,
        Some(&message),
    );
    page.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    let refused = WebError::new(StatusCode::TOO_MANY_REQUESTS, "busy", message).with_page(page);
    audited(state, who, tool, refused).await
}

/// The sign-in form showing why `error` refused the sign-in.
fn refusal(state: &WebState, request: &WebRequest, error: &Error) -> WebError {
    let refusal = WebError::from_store(error);
    let mut page = login::form(state, request, refusal.status, Some(&refusal.message));
    if let Error::AdminRefused(AdminRefusal::Locked { retry_after, .. }) = error
        && let Ok(value) = HeaderValue::from_str(&retry_after.to_string())
    {
        page.headers_mut().insert(header::RETRY_AFTER, value);
    }
    refusal.with_page(page)
}

/// Writes the audit row for `refused` under the lock, and replies with it.
async fn audited(
    state: &Arc<WebState>,
    who: Option<Admin>,
    tool: &'static str,
    refused: WebError,
) -> Response {
    let code = refused.code;
    let shared = Arc::clone(&state.shared);
    let wrote = tokio::task::spawn_blocking(move || {
        if let Ok(store) = shared.store().lock() {
            write_audit(&store, who.as_ref(), tool, code);
        }
    })
    .await;
    if wrote.is_err() {
        tracing::error!("cannot write the web sign-in audit entry");
    }
    refused.into_response()
}

/// Runs `work` with the store on a blocking thread; `None` when the store
/// cannot be had.
async fn under_lock<T, F>(state: &Arc<WebState>, work: F) -> Option<T>
where
    F: FnOnce(&mut Store) -> T + Send + 'static,
    T: Send + 'static,
{
    let shared = Arc::clone(&state.shared);
    tokio::task::spawn_blocking(move || {
        let mut store = shared.store().lock().ok()?;
        Some(work(&mut store))
    })
    .await
    .ok()
    .flatten()
}

fn set(reply: &mut Response, cookie: &SecretString) {
    if let Ok(value) = HeaderValue::from_str(cookie.expose_secret()) {
        reply.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn internal() -> Response {
    WebError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
    .into_response()
}
