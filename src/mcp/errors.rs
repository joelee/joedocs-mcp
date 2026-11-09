//! Tool errors: a stable `code`, a message for the agent, and details.
//!
//! Errors are returned as tool results with `isError: true` and
//! `structuredContent: {"error": {...}}`, so the agent can read and act on
//! them. Internal faults are logged in full and reported only as
//! `internal`, so paths and database details never reach clients.

use rmcp::model::CallToolResult;
use serde_json::{Map, Value, json};

use crate::store;

/// An error returned from a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    /// Stable machine-readable code, e.g. `conflict`.
    pub code: &'static str,
    /// What went wrong and, where useful, what to do.
    pub message: String,
    /// Extra fields, e.g. `current_revision`.
    pub details: Map<String, Value>,
}

impl ToolError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.details.insert(key.to_owned(), value.into());
        self
    }

    /// A server fault.
    pub(super) fn internal(message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::error!(%message, "internal error");
        Self::new(
            "internal",
            "internal server error; the server log has details",
        )
    }

    /// The key may read but not write this project.
    pub(super) fn permission_denied(project: &str) -> Self {
        Self::new(
            "permission_denied",
            format!("this API key may only read project {project}"),
        )
        .with("project", project)
    }

    /// The request has no authenticated caller (should not happen behind
    /// the middleware).
    pub(super) fn unauthenticated() -> Self {
        Self::new("unauthenticated", "no API key on this request")
    }

    /// The tool result carrying this error.
    #[must_use]
    pub fn into_result(self) -> CallToolResult {
        let mut error = Map::new();
        error.insert("code".to_owned(), self.code.into());
        error.insert("message".to_owned(), self.message.into());
        error.extend(self.details);
        CallToolResult::structured_error(json!({ "error": error }))
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<store::Error> for ToolError {
    fn from(error: store::Error) -> Self {
        use store::Error as E;
        let message = error.to_string();
        match error {
            E::Invalid { field, .. } => Self::new("invalid_argument", message).with("field", field),
            E::NotFound(id) => Self::new("not_found", message).with("document_id", id),
            E::FileNotFound { id, file, .. } => Self::new("not_found", message)
                .with("document_id", id)
                .with("file", file),
            E::ProjectNotFound(project) => Self::new("not_found", message).with("project", project),
            E::KeyNotFound(_) => Self::new("not_found", message),
            E::Archived(id) => Self::new(
                "archived",
                format!("{message}; an administrator can restore it"),
            )
            .with("document_id", id),
            E::AlreadyExists(_) => Self::new("already_exists", message),
            E::Conflict { id, current, .. } => Self::new("conflict", message)
                .with("document_id", id)
                .with("current_revision", current),
            E::FileConflict {
                id,
                file,
                file_revision,
                current,
                ..
            } => Self::new("conflict", message)
                .with("document_id", id)
                .with("file", file)
                .with("file_revision", file_revision)
                .with("current_revision", current),
            E::Integrity { id, .. } => Self::new(
                "integrity",
                format!(
                    "document {id} was changed outside JoeDocs, so it will not be overwritten; \
                     an administrator must resolve it (joedocs check)"
                ),
            )
            .with("document_id", id),
            E::TooLarge { size, limit } => Self::new("too_large", message)
                .with("size", size)
                .with("limit", limit),
            other => Self::internal(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structured(error: ToolError) -> Value {
        let result = error.into_result();
        assert_eq!(result.is_error, Some(true));
        result.structured_content.unwrap()
    }

    #[test]
    fn conflicts_carry_the_current_revision() {
        let error = ToolError::from(store::Error::Conflict {
            id: "JD-1".into(),
            expected: 1,
            current: 3,
        });
        let value = structured(error);
        assert_eq!(value["error"]["code"], "conflict");
        assert_eq!(value["error"]["current_revision"], 3);
        assert_eq!(value["error"]["document_id"], "JD-1");
    }

    #[test]
    fn store_errors_map_to_codes() {
        let cases = [
            (store::Error::invalid("title", "x"), "invalid_argument"),
            (store::Error::NotFound("JD-1".into()), "not_found"),
            (
                store::Error::FileNotFound {
                    id: "JD-1".into(),
                    file: "notes".into(),
                    at: String::new(),
                },
                "not_found",
            ),
            (store::Error::ProjectNotFound("p".into()), "not_found"),
            (store::Error::KeyNotFound("k".into()), "not_found"),
            (store::Error::Archived("JD-1".into()), "archived"),
            (store::Error::AlreadyExists("x".into()), "already_exists"),
            (
                store::Error::Integrity {
                    id: "JD-1".into(),
                    path: "/secret/path".into(),
                },
                "integrity",
            ),
            (store::Error::TooLarge { size: 9, limit: 1 }, "too_large"),
            (
                store::Error::NotInitialised("/var/lib/joedocs".into()),
                "internal",
            ),
        ];
        for (error, code) in cases {
            let value = structured(ToolError::from(error));
            assert_eq!(value["error"]["code"], code);
            let text = value.to_string();
            assert!(
                !text.contains("/secret") && !text.contains("/var/lib"),
                "{text}"
            );
        }
    }

    #[test]
    fn helpers_build_their_codes() {
        assert_eq!(ToolError::permission_denied("p").code, "permission_denied");
        assert_eq!(
            ToolError::unauthenticated().to_string(),
            "unauthenticated: no API key on this request"
        );
        assert_eq!(
            ToolError::internal("boom").message,
            "internal server error; the server log has details"
        );
    }
}
