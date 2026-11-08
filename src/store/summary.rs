//! Counts and state of the store, for `joedocs status`.

use super::{Error, Store, index_file};
use crate::db;

/// One project's document counts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectCount {
    /// Project name.
    pub name: String,
    /// Document ID prefix.
    pub key: String,
    /// Live documents.
    pub live: u64,
    /// Archived documents.
    pub archived: u64,
}

/// What the store holds and whether its index is current.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Every project with its counts, by name.
    pub projects: Vec<ProjectCount>,
    /// API keys that are not revoked.
    pub keys_active: u64,
    /// Revoked API keys.
    pub keys_revoked: u64,
    /// Admin tokens that are not revoked.
    pub admin_tokens_active: u64,
    /// Revoked admin tokens.
    pub admin_tokens_revoked: u64,
    /// The history's latest commit.
    pub history_head: Option<String>,
    /// When the latest revision was recorded.
    pub last_change: Option<String>,
    /// Whether the index matches the history (`serve` or `reindex` fixes it).
    pub index_current: bool,
    /// Projects' `INDEX.md` files that are missing or out of date.
    pub stale_index_files: Vec<String>,
}

impl Store {
    /// Summarises the store. Changes nothing.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn summary(&self) -> Result<Summary, Error> {
        let projects = db::document_counts(&self.conn)?
            .into_iter()
            .map(|(name, key, live, archived)| ProjectCount {
                name,
                key,
                live,
                archived,
            })
            .collect();
        let keys = db::list_keys(&self.conn)?;
        let revoked = keys.iter().filter(|k| k.revoked_at.is_some()).count();
        let admins = db::list_admin_tokens(&self.conn)?;
        let admins_revoked = admins.iter().filter(|t| t.revoked_at.is_some()).count();
        Ok(Summary {
            admin_tokens_active: u64::try_from(admins.len() - admins_revoked).unwrap_or(u64::MAX),
            admin_tokens_revoked: u64::try_from(admins_revoked).unwrap_or(u64::MAX),
            projects,
            keys_active: u64::try_from(keys.len() - revoked).unwrap_or(u64::MAX),
            keys_revoked: u64::try_from(revoked).unwrap_or(u64::MAX),
            history_head: self.history.head()?,
            last_change: db::last_change_at(&self.conn)?,
            index_current: !self.needs_reindex()?,
            stale_index_files: index_file::stale(&self.conn, &self.paths.home, &self.settings)?,
        })
    }
}
