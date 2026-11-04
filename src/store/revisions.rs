//! Reading a document's history: its revisions and diffs between them.

use super::documents::{DocumentInfo, RevisionInfo, live_row, parse_id, revision_with_content};
use super::{Error, Store};
use crate::db;

/// Most revisions one history page may return.
pub const MAX_HISTORY_LIMIT: u64 = 1000;

/// Most context lines a diff may ask for.
pub const MAX_DIFF_CONTEXT: u32 = 100;

/// One page of a document's revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    /// The document.
    pub document: DocumentInfo,
    /// Revisions, newest first.
    pub revisions: Vec<RevisionInfo>,
    /// How many revisions there are.
    pub total: u64,
    /// `start` for the next page, if there is one.
    pub next_start: Option<u64>,
}

/// A diff between two revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// Document ID.
    pub id: String,
    /// The older side.
    pub from: u64,
    /// The newer side.
    pub to: u64,
    /// Unified diff of the whole file (front matter and body); empty when
    /// the revisions are the same.
    pub diff: String,
}

impl Store {
    /// A page of a document's revisions, newest first. `start` is the
    /// 1-based position in that order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Archived`], [`Error::Invalid`]
    /// for a bad start or limit, and database errors.
    pub fn history(&self, id: &str, start: u64, limit: u64) -> Result<HistoryPage, Error> {
        if start == 0 {
            return Err(Error::invalid("start", "must be 1 or more"));
        }
        if limit == 0 || limit > MAX_HISTORY_LIMIT {
            return Err(Error::invalid(
                "limit",
                format!("must be 1 to {MAX_HISTORY_LIMIT}"),
            ));
        }
        let row = live_row(&self.conn, &parse_id(id)?)?;
        let offset = start - 1;
        let revisions: Vec<RevisionInfo> = db::list_revisions(&self.conn, &row.id, offset, limit)?
            .into_iter()
            .map(RevisionInfo::from)
            .collect();
        let shown = offset + revisions.len() as u64;
        Ok(HistoryPage {
            total: row.revision,
            next_start: (shown < row.revision).then_some(shown + 1),
            document: DocumentInfo::from(&row),
            revisions,
        })
    }

    /// The unified diff from revision `from` to revision `to` (the latest
    /// when `None`), with `context` lines around each change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Archived`], [`Error::Invalid`]
    /// for a revision that does not exist or has no content, and storage
    /// errors.
    pub fn diff(&self, id: &str, from: u64, to: Option<u64>, context: u32) -> Result<Diff, Error> {
        if context > MAX_DIFF_CONTEXT {
            return Err(Error::invalid(
                "context",
                format!("must be 0 to {MAX_DIFF_CONTEXT}"),
            ));
        }
        let row = live_row(&self.conn, &parse_id(id)?)?;
        let to = to.unwrap_or(row.revision);
        let old = revision_with_content(&self.conn, &row, from, "from")?;
        let new = revision_with_content(&self.conn, &row, to, "to")?;
        let hunks = self.history.diff(
            (&old.commit_id, &old.path),
            (&new.commit_id, &new.path),
            context,
        )?;
        let diff = if hunks.is_empty() {
            hunks
        } else {
            format!(
                "--- {id} revision {from}\n+++ {id} revision {to}\n{hunks}",
                id = row.id
            )
        };
        Ok(Diff {
            id: row.id,
            from,
            to,
            diff,
        })
    }
}
