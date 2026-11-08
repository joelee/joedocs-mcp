//! The consistent snapshot behind `joedocs backup`: taken under the write
//! lock, so no write is half done, and copied into a staging directory so
//! the lock is held only while copying, not while compressing.

use std::fs;
use std::path::Path;

use super::{Error, Store, lock};
use crate::db;

/// What a snapshot captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The history's latest commit at the time of the snapshot.
    pub history_head: Option<String>,
    /// Live documents at the time of the snapshot.
    pub documents: usize,
    /// Vault entries that were skipped because they are symlinks.
    pub skipped_links: Vec<String>,
}

impl Store {
    /// Copies the database (with `VACUUM INTO`), the history and the vault
    /// into `staging/state/joedocs.db`, `staging/state/history.git` and
    /// `staging/content`, holding the write lock throughout.
    ///
    /// # Errors
    ///
    /// Returns I/O, database and history errors.
    pub fn snapshot(&self, staging: &Path) -> Result<Snapshot, Error> {
        let _lock = lock(&self.paths)?;
        let state = staging.join("state");
        fs::create_dir_all(&state).map_err(Error::io("create", &state))?;
        let db_copy = state.join("joedocs.db");
        self.conn
            .execute("VACUUM INTO ?1", [db_copy.to_string_lossy().as_ref()])?;
        let mut skipped_links = Vec::new();
        copy_tree(
            &self.paths.git_dir(),
            &state.join("history.git"),
            "",
            &mut skipped_links,
        )?;
        skipped_links.clear();
        copy_tree(
            &self.paths.home,
            &staging.join("content"),
            "",
            &mut skipped_links,
        )?;
        Ok(Snapshot {
            history_head: self.history.head()?,
            documents: db::live_documents(&self.conn)?.len(),
            skipped_links,
        })
    }
}

/// Copies the regular files and directories under `from` to `to`, noting
/// symlinks (relative to the top) in `skipped` instead of following them.
fn copy_tree(from: &Path, to: &Path, prefix: &str, skipped: &mut Vec<String>) -> Result<(), Error> {
    fs::create_dir_all(to).map_err(Error::io("create", to))?;
    for entry in fs::read_dir(from).map_err(Error::io("read", from))? {
        let entry = entry.map_err(Error::io("read", from))?;
        let name = entry.file_name();
        let source = entry.path();
        let target = to.join(&name);
        let relative = format!("{prefix}{}", name.to_string_lossy());
        let kind = fs::symlink_metadata(&source).map_err(Error::io("read", &source))?;
        if kind.is_dir() {
            copy_tree(&source, &target, &format!("{relative}/"), skipped)?;
        } else if kind.is_file() {
            fs::copy(&source, &target).map_err(Error::io("copy", &source))?;
        } else {
            skipped.push(relative);
        }
    }
    Ok(())
}
