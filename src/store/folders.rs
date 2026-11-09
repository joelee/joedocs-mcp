//! Whole-document operations on multi-file documents: moving the folder
//! when the document is renamed or moved, archiving and unarchiving the
//! whole folder, restoring the folder as it was at an earlier revision, and
//! archiving one file.
//!
//! The sub-files keep their paths inside the folder and their content when
//! the folder moves, so only the main file is named in the commit trailers;
//! a rebuild moves the other files with it.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::documents::{
    Action, Change, Changed, FileTouch, check_expected, finish, integrity, live_row, parse_id,
    read_verified, remove_empty_parent, revision_with_content, stem_of, tidy_line, write_revision,
};
use super::file_changes::{
    FileDraft, bump, check_file_revision, new_row, parse_path, remove_empty_dirs, render,
    require_multifile, save, verify_all,
};
use super::files::{files_at, main_stem, read_verified_file, vault_path};
use super::journal::Journal;
use super::{Actor, Error, Settings, Store, begin_write, index_file};
use crate::db::{self, Connection, DocumentRow, FileChange, FileRow, RevisionRow};
use crate::document::FilePath;
use crate::document::front_matter::{self, Fields};
use crate::history::History;

/// The result of [`Store::archive_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedFile {
    /// Document ID.
    pub id: String,
    /// The new revision.
    pub revision: u64,
    /// The archived file's path inside the folder, without `.md`.
    pub file: String,
    /// Its vault path, now removed.
    pub path: String,
    /// When it was archived.
    pub archived_at: String,
}

/// The folder of a main file's vault path.
fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// Whether `path` is a multi-file document's main file,
/// `<project>/<type>/<name>/<name>.md`.
fn is_main_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() == 4 && parts[3].strip_suffix(".md") == Some(parts[2])
}

/// Writes the main file as `write_revision` does, from `old_path`. When a
/// multi-file document's folder changes, its sub-files move with it: a
/// change of case only renames the folder itself (so it works on
/// case-insensitive file systems too), otherwise each live sub-file is
/// renamed into the new folder. Returns the vault paths the moves touched,
/// for the commit.
pub(super) fn write_main(
    conn: &Connection,
    home: &Path,
    settings: &Settings,
    row: DocumentRow,
    extra: &Fields,
    body: &str,
    old_path: &str,
) -> Result<(DocumentRow, Fields, Journal, Vec<String>), Error> {
    let old_folder = folder_of(old_path).to_owned();
    let new_folder = folder_of(&row.path).to_owned();
    if !row.multifile || old_folder == new_folder {
        let (row, fields, journal) =
            write_revision(conn, home, settings, row, extra, body, Some(old_path))?;
        return Ok((row, fields, journal, Vec::new()));
    }
    let live = db::live_files(conn, &row.id)?;
    let mut moved = Vec::with_capacity(live.len() * 2);
    for file in &live {
        moved.push(format!("{old_folder}/{}.md", file.path));
        moved.push(format!("{new_folder}/{}.md", file.path));
    }
    if old_folder.eq_ignore_ascii_case(&new_folder) {
        super::vault::refuse_occupied(home, &old_folder, &new_folder)?;
        let mut journal = Journal::default();
        journal.rename(&home.join(&old_folder), &home.join(&new_folder))?;
        let name = old_path.rsplit_once('/').map_or(old_path, |(_, name)| name);
        let from = format!("{new_folder}/{name}");
        let (row, fields, written) =
            write_revision(conn, home, settings, row, extra, body, Some(&from))?;
        return Ok((row, fields, journal.then(written), moved));
    }
    let (row, fields, mut journal) =
        write_revision(conn, home, settings, row, extra, body, Some(old_path))?;
    for pair in moved.chunks(2) {
        if let [from, to] = pair {
            journal.rename(&home.join(from), &home.join(to))?;
        }
    }
    Ok((row, fields, journal, moved))
}

/// The main file's touch for a revision of `row` whose main file was at
/// `old_path`: renamed when it moved, else modified. Empty for a
/// single-file document.
pub(super) fn main_moved(row: &DocumentRow, old_path: &str) -> Vec<FileTouch> {
    if !row.multifile {
        return Vec::new();
    }
    let moved = old_path != row.path;
    vec![FileTouch {
        path: main_stem(row).to_owned(),
        main: true,
        change: if moved {
            FileChange::Renamed
        } else {
            FileChange::Modified
        },
        from: moved.then(|| stem_of(old_path).to_owned()),
    }]
}

/// Tidies the vault after a document's file left `old_path`: removes the
/// empty type and project directories, and for a multi-file document the
/// old folder (its `INDEX.md` and empty directories), unless the document
/// is still in it (`new_path` in the same folder, in any case).
pub(super) fn clean_after_move(home: &Path, old_path: &str, new_path: Option<&str>) {
    if !is_main_path(old_path) {
        remove_empty_parent(&home.join(old_path));
        return;
    }
    let old_folder = folder_of(old_path);
    if new_path.is_some_and(|new| folder_of(new).eq_ignore_ascii_case(old_folder)) {
        return;
    }
    let dir = home.join(old_folder);
    let _ = fs::remove_file(dir.join(index_file::FILE_NAME));
    remove_empty_tree(&dir);
    remove_empty_parent(&dir);
}

/// Removes `dir` and every directory below it that is (or becomes) empty.
fn remove_empty_tree(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_tree(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Archives every live sub-file of `row` (already at its archive revision)
/// with the document: marks the rows, removes their search entries and
/// their files, and adds their touches. Returns their vault paths.
pub(super) fn archive_files(
    conn: &Connection,
    home: &Path,
    row: &DocumentRow,
    actor: &Actor,
    journal: &mut Journal,
    touched: &mut Vec<FileTouch>,
) -> Result<Vec<String>, Error> {
    if !row.multifile {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for mut file in db::live_files(conn, &row.id)? {
        archive_row(conn, row, actor, &mut file)?;
        let vault = vault_path(row, &file.path);
        journal.remove(&home.join(&vault))?;
        touched.push(FileTouch {
            path: file.path,
            main: false,
            change: FileChange::Removed,
            from: None,
        });
        paths.push(vault);
    }
    Ok(paths)
}

/// Marks a sub-file archived at `row`'s revision and drops its search
/// entries.
fn archive_row(
    conn: &Connection,
    row: &DocumentRow,
    actor: &Actor,
    file: &mut FileRow,
) -> Result<(), Error> {
    file.revision = row.revision;
    file.updated_at.clone_from(&row.updated_at);
    actor.name().clone_into(&mut file.updated_by);
    actor.client().clone_into(&mut file.client);
    file.archived_at = Some(row.updated_at.clone());
    file.archived_by = Some(actor.name().to_owned());
    db::save_file(conn, file)?;
    db::unindex_file(conn, file.id)?;
    Ok(())
}

/// Sub-files as they were at a revision: case-folded path → (path, draft).
pub(super) type FilesThen = BTreeMap<String, (String, FileDraft)>;

/// The sub-files of `row` as they were at revision `at` (whose main file
/// was at `at.path`), read from the history: case-folded path → (path,
/// draft).
pub(super) fn files_then(
    conn: &Connection,
    history: &dyn History,
    row: &DocumentRow,
    at: &RevisionRow,
) -> Result<FilesThen, Error> {
    let changes = db::revision_files(conn, &row.id)?;
    let then = folder_of(&at.path);
    let mut files = BTreeMap::new();
    for (key, (path, _)) in files_at(&changes, at.revision).files {
        let bytes = history.show(&at.commit_id, &format!("{then}/{path}.md"))?;
        let text = String::from_utf8(bytes).map_err(|_| integrity(row))?;
        let draft = FileDraft::of(&text).ok_or_else(|| integrity(row))?;
        files.insert(key, (path, draft));
    }
    Ok(files)
}

/// Refuses a change that would leave `row` with more than `max_files` live
/// files besides its main file.
pub(super) fn check_file_limit(
    row: &DocumentRow,
    files: usize,
    max_files: u64,
) -> Result<(), Error> {
    if files as u64 > max_files {
        return Err(Error::invalid(
            "documents.max_files",
            format!(
                "{} would have {files} files besides its main file, over the limit of {max_files}",
                row.id
            ),
        ));
    }
    Ok(())
}

/// What a folder change works on: the transaction, the vault, the
/// settings, the document (at its new revision) and who changes it.
pub(super) struct Scope<'a> {
    pub(super) conn: &'a Connection,
    pub(super) home: &'a Path,
    pub(super) settings: &'a Settings,
    pub(super) row: &'a DocumentRow,
    pub(super) actor: &'a Actor,
}

/// The file changes gathered for one commit: the undoable writes, the
/// files' touches and the vault paths changed.
pub(super) struct Pending {
    pub(super) journal: Journal,
    pub(super) touched: Vec<FileTouch>,
    pub(super) paths: Vec<String>,
}

impl Pending {
    fn touch(&mut self, path: String, change: FileChange, from: Option<String>) {
        self.touched.push(FileTouch {
            path,
            main: false,
            change,
            from,
        });
    }
}

/// Writes `files` into the folder as new sub-files at the document's
/// revision.
pub(super) fn add_files(
    scope: &Scope<'_>,
    files: Vec<(String, FileDraft)>,
    pending: &mut Pending,
) -> Result<(), Error> {
    let row = scope.row;
    for (path, draft) in files {
        let text = render(row, &row.updated_at, &draft, scope.settings)?;
        let checked = FilePath::parse(&path, main_stem(row)).map_err(|_| integrity(row))?;
        let mut file = new_row(row, &checked);
        row.updated_at.clone_into(&mut file.created_at);
        scope.actor.name().clone_into(&mut file.created_by);
        save(scope.conn, row, scope.actor, &mut file, &text)?;
        let vault = vault_path(row, &file.path);
        super::vault::refuse_unknown(scope.home, &vault)?;
        pending
            .journal
            .write(&scope.home.join(&vault), text.as_bytes())?;
        pending.touch(file.path, FileChange::Added, None);
        pending.paths.push(vault);
    }
    Ok(())
}

/// Brings each live sub-file to its state in `wanted` (taking it out of
/// `wanted`), or archives it when it is not there. Returns the vault paths
/// of the archived files.
fn restore_live(
    scope: &Scope<'_>,
    live: Vec<FileRow>,
    wanted: &mut BTreeMap<String, (String, FileDraft)>,
    pending: &mut Pending,
) -> Result<Vec<String>, Error> {
    let (row, home) = (scope.row, scope.home);
    let mut removed = Vec::new();
    for mut file in live {
        let vault = vault_path(row, &file.path);
        let Some((path, draft)) = wanted.remove(&file.path.to_lowercase()) else {
            archive_row(scope.conn, row, scope.actor, &mut file)?;
            pending.journal.remove(&home.join(&vault))?;
            pending.touch(file.path.clone(), FileChange::Removed, None);
            pending.paths.push(vault.clone());
            removed.push(vault);
            continue;
        };
        let text = read_verified_file(home, row, &file)?;
        if path == file.path && FileDraft::of(&text).as_ref() == Some(&draft) {
            continue;
        }
        let from = (path != file.path).then(|| std::mem::replace(&mut file.path, path));
        let rendered = render(row, &row.updated_at, &draft, scope.settings)?;
        save(scope.conn, row, scope.actor, &mut file, &rendered)?;
        let new_vault = vault_path(row, &file.path);
        if from.is_some() {
            pending
                .journal
                .rename(&home.join(&vault), &home.join(&new_vault))?;
        }
        pending
            .journal
            .write(&home.join(&new_vault), rendered.as_bytes())?;
        let change = if from.is_some() {
            FileChange::Renamed
        } else {
            FileChange::Modified
        };
        pending.touch(file.path.clone(), change, from);
        pending.paths.push(vault);
        pending.paths.push(new_vault);
    }
    Ok(removed)
}

impl Store {
    /// Removes one sub-file of a multi-file document from the vault as a
    /// new revision. Its history stays readable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a single-file document, a bad path or
    /// the main file, [`Error::FileNotFound`], [`Error::FileConflict`] when
    /// the file changed after `expected_revision`, [`Error::Integrity`]
    /// when a file of the document was changed by hand, and storage errors.
    pub fn archive_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        expected_revision: Option<u64>,
        reason: Option<&str>,
    ) -> Result<ArchivedFile, Error> {
        let id = parse_id(id)?;
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
        require_multifile(&row, "file")?;
        let path = parse_path(&row, file, "file")?;
        let live = db::live_files(&tx, &row.id)?;
        let mut target = live
            .iter()
            .find(|f| f.path.to_lowercase() == path.key())
            .cloned()
            .ok_or_else(|| Error::FileNotFound {
                id: row.id.clone(),
                file: path.as_str(),
                at: String::new(),
            })?;
        check_file_revision(&row, &target, expected_revision)?;
        verify_all(&paths.home, &row, &live)?;

        bump(&tx, &mut row, actor, &now)?;
        archive_row(&tx, &row, actor, &mut target)?;
        let vault = vault_path(&row, &target.path);
        let mut journal = Journal::default();
        journal.remove(&paths.home.join(&vault))?;
        let touched = [FileTouch {
            path: target.path.clone(),
            main: false,
            change: FileChange::Removed,
            from: None,
        }];
        let summary = match tidy_line(reason) {
            Some(reason) => format!("{}: {reason}", target.path),
            None => target.path.clone(),
        };
        let commit = Change {
            row: &row,
            at,
            action: Action::ArchiveFile,
            summary: &summary,
            actor,
            paths: &[&vault],
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        remove_empty_dirs(&paths.home, &row, &vault);
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(ArchivedFile {
            id: row.id,
            revision: row.revision,
            file: target.path,
            path: vault,
            archived_at: now,
        })
    }

    /// Restores a multi-file document's folder as it was at `version`: the
    /// main file's body, status, tags and custom keys (its title, type and
    /// aliases stay current), every sub-file that existed then with its
    /// content and path, and archives the sub-files made since. One new
    /// revision.
    pub(super) fn restore_folder(
        &mut self,
        actor: &Actor,
        id: &str,
        version: u64,
        expected_revision: Option<u64>,
        summary: Option<String>,
    ) -> Result<Changed, Error> {
        let id = parse_id(id)?;
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
        if let Some(expected) = expected_revision {
            check_expected(&row, expected, Action::Restore)?;
        }
        let then = revision_with_content(&tx, &row, version, "version")?;
        let live = db::live_files(&tx, &row.id)?;
        let current = read_verified(&paths.home, &row)?;
        verify_all(&paths.home, &row, &live)?;
        let (status, extra, body) =
            restored_main(history.as_ref(), &row, &then, &current, settings)?;
        let mut wanted = files_then(&tx, history.as_ref(), &row, &then)?;
        if let Some((path, _)) = wanted.get(&main_stem(&row).to_lowercase()) {
            return Err(Error::AlreadyExists(format!(
                "file {path} in {}: it is named like the main file; rename the document first",
                row.id
            )));
        }
        check_file_limit(&row, wanted.len(), settings.max_files)?;
        // A file that comes back must not land on a file the index does not
        // know: where no indexed file is, nothing may be there; where its
        // name changes only in case, it must be the same file (a
        // case-sensitive filesystem can hold both spellings).
        for (key, (path, _)) in &wanted {
            match live.iter().find(|f| f.path.to_lowercase() == *key) {
                None => super::vault::refuse_unknown(&paths.home, &vault_path(&row, path))?,
                Some(file) if file.path != *path => super::vault::refuse_occupied(
                    &paths.home,
                    &vault_path(&row, &file.path),
                    &vault_path(&row, path),
                )?,
                Some(_) => {}
            }
        }

        let main_path = row.path.clone();
        row.status = status;
        row.revision += 1;
        row.main_revision = row.revision;
        now.clone_into(&mut row.updated_at);
        actor.name().clone_into(&mut row.updated_by);
        actor.client().clone_into(&mut row.client);
        let (row, fields, journal) = write_revision(
            &tx,
            &paths.home,
            settings,
            row,
            &extra,
            &body,
            Some(&main_path),
        )?;
        let mut pending = Pending {
            journal,
            touched: super::documents::main_touch(&row, FileChange::Modified),
            paths: vec![row.path.clone()],
        };
        let scope = Scope {
            conn: &tx,
            home: &paths.home,
            settings,
            row: &row,
            actor,
        };
        let removed = restore_live(&scope, live, &mut wanted, &mut pending)?;
        add_files(&scope, wanted.into_values().collect(), &mut pending)?;
        let Pending {
            journal,
            touched,
            paths: changed,
        } = pending;
        let summary = summary.unwrap_or_else(|| format!("revision {version}"));
        let commit_paths: Vec<&str> = changed.iter().map(String::as_str).collect();
        let commit = Change {
            row: &row,
            at,
            action: Action::Restore,
            summary: &summary,
            actor,
            paths: &commit_paths,
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        for vault in &removed {
            remove_empty_dirs(&paths.home, &row, vault);
        }
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(Changed {
            id: row.id.clone(),
            revision: row.revision,
            path: row.path.clone(),
            file: None,
            front_matter: fields,
        })
    }
}

/// The main file's status, extra keys and body as restored from revision
/// `then`, keeping the current aliases (as `restore_version` does).
fn restored_main(
    history: &dyn History,
    row: &DocumentRow,
    then: &RevisionRow,
    current: &str,
    settings: &Settings,
) -> Result<(String, Fields, String), Error> {
    let bytes = history.show(&then.commit_id, &then.path)?;
    let text = String::from_utf8(bytes).map_err(|_| integrity(row))?;
    let parsed = front_matter::parse(&text).map_err(|_| integrity(row))?;
    let now = front_matter::parse(current).map_err(|_| integrity(row))?;
    let mut extra = front_matter::extras(&parsed.fields);
    match now.fields.get("aliases") {
        Some(aliases) => {
            extra.insert("aliases".to_owned(), aliases.clone());
        }
        None => {
            extra.shift_remove("aliases");
        }
    }
    let status = parsed
        .fields
        .get("status")
        .and_then(serde_json::Value::as_str)
        .filter(|s| settings.statuses.iter().any(|allowed| allowed == s))
        .map_or_else(|| row.status.clone(), str::to_owned);
    Ok((status, extra, parsed.body.to_owned()))
}
