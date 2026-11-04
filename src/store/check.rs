//! `joedocs check`: drift between the vault, the history and the index.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::reindex::drifted;
use super::{Error, Store};
use crate::db;

/// What [`Store::check`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// The index is behind the history or from an older build (`serve` or
    /// `reindex` fixes it).
    pub index_behind: bool,
    /// Live documents whose vault file is missing or differs from the
    /// history, as `(id, path)`.
    pub drifted: Vec<(String, String)>,
    /// Vault files that belong to no live document, vault-relative.
    pub unknown_files: Vec<String>,
}

impl CheckReport {
    /// How many problems were found.
    #[must_use]
    pub fn problems(&self) -> usize {
        usize::from(self.index_behind) + self.drifted.len() + self.unknown_files.len()
    }
}

impl Store {
    /// Compares the vault with the history and the index. Changes nothing.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn check(&self) -> Result<CheckReport, Error> {
        let index_behind = self.needs_reindex()?;
        let drifted = drifted(&self.conn, &self.paths.home)?;
        let known: HashSet<String> = db::live_documents(&self.conn)?
            .into_iter()
            .map(|row| row.path)
            .collect();
        let mut files = Vec::new();
        walk(&self.paths.home, "", &mut files)?;
        files.sort();
        Ok(CheckReport {
            index_behind,
            drifted,
            unknown_files: files.into_iter().filter(|f| !known.contains(f)).collect(),
        })
    }
}

/// Collects the files under `dir`, skipping names that start with a dot.
fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) -> Result<(), Error> {
    for entry in fs::read_dir(dir).map_err(Error::io("read", dir))? {
        let entry = entry.map_err(Error::io("read", dir))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}{name}");
        let kind = entry
            .file_type()
            .map_err(Error::io("read", &entry.path()))?;
        if kind.is_dir() {
            walk(&entry.path(), &format!("{path}/"), files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}
