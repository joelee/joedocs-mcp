//! Who made an admin request: an admin token (deprecated since 0.5.1) or a
//! signed-in user. The admin API and the web interface carry one [`Admin`]
//! from the credential check to the audit row and to the author of any
//! change.

use crate::db::{AdminTokenRow, UserRow};

/// The caller of an admin API route or web page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admin {
    /// An admin token, with its session.
    Token(AdminTokenRow),
    /// A signed-in user.
    User(UserRow),
}

impl Admin {
    /// The credential's ID, written to the audit log's key column.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Token(token) => &token.id,
            Self::User(user) => &user.id,
        }
    }

    /// The token's or user's name: the audit log's key name and the author
    /// of changes made from the web interface.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Token(token) => &token.name,
            Self::User(user) => &user.name,
        }
    }

    /// The admin token, when the caller used one.
    #[must_use]
    pub fn token(&self) -> Option<&AdminTokenRow> {
        match self {
            Self::Token(token) => Some(token),
            Self::User(_) => None,
        }
    }

    /// The user, when the caller signed in as one.
    #[must_use]
    pub fn user(&self) -> Option<&UserRow> {
        match self {
            Self::Token(_) => None,
            Self::User(user) => Some(user),
        }
    }
}
