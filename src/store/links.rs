//! Read-only lookups for wiki links in the web interface: a document by
//! title or alias, and a file of a multi-file document by path or name.

use super::{Error, Store};
use crate::db;

impl Store {
    /// The live document a wiki link `[[target]]` names: by title or alias,
    /// ignoring case, preferring one in `project`, then a title over an
    /// alias. Changes nothing.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn find_by_title(&self, project: &str, target: &str) -> Result<Option<String>, Error> {
        let target = target.trim();
        if target.is_empty() {
            return Ok(None);
        }
        Ok(db::find_by_title(&self.conn, project, target)?)
    }

    /// The live file of `document_id` a wiki link names: its path (with or
    /// without `.md`, ignoring case), or a file name only one of its files
    /// has. Changes nothing.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn find_file(&self, document_id: &str, target: &str) -> Result<Option<String>, Error> {
        let target = target.trim().trim_matches('/');
        let target = target
            .strip_suffix(".md")
            .or_else(|| target.strip_suffix(".MD"))
            .unwrap_or(target);
        if target.is_empty() {
            return Ok(None);
        }
        let found = db::find_files(&self.conn, document_id, target)?;
        if let Some(exact) = found.iter().find(|p| p.eq_ignore_ascii_case(target)) {
            return Ok(Some(exact.clone()));
        }
        Ok(match found.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        })
    }
}
