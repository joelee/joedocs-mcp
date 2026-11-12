//! The remote admin API, `/admin/v1/`: served only when `admin.enabled` is
//! on, never as MCP tools. Every request needs an admin token
//! (`Authorization: Bearer jda_...`); everything but login also needs a
//! session (`JoeDocs-Session: jds_...`) that `POST /admin/v1/session` opened
//! with the token and a current TOTP code. Every call, refused or not,
//! writes one audit row (`admin:<operation>`), and no token or code is ever
//! logged.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use secrecy::SecretString;
use serde_json::{Value, json};

use super::{ServerOptions, Shared, with_default_port};
use crate::auth::OsRandom;
use crate::store::Admin;
use crate::store::{self, AdminRefusal, Store};

/// The header that carries an admin session token.
pub const SESSION_HEADER: &str = "joedocs-session";

/// Admin API settings, from `[admin]` and the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminOptions {
    /// `admin.session_hours`.
    pub session_hours: u32,
    /// The config file in use, for `status`.
    pub config_file: Option<PathBuf>,
}

/// A test hook run between staging and compressing a remote backup, or
/// while a password is hashed.
#[cfg(test)]
type StagingHook = Arc<dyn Fn(&Shared) + Send + Sync>;

#[path = "user_api.rs"]
mod user_api;

struct ApiState {
    shared: Arc<Shared>,
    /// Called by a remote backup between staging and compressing (tests).
    #[cfg(test)]
    after_staging: Option<StagingHook>,
    /// Called while a password is hashed (tests).
    #[cfg(test)]
    during_hash: Option<StagingHook>,
    session_hours: u32,
    /// `admin.device_days`.
    device_days: u32,
    /// `admin.token_login`.
    #[expect(dead_code, reason = "read by the admin-token switch (JD-37 STEP-09)")]
    token_login: bool,
    config_file: Option<PathBuf>,
    bind: SocketAddr,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    web_ui: bool,
}

/// The routes, to be nested under `/admin/v1`.
pub(super) fn routes(
    shared: &Arc<Shared>,
    admin: &AdminOptions,
    options: &ServerOptions,
) -> Router {
    let state = Arc::new(ApiState {
        shared: Arc::clone(shared),
        #[cfg(test)]
        after_staging: None,
        #[cfg(test)]
        during_hash: None,
        session_hours: admin.session_hours,
        device_days: options.device_days,
        token_login: options.token_login,
        config_file: admin.config_file.clone(),
        bind: options.bind,
        allowed_hosts: options.allowed_hosts.clone(),
        allowed_origins: options.allowed_origins.clone(),
        web_ui: options.web.is_some(),
    });
    Router::new()
        .route("/", axum::routing::any(dispatch))
        .route("/{*rest}", axum::routing::any(dispatch))
        .with_state(state)
}

/// A refusal or failure, sent as `{"error": {"code", "message"}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    fn unauthorized() -> Self {
        Self::from(&store::Error::AdminRefused(AdminRefusal::Unauthorized))
    }

    fn no_session() -> Self {
        Self::from(&store::Error::AdminRefused(AdminRefusal::NoSession))
    }
}

/// The HTTP status and error code for a store error, shared by the admin
/// API and the web interface. A failure that is not the caller's is logged
/// and reported as `internal`.
pub(crate) fn classify(error: &store::Error) -> (StatusCode, &'static str) {
    use store::Error as E;
    match error {
        E::AdminRefused(AdminRefusal::Unauthorized) => (StatusCode::UNAUTHORIZED, "unauthorized"),
        E::AdminRefused(AdminRefusal::NoSession) => (StatusCode::UNAUTHORIZED, "no_session"),
        E::AdminRefused(AdminRefusal::DeviceInvalid) => {
            (StatusCode::UNAUTHORIZED, "device_invalid")
        }
        E::AdminRefused(AdminRefusal::SessionExpired) => {
            (StatusCode::UNAUTHORIZED, "session_expired")
        }
        E::AdminRefused(AdminRefusal::Locked { .. }) => (StatusCode::TOO_MANY_REQUESTS, "locked"),
        E::NotFound(_)
        | E::FileNotFound { .. }
        | E::ProjectNotFound(_)
        | E::KeyNotFound(_)
        | E::UserNotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        E::Archived(_) => (StatusCode::CONFLICT, "archived"),
        E::AlreadyExists(_) => (StatusCode::CONFLICT, "already_exists"),
        E::Conflict { .. } | E::FileConflict { .. } => (StatusCode::CONFLICT, "conflict"),
        other if other.is_user_error() => (StatusCode::BAD_REQUEST, "invalid_argument"),
        other => {
            tracing::error!(error = %other, "admin API store failure");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

impl From<&store::Error> for ApiError {
    fn from(error: &store::Error) -> Self {
        let (status, code) = classify(error);
        if code == "internal" {
            return Self::new(status, code, "internal error");
        }
        let mut api = Self::new(status, code, error.to_string());
        if let store::Error::AdminRefused(AdminRefusal::Locked { retry_after, .. }) = error {
            api.retry_after = Some(*retry_after);
        }
        api
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message}});
        let mut response = (self.status, axum::Json(body)).into_response();
        if let Some(seconds) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"joedocs-admin\""),
            );
        }
        response
    }
}

/// What a request carries, read once.
#[derive(Clone)]
struct Call {
    operation: &'static str,
    client: String,
    token: Option<SecretString>,
    session: Option<SecretString>,
    allowed: Result<(), ApiError>,
    /// `admin.device_days`, for checking a device credential.
    device_days: u32,
}

impl Call {
    fn new(state: &ApiState, operation: &'static str, headers: &HeaderMap) -> Self {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        };
        let token = text(header::AUTHORIZATION.as_str())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, token)| {
                scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()
            })
            .map(|(_, token)| SecretString::from(token.trim().to_owned()));
        let session = text(SESSION_HEADER)
            .filter(|s| !s.is_empty())
            .map(|s| SecretString::from(s.to_owned()));
        let client = text(header::USER_AGENT.as_str())
            .unwrap_or("unknown")
            .to_owned();
        Self {
            operation,
            client,
            token,
            session,
            allowed: check_origin(state, headers),
            device_days: state.device_days,
        }
    }
}

/// Refuses a `Host` outside `server.allowed_hosts` and an `Origin` outside
/// `server.allowed_origins` (DNS rebinding and browser requests).
fn check_origin(state: &ApiState, headers: &HeaderMap) -> Result<(), ApiError> {
    if !host_allowed(&state.allowed_hosts, headers) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this Host is not allowed",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = with_default_port(origin.to_str().unwrap_or(""));
        if !state.allowed_origins.contains(&origin) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "this Origin is not allowed",
            ));
        }
    }
    Ok(())
}

/// Whether the request's `Host` is one of `allowed_hosts` (DNS-rebinding
/// protection), with or without its port.
pub(crate) fn host_allowed(allowed_hosts: &[String], headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bare = strip_port(host);
    allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host) || allowed.eq_ignore_ascii_case(bare))
}

/// `host:port` → `host`, `[::1]:80` → `::1`.
pub(crate) fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            host
        }
        _ => authority,
    }
}

/// Runs `work` on the store for an authenticated session, and writes the
/// audit row for the call whatever happens.
async fn authorized<F>(state: Arc<ApiState>, call: Call, work: F) -> Response
where
    F: FnOnce(&mut Store, &ApiState, &Admin) -> Result<Response, ApiError> + Send + 'static,
{
    run(state, call, move |store, state, call| {
        match authenticate(store, call) {
            Ok(admin) => {
                let result = work(store, state, &admin);
                Ok((Some(admin), result))
            }
            Err(refusal) => Ok((refusal.who, Err(refusal.error))),
        }
    })
    .await
}

/// A refused call, and who made it when the admin token is valid.
struct Refused {
    who: Option<Admin>,
    error: ApiError,
}

fn refused(who: Option<Admin>, error: ApiError) -> Box<Refused> {
    Box::new(Refused { who, error })
}

/// The admin token and session of `call`, or the refusal and who called
/// (when the token is valid).
fn authenticate(store: &Store, call: &Call) -> Result<Admin, Box<Refused>> {
    if user_api::has_device(call) {
        return user_api::authenticate(store, call).map(|(admin, _)| admin);
    }
    let token = call
        .token
        .as_ref()
        .ok_or_else(|| refused(None, ApiError::unauthorized()))?;
    let admin = store
        .admin_token_for(token)
        .map_err(|e| refused(None, ApiError::from(&e)))?
        .ok_or_else(|| refused(None, ApiError::unauthorized()))?;
    let checked = call
        .session
        .as_ref()
        .ok_or_else(ApiError::no_session)
        .and_then(|session| {
            store
                .admin_session(token, session)
                .map_err(|e| ApiError::from(&e))
        });
    // A session can only be renewed by logging in, so say at once that the
    // token is locked rather than inviting a code.
    checked.map(Admin::Token).map_err(|error| {
        let lock = locked(store, &admin).unwrap_or(error);
        refused(Some(Admin::Token(admin.clone())), lock)
    })
}

/// Writes the audit row for `call`.
fn write_audit(store: &Store, who: Option<&Admin>, call: &Call, outcome: &str) {
    if let Err(error) = store.audit_admin(who, &call.client, call.operation, outcome) {
        tracing::error!(%error, "cannot write the admin audit entry");
    }
}

/// The lock refusal for `admin`, when it is locked now.
fn locked(store: &Store, admin: &crate::db::AdminTokenRow) -> Option<ApiError> {
    let until = admin.locked_until.as_ref()?;
    let now = store.now();
    if *until <= now {
        return None;
    }
    let seconds = |t: &str| {
        t.parse::<jiff::Timestamp>()
            .map_or(0, jiff::Timestamp::as_second)
    };
    let retry_after = u64::try_from(seconds(until) - seconds(&now))
        .unwrap_or(1)
        .max(1);
    Some(ApiError::from(&store::Error::AdminRefused(
        AdminRefusal::Locked {
            until: until.clone(),
            retry_after,
        },
    )))
}

/// Runs `work` on the store in a blocking task, after the `Host`/`Origin`
/// check, and audits the call. `work` returns who called (if known) and the
/// response.
async fn run<F>(state: Arc<ApiState>, call: Call, work: F) -> Response
where
    F: FnOnce(
            &mut Store,
            &ApiState,
            &Call,
        ) -> Result<(Option<Admin>, Result<Response, ApiError>), ApiError>
        + Send
        + 'static,
{
    let shared = Arc::clone(&state.shared);
    let task = tokio::task::spawn_blocking(move || {
        let Ok(mut store) = shared.store.lock() else {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
            .into_response();
        };
        let (who, result) = match &call.allowed {
            Err(refused) => {
                let who = call
                    .token
                    .as_ref()
                    .and_then(|t| store.admin_token_for(t).ok().flatten())
                    .map(Admin::Token);
                (who, Err(refused.clone()))
            }
            Ok(()) => match work(&mut store, &state, &call) {
                Ok((who, result)) => (who, result),
                Err(refused) => (None, Err(refused)),
            },
        };
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => error.code,
        };
        write_audit(&store, who.as_ref(), &call, outcome);
        result.unwrap_or_else(IntoResponse::into_response)
    });
    match task.await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "admin API task failed");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
            .into_response()
        }
    }
}

/// The largest request body the admin API reads.
pub const BODY_LIMIT: usize = 64 * 1024;

/// One admin API route, with its path parameter.
enum Route {
    Login,
    Logout,
    AuthLogin,
    AuthSetup,
    Me,
    MePassword,
    Devices,
    DeviceRevoke(String),
    Status,
    ProjectsList,
    ProjectsAdd,
    KeysList,
    KeysCreate,
    KeysGrant(String),
    KeysUngrant(String),
    KeysRotate(String),
    KeysRevoke(String),
    ArchiveList,
    ArchiveRestore(String),
    Check,
    Reindex,
    DocumentsList,
    Search,
    History(String),
    Diff(String),
    Audit,
    Document(String),
    DocumentConvert(String),
    FileMove(String),
    FileRestore(String),
    Backup,
    Unknown,
}

impl Route {
    /// The route for `method` and `path` (relative to `/admin/v1`).
    fn of(method: &axum::http::Method, path: &str) -> Self {
        use axum::http::Method as M;
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let get = *method == M::GET;
        let post = *method == M::POST;
        match parts.as_slice() {
            ["session"] if post => Self::Login,
            ["session"] if *method == M::DELETE => Self::Logout,
            ["auth", "login"] if post => Self::AuthLogin,
            ["auth", "setup"] if post => Self::AuthSetup,
            ["me"] if get => Self::Me,
            ["me", "password"] if post => Self::MePassword,
            ["me", "devices"] if get => Self::Devices,
            ["me", "devices", name, "revoke"] if post => Self::DeviceRevoke(percent_decode(name)),
            ["status"] if get => Self::Status,
            ["projects"] if get => Self::ProjectsList,
            ["projects"] if post => Self::ProjectsAdd,
            ["keys"] if get => Self::KeysList,
            ["keys"] if post => Self::KeysCreate,
            ["keys", name, "grants"] if post => Self::KeysGrant((*name).to_owned()),
            ["keys", name, "ungrant"] if post => Self::KeysUngrant(percent_decode(name)),
            ["keys", name, "rotate"] if post => Self::KeysRotate((*name).to_owned()),
            ["keys", name, "revoke"] if post => Self::KeysRevoke((*name).to_owned()),
            ["archive"] if get => Self::ArchiveList,
            ["archive", id, "restore"] if post => Self::ArchiveRestore((*id).to_owned()),
            ["check"] if get => Self::Check,
            ["reindex"] if post => Self::Reindex,
            ["documents"] if get => Self::DocumentsList,
            ["search"] if post => Self::Search,
            ["audit"] if get => Self::Audit,
            ["documents", id] if get => Self::Document((*id).to_owned()),
            ["documents", id, "history"] if get => Self::History(percent_decode(id)),
            ["documents", id, "diff"] if get => Self::Diff(percent_decode(id)),
            ["documents", id, "convert"] if post => Self::DocumentConvert((*id).to_owned()),
            ["documents", id, "files", "move"] if post => Self::FileMove((*id).to_owned()),
            ["documents", id, "files", "restore"] if post => Self::FileRestore((*id).to_owned()),
            ["backup"] if get => Self::Backup,
            _ => Self::Unknown,
        }
    }

    /// The audit log's tool name.
    fn operation(&self) -> &'static str {
        match self {
            Self::Login => "admin:session.create",
            Self::Logout => "admin:session.delete",
            Self::AuthLogin => "admin:auth.login",
            Self::AuthSetup => "admin:auth.setup",
            Self::Me => "admin:me.status",
            Self::MePassword => "admin:me.password",
            Self::Devices => "admin:devices.list",
            Self::DeviceRevoke(_) => "admin:devices.revoke",
            Self::Status => "admin:status",
            Self::ProjectsList => "admin:projects.list",
            Self::ProjectsAdd => "admin:projects.add",
            Self::KeysList => "admin:keys.list",
            Self::KeysCreate => "admin:keys.create",
            Self::KeysGrant(_) => "admin:keys.grant",
            Self::KeysUngrant(_) => "admin:keys.ungrant",
            Self::KeysRotate(_) => "admin:keys.rotate",
            Self::KeysRevoke(_) => "admin:keys.revoke",
            Self::ArchiveList => "admin:archive.list",
            Self::ArchiveRestore(_) => "admin:archive.restore",
            Self::Check => "admin:check",
            Self::Reindex => "admin:reindex",
            Self::DocumentsList => "admin:documents.list",
            Self::Search => "admin:documents.search",
            Self::History(_) => "admin:documents.history",
            Self::Diff(_) => "admin:documents.diff",
            Self::Audit => "admin:audit.list",
            Self::Document(_) => "admin:documents.get",
            Self::DocumentConvert(_) => "admin:document.convert",
            Self::FileMove(_) => "admin:document.move_file",
            Self::FileRestore(_) => "admin:document.restore_file",
            Self::Backup => "admin:backup",
            Self::Unknown => "admin:unknown",
        }
    }
}

/// Every `/admin/v1/` request. It uses no extractor that can reject a
/// request before it is checked and audited: the request is read here,
/// with [`BODY_LIMIT`], and every outcome goes through [`run`].
async fn dispatch(State(state): State<Arc<ApiState>>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let route = Route::of(&parts.method, parts.uri.path());
    let call = Call::new(&state, route.operation(), &parts.headers);
    let Ok(body) = axum::body::to_bytes(body, BODY_LIMIT).await else {
        let too_large = ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("request bodies are limited to {BODY_LIMIT} bytes"),
        );
        return refuse(state, call, too_large).await;
    };
    let uri = parts.uri;
    match route {
        Route::Login => login(state, call, body).await,
        Route::Logout => logout(state, call).await,
        Route::AuthLogin => user_api::sign_in(state, call, body).await,
        Route::AuthSetup => user_api::setup(state, call, body).await,
        Route::Me => user_api::me(state, call).await,
        Route::MePassword => user_api::change_password(state, call, body).await,
        Route::Devices => user_api::devices(state, call).await,
        Route::DeviceRevoke(name) => user_api::revoke_device(state, call, name).await,
        Route::Status => status(state, call).await,
        Route::ProjectsList => projects_list(state, call).await,
        Route::ProjectsAdd => projects_add(state, call, body).await,
        Route::KeysList => keys_list(state, call).await,
        Route::KeysCreate => keys_create(state, call, body).await,
        Route::KeysGrant(name) => keys_grant(state, call, name, body).await,
        Route::KeysUngrant(name) => keys_ungrant(state, call, name, body).await,
        Route::KeysRotate(name) => keys_rotate(state, call, name).await,
        Route::KeysRevoke(name) => keys_revoke(state, call, name).await,
        Route::ArchiveList => archive_list(state, call, uri).await,
        Route::ArchiveRestore(id) => archive_restore(state, call, id).await,
        Route::Check => check(state, call).await,
        Route::Reindex => reindex(state, call).await,
        Route::DocumentsList => documents_list(state, call, uri).await,
        Route::Search => search(state, call, body).await,
        Route::History(id) => history(state, call, id, uri).await,
        Route::Diff(id) => diff(state, call, id, uri).await,
        Route::Audit => audit(state, call, uri).await,
        Route::Document(id) => document(state, call, id, uri).await,
        Route::DocumentConvert(id) => document_command(state, call, id, body, "convert").await,
        Route::FileMove(id) => document_command(state, call, id, body, "move").await,
        Route::FileRestore(id) => document_command(state, call, id, body, "restore").await,
        Route::Backup => backup(state, call).await,
        Route::Unknown => {
            let missing = ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such admin API route",
            );
            refuse(state, call, missing).await
        }
    }
}

/// Answers `call` with `error` (after the `Host`/`Origin` check, which
/// takes precedence), naming the caller in the audit row when the token is
/// valid.
async fn refuse(state: Arc<ApiState>, call: Call, error: ApiError) -> Response {
    run(state, call, move |store, _, call| {
        let who = call
            .token
            .as_ref()
            .and_then(|t| store.admin_token_for(t).ok().flatten())
            .map(Admin::Token);
        Ok((who, Err(error)))
    })
    .await
}

/// `POST /admin/v1/session` with `{"code": "123456"}`: logs in.
async fn login(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    run(state, call, move |store, state, call| {
        if user_api::has_device(call) {
            return Ok(user_api::renew(store, state, call, &body));
        }
        let token = call.token.as_ref().ok_or_else(ApiError::unauthorized)?;
        let who = store
            .admin_token_for(token)
            .map_err(|e| ApiError::from(&e))?
            .map(Admin::Token);
        let code = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("code").and_then(Value::as_str).map(str::to_owned));
        let Some(code) = code else {
            let missing = ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                "send {\"code\": \"123456\"}",
            );
            return Ok((who, Err(missing)));
        };
        let result = store
            .admin_login(token, &code, state.session_hours, &OsRandom)
            .map(|opened| {
                use secrecy::ExposeSecret as _;
                axum::Json(json!({
                    "session": opened.token.expose_secret(),
                    "expires_at": opened.expires_at,
                    "name": opened.admin.name,
                }))
                .into_response()
            })
            .map_err(|e| ApiError::from(&e));
        Ok((who, result))
    })
    .await
}

/// `DELETE /admin/v1/session`: logs out.
async fn logout(state: Arc<ApiState>, call: Call) -> Response {
    run(state, call, move |store, _, call| {
        if user_api::has_device(call) {
            return Ok(user_api::sign_out(store, call));
        }
        let token = call.token.as_ref().ok_or_else(ApiError::unauthorized)?;
        let who = store
            .admin_token_for(token)
            .map_err(|e| ApiError::from(&e))?
            .map(Admin::Token);
        if who.is_none() {
            return Err(ApiError::unauthorized());
        }
        let result = match &call.session {
            None => Err(ApiError::no_session()),
            Some(session) => store
                .end_admin_session(token, session)
                .map(|_| StatusCode::NO_CONTENT.into_response())
                .map_err(|e| ApiError::from(&e)),
        };
        Ok((who, result))
    })
    .await
}

/// `GET /admin/v1/status`: the `joedocs status` report.
async fn status(state: Arc<ApiState>, call: Call) -> Response {
    authorized(state, call, |store, state, _| {
        let report = crate::status::collect_parts(
            store,
            state.config_file.clone(),
            state.bind,
            true,
            state.web_ui,
            &crate::systemd::Systemctl,
        )
        .map_err(|e| match e {
            crate::Error::Store(error) => ApiError::from(&error),
            other => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                other.to_string(),
            ),
        })?;
        Ok(axum::Json(report).into_response())
    })
    .await
}

/// A command's text output as `{"output": ...}`, the same text the local
/// command prints. `check` problems are `{"output", "problems": n}`.
fn text(run: impl FnOnce(&mut Vec<u8>) -> Result<(), crate::Error>) -> Result<Response, ApiError> {
    let mut out = Vec::new();
    let problems = match run(&mut out) {
        Ok(()) => 0,
        Err(crate::Error::Problems(count)) => count,
        Err(error) => return Err(from_crate(&error)),
    };
    let output = String::from_utf8_lossy(&out).into_owned();
    Ok(axum::Json(json!({"output": output, "problems": problems})).into_response())
}

/// Maps a command error to the API error the client turns back into the
/// same message and exit code.
fn from_crate(error: &crate::Error) -> ApiError {
    match error {
        crate::Error::Store(error) => ApiError::from(error),
        crate::Error::Archived(_) => {
            ApiError::new(StatusCode::CONFLICT, "archived", error.to_string())
        }
        crate::Error::Backup(inner) if inner.is_user_error() => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            error.to_string(),
        ),
        other => {
            tracing::error!(error = %other, "admin API command failed");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    }
}

/// The JSON body, or an `invalid_argument` refusal.
fn body_json(body: &[u8]) -> Result<Value, ApiError> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            format!("invalid JSON: {e}"),
        )
    })
}

fn field(body: &Value, name: &str) -> Result<String, ApiError> {
    body.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("`{name}` is required"),
            )
        })
}

fn grants(body: &Value) -> Result<Vec<crate::auth::Grant>, ApiError> {
    body.get("grants")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, Clone::clone)
        .iter()
        .map(|g| {
            g.as_str()
                .unwrap_or_default()
                .parse()
                .map_err(|e: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_argument", e))
        })
        .collect()
}

/// A query parameter from `uri`.
/// `text` with `%XX` escapes decoded (as UTF-8, lossily).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(high), Some(low)) => {
                out.push(u8::try_from(high * 16 + low).unwrap_or(b'?'));
                i += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A raw query parameter from `uri`, where an empty value counts as absent:
/// kept for `version` and `archive` on `GET /admin/v1/documents/{id}`, whose
/// empty value has meant "not given" since 0.4.0. Everything else uses
/// [`present`].
fn query(uri: &axum::http::Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name && !value.is_empty()).then(|| value.to_owned())
    })
}

/// The raw value of query parameter `name` when it is present, even empty
/// (`name=`): an empty value is a value, which the store refuses or matches
/// as it does locally. A pair without `=` counts as absent.
fn present(uri: &axum::http::Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}

/// A query parameter from `uri`, percent-decoded; present but empty is
/// `Some("")`.
fn decoded(uri: &axum::http::Uri, name: &str) -> Option<String> {
    present(uri, name).map(|value| percent_decode(&value))
}

/// A number query parameter, or `invalid_argument`.
fn number(uri: &axum::http::Uri, name: &str) -> Result<Option<u64>, ApiError> {
    decoded(uri, name)
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    format!("`{name}` must be a number, not \"{value}\""),
                )
            })
        })
        .transpose()
}

/// The arguments of `GET /admin/v1/documents`.
fn list_args(uri: &axum::http::Uri) -> Result<crate::cli::ListArgs, ApiError> {
    use crate::cli::SortArg;
    let invalid =
        |message: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_argument", message);
    let sort = match decoded(uri, "sort") {
        None => SortArg::default(),
        Some(name) => SortArg::from_api_name(&name)
            .ok_or_else(|| invalid(format!("`sort` cannot be \"{name}\"")))?,
    };
    Ok(crate::cli::ListArgs {
        project: decoded(uri, "project").ok_or_else(|| invalid("`project` is required".into()))?,
        doc_type: decoded(uri, "type"),
        status: decoded(uri, "status"),
        sort,
        start: number(uri, "start")?.unwrap_or(1),
        limit: number(uri, "limit")?.unwrap_or(crate::cli::DEFAULT_LIST_LIMIT),
        json: false,
    })
}

/// `GET /admin/v1/documents`: the `list_documents` result.
async fn documents_list(state: Arc<ApiState>, call: Call, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let args = list_args(&uri)?;
        let reply = crate::browse::list(store, &args).map_err(|e| from_crate(&e))?;
        Ok(axum::Json(reply).into_response())
    })
    .await
}

/// `POST /admin/v1/search` with a [`crate::browse::SearchSpec`]: the
/// `search_documents` result over every project, or one.
async fn search(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    authorized(state, call, move |store, _, _| {
        let spec: crate::browse::SearchSpec =
            serde_json::from_value(body_json(&body)?).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    format!("invalid search: {e}"),
                )
            })?;
        let reply = crate::browse::search(store, &spec).map_err(|e| from_crate(&e))?;
        Ok(axum::Json(reply).into_response())
    })
    .await
}

/// `GET /admin/v1/documents/{id}/history`: the `get_document_history`
/// result.
async fn history(state: Arc<ApiState>, call: Call, id: String, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let args = crate::cli::HistoryArgs {
            document_id: id,
            part: decoded(&uri, "file"),
            start: number(&uri, "start")?.unwrap_or(1),
            limit: number(&uri, "limit")?.unwrap_or(crate::cli::DEFAULT_HISTORY_LIMIT),
            json: false,
        };
        let reply = crate::browse::history(store, &args).map_err(|e| from_crate(&e))?;
        Ok(axum::Json(reply).into_response())
    })
    .await
}

/// `GET /admin/v1/documents/{id}/diff?from=N`: the `diff_document` result.
async fn diff(state: Arc<ApiState>, call: Call, id: String, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let invalid =
            |message: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_argument", message);
        let context = match number(&uri, "context")? {
            None => 3,
            Some(n) => u32::try_from(n).map_err(|_| invalid(format!("`context` cannot be {n}")))?,
        };
        let args = crate::cli::DiffArgs {
            document_id: id,
            from: number(&uri, "from")?.ok_or_else(|| invalid("`from` is required".into()))?,
            to: number(&uri, "to")?,
            context,
            part: decoded(&uri, "file"),
            json: false,
        };
        let reply = crate::browse::diff(store, &args).map_err(|e| from_crate(&e))?;
        Ok(axum::Json(reply).into_response())
    })
    .await
}

/// `GET /admin/v1/audit`: a page of the audit log, newest first.
async fn audit(state: Arc<ApiState>, call: Call, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let args = crate::cli::AuditArgs {
            since: decoded(&uri, "since"),
            until: decoded(&uri, "until"),
            key: decoded(&uri, "key"),
            tool: decoded(&uri, "tool"),
            document: decoded(&uri, "document"),
            outcome: decoded(&uri, "outcome"),
            start: number(&uri, "start")?.unwrap_or(1),
            limit: number(&uri, "limit")?.unwrap_or(crate::cli::DEFAULT_AUDIT_LIMIT),
            json: false,
        };
        let reply = crate::browse::audit(store, &args).map_err(|e| from_crate(&e))?;
        Ok(axum::Json(reply).into_response())
    })
    .await
}

async fn projects_list(state: Arc<ApiState>, call: Call) -> Response {
    authorized(state, call, |store, _, _| {
        text(|out| crate::admin::project(store, &crate::cli::ProjectCommand::List, out))
    })
    .await
}

async fn projects_add(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    authorized(state, call, move |store, _, _| {
        let body = body_json(&body)?;
        let command = crate::cli::ProjectCommand::Add {
            name: field(&body, "name")?,
            key: field(&body, "key")?,
            title: body.get("title").and_then(Value::as_str).map(str::to_owned),
        };
        text(|out| crate::admin::project(store, &command, out))
    })
    .await
}

async fn keys_list(state: Arc<ApiState>, call: Call) -> Response {
    authorized(state, call, |store, _, _| {
        text(|out| crate::admin::key(store, &crate::cli::KeyCommand::List, &OsRandom, out))
    })
    .await
}

async fn keys_create(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    authorized(state, call, move |store, _, _| {
        let body = body_json(&body)?;
        let command = crate::cli::KeyCommand::Create {
            name: field(&body, "name")?,
            grants: grants(&body)?,
        };
        text(|out| crate::admin::key(store, &command, &OsRandom, out))
    })
    .await
}

async fn keys_grant(state: Arc<ApiState>, call: Call, name: String, body: Bytes) -> Response {
    authorized(state, call, move |store, _, _| {
        let command = crate::cli::KeyCommand::Grant {
            name,
            grants: grants(&body_json(&body)?)?,
        };
        text(|out| crate::admin::key(store, &command, &OsRandom, out))
    })
    .await
}

/// `POST /admin/v1/keys/{name}/ungrant` with `{"projects": ["a"]}`.
async fn keys_ungrant(state: Arc<ApiState>, call: Call, name: String, body: Bytes) -> Response {
    authorized(state, call, move |store, _, _| {
        let body = body_json(&body)?;
        let projects: Vec<String> = body
            .get("projects")
            .and_then(Value::as_array)
            .and_then(|list| {
                list.iter()
                    .map(|p| p.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .filter(|list| !list.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    "`projects` must be a list of project names",
                )
            })?;
        let command = crate::cli::KeyCommand::Ungrant { name, projects };
        text(|out| crate::admin::key(store, &command, &OsRandom, out))
    })
    .await
}

async fn keys_rotate(state: Arc<ApiState>, call: Call, name: String) -> Response {
    authorized(state, call, move |store, _, _| {
        let command = crate::cli::KeyCommand::Rotate { name };
        text(|out| crate::admin::key(store, &command, &OsRandom, out))
    })
    .await
}

async fn keys_revoke(state: Arc<ApiState>, call: Call, name: String) -> Response {
    authorized(state, call, move |store, _, _| {
        let command = crate::cli::KeyCommand::Revoke { name };
        text(|out| crate::admin::key(store, &command, &OsRandom, out))
    })
    .await
}

async fn archive_list(state: Arc<ApiState>, call: Call, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let command = crate::cli::ArchiveCommand::List {
            project: decoded(&uri, "project"),
            doc_type: decoded(&uri, "type"),
        };
        text(|out| crate::admin::archive(store, &command, out))
    })
    .await
}

async fn archive_restore(state: Arc<ApiState>, call: Call, id: String) -> Response {
    authorized(state, call, move |store, _, _| {
        let command = crate::cli::ArchiveCommand::Restore { document_id: id };
        text(|out| crate::admin::archive(store, &command, out))
    })
    .await
}

/// `POST /admin/v1/documents/{id}/convert`, `.../files/move` with
/// `{"file", "to", "as"?}` and `.../files/restore` with `{"file", "version"}`.
async fn document_command(
    state: Arc<ApiState>,
    call: Call,
    id: String,
    body: Bytes,
    which: &'static str,
) -> Response {
    authorized(state, call, move |store, _, _| {
        let body = body_json(&body)?;
        let command = match which {
            "convert" => crate::cli::DocumentCommand::Convert { document_id: id },
            "move" => crate::cli::DocumentCommand::MoveFile {
                document_id: id,
                file: field(&body, "file")?,
                to: field(&body, "to")?,
                as_file: body.get("as").and_then(Value::as_str).map(str::to_owned),
            },
            _ => crate::cli::DocumentCommand::RestoreFile {
                document_id: id,
                file: field(&body, "file")?,
                version: body.get("version").and_then(Value::as_u64).ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_argument",
                        "`version` (a revision number) is required",
                    )
                })?,
            },
        };
        text(|out| crate::admin::document(store, &command, out))
    })
    .await
}

async fn check(state: Arc<ApiState>, call: Call) -> Response {
    authorized(state, call, |store, _, _| {
        text(|out| crate::admin::check(store, out))
    })
    .await
}

async fn reindex(state: Arc<ApiState>, call: Call) -> Response {
    authorized(state, call, |store, _, _| {
        text(|out| crate::admin::reindex(store, out))
    })
    .await
}

async fn document(state: Arc<ApiState>, call: Call, id: String, uri: axum::http::Uri) -> Response {
    authorized(state, call, move |store, _, _| {
        let version = match query(&uri, "version") {
            Some(v) => Some(v.parse::<u64>().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    format!("\"{v}\" is not a revision number"),
                )
            })?),
            None => None,
        };
        if query(&uri, "archive").as_deref() == Some("true") {
            let bytes =
                crate::admin::folder_archive(store, &id, version).map_err(|e| from_crate(&e))?;
            let mut response = bytes.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/gzip"),
            );
            return Ok(response);
        }
        if let Some(part) = present(&uri, "part") {
            let part = percent_decode(&part);
            let markdown =
                crate::admin::fetch_file(store, &id, &part, version).map_err(|e| from_crate(&e))?;
            return Ok(axum::Json(json!({"markdown": markdown})).into_response());
        }
        let markdown = crate::admin::fetch(store, &id, version).map_err(|e| from_crate(&e))?;
        let files = crate::admin::other_files(store, &id, version).map_err(|e| from_crate(&e))?;
        let reply = match files {
            Some(files) => json!({"markdown": markdown, "files": files}),
            None => json!({"markdown": markdown}),
        };
        Ok(axum::Json(reply).into_response())
    })
    .await
}

/// `GET /admin/v1/backup`: a backup archive, streamed.
async fn backup(state: Arc<ApiState>, call: Call) -> Response {
    let task = tokio::task::spawn_blocking(move || backup_blocking(&state, &call));
    task.await.unwrap_or_else(|error| {
        tracing::error!(%error, "admin API backup task failed");
        internal().into_response()
    })
}

fn internal() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
}

/// The backup in three phases: check, authenticate and stage under the
/// store mutex (the snapshot also takes the write lock); compress without
/// it, so MCP and admin calls carry on; then take it briefly to audit.
fn backup_blocking(state: &ApiState, call: &Call) -> Response {
    backup_download(
        &state.shared,
        |store| {
            let checked = match &call.allowed {
                Err(forbidden) => {
                    let who = call
                        .token
                        .as_ref()
                        .and_then(|t| store.admin_token_for(t).ok().flatten())
                        .map(Admin::Token);
                    Err(refused(who, forbidden.clone()))
                }
                Ok(()) => authenticate(store, call),
            };
            checked.map_err(|refusal| {
                write_audit(store, refusal.who.as_ref(), call, refusal.error.code);
                Box::new(refusal.error.into_response())
            })
        },
        |error| {
            let refused = from_crate(error);
            (refused.code, refused.into_response())
        },
        |store, admin, outcome| write_audit(store, Some(admin), call, outcome),
        || {
            #[cfg(test)]
            if let Some(hook) = &state.after_staging {
                hook(&state.shared);
            }
        },
    )
}

/// A backup download, shared by `GET /admin/v1/backup` and the web
/// interface's Maintenance page, in three phases. Under the store lock,
/// `authorize` checks the caller (auditing a refusal itself) and the store
/// is staged (the snapshot also takes the write lock). Without it, the
/// archive is compressed to a temporary file in the state directory, so MCP
/// and admin calls carry on meanwhile. Then, briefly under the lock again,
/// `audit` records the outcome. The reply streams the archive as
/// `application/gzip` with `JoeDocs-Documents` and `JoeDocs-Skipped-Links`;
/// `refusal` gives an error's audit code and reply.
pub(crate) fn backup_download<W>(
    shared: &Shared,
    authorize: impl FnOnce(&Store) -> Result<W, Box<Response>>,
    refusal: impl Fn(&crate::Error) -> (&'static str, Response),
    audit: impl Fn(&Store, &W, &str),
    after_staging: impl FnOnce(),
) -> Response {
    let staged = {
        let Ok(store) = shared.store.lock() else {
            let poisoned = std::io::Error::other("the store lock is poisoned");
            return refusal(&crate::Error::Output(poisoned)).1;
        };
        let who = match authorize(&store) {
            Ok(who) => who,
            Err(reply) => return *reply,
        };
        match crate::backup::stage(&store) {
            Ok(staged) => (who, staged, store.paths().state_dir.clone()),
            Err(error) => {
                let (code, reply) = refusal(&crate::Error::Backup(error));
                audit(&store, &who, code);
                return reply;
            }
        }
    };
    let (who, staged, state_dir) = staged;
    after_staging();
    let mut random = [0_u8; 8];
    let _ = getrandom::fill(&mut random);
    let path = state_dir.join(format!(
        "backup-download-{}-{}.tgz",
        std::process::id(),
        crate::auth::hex(&random)
    ));
    let result = crate::backup::write_staged(&staged, &path)
        .map_err(crate::Error::Backup)
        .and_then(|_| {
            let file = std::fs::File::open(&path);
            // The open file stays readable after its name is removed.
            let _ = std::fs::remove_file(&path);
            file.map_err(|e| {
                tracing::error!(error = %e, "cannot open the backup for download");
                crate::Error::Output(e)
            })
        })
        .map(|file| {
            let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
            let mut response = axum::body::Body::from_stream(stream).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/gzip"),
            );
            if let Ok(value) = HeaderValue::from_str(&staged.manifest().documents.to_string()) {
                headers.insert("joedocs-documents", value);
            }
            if let Ok(value) = HeaderValue::from_str(
                &serde_json::to_string(staged.skipped_links()).unwrap_or_default(),
            ) {
                headers.insert("joedocs-skipped-links", value);
            }
            response
        });
    drop(staged);
    let (outcome, reply) = match result {
        Ok(reply) => ("ok", reply),
        Err(error) => refusal(&error),
    };
    if let Ok(store) = shared.store.lock() {
        audit(&store, &who, outcome);
    } else {
        tracing::error!("cannot write the audit entry: the store lock is poisoned");
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_backup_compresses_without_holding_the_store() {
        use secrecy::ExposeSecret as _;
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::TempDir::new().unwrap();
        let paths = crate::store::Paths {
            home: dir.path().join("vault"),
            state_dir: dir.path().join("state"),
        };
        let git = crate::history::GitCli::new(paths.git_dir(), &paths.home);
        Store::init(&paths, &git).unwrap();
        let clock = Arc::new(crate::clock::FixedClock::at(1_793_000_000));
        let mut store = Store::open(
            paths.clone(),
            crate::store::Settings::from_config(&crate::Config::default()),
            Box::new(git),
            Arc::clone(&clock) as Arc<dyn crate::clock::Clock>,
        )
        .unwrap();
        let created = store.create_admin_token("laptop", &OsRandom).unwrap();
        let secret = crate::totp::base32_decode(created.totp_secret.expose_secret()).unwrap();
        let code = crate::totp::code(&secret, crate::totp::step_at(1_793_000_000));
        let session = store
            .admin_login(&created.token, &code, 8, &OsRandom)
            .unwrap();
        let free = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&free);
        let state = ApiState {
            shared: Shared::new(store),
            after_staging: Some(Arc::new(move |shared: &Shared| {
                seen.store(shared.store.try_lock().is_ok(), Ordering::SeqCst);
            })),
            during_hash: None,
            session_hours: 8,
            device_days: 30,
            token_login: true,
            config_file: None,
            bind: "127.0.0.1:8750".parse().unwrap(),
            allowed_hosts: vec!["127.0.0.1".into()],
            allowed_origins: Vec::new(),
            web_ui: false,
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", created.token.expose_secret())).unwrap(),
        );
        headers.insert(
            SESSION_HEADER,
            HeaderValue::from_str(session.token.expose_secret()).unwrap(),
        );
        let call = Call::new(&state, "admin:backup", &headers);
        let response = backup_blocking(&state, &call);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            free.load(Ordering::SeqCst),
            "the store mutex was held while compressing"
        );
        let store = state.shared.store.lock().unwrap();
        let rows: i64 = rusqlite::Connection::open(paths.db())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE tool = 'admin:backup' AND outcome = 'ok'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(store);
        assert_eq!(rows, 1);
        let leftovers = std::fs::read_dir(&paths.state_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("backup-"))
            .count();
        assert_eq!(leftovers, 0, "staging and download files are removed");
    }

    #[test]
    fn ports_are_stripped_from_hosts() {
        assert_eq!(strip_port("docs.example.net:443"), "docs.example.net");
        assert_eq!(strip_port("docs.example.net"), "docs.example.net");
        assert_eq!(strip_port("[::1]:8750"), "::1");
        assert_eq!(strip_port("::1"), "::1");
        assert_eq!(strip_port("host:abc"), "host:abc");
    }

    #[test]
    fn store_errors_map_to_statuses_and_codes() {
        let cases = [
            (
                store::Error::NotFound("JD-1".into()),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                store::Error::FileNotFound {
                    id: "JD-1".into(),
                    file: "a".into(),
                    at: String::new(),
                },
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                store::Error::FileConflict {
                    id: "JD-1".into(),
                    file: "a".into(),
                    expected: 1,
                    file_revision: 2,
                    current: 3,
                },
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                store::Error::Archived("JD-1".into()),
                StatusCode::CONFLICT,
                "archived",
            ),
            (
                store::Error::AlreadyExists("x".into()),
                StatusCode::CONFLICT,
                "already_exists",
            ),
            (
                store::Error::invalid("name", "bad"),
                StatusCode::BAD_REQUEST,
                "invalid_argument",
            ),
            (
                store::Error::AdminRefused(AdminRefusal::SessionExpired),
                StatusCode::UNAUTHORIZED,
                "session_expired",
            ),
        ];
        for (error, status, code) in cases {
            let api = ApiError::from(&error);
            assert_eq!((api.status, api.code), (status, code), "{error}");
        }
        let locked = ApiError::from(&store::Error::AdminRefused(AdminRefusal::Locked {
            until: "later".into(),
            retry_after: 42,
        }));
        assert_eq!(locked.retry_after, Some(42));
        let response = locked.into_response();
        assert_eq!(response.headers()[header::RETRY_AFTER], "42");
    }

    /// A store with the user `alice`, whose one-time password is returned,
    /// and the API state around it, with `hook` run while a password is
    /// hashed.
    fn user_state(hook: Option<StagingHook>) -> (tempfile::TempDir, Arc<ApiState>, String) {
        use secrecy::ExposeSecret as _;
        let dir = tempfile::TempDir::new().unwrap();
        let paths = crate::store::Paths {
            home: dir.path().join("vault"),
            state_dir: dir.path().join("state"),
        };
        let git = crate::history::GitCli::new(paths.git_dir(), &paths.home);
        Store::init(&paths, &git).unwrap();
        let mut store = Store::open(
            paths,
            crate::store::Settings::from_config(&crate::Config::default()),
            Box::new(git),
            Arc::new(crate::clock::FixedClock::at(1_793_000_000)) as Arc<dyn crate::clock::Clock>,
        )
        .unwrap();
        let password = crate::password::initial_password(&OsRandom).unwrap();
        let hash = crate::password::hash(&password, &OsRandom).unwrap();
        store.create_user("alice", &hash, &OsRandom).unwrap();
        let state = Arc::new(ApiState {
            shared: Shared::new(store),
            after_staging: None,
            during_hash: hook,
            session_hours: 8,
            device_days: 30,
            token_login: true,
            config_file: None,
            bind: "127.0.0.1:8750".parse().unwrap(),
            allowed_hosts: vec!["127.0.0.1".into()],
            allowed_origins: Vec::new(),
            web_ui: false,
        });
        (dir, state, password.expose_secret().to_owned())
    }

    fn sign_in_call(state: &ApiState) -> Call {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1"));
        Call::new(state, "admin:auth.login", &headers)
    }

    #[tokio::test]
    async fn passwords_are_hashed_while_the_store_is_free_for_other_requests() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let read = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&read);
        let hook: StagingHook = Arc::new(move |shared: &Shared| {
            // Another request's read, while this sign-in hashes.
            if let Ok(store) = shared.store.try_lock()
                && store.users().is_ok()
            {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        });
        let (_dir, state, password) = user_state(Some(hook));
        let body = serde_json::json!({"user": "alice", "password": password}).to_string();
        let response = user_api::sign_in(
            Arc::clone(&state),
            sign_in_call(&state),
            body.clone().into(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            read.load(Ordering::SeqCst),
            1,
            "the store was locked during the hash"
        );
        // An unknown name takes the same path: its dummy hash runs too.
        let body = serde_json::json!({"user": "nobody", "password": password}).to_string();
        let response =
            user_api::sign_in(Arc::clone(&state), sign_in_call(&state), body.into()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            read.load(Ordering::SeqCst),
            2,
            "the unknown name was hashed too"
        );
    }

    #[tokio::test]
    async fn sign_ins_are_refused_busy_while_both_hashing_places_stay_taken() {
        let (dir, state, password) = user_state(None);
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = Arc::new(std::sync::Mutex::new(wait));
        let mut held = Vec::new();
        for _ in 0..crate::password::HASHING_PERMITS {
            let (shared, wait) = (Arc::clone(&state.shared), Arc::clone(&wait));
            held.push(tokio::spawn(async move {
                shared
                    .hashing()
                    .run(move || {
                        let _ = wait.lock().map(|w| w.recv());
                    })
                    .await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body = serde_json::json!({"user": "alice", "password": password}).to_string();
        let started = std::time::Instant::now();
        let response =
            user_api::sign_in(Arc::clone(&state), sign_in_call(&state), body.into()).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        for _ in 0..crate::password::HASHING_PERMITS {
            release.send(()).unwrap();
        }
        for task in held {
            task.await.unwrap().unwrap();
        }
        let busy: i64 = rusqlite::Connection::open(dir.path().join("state/joedocs.db"))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE tool = 'admin:auth.login' AND outcome = 'busy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(busy, 1);
    }
}
