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
use axum::routing::{get, post};
use secrecy::SecretString;
use serde_json::{Value, json};

use super::{ServerOptions, Shared, with_default_port};
use crate::auth::OsRandom;
use crate::db::AdminTokenRow;
use crate::store::{self, Actor, AdminRefusal, Store};

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

struct ApiState {
    shared: Arc<Shared>,
    session_hours: u32,
    config_file: Option<PathBuf>,
    bind: SocketAddr,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
}

/// The routes, to be nested under `/admin/v1`.
pub(super) fn routes(
    shared: &Arc<Shared>,
    admin: &AdminOptions,
    options: &ServerOptions,
) -> Router {
    let state = Arc::new(ApiState {
        shared: Arc::clone(shared),
        session_hours: admin.session_hours,
        config_file: admin.config_file.clone(),
        bind: options.bind,
        allowed_hosts: options.allowed_hosts.clone(),
        allowed_origins: options.allowed_origins.clone(),
    });
    Router::new()
        .route("/session", post(login).delete(logout))
        .route("/status", get(status))
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

impl From<&store::Error> for ApiError {
    fn from(error: &store::Error) -> Self {
        use store::Error as E;
        let (status, code) = match error {
            E::AdminRefused(AdminRefusal::Unauthorized) => {
                (StatusCode::UNAUTHORIZED, "unauthorized")
            }
            E::AdminRefused(AdminRefusal::NoSession) => (StatusCode::UNAUTHORIZED, "no_session"),
            E::AdminRefused(AdminRefusal::SessionExpired) => {
                (StatusCode::UNAUTHORIZED, "session_expired")
            }
            E::AdminRefused(AdminRefusal::Locked { retry_after, .. }) => {
                let mut api = Self::new(StatusCode::TOO_MANY_REQUESTS, "locked", error.to_string());
                api.retry_after = Some(*retry_after);
                return api;
            }
            E::NotFound(_) | E::ProjectNotFound(_) | E::KeyNotFound(_) => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            E::Archived(_) => (StatusCode::CONFLICT, "archived"),
            E::AlreadyExists(_) => (StatusCode::CONFLICT, "already_exists"),
            E::Conflict { .. } => (StatusCode::CONFLICT, "conflict"),
            other if other.is_user_error() => (StatusCode::BAD_REQUEST, "invalid_argument"),
            other => {
                tracing::error!(error = %other, "admin API store failure");
                return Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                );
            }
        };
        Self::new(status, code, error.to_string())
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
struct Call {
    operation: &'static str,
    client: String,
    token: Option<SecretString>,
    session: Option<SecretString>,
    allowed: Result<(), ApiError>,
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
        }
    }
}

/// Refuses a `Host` outside `server.allowed_hosts` and an `Origin` outside
/// `server.allowed_origins` (DNS rebinding and browser requests).
fn check_origin(state: &ApiState, headers: &HeaderMap) -> Result<(), ApiError> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bare = strip_port(host);
    let host_ok = state
        .allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host) || allowed.eq_ignore_ascii_case(bare));
    if !host_ok {
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

/// `host:port` → `host`, `[::1]:80` → `::1`.
fn strip_port(authority: &str) -> &str {
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
    F: FnOnce(&mut Store, &ApiState, &AdminTokenRow) -> Result<Response, ApiError> + Send + 'static,
{
    run(state, call, move |store, state, call| {
        let token = call.token.as_ref().ok_or_else(ApiError::unauthorized)?;
        let admin = store
            .admin_token_for(token)
            .map_err(|e| ApiError::from(&e))?
            .ok_or_else(ApiError::unauthorized)?;
        let identity = Some(admin.clone());
        let session = call.session.as_ref().ok_or_else(ApiError::no_session);
        let checked = session.and_then(|session| {
            store
                .admin_session(token, session)
                .map_err(|e| ApiError::from(&e))
        });
        let result = match checked {
            Ok(admin) => work(store, state, &admin),
            // A session can only be renewed by logging in, so say at once
            // that the token is locked rather than inviting a code.
            Err(refused) => Err(locked(store, &admin).unwrap_or(refused)),
        };
        Ok((identity, result))
    })
    .await
}

/// The lock refusal for `admin`, when it is locked now.
fn locked(store: &Store, admin: &AdminTokenRow) -> Option<ApiError> {
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
        ) -> Result<(Option<AdminTokenRow>, Result<Response, ApiError>), ApiError>
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
                    .and_then(|t| store.admin_token_for(t).ok().flatten());
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
        let (id, name) = who.map_or(("unknown".to_owned(), "unknown".to_owned()), |w| {
            (w.id, w.name)
        });
        if let Err(error) = store.audit(
            &id,
            &Actor::new(name, &call.client),
            call.operation,
            None,
            outcome,
        ) {
            tracing::error!(%error, "cannot write the admin audit entry");
        }
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

/// `POST /admin/v1/session` with `{"code": "123456"}`: logs in.
async fn login(State(state): State<Arc<ApiState>>, headers: HeaderMap, body: Bytes) -> Response {
    let call = Call::new(&state, "admin:session.create", &headers);
    run(state, call, move |store, state, call| {
        let token = call.token.as_ref().ok_or_else(ApiError::unauthorized)?;
        let who = store
            .admin_token_for(token)
            .map_err(|e| ApiError::from(&e))?;
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
async fn logout(State(state): State<Arc<ApiState>>, headers: HeaderMap) -> Response {
    let call = Call::new(&state, "admin:session.delete", &headers);
    run(state, call, move |store, _, call| {
        let token = call.token.as_ref().ok_or_else(ApiError::unauthorized)?;
        let who = store
            .admin_token_for(token)
            .map_err(|e| ApiError::from(&e))?;
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
async fn status(State(state): State<Arc<ApiState>>, headers: HeaderMap) -> Response {
    let call = Call::new(&state, "admin:status", &headers);
    authorized(state, call, |store, state, _| {
        let report = crate::status::collect_parts(
            store,
            state.config_file.clone(),
            state.bind,
            true,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
