//! The admin web interface, served by `joedocs serve` under `/admin/` when
//! `admin.enabled` and `admin.web_ui` are both on (JD-32).
//!
//! Pages are rendered on the server with Askama templates; htmx only makes
//! them faster, and every page and form also works as a plain link or form
//! post. An admin signs in with an admin token and a TOTP code; the server
//! then keeps a web session (`jdw_...`) in an `HttpOnly`, `SameSite=Strict`
//! cookie, and the admin token is never stored by the browser.
//!
//! Every request, in this order: the `Host` check (`server.allowed_hosts`);
//! static files are then served without further checks or audit; for a
//! page, the web session; for a `POST`, the same-origin rule and the CSRF
//! token; then the handler, on a blocking thread with the store; then one
//! audit row with client `joedocs-web` and the tool below. Nothing runs
//! before it is checked.
//!
//! | Route | Audit tool |
//! | --- | --- |
//! | `GET /admin/login` | `admin:web.login_page` |
//! | `POST /admin/login` | `admin:session.create` |
//! | `POST /admin/logout` | `admin:session.delete` |
//! | `GET /admin/` | `admin:status` |
//! | `GET /admin/documents` | `admin:documents.list` |
//! | `GET /admin/documents/{id}` | `admin:documents.get` |
//! | `GET /admin/documents/{id}/history` | `admin:documents.history` |
//! | `GET /admin/documents/{id}/diff` | `admin:documents.diff` |
//! | `GET /admin/documents/{id}/download` | `admin:documents.get` |
//! | `GET /admin/search` | `admin:documents.search` |
//! | `GET /admin/projects` | `admin:projects.list` |
//! | `POST /admin/projects` | `admin:projects.add` |
//! | `GET /admin/keys` | `admin:keys.list` |
//! | `POST /admin/keys` | `admin:keys.create` |
//! | `POST /admin/keys/{name}/grant` | `admin:keys.grant` |
//! | `POST /admin/keys/{name}/ungrant` | `admin:keys.ungrant` |
//! | `POST /admin/keys/{name}/rotate` | `admin:keys.rotate` |
//! | `POST /admin/keys/{name}/revoke` | `admin:keys.revoke` |
//! | `GET /admin/audit` | `admin:audit.list` |
//! | any other path under `/admin/` | `admin:web.unknown` |
//!
//! Only `/admin/` is a stable entry point; other page paths, the HTML and
//! the cookie may change in any release. Scripts use `/admin/v1/`.

mod assets;
pub(crate) mod markdown;
mod pages;
mod plantuml;
mod session;

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::any;
use secrecy::SecretString;

use crate::db::AdminTokenRow;
use crate::mcp::{BODY_LIMIT, ServerOptions, Shared, classify, host_allowed};
use crate::store::{self, Actor, AdminRefusal, Store};

/// The client name on the web interface's audit rows.
pub const CLIENT: &str = "joedocs-web";

/// Web interface settings, from `[admin]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebOptions {
    /// `admin.session_hours`.
    pub session_hours: u32,
    /// `admin.web_plantuml_url`, or empty.
    pub plantuml_url: String,
}

/// The extra directive a page that loads Pele needs: Pele sets `style`
/// attributes on the SVG it draws (no `<style>` elements, no scripts).
pub(crate) const PELE_STYLE_ATTRIBUTES: &str = "; style-src-attr 'unsafe-inline'";

/// State shared by every page request.
pub(crate) struct WebState {
    shared: Arc<Shared>,
    options: WebOptions,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    csp: HeaderValue,
    /// `server.bind`, for the Overview's health probe.
    bind: std::net::SocketAddr,
    /// The config file in use, for the Overview.
    config_file: Option<std::path::PathBuf>,
}

/// The routes: `/` and `/admin` redirect to `/admin/`, and everything under
/// `/admin/` that is not `/admin/v1/` is a page or a static file.
pub(crate) fn routes(shared: &Arc<Shared>, web: &WebOptions, server: &ServerOptions) -> Router {
    let csp = HeaderValue::from_str(&content_security_policy(&web.plantuml_url))
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"));
    let state = Arc::new(WebState {
        shared: Arc::clone(shared),
        options: web.clone(),
        allowed_hosts: server.allowed_hosts.clone(),
        allowed_origins: server.allowed_origins.clone(),
        csp,
        bind: server.bind,
        config_file: server.admin.as_ref().and_then(|a| a.config_file.clone()),
    });
    Router::new()
        .route("/", any(dispatch))
        .route("/admin", any(dispatch))
        .route("/admin/", any(dispatch))
        .route("/admin/{*rest}", any(dispatch))
        .with_state(state)
}

/// The policy every page sends. Only `img-src` varies: it names the
/// `PlantUML` server's origin when one is set.
fn content_security_policy(plantuml_url: &str) -> String {
    let plantuml = plantuml_origin(plantuml_url)
        .map(|origin| format!(" {origin}"))
        .unwrap_or_default();
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:{plantuml}; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    )
}

/// `scheme://host[:port]` of a `PlantUML` server URL.
fn plantuml_origin(url: &str) -> Option<&str> {
    let after = url.find("://")? + 3;
    let end = url[after..].find('/').map_or(url.len(), |i| after + i);
    (end > after).then(|| &url[..end])
}

/// A page route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    LoginPage,
    Login,
    Logout,
    Home,
    DocumentsList,
    Document(String),
    History(String),
    Diff(String),
    Download(String),
    Search,
    ProjectsList,
    ProjectsAdd,
    KeysList,
    KeysCreate,
    KeysGrant(String),
    KeysUngrant(String),
    KeysRotate(String),
    KeysRevoke(String),
    Audit,
    Unknown,
}

/// Who may use a route.
enum Access {
    /// Anyone (the login form).
    Public,
    /// Logging in: the same-origin rule, but no session yet.
    Login,
    /// A current web session, and for a `POST` the CSRF token.
    Session,
}

impl Route {
    fn of(method: &Method, path: &str) -> Self {
        let get = *method == Method::GET || *method == Method::HEAD;
        let post = *method == Method::POST;
        let parts: Vec<&str> = path.trim_start_matches("/admin/").split('/').collect();
        match (path, parts.as_slice()) {
            ("/admin/login", _) if get => Self::LoginPage,
            ("/admin/login", _) if post => Self::Login,
            ("/admin/logout", _) if post => Self::Logout,
            ("/admin/", _) if get => Self::Home,
            (_, ["documents"]) if get => Self::DocumentsList,
            (_, ["documents", id]) if get && !id.is_empty() => Self::Document(percent_decode(id)),
            (_, ["documents", id, "history"]) if get && !id.is_empty() => {
                Self::History(percent_decode(id))
            }
            (_, ["documents", id, "diff"]) if get && !id.is_empty() => {
                Self::Diff(percent_decode(id))
            }
            (_, ["documents", id, "download"]) if get && !id.is_empty() => {
                Self::Download(percent_decode(id))
            }
            ("/admin/search", _) if get => Self::Search,
            ("/admin/projects", _) if get => Self::ProjectsList,
            ("/admin/projects", _) if post => Self::ProjectsAdd,
            ("/admin/keys", _) if get => Self::KeysList,
            ("/admin/keys", _) if post => Self::KeysCreate,
            ("/admin/audit", _) if get => Self::Audit,
            (_, ["keys", name, action]) if post && !name.is_empty() => {
                let name = percent_decode(name);
                match *action {
                    "grant" => Self::KeysGrant(name),
                    "ungrant" => Self::KeysUngrant(name),
                    "rotate" => Self::KeysRotate(name),
                    "revoke" => Self::KeysRevoke(name),
                    _ => Self::Unknown,
                }
            }
            _ => Self::Unknown,
        }
    }

    /// The audit log's tool name: the matching admin API operation, or
    /// `admin:web.<page>` for a page the API does not have.
    fn operation(&self) -> &'static str {
        match self {
            Self::LoginPage => "admin:web.login_page",
            Self::Login => "admin:session.create",
            Self::Logout => "admin:session.delete",
            Self::Home => "admin:status",
            Self::DocumentsList => "admin:documents.list",
            Self::Document(_) | Self::Download(_) => "admin:documents.get",
            Self::History(_) => "admin:documents.history",
            Self::Diff(_) => "admin:documents.diff",
            Self::Search => "admin:documents.search",
            Self::ProjectsList => "admin:projects.list",
            Self::ProjectsAdd => "admin:projects.add",
            Self::KeysList => "admin:keys.list",
            Self::KeysCreate => "admin:keys.create",
            Self::KeysGrant(_) => "admin:keys.grant",
            Self::KeysUngrant(_) => "admin:keys.ungrant",
            Self::KeysRotate(_) => "admin:keys.rotate",
            Self::KeysRevoke(_) => "admin:keys.revoke",
            Self::Audit => "admin:audit.list",
            Self::Unknown => "admin:web.unknown",
        }
    }

    fn access(&self) -> Access {
        match self {
            Self::LoginPage => Access::Public,
            Self::Login => Access::Login,
            // Everything else needs a session, so a new route is closed
            // until it is made public on purpose.
            _ => Access::Session,
        }
    }
}

/// A request, read once.
pub(crate) struct WebRequest {
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    raw_query: Option<String>,
    headers: HeaderMap,
    form: Vec<(String, String)>,
}

impl WebRequest {
    fn new(parts: &axum::http::request::Parts, body: &Bytes) -> Self {
        let raw_query = parts.uri.query().map(str::to_owned);
        let query = raw_query
            .as_deref()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_default();
        let form = if parts.method == Method::POST {
            form_urlencoded::parse(body).into_owned().collect()
        } else {
            Vec::new()
        };
        Self {
            method: parts.method.clone(),
            path: parts.uri.path().to_owned(),
            query,
            raw_query,
            headers: parts.headers.clone(),
            form,
        }
    }

    /// The first query parameter `key`.
    pub(crate) fn query(&self, key: &str) -> Option<&str> {
        first(&self.query, key)
    }

    /// The first form field `key`.
    pub(crate) fn form(&self, key: &str) -> Option<&str> {
        first(&self.form, key)
    }

    /// Every query parameter `key`, in order.
    pub(crate) fn query_all(&self, key: &str) -> Vec<&str> {
        self.query
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Whether htmx sent it.
    pub(crate) fn is_htmx(&self) -> bool {
        session::text(&self.headers, "hx-request") == Some("true")
    }

    /// Whether htmx asked for part of a page: a region to swap in. A
    /// history restore asks for the whole page (`HX-Request-Type: full`).
    pub(crate) fn wants_fragment(&self) -> bool {
        self.is_htmx() && session::text(&self.headers, "hx-request-type") == Some("partial")
    }

    /// The path and query, to come back to after logging in.
    fn here(&self) -> String {
        match &self.raw_query {
            Some(q) if !q.is_empty() => format!("{}?{q}", self.path),
            _ => self.path.clone(),
        }
    }
}

/// A path segment with `%XX` escapes decoded.
fn percent_decode(segment: &str) -> String {
    form_urlencoded::parse(format!("x={}", segment.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn first<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// The signed-in admin of a page request.
pub(crate) struct Session {
    /// The admin token that opened the session.
    pub(crate) admin: AdminTokenRow,
    /// When the session expires.
    pub(crate) expires_at: String,
    /// The CSRF token forms carry.
    pub(crate) csrf: String,
    /// The session token itself, to end it.
    pub(crate) token: SecretString,
}

/// A refused or failed request: its status, its audit outcome, and the page
/// to show (an error page unless the handler gives its own).
pub(crate) struct WebError {
    status: StatusCode,
    code: &'static str,
    message: String,
    page: Option<Box<Response>>,
}

impl WebError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            page: None,
        }
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn from_store(error: &store::Error) -> Self {
        let (status, code) = classify(error);
        let message = if code == "internal" {
            "internal error".to_owned()
        } else {
            error.to_string()
        };
        Self::new(status, code, message)
    }

    /// Shows `page` instead of the error page.
    pub(crate) fn with_page(mut self, page: Response) -> Self {
        self.page = Some(Box::new(page));
        self
    }

    fn into_response(self) -> Response {
        if let Some(page) = self.page {
            return *page;
        }
        let page = pages::ErrorPage {
            title: "Error",
            assets: assets::urls(),
            status: self.status.as_u16(),
            message: &self.message,
            link: None,
        };
        render(self.status, &page)
    }
}

/// `template` as an HTML reply with `status`.
pub(crate) fn render(status: StatusCode, template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => html_reply(status, html),
        Err(error) => {
            tracing::error!(%error, "cannot render a web interface page");
            html_reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                "<!doctype html><title>Error</title><p>internal error</p>".to_owned(),
            )
        }
    }
}

fn html_reply(status: StatusCode, html: String) -> Response {
    let mut response = (status, html).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// A `303 See Other` to `to`.
pub(crate) fn redirect(to: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(to) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

/// Every page request. It uses no extractor that can reject a request
/// before it is checked and audited.
async fn dispatch(State(state): State<Arc<WebState>>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let host_ok = host_allowed(&state.allowed_hosts, &parts.headers);
    if assets::is_asset_path(path) {
        let mut response = if host_ok {
            assets::serve(path)
        } else {
            StatusCode::FORBIDDEN.into_response()
        };
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, state.csp.clone());
        return response;
    }
    if host_ok && (path == "/" || path == "/admin") {
        return secured(redirect("/admin/"), &state.csp);
    }
    let route = Route::of(&parts.method, path);
    let Ok(body) = axum::body::to_bytes(body, BODY_LIMIT).await else {
        let request = WebRequest::new(&parts, &Bytes::new());
        let too_large = WebError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("request bodies are limited to {BODY_LIMIT} bytes"),
        );
        return run(state, route, request, Err(too_large)).await;
    };
    let request = WebRequest::new(&parts, &body);
    let checked = if host_ok {
        Ok(())
    } else {
        Err(WebError::forbidden("this Host is not allowed"))
    };
    run(state, route, request, checked).await
}

/// Runs the checks and the handler on the store in a blocking task, and
/// writes the audit row whatever happens.
async fn run(
    state: Arc<WebState>,
    route: Route,
    request: WebRequest,
    checked: Result<(), WebError>,
) -> Response {
    let csp = state.csp.clone();
    let task = tokio::task::spawn_blocking(move || {
        let Ok(mut store) = state.shared.store().lock() else {
            return WebError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
            .into_response();
        };
        let (who, result) = match checked {
            Err(refused) => (None, Err(refused)),
            Ok(()) => handle(&mut store, &state, &route, &request),
        };
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => error.code,
        };
        write_audit(&store, who.as_ref(), route.operation(), outcome);
        result.unwrap_or_else(WebError::into_response)
    });
    let response = match task.await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "web interface task failed");
            WebError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
            .into_response()
        }
    };
    secured(response, &csp)
}

/// Checks the session and, for a `POST`, the same-origin rule and the CSRF
/// token, then runs the page. Returns who made the request, when known.
fn handle(
    store: &mut Store,
    state: &WebState,
    route: &Route,
    request: &WebRequest,
) -> (Option<AdminTokenRow>, Result<Response, WebError>) {
    match route.access() {
        Access::Public => (None, pages::public(state, route, request)),
        Access::Login => pages::login::submit(store, state, request),
        Access::Session => {
            let session = match open_session(store, request) {
                Ok(session) => session,
                Err(refused) => return (None, Err(refused)),
            };
            let who = Some(session.admin.clone());
            if request.method == Method::POST {
                if !session::same_origin(&request.headers, &state.allowed_origins) {
                    return (
                        who,
                        Err(WebError::forbidden(
                            "this form was not sent from this site; reload the page and try again",
                        )),
                    );
                }
                if !session::csrf_matches(&session.token, request.form("csrf")) {
                    return (
                        who,
                        Err(WebError::forbidden(
                            "this form has expired; reload the page and try again",
                        )),
                    );
                }
            }
            (who, pages::page(store, state, route, request, &session))
        }
    }
}

/// The request's web session, or a refusal that sends the browser to the
/// login page (and back here afterwards, for a page).
fn open_session(store: &Store, request: &WebRequest) -> Result<Session, WebError> {
    let cookie = session::session_cookie(&request.headers);
    let checked = cookie
        .as_ref()
        .ok_or_else(|| store::Error::AdminRefused(AdminRefusal::NoSession))
        .and_then(|token| store.web_session(token));
    match (checked, cookie) {
        (Ok(web), Some(token)) => Ok(Session {
            csrf: session::csrf_token(&token),
            admin: web.admin,
            expires_at: web.expires_at,
            token,
        }),
        (Err(error), cookie) => {
            let next = if request.method == Method::GET {
                request.here()
            } else {
                "/admin/".to_owned()
            };
            let mut to = "/admin/login?next=".to_owned();
            to.extend(form_urlencoded::byte_serialize(next.as_bytes()));
            let mut page = if request.is_htmx() {
                let mut page = StatusCode::OK.into_response();
                if let Ok(value) = HeaderValue::from_str(&to) {
                    page.headers_mut().insert("hx-redirect", value);
                }
                page
            } else {
                redirect(&to)
            };
            if cookie.is_some() {
                for clear in session::clear_cookies() {
                    page.headers_mut()
                        .append(header::SET_COOKIE, HeaderValue::from_static(clear));
                }
            }
            Err(WebError::from_store(&error).with_page(page))
        }
        (Ok(_), None) => Err(WebError::from_store(&store::Error::AdminRefused(
            AdminRefusal::NoSession,
        ))),
    }
}

/// Adds the headers every reply carries. A page that set its own policy
/// (one that loads Pele) keeps it.
///
/// The referrer policy is `same-origin`, not `no-referrer`: other sites
/// still get no referrer, but under `no-referrer` browsers send
/// `Origin: null` on the interface's own form posts, which the same-origin
/// check must refuse.
fn secured(mut response: Response, csp: &HeaderValue) -> Response {
    let headers = response.headers_mut();
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp.clone());
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Writes the audit row for a page request.
fn write_audit(store: &Store, who: Option<&AdminTokenRow>, tool: &str, outcome: &str) {
    let (id, name) = who.map_or(("unknown", "unknown"), |w| (w.id.as_str(), w.name.as_str()));
    if let Err(error) = store.audit(id, &Actor::new(name, CLIENT), tool, None, outcome) {
        tracing::error!(%error, "cannot write the web interface audit entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_names_the_plantuml_origin_only_when_set() {
        let plain = content_security_policy("");
        assert!(plain.contains("img-src 'self' data:;"), "{plain}");
        let with = content_security_policy("https://www.plantuml.com/plantuml");
        assert!(
            with.contains("img-src 'self' data: https://www.plantuml.com;"),
            "{with}"
        );
        assert_eq!(
            plantuml_origin("http://localhost:8080"),
            Some("http://localhost:8080")
        );
        assert_eq!(plantuml_origin("nonsense"), None);
    }

    #[test]
    fn routes_and_their_audit_tools() {
        assert_eq!(Route::of(&Method::GET, "/admin/login"), Route::LoginPage);
        assert_eq!(Route::of(&Method::POST, "/admin/login"), Route::Login);
        assert_eq!(Route::of(&Method::POST, "/admin/logout"), Route::Logout);
        assert_eq!(Route::of(&Method::GET, "/admin/logout"), Route::Unknown);
        assert_eq!(Route::of(&Method::GET, "/admin/"), Route::Home);
        assert_eq!(Route::Home.operation(), "admin:status");
        assert_eq!(Route::Unknown.operation(), "admin:web.unknown");
    }
}
