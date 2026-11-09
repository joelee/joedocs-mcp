//! The files of multi-file documents: creating, reading and listing them.
//!
//! A multi-file document is a folder `<project>/<type>/<stem>/`. Its main
//! file `<stem>/<stem>.md` keeps the document's front matter and its
//! `documents` row; every other file (a sub-file) has a `document_files`
//! row and its own front matter (`document`, `revision`, `updated`, then
//! the client's keys). Every change to any file raises the document's
//! revision and is one history commit naming the files it changed in
//! `JoeDocs-File` trailers.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use super::documents::{
    Action, Change, Changed, Fetched, FileTouch, document_info, finish, integrity, live_row,
    parse_id, revision_with_content, sha256, tidy_line,
};
use super::journal::Journal;
use super::{Actor, Error, Store, begin_write, index_file};
use crate::db::{self, Connection, DocumentRow, FileChange, FileRow, RevisionFileRow};
use crate::document::FilePath;
use crate::document::front_matter::{self, Fields, FileHeader};

/// A request to add a file to a multi-file document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewFile {
    /// Document ID.
    pub document_id: String,
    /// Path inside the document folder, with or without `.md`.
    pub file: String,
    /// Markdown body, without front matter.
    pub content: String,
    /// Front matter keys: any but `document`, `revision` and `updated`.
    pub front_matter: Fields,
    /// The document revision the client last read, if it gives one.
    pub expected_revision: Option<u64>,
    /// Commit summary; the file's path when `None`.
    pub summary: Option<String>,
}

/// One live file of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Path inside the document folder, without `.md`; the main file's
    /// stem for the main file.
    pub file: String,
    /// Whether this is the main file.
    pub main: bool,
    /// The document revision at which the file last changed.
    pub revision: u64,
    /// When it last changed.
    pub updated: String,
}

/// The folder of a multi-file document's main file `row.path`, e.g.
/// `joedocs/plans/Roadmap`.
pub(super) fn folder(row: &DocumentRow) -> &str {
    row.path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The main file's stem, which is also the folder's name.
pub(super) fn main_stem(row: &DocumentRow) -> &str {
    let folder = folder(row);
    folder.rsplit_once('/').map_or(folder, |(_, name)| name)
}

/// The vault path of the file `path` (without `.md`) in `row`'s folder.
pub(super) fn vault_path(row: &DocumentRow, path: &str) -> String {
    format!("{}/{path}.md", folder(row))
}

fn file_integrity(row: &DocumentRow, file: &FileRow) -> Error {
    Error::Integrity {
        id: row.id.clone(),
        path: vault_path(row, &file.path),
    }
}

/// Reads a sub-file and checks it is the one the index recorded.
pub(super) fn read_verified_file(
    home: &Path,
    row: &DocumentRow,
    file: &FileRow,
) -> Result<String, Error> {
    let path = home.join(vault_path(row, &file.path));
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(file_integrity(row, file));
        }
        Err(error) => return Err(Error::io("read", &path)(error)),
    };
    if sha256(&bytes) != file.content_sha256 {
        return Err(file_integrity(row, file));
    }
    String::from_utf8(bytes).map_err(|_| file_integrity(row, file))
}

/// Checks every live sub-file of `row` against the index, so a document
/// with a file changed by hand is not written to.
pub(super) fn verify_files(conn: &Connection, home: &Path, row: &DocumentRow) -> Result<(), Error> {
    for file in db::live_files(conn, &row.id)? {
        read_verified_file(home, row, &file)?;
    }
    Ok(())
}

/// The main file and the live sub-files at document revision `revision`,
/// replayed from the recorded file changes: the main file's stem and the
/// revision it last changed at, and each sub-file's path and revision by
/// its case-folded path.
pub(super) struct AtRevision {
    pub(super) main: Option<(String, u64)>,
    pub(super) files: BTreeMap<String, (String, u64)>,
}

pub(super) fn files_at(changes: &[RevisionFileRow], revision: u64) -> AtRevision {
    let mut at = AtRevision {
        main: None,
        files: BTreeMap::new(),
    };
    for change in changes.iter().filter(|c| c.revision <= revision) {
        if change.main {
            at.main = Some((change.path.clone(), change.revision));
            continue;
        }
        let key = change.path.to_lowercase();
        match change.change {
            FileChange::Added | FileChange::Modified => {
                at.files.insert(key, (change.path.clone(), change.revision));
            }
            FileChange::Renamed => {
                if let Some(from) = &change.from_path {
                    at.files.remove(&from.to_lowercase());
                }
                at.files.insert(key, (change.path.clone(), change.revision));
            }
            FileChange::Removed => {
                at.files.remove(&key);
            }
        }
    }
    at
}

/// The main-file changes of a document's revisions from before it became
/// multi-file (`revisions`, oldest first), so that its file history covers
/// them: `added` for `create` and `unarchive`, `removed` for `archive`,
/// `renamed` when the file name changed, `modified` otherwise. Both
/// `document convert` and a rebuild of the index record them, from the same
/// revisions, so the two agree.
pub(super) fn backfill_main(revisions: &[db::RevisionRow]) -> Vec<RevisionFileRow> {
    let mut rows = Vec::with_capacity(revisions.len());
    let mut previous: Option<String> = None;
    for revision in revisions {
        let stem = super::documents::stem_of(&revision.path).to_owned();
        let (change, from) = match revision.action.as_str() {
            "create" | "unarchive" => (FileChange::Added, None),
            "archive" => (FileChange::Removed, None),
            _ => match &previous {
                Some(before) if *before != stem => (FileChange::Renamed, Some(before.clone())),
                _ => (FileChange::Modified, None),
            },
        };
        previous = (change != FileChange::Removed).then(|| stem.clone());
        rows.push(RevisionFileRow {
            document_id: revision.document_id.clone(),
            revision: revision.revision,
            path: stem,
            main: true,
            change,
            from_path: from,
        });
    }
    rows
}

/// The paths of `row`'s live sub-files at revision `wanted`, sorted.
pub(super) fn live_paths_at(
    conn: &Connection,
    row: &DocumentRow,
    wanted: u64,
) -> Result<Vec<String>, Error> {
    if wanted == row.revision {
        return Ok(db::live_files(conn, &row.id)?
            .into_iter()
            .map(|f| f.path)
            .collect());
    }
    let changes = db::revision_files(conn, &row.id)?;
    let mut paths: Vec<String> = files_at(&changes, wanted)
        .files
        .into_values()
        .map(|(path, _)| path)
        .collect();
    paths.sort();
    Ok(paths)
}

/// Checks a new file for `row` before anything is written: the document
/// is multi-file and not behind `expected_revision`, the path is valid and
/// free (ignoring case, in the index and on disk), the document has room,
/// and none of its files was changed by hand.
fn check_new_file(
    conn: &Connection,
    home: &Path,
    max_files: u64,
    row: &DocumentRow,
    request: &NewFile,
) -> Result<FilePath, Error> {
    if !row.multifile {
        return Err(Error::invalid(
            "document_id",
            format!(
                "{0} is a single-file document; an administrator can make it multi-file with `joedocs document convert {0}`",
                row.id
            ),
        ));
    }
    if let Some(expected) = request.expected_revision
        && expected > row.revision
    {
        return Err(Error::Conflict {
            id: row.id.clone(),
            expected,
            current: row.revision,
        });
    }
    let path = file_path(row, &request.file)?;
    let live = db::live_files(conn, &row.id)?;
    if live.iter().any(|f| f.path.to_lowercase() == path.key()) {
        return Err(Error::AlreadyExists(format!("file {path} in {}", row.id)));
    }
    refuse_folder_case("file", row, &path.as_str(), live_paths(&live))?;
    if live.len() as u64 >= max_files {
        return Err(Error::invalid(
            "documents.max_files",
            format!(
                "{} already has {} files besides its main file, the most allowed",
                row.id,
                live.len()
            ),
        ));
    }
    let vault = vault_path(row, &path.as_str());
    super::vault::refuse_unknown(home, &vault)?;
    super::documents::read_verified(home, row)?;
    for file in &live {
        read_verified_file(home, row, file)?;
    }
    Ok(path)
}

/// Whether `file` names `row`'s main file (its stem, with or without
/// `.md`, in any case).
fn is_main_name(row: &DocumentRow, file: &str) -> bool {
    let name = file
        .strip_suffix(".md")
        .or_else(|| file.strip_suffix(".MD"))
        .unwrap_or(file);
    name.to_lowercase() == main_stem(row).to_lowercase()
}

fn multifile_row(row: &DocumentRow) -> Result<(), Error> {
    if row.multifile {
        Ok(())
    } else {
        Err(Error::invalid(
            "file",
            format!(
                "{0} is a single-file document; leave out file, or ask an administrator to make it multi-file with `joedocs document convert {0}`",
                row.id
            ),
        ))
    }
}

fn file_path(row: &DocumentRow, file: &str) -> Result<FilePath, Error> {
    FilePath::parse(file, main_stem(row)).map_err(|message| Error::invalid("file", message))
}

impl Store {
    /// A live document's index entry, with its files counted, without
    /// reading its file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Archived`] and database errors.
    pub fn document_info(&self, id: &str) -> Result<super::DocumentInfo, Error> {
        let row = live_row(&self.conn, &parse_id(id)?)?;
        document_info(&self.conn, &row)
    }

    /// Adds a sub-file to a multi-file document as a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a single-file document, a bad path or
    /// front matter, or too many files; [`Error::AlreadyExists`] when the
    /// path is taken (ignoring case) by a live file or by a file the index
    /// does not know; [`Error::Conflict`] when `expected_revision` is ahead
    /// of the document; [`Error::Integrity`] when a file of the document was
    /// changed by hand; and the errors of [`Store::create`].
    pub fn create_file(&mut self, actor: &Actor, request: &NewFile) -> Result<Changed, Error> {
        let id = parse_id(&request.document_id)?;
        let at = self.clock.now();
        let now = crate::clock::format_timestamp(at);
        let Self {
            paths,
            settings,
            conn,
            history,
            ..
        } = self;
        let _lock = begin_write(paths, conn, history.as_ref())?;
        let tx = conn.transaction()?;
        let mut row = live_row(&tx, &id)?;
        let path = check_new_file(&tx, &paths.home, settings.max_files, &row, request)?;
        let vault = vault_path(&row, &path.as_str());
        let mut extra = Fields::new();
        front_matter::apply_file(&mut extra, &request.front_matter, &[])
            .map_err(|e| Error::invalid("front_matter", e.0))?;

        row.revision += 1;
        row.updated_at.clone_from(&now);
        actor.name().clone_into(&mut row.updated_by);
        actor.client().clone_into(&mut row.client);
        let header = FileHeader {
            document: row.id.clone(),
            revision: row.revision,
            updated: now.clone(),
        };
        let text = front_matter::render_file(&header, &extra, &request.content)
            .map_err(|e| Error::invalid("front_matter", e.0))?;
        let size = text.len() as u64;
        if size > settings.max_document_bytes {
            return Err(Error::TooLarge {
                size,
                limit: settings.max_document_bytes,
            });
        }
        db::save_document(&tx, &row)?;
        let mut file = FileRow {
            id: 0,
            document_id: row.id.clone(),
            path: path.as_str(),
            revision: row.revision,
            created_at: now.clone(),
            created_by: actor.name().to_owned(),
            updated_at: now.clone(),
            updated_by: actor.name().to_owned(),
            client: actor.client().to_owned(),
            content_sha256: sha256(text.as_bytes()),
            archived_at: None,
            archived_by: None,
        };
        file.id = db::insert_file(&tx, &file)?;
        let parsed = front_matter::parse(&text).map_err(|e| Error::invalid("front_matter", e.0))?;
        db::index_file(&tx, &file, &parsed.fields, parsed.body)?;

        let mut journal = Journal::default();
        journal.write(&paths.home.join(&vault), text.as_bytes())?;
        let summary = tidy_line(request.summary.as_deref()).unwrap_or_else(|| path.as_str());
        let touched = [FileTouch {
            path: path.as_str(),
            main: false,
            change: FileChange::Added,
            from: None,
        }];
        let commit = Change {
            row: &row,
            at,
            action: Action::CreateFile,
            summary: &summary,
            actor,
            paths: &[&vault],
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(Changed {
            id: row.id,
            revision: row.revision,
            path: vault,
            file: Some(path.as_str()),
            front_matter: parsed.fields,
        })
    }

    /// Reads one file of a multi-file document: the latest revision, or
    /// the file as it was at document revision `version`. Naming the main
    /// file's stem reads the main file, as [`Store::get`] does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a single-file document or a bad path,
    /// [`Error::FileNotFound`] when the document has no such file (then),
    /// and the errors of [`Store::get`].
    pub fn get_file(&self, id: &str, file: &str, version: Option<u64>) -> Result<Fetched, Error> {
        let parsed_id = parse_id(id)?;
        let row = live_row(&self.conn, &parsed_id)?;
        multifile_row(&row)?;
        if is_main_name(&row, file) {
            let mut fetched = self.get(id, version)?;
            fetched.file = Some(main_stem(&row).to_owned());
            return Ok(fetched);
        }
        let path = file_path(&row, file)?;
        let wanted = version.unwrap_or(row.revision);
        let revision = revision_with_content(&self.conn, &row, wanted, "version")?;
        let not_found = |at: String| Error::FileNotFound {
            id: row.id.clone(),
            file: path.as_str(),
            at,
        };
        let (stored, markdown) = if wanted == row.revision {
            let found = db::live_files(&self.conn, &row.id)?
                .into_iter()
                .find(|f| f.path.to_lowercase() == path.key())
                .ok_or_else(|| not_found(String::new()))?;
            let markdown = match read_verified_file(&self.paths.home, &row, &found) {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(%error, "serving the committed file instead of the vault's");
                    self.committed_file(&row, &revision.commit_id, &vault_path(&row, &found.path))?
                }
            };
            (found.path, markdown)
        } else {
            let changes = db::revision_files(&self.conn, &row.id)?;
            let (stored, _) = files_at(&changes, wanted)
                .files
                .remove(&path.key())
                .ok_or_else(|| not_found(format!(" at revision {wanted}")))?;
            let folder = revision.path.rsplit_once('/').map_or("", |(f, _)| f);
            let markdown =
                self.committed_file(&row, &revision.commit_id, &format!("{folder}/{stored}.md"))?;
            (stored, markdown)
        };
        let files = live_paths_at(&self.conn, &row, wanted)?;
        Ok(Fetched {
            document: document_info(&self.conn, &row)?,
            revision: revision.into(),
            markdown,
            file: Some(stored),
            files,
        })
    }

    fn committed_file(&self, row: &DocumentRow, commit: &str, path: &str) -> Result<String, Error> {
        let bytes = self.history.show(commit, path)?;
        String::from_utf8(bytes).map_err(|_| integrity(row))
    }

    /// Lists a document's live files: the main file first, then the
    /// sub-files by path; at document revision `version` when given.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Archived`], [`Error::Invalid`]
    /// for a revision that does not exist, and storage errors.
    pub fn list_files(&self, id: &str, version: Option<u64>) -> Result<Vec<FileInfo>, Error> {
        let row = live_row(&self.conn, &parse_id(id)?)?;
        let wanted = version.unwrap_or(row.revision);
        revision_with_content(&self.conn, &row, wanted, "version")?;
        let time = |revision: u64| -> Result<String, Error> {
            Ok(db::get_revision(&self.conn, &row.id, revision)?
                .map(|r| r.created_at)
                .unwrap_or_default())
        };
        if !row.multifile || wanted == row.revision {
            let mut files = vec![FileInfo {
                file: if row.multifile {
                    main_stem(&row).to_owned()
                } else {
                    super::documents::stem_of(&row.path).to_owned()
                },
                main: true,
                revision: if row.multifile {
                    row.main_revision
                } else {
                    wanted
                },
                updated: time(if row.multifile {
                    row.main_revision
                } else {
                    wanted
                })?,
            }];
            if row.multifile {
                files.extend(
                    db::live_files(&self.conn, &row.id)?
                        .into_iter()
                        .map(|f| FileInfo {
                            file: f.path,
                            main: false,
                            revision: f.revision,
                            updated: f.updated_at,
                        }),
                );
            }
            return Ok(files);
        }
        let changes = db::revision_files(&self.conn, &row.id)?;
        let at = files_at(&changes, wanted);
        let mut files = Vec::with_capacity(at.files.len() + 1);
        if let Some((stem, revision)) = at.main {
            files.push(FileInfo {
                file: stem,
                main: true,
                revision,
                updated: time(revision)?,
            });
        }
        let mut subs: Vec<(String, u64)> = at.files.into_values().collect();
        subs.sort();
        for (file, revision) in subs {
            files.push(FileInfo {
                file,
                main: false,
                revision,
                updated: time(revision)?,
            });
        }
        Ok(files)
    }
}

/// Refuses `path` for a file of `row` when one of its folders differs only
/// in case from a folder of `used` (the document's live files), which a
/// case-insensitive file system would hold as one folder.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for `field`, naming both spellings.
pub(super) fn refuse_folder_case<'a>(
    field: &'static str,
    row: &DocumentRow,
    path: &str,
    used: impl IntoIterator<Item = &'a str>,
) -> Result<(), Error> {
    match crate::document::folder_case_clash(path, used) {
        Some((ours, theirs)) => Err(Error::invalid(
            field,
            folder_case_message(&ours, &theirs, &row.id),
        )),
        None => Ok(()),
    }
}

/// The message of a [`refuse_folder_case`] refusal.
pub(super) fn folder_case_message(ours: &str, theirs: &str, id: &str) -> String {
    format!("folder {ours} differs only in case from {theirs}, which {id} already uses")
}

/// The paths of `files`, for [`refuse_folder_case`].
pub(super) fn live_paths(files: &[FileRow]) -> impl Iterator<Item = &str> {
    files.iter().map(|f| f.path.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(
        revision: u64,
        path: &str,
        main: bool,
        kind: FileChange,
        from: Option<&str>,
    ) -> RevisionFileRow {
        RevisionFileRow {
            document_id: "JD-1".into(),
            revision,
            path: path.into(),
            main,
            change: kind,
            from_path: from.map(str::to_owned),
        }
    }

    #[test]
    fn files_at_a_revision_follow_adds_changes_renames_and_removals() {
        let changes = [
            change(1, "Roadmap", true, FileChange::Added, None),
            change(2, "a", false, FileChange::Added, None),
            change(3, "b", false, FileChange::Added, None),
            change(4, "A2", false, FileChange::Renamed, Some("a")),
            change(5, "b", false, FileChange::Removed, None),
            change(6, "Roadmap", true, FileChange::Modified, None),
            change(6, "A2", false, FileChange::Modified, None),
        ];
        let names =
            |at: &AtRevision| -> Vec<(String, u64)> { at.files.values().cloned().collect() };
        let at = files_at(&changes, 3);
        assert_eq!(at.main, Some(("Roadmap".into(), 1)));
        assert_eq!(names(&at), [("a".into(), 2), ("b".into(), 3)]);
        assert_eq!(
            names(&files_at(&changes, 4)),
            [("A2".into(), 4), ("b".into(), 3)]
        );
        let at = files_at(&changes, 6);
        assert_eq!(at.main, Some(("Roadmap".into(), 6)));
        assert_eq!(names(&at), [("A2".into(), 6)]);
    }
}
