//! Reading a document's history: its revisions and diffs between them.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::documents::{DocumentInfo, RevisionInfo, live_row, parse_id, revision_with_content};
use super::files::{files_at, main_stem};
use super::{Error, Store};
use crate::db::{self, DocumentRow, FileChange, RevisionFileRow, RevisionRow};
use crate::document::FilePath;

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
    /// the revisions are the same. For a multi-file document, the diffs of
    /// every file that changed, one after another, each headed by its path.
    pub diff: String,
    /// For a multi-file document, the files the diff covers: the main file
    /// first if it changed, then the others by path. Empty otherwise.
    pub files: Vec<String>,
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
        if row.multifile {
            return self.folder_diff(&row, &old, &new, context);
        }
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
            files: Vec::new(),
        })
    }

    /// A page of the revisions that changed one file of a multi-file
    /// document, newest first, following the file back through its renames
    /// (to when it was created). Naming the main file lists the revisions
    /// that changed it. Archived files have a history too.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FileNotFound`] for a path no revision has used,
    /// [`Error::Invalid`] for a single-file document, a bad path, start or
    /// limit, and the errors of [`Store::history`].
    pub fn history_file(
        &self,
        id: &str,
        file: &str,
        start: u64,
        limit: u64,
    ) -> Result<HistoryPage, Error> {
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
        let changes = db::revision_files(&self.conn, &row.id)?;
        let lineage = lineage(&row, &changes, file, row.revision)?;
        let total = lineage.len() as u64;
        let offset = start - 1;
        let mut revisions = Vec::new();
        for (revision, path) in lineage
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
        {
            let found = db::get_revision(&self.conn, &row.id, revision)?.ok_or_else(|| {
                Error::invalid("file", format!("revision {revision} is not indexed"))
            })?;
            let vault = file_vault(&found, &path, revision_is_main(&changes, revision, &path));
            let mut info = RevisionInfo::from(found);
            info.path = vault;
            info.file = Some(path);
            revisions.push(info);
        }
        let shown = offset + revisions.len() as u64;
        Ok(HistoryPage {
            document: DocumentInfo::from(&row),
            revisions,
            total,
            next_start: (shown < total).then_some(shown + 1),
        })
    }

    /// The unified diff of one file of a multi-file document from revision
    /// `from` to `to` (the latest when `None`). `file` is its path at `to`;
    /// the diff follows it back through renames to its path at `from`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FileNotFound`] when the file did not exist at either
    /// revision, and the errors of [`Store::diff`].
    pub fn diff_file(
        &self,
        id: &str,
        file: &str,
        from: u64,
        to: Option<u64>,
        context: u32,
    ) -> Result<Diff, Error> {
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
        let changes = db::revision_files(&self.conn, &row.id)?;
        let lineage = lineage(&row, &changes, file, to)?;
        let main = lineage
            .first()
            .is_some_and(|(revision, path)| revision_is_main(&changes, *revision, path));
        let not_at = |revision: u64, name: &str| Error::FileNotFound {
            id: row.id.clone(),
            file: name.to_owned(),
            at: format!(" at revision {revision}"),
        };
        let name_to = name_at(&changes, main, &lineage, to).ok_or_else(|| not_at(to, file))?;
        let name_from =
            name_at(&changes, main, &lineage, from).ok_or_else(|| not_at(from, &name_to))?;
        let hunks = self.history.diff(
            (&old.commit_id, &file_vault(&old, &name_from, main)),
            (&new.commit_id, &file_vault(&new, &name_to, main)),
            context,
        )?;
        let diff = if hunks.is_empty() {
            hunks
        } else {
            format!(
                "--- {id} revision {from}: {name_from}\n+++ {id} revision {to}: {name_to}\n{hunks}",
                id = row.id
            )
        };
        Ok(Diff {
            id: row.id,
            from,
            to,
            diff,
            files: vec![name_to],
        })
    }

    /// The diffs of every file of a multi-file document that changed
    /// between two revisions. A file that exists at only one of them shows
    /// as wholly added or removed.
    fn folder_diff(
        &self,
        row: &DocumentRow,
        old: &RevisionRow,
        new: &RevisionRow,
        context: u32,
    ) -> Result<Diff, Error> {
        let changes = db::revision_files(&self.conn, &row.id)?;
        let before = files_at(&changes, old.revision);
        let after = files_at(&changes, new.revision);
        let mut diff = String::new();
        let mut files = Vec::new();
        let head = |diff: &mut String, from: &str, to: &str| {
            let _ = write!(
                diff,
                "--- {id} revision {}: {from}\n+++ {id} revision {}: {to}\n",
                old.revision,
                new.revision,
                id = row.id
            );
        };
        if let (Some((stem_from, rev_from)), Some((stem_to, rev_to))) = (&before.main, &after.main)
            && rev_from != rev_to
        {
            let hunks = self.history.diff(
                (&old.commit_id, &old.path),
                (&new.commit_id, &new.path),
                context,
            )?;
            if !hunks.is_empty() {
                head(&mut diff, stem_from, stem_to);
                diff.push_str(&hunks);
                files.push(stem_to.clone());
            }
        }
        let keys: BTreeSet<&String> = before.files.keys().chain(after.files.keys()).collect();
        let mut by_path: Vec<(String, Side<'_>, Side<'_>)> = keys
            .into_iter()
            .map(|key| {
                let (then, now) = (before.files.get(key), after.files.get(key));
                let name = now
                    .or(then)
                    .map(|(path, _)| path.clone())
                    .unwrap_or_default();
                (name, then, now)
            })
            .collect();
        by_path.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, then, now) in by_path {
            match (then, now) {
                (Some((path_from, rev_from)), Some((path_to, rev_to))) if rev_from != rev_to => {
                    let hunks = self.history.diff(
                        (&old.commit_id, &file_vault(old, path_from, false)),
                        (&new.commit_id, &file_vault(new, path_to, false)),
                        context,
                    )?;
                    if !hunks.is_empty() {
                        head(&mut diff, path_from, path_to);
                        diff.push_str(&hunks);
                        files.push(path_to.clone());
                    }
                }
                (Some((path, _)), None) => {
                    let text = self
                        .history
                        .show(&old.commit_id, &file_vault(old, path, false))?;
                    head(&mut diff, path, &format!("{path} (removed)"));
                    whole(&mut diff, &String::from_utf8_lossy(&text), '-');
                    files.push(path.clone());
                }
                (None, Some((path, _))) => {
                    let text = self
                        .history
                        .show(&new.commit_id, &file_vault(new, path, false))?;
                    head(&mut diff, &format!("{path} (added)"), path);
                    whole(&mut diff, &String::from_utf8_lossy(&text), '+');
                    files.push(path.clone());
                }
                _ => {}
            }
        }
        Ok(Diff {
            id: row.id.clone(),
            from: old.revision,
            to: new.revision,
            diff,
            files,
        })
    }
}

/// One side of a folder diff: a file's path and last-changed revision at
/// that revision, if it existed.
type Side<'a> = Option<&'a (String, u64)>;

/// A hunk adding (`+`) or removing (`-`) all of `text`.
fn whole(diff: &mut String, text: &str, sign: char) {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let _ = if sign == '+' {
        writeln!(diff, "@@ -0,0 +1,{n} @@")
    } else {
        writeln!(diff, "@@ -1,{n} +0,0 @@")
    };
    for line in lines {
        let _ = writeln!(diff, "{sign}{line}");
    }
}

/// Whether the change recorded for `path` at `revision` is the main file's.
fn revision_is_main(changes: &[RevisionFileRow], revision: u64, path: &str) -> bool {
    changes
        .iter()
        .any(|c| c.revision == revision && c.main && c.path == path)
}

/// The vault path, at `revision`, of the file `path` (the main file's stem
/// when `main`).
fn file_vault(revision: &RevisionRow, path: &str, main: bool) -> String {
    if main {
        return revision.path.clone();
    }
    let folder = revision.path.rsplit_once('/').map_or("", |(f, _)| f);
    format!("{folder}/{path}.md")
}

/// The revisions up to `upto` that changed the file `file` names, newest
/// first, each with the file's path then: for the main file every revision
/// that changed it; for another file, back through its renames to its
/// creation.
fn lineage(
    row: &DocumentRow,
    changes: &[RevisionFileRow],
    file: &str,
    upto: u64,
) -> Result<Vec<(u64, String)>, Error> {
    if !row.multifile {
        return Err(Error::invalid(
            "file",
            format!("{} is a single-file document; leave out file", row.id),
        ));
    }
    let stem = file
        .strip_suffix(".md")
        .or_else(|| file.strip_suffix(".MD"))
        .unwrap_or(file);
    let newest = changes.iter().rev().filter(|c| c.revision <= upto);
    if stem.to_lowercase() == main_stem(row).to_lowercase() {
        return Ok(newest
            .filter(|c| c.main)
            .map(|c| (c.revision, c.path.clone()))
            .collect());
    }
    let path = FilePath::parse(file, main_stem(row)).map_err(|m| Error::invalid("file", m))?;
    let mut name = path.key();
    let mut found = Vec::new();
    for change in newest.filter(|c| !c.main) {
        if change.path.to_lowercase() != name {
            continue;
        }
        found.push((change.revision, change.path.clone()));
        match change.change {
            FileChange::Renamed => {
                if let Some(from) = &change.from_path {
                    name = from.to_lowercase();
                }
            }
            FileChange::Added => break,
            FileChange::Modified | FileChange::Removed => {}
        }
    }
    if found.is_empty() {
        return Err(Error::FileNotFound {
            id: row.id.clone(),
            file: path.as_str(),
            at: String::new(),
        });
    }
    Ok(found)
}

/// The file's path at `revision`, given its `lineage` (newest first), if
/// it existed then.
fn name_at(
    changes: &[RevisionFileRow],
    main: bool,
    lineage: &[(u64, String)],
    revision: u64,
) -> Option<String> {
    let at = files_at(changes, revision);
    if main {
        return at.main.map(|(stem, _)| stem);
    }
    // Its newest change at or before `revision` gives its name then.
    let (_, path) = lineage.iter().find(|(r, _)| *r <= revision)?;
    at.files
        .get(&path.to_lowercase())
        .map(|(path, _)| path.clone())
}
