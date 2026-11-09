//! `joedocs check`: drift between the vault, the history and the index.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::reindex::drifted;
use super::{Error, Store, index_file};
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
    /// Projects' `INDEX.md` files that are missing or out of date,
    /// vault-relative (`reindex` rewrites them).
    pub stale_index_files: Vec<String>,
}

impl CheckReport {
    /// How many problems were found.
    #[must_use]
    pub fn problems(&self) -> usize {
        usize::from(self.index_behind)
            + self.drifted.len()
            + self.unknown_files.len()
            + self.stale_index_files.len()
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
        let mut known = HashSet::new();
        for row in db::live_documents(&self.conn)? {
            if row.multifile {
                // Never unknown, like a project's INDEX.md, even when
                // index files are off and an old one is left in place.
                known.insert(index_file::folder_index_path(&row));
                for file in db::live_files(&self.conn, &row.id)? {
                    known.insert(super::files::vault_path(&row, &file.path));
                }
            }
            known.insert(row.path);
        }
        let projects: HashSet<String> = db::list_projects(&self.conn)?
            .into_iter()
            .map(|project| project.name)
            .collect();
        let mut files = Vec::new();
        walk(&self.paths.home, "", &mut files)?;
        files.sort();
        Ok(CheckReport {
            index_behind,
            drifted,
            unknown_files: files
                .into_iter()
                .filter(|f| !known.contains(f) && !is_index_file(f, &projects))
                .collect(),
            stale_index_files: index_file::stale(&self.conn, &self.paths.home, &self.settings)?,
        })
    }
}

/// Whether `path` (vault-relative) is a registered project's `INDEX.md`, a
/// reserved path that is never reported as unknown.
fn is_index_file(path: &str, projects: &HashSet<String>) -> bool {
    path.split_once('/')
        .is_some_and(|(project, rest)| rest == index_file::FILE_NAME && projects.contains(project))
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
