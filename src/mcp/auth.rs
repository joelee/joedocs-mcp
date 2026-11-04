//! Bearer API key authentication for the MCP endpoint.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use secrecy::SecretString;

use super::Shared;
use crate::auth::{Grant, Permission};
use crate::db::KeyRow;

/// The authenticated API key, attached to each request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Public key ID.
    pub key_id: String,
    /// Key name: the author of every change.
    pub name: String,
    /// Project grants.
    pub grants: Vec<Grant>,
}

impl Caller {
    /// The permission this key has on `project`, if any.
    #[must_use]
    pub fn permission(&self, project: &str) -> Option<Permission> {
        self.grants
            .iter()
            .find(|g| g.project == project)
            .map(|g| g.permission)
    }
}

impl From<KeyRow> for Caller {
    fn from(key: KeyRow) -> Self {
        Self {
            key_id: key.id,
            name: key.name,
            grants: key.grants,
        }
    }
}

/// The token from `Authorization: Bearer <token>`.
fn bearer(request: &Request) -> Option<SecretString> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
        .then(|| SecretString::from(token.to_owned()))
}

fn refuse(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if status == StatusCode::UNAUTHORIZED {
        headers.insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"joedocs\""),
        );
    }
    response
}

/// Middleware: rejects requests without a valid, unrevoked API key and
/// attaches the [`Caller`] to the rest.
pub(super) async fn require_key(
    State(shared): State<Arc<Shared>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer(&request) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "send an API key as Authorization: Bearer jd_...",
        );
    };
    let found = shared
        .with_store(move |store| Ok(store.authenticate(&token)?))
        .await;
    match found {
        Ok(Some(key)) => {
            tracing::debug!(key = %key.name, "authenticated");
            request.extensions_mut().insert(Caller::from(key));
            next.run(request).await
        }
        Ok(None) => refuse(
            StatusCode::UNAUTHORIZED,
            "the API key is not valid or has been revoked",
        ),
        Err(error) => {
            tracing::error!(%error, "cannot check an API key");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::*;

    fn request(authorization: Option<&str>) -> Request {
        let mut builder = axum::http::Request::builder();
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn bearer_tokens_are_read_case_insensitively() {
        use secrecy::ExposeSecret as _;
        let token = |value| bearer(&request(value)).map(|t| t.expose_secret().to_owned());
        assert_eq!(token(Some("Bearer abc")).as_deref(), Some("abc"));
        assert_eq!(token(Some("bearer  abc ")).as_deref(), Some("abc"));
        assert_eq!(token(Some("Basic abc")), None);
        assert_eq!(token(Some("Bearer ")), None);
        assert_eq!(token(Some("Bearer")), None);
        assert_eq!(token(None), None);
    }

    #[test]
    fn callers_look_up_their_permission() {
        let caller = Caller {
            key_id: "k".into(),
            name: "laptop".into(),
            grants: vec!["a:read".parse().unwrap()],
        };
        assert_eq!(caller.permission("a"), Some(Permission::Read));
        assert_eq!(caller.permission("b"), None);
    }

    #[test]
    fn refusals_say_how_to_authenticate() {
        let response = refuse(StatusCode::UNAUTHORIZED, "x");
        assert_eq!(
            response.headers()[header::WWW_AUTHENTICATE],
            "Bearer realm=\"joedocs\""
        );
        let response = refuse(StatusCode::INTERNAL_SERVER_ERROR, "x");
        assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
    }
}
