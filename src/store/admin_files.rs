//! Administrator-only file operations (never MCP tools): converting a
//! single-file document into a multi-file one, moving a file to another
//! multi-file document, and restoring one file as it was at an earlier
//! revision.

use std::fmt::Write as _;

use rusqlite::Transaction;

use super::documents::{
    Action, Change, Changed, FILE_TRAILER, FileTouch, Layout, choose_path, finish, integrity,
    live_row, main_touch, parse_id, read_verified, revision_with_content, write_revision,
};
use super::file_changes::{
    FileDraft, bump, new_row, parse_path, remove_empty_dirs, render, require_multifile, save,
    verify_all,
};
use super::files::{files_at, main_stem, read_verified_file, vault_path};
use super::folders::clean_after_move;
use super::journal::Journal;
use super::{Actor, Error, Store, begin_write, index_file};
use crate::db::{self, DocumentRow, FileChange, FileRow, RevisionFileRow, RevisionRow};
use crate::document::front_matter;
use crate::history::{Commit, History};

/// The trailer naming the document a file moved to.
pub(super) const TARGET_DOCUMENT: &str = "JoeDocs-Target-Document";
/// The trailer giving the target document's new revision.
pub(super) const TARGET_REVISION: &str = "JoeDocs-Target-Revision";
/// The trailer giving the file's path in the target document.
pub(super) const TARGET_FILE: &str = "JoeDocs-Target-File";

/// The result of [`Store::move_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedFile {
    /// The document the file left.
    pub source: String,
    /// Its new revision.
    pub source_revision: u64,
    /// The document the file joined.
    pub target: String,
    /// Its new revision.
    pub target_revision: u64,
    /// The file's path in the target document, without `.md`.
    pub file: String,
    /// Its vault path.
    pub path: String,
}

impl Store {
    /// Turns a single-file document into a multi-file one: its file moves
    /// from `<type>/<stem>.md` to `<type>/<stem>/<stem>.md` as one new
    /// revision, keeping its ID, history, front matter and body.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a document that is already
    /// multi-file, [`Error::AlreadyExists`] when no folder name is free,
    /// [`Error::Integrity`] when its file was changed by hand, and storage
    /// errors.
    pub fn convert(&mut self, actor: &Actor, id: &str) -> Result<Changed, Error> {
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
        if row.multifile {
            return Err(Error::invalid(
                "document_id",
                format!("{} is already a multi-file document", row.id),
            ));
        }
        let text = read_verified(&paths.home, &row)?;
        let parsed = front_matter::parse(&text).map_err(|_| integrity(&row))?;
        let extra = front_matter::extras(&parsed.fields);
        let old_path = row.path.clone();
        row.path = choose_path(
            &tx,
            &paths.home,
            &row.project,
            &row.doc_type,
            &row.title,
            &row.id,
            None,
            &Layout::Folder(&[]),
        )?;
        row.multifile = true;
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
            parsed.body,
            Some(&old_path),
        )?;
        // Its file history covers the revisions before it became multi-file.
        let mut earlier = db::list_revisions(&tx, &row.id, 0, row.revision)?;
        earlier.reverse();
        for change in super::files::backfill_main(&earlier) {
            db::insert_revision_file(&tx, &change)?;
        }
        let touched = main_touch(&row, FileChange::Added);
        let commit = Change {
            row: &row,
            at,
            action: Action::Convert,
            summary: "single file → folder",
            actor,
            paths: &[&old_path, &row.path],
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        clean_after_move(&paths.home, &old_path, Some(&row.path));
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(Changed {
            id: row.id.clone(),
            revision: row.revision,
            path: row.path.clone(),
            file: None,
            front_matter: fields,
        })
    }

    /// Moves a sub-file to another multi-file document, as `as_file` or
    /// under its own path, in one commit that raises both documents'
    /// revisions. The file's front matter then names the target.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when either document is single-file, they
    /// are the same, the path is bad or the main file's, or the target has
    /// no room; [`Error::FileNotFound`]; [`Error::AlreadyExists`] when the
    /// path is taken in the target; [`Error::Integrity`] when a file of
    /// either document was changed by hand; and storage errors.
    pub fn move_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        to: &str,
        as_file: Option<&str>,
    ) -> Result<MovedFile, Error> {
        let source_id = parse_id(id)?;
        let target_id = parse_id(to).map_err(|_| {
            Error::invalid("to", format!("\"{to}\" is not a document ID like JD-42"))
        })?;
        if source_id == target_id {
            return Err(Error::invalid(
                "to",
                "is the same document; rename the file within it instead",
            ));
        }
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
        let mut source = live_row(&tx, &source_id)?;
        require_multifile(&source, "document_id")?;
        let mut target = live_row(&tx, &target_id)?;
        if !target.multifile {
            return Err(Error::invalid(
                "to",
                format!("{} is a single-file document", target.id),
            ));
        }
        let path = parse_path(&source, file, "file")?;
        let source_live = db::live_files(&tx, &source.id)?;
        let mut moving = source_live
            .iter()
            .find(|f| f.path.to_lowercase() == path.key())
            .cloned()
            .ok_or_else(|| Error::FileNotFound {
                id: source.id.clone(),
                file: path.as_str(),
                at: String::new(),
            })?;
        let new_path = parse_path(&target, as_file.unwrap_or(&moving.path), "as")?;
        let target_live = db::live_files(&tx, &target.id)?;
        if target_live
            .iter()
            .any(|f| f.path.to_lowercase() == new_path.key())
        {
            return Err(Error::AlreadyExists(format!(
                "file {new_path} in {}",
                target.id
            )));
        }
        let new_vault = vault_path(&target, &new_path.as_str());
        super::vault::refuse_unknown(&paths.home, &new_vault)?;
        if target_live.len() as u64 >= settings.max_files {
            return Err(Error::invalid(
                "documents.max_files",
                format!("{} already has the most files allowed", target.id),
            ));
        }
        verify_all(&paths.home, &source, &source_live)?;
        verify_all(&paths.home, &target, &target_live)?;
        let old_vault = vault_path(&source, &moving.path);
        let text = read_verified_file(&paths.home, &source, &moving)?;
        let draft = FileDraft::of(&text).ok_or_else(|| integrity(&source))?;

        bump(&tx, &mut source, actor, &now)?;
        bump(&tx, &mut target, actor, &now)?;
        let rendered = render(&target, &now, &draft, settings)?;
        let old_rel = std::mem::replace(&mut moving.path, new_path.as_str());
        target.id.clone_into(&mut moving.document_id);
        save(&tx, &target, actor, &mut moving, &rendered)?;
        let mut journal = Journal::default();
        journal.write(&paths.home.join(&new_vault), rendered.as_bytes())?;
        journal.remove(&paths.home.join(&old_vault))?;
        let moved = Move {
            source: &source,
            target: &target,
            old: &old_rel,
            new: &moving.path,
            vaults: [&old_vault, &new_vault],
        };
        finish_move(tx, history.as_ref(), journal, &moved, at, actor)?;
        remove_empty_dirs(&paths.home, &source, &old_vault);
        index_file::refresh_document(conn, &paths.home, settings, &source);
        index_file::refresh_document(conn, &paths.home, settings, &target);
        Ok(MovedFile {
            source: source.id,
            source_revision: source.revision,
            target: target.id,
            target_revision: target.revision,
            file: moving.path,
            path: new_vault,
        })
    }

    /// Brings back one sub-file as it was at document revision `version`
    /// (named by its path then), as one new revision: its content then, at
    /// its path then. It works whether the file has since changed, been
    /// renamed or been archived.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FileNotFound`] when the document had no such file
    /// then, [`Error::Invalid`] when the file is already as it was,
    /// [`Error::AlreadyExists`] when another live file holds its path then,
    /// and the errors of [`Store::update_file`].
    pub fn restore_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        version: u64,
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
        require_multifile(&row, "document_id")?;
        let RestorePlan {
            mut target,
            from,
            change,
            draft,
            path_then,
        } = plan_restore(
            &tx,
            history.as_ref(),
            &paths.home,
            settings.max_files,
            &row,
            file,
            version,
        )?;
        if change == FileChange::Added {
            now.clone_into(&mut target.created_at);
            actor.name().clone_into(&mut target.created_by);
        }
        let new_vault = vault_path(&row, &path_then);

        bump(&tx, &mut row, actor, &now)?;
        let rendered = render(&row, &now, &draft, settings)?;
        let old_vault = vault_path(&row, &target.path);
        path_then.clone_into(&mut target.path);
        save(&tx, &row, actor, &mut target, &rendered)?;
        let mut journal = Journal::default();
        if from.is_some() {
            journal.rename(&paths.home.join(&old_vault), &paths.home.join(&new_vault))?;
        }
        journal.write(&paths.home.join(&new_vault), rendered.as_bytes())?;
        let touched = [FileTouch {
            path: target.path.clone(),
            main: false,
            change,
            from,
        }];
        let summary = format!("{} to revision {version}", target.path);
        let commit = Change {
            row: &row,
            at,
            action: Action::RestoreFile,
            summary: &summary,
            actor,
            paths: &[&old_vault, &new_vault],
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        if old_vault != new_vault {
            remove_empty_dirs(&paths.home, &row, &old_vault);
        }
        let fields = front_matter::parse(&rendered)
            .map(|p| p.fields)
            .unwrap_or_default();
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(Changed {
            id: row.id,
            revision: row.revision,
            path: new_vault,
            file: Some(target.path),
            front_matter: fields,
        })
    }
}

/// What [`Store::restore_file`] will write: the file's row (a new one, ID
/// 0, when it was archived), its old path when it moves back, the change,
/// its draft then and its path then.
struct RestorePlan {
    target: FileRow,
    from: Option<String>,
    change: FileChange,
    draft: FileDraft,
    path_then: String,
}

/// Checks a single-file restore of `file` (its path at `version`) and plans
/// it, before anything is written.
fn plan_restore(
    conn: &rusqlite::Connection,
    history: &dyn History,
    home: &std::path::Path,
    max_files: u64,
    row: &DocumentRow,
    file: &str,
    version: u64,
) -> Result<RestorePlan, Error> {
    let then = revision_with_content(conn, row, version, "version")?;
    let changes = db::revision_files(conn, &row.id)?;
    // `file` is the path at `version`, which may be the main file's name
    // today; that collision is refused below, with `already_exists`.
    let path = crate::document::FilePath::parse(file, "")
        .map_err(|message| Error::invalid("file", message))?;
    let (path_then, _) = files_at(&changes, version)
        .files
        .remove(&path.key())
        .ok_or_else(|| Error::FileNotFound {
            id: row.id.clone(),
            file: path.as_str(),
            at: format!(" at revision {version}"),
        })?;
    if !path_then.contains('/') && path_then.to_lowercase() == main_stem(row).to_lowercase() {
        return Err(Error::AlreadyExists(format!(
            "file {path_then} in {}: it is named like the main file; rename the document first",
            row.id
        )));
    }
    let live = db::live_files(conn, &row.id)?;
    verify_all(home, row, &live)?;
    let draft = draft_then(history, row, &then, &path_then)?;
    let current = current_name(&changes, version, &path_then)
        .and_then(|name| {
            live.iter()
                .find(|f| f.path.to_lowercase() == name.to_lowercase())
        })
        .cloned();
    let held = live.iter().any(|f| {
        f.path.to_lowercase() == path_then.to_lowercase()
            && Some(f.id) != current.as_ref().map(|c| c.id)
    });
    if held {
        return Err(Error::AlreadyExists(format!(
            "file {path_then} in {}; it is another file now",
            row.id
        )));
    }
    let (target, from, change) = if let Some(file) = current {
        let text = read_verified_file(home, row, &file)?;
        if file.path == path_then && FileDraft::of(&text).as_ref() == Some(&draft) {
            return Err(Error::invalid(
                "version",
                format!("{path_then} is already as it was at revision {version}"),
            ));
        }
        let from = (file.path != path_then).then(|| file.path.clone());
        // Renamed back: nothing may be at that path, unless it is this very
        // file under another case on a case-insensitive filesystem.
        if from.is_some() {
            super::vault::refuse_occupied(
                home,
                &vault_path(row, &file.path),
                &vault_path(row, &path_then),
            )?;
        }
        let change = if from.is_some() {
            FileChange::Renamed
        } else {
            FileChange::Modified
        };
        (file, from, change)
    } else {
        if live.len() as u64 >= max_files {
            return Err(Error::invalid(
                "documents.max_files",
                format!("{} already has the most files allowed", row.id),
            ));
        }
        super::vault::refuse_unknown(home, &vault_path(row, &path_then))?;
        let checked = parse_path(row, &path_then, "file")?;
        (new_row(row, &checked), None, FileChange::Added)
    };
    Ok(RestorePlan {
        target,
        from,
        change,
        draft,
        path_then,
    })
}

/// A sub-file's draft as committed at revision `then` under `path`.
fn draft_then(
    history: &dyn History,
    row: &DocumentRow,
    then: &RevisionRow,
    path: &str,
) -> Result<FileDraft, Error> {
    let folder = then.path.rsplit_once('/').map_or("", |(folder, _)| folder);
    let bytes = history.show(&then.commit_id, &format!("{folder}/{path}.md"))?;
    let text = String::from_utf8(bytes).map_err(|_| integrity(row))?;
    FileDraft::of(&text).ok_or_else(|| integrity(row))
}

/// The path, now, of the sub-file that was `path` at revision `since`,
/// following its renames; `None` when it was archived since.
fn current_name(changes: &[RevisionFileRow], since: u64, path: &str) -> Option<String> {
    let mut name = path.to_lowercase();
    let mut current = path.to_owned();
    for change in changes.iter().filter(|c| c.revision > since && !c.main) {
        match change.change {
            FileChange::Renamed
                if change
                    .from_path
                    .as_deref()
                    .map(str::to_lowercase)
                    .as_deref()
                    == Some(name.as_str()) =>
            {
                change.path.clone_into(&mut current);
                name = change.path.to_lowercase();
            }
            FileChange::Removed if change.path.to_lowercase() == name => return None,
            _ => {}
        }
    }
    Some(current)
}

/// A file moving between documents, for [`finish_move`].
struct Move<'a> {
    source: &'a DocumentRow,
    target: &'a DocumentRow,
    /// Its path in the source, and in the target, without `.md`.
    old: &'a str,
    new: &'a str,
    /// Its old and new vault paths.
    vaults: [&'a str; 2],
}

/// Commits a file's move between documents as one history commit naming
/// both, then records a revision of each and commits the transaction. If
/// the history commit fails, the vault files are restored and the
/// transaction rolls back.
fn finish_move(
    tx: Transaction<'_>,
    history: &dyn History,
    journal: Journal,
    moved: &Move<'_>,
    at: jiff::Timestamp,
    actor: &Actor,
) -> Result<(), Error> {
    let Move {
        source,
        target,
        old,
        new,
        vaults,
    } = *moved;
    let subject = format!(
        "{} {} to {}: {old} → {new}",
        Action::MoveFile.verb(),
        source.id,
        target.id
    );
    let mut message = format!(
        "{subject}\n\nJoeDocs-Document: {}\nJoeDocs-Revision: {}\nJoeDocs-Action: {}\nJoeDocs-Client: {}\n",
        source.id,
        source.revision,
        Action::MoveFile.name(),
        actor.client(),
    );
    let _ = write!(
        message,
        "{FILE_TRAILER}: {old}.md\n{TARGET_DOCUMENT}: {}\n{TARGET_REVISION}: {}\n{TARGET_FILE}: {new}.md\n",
        target.id, target.revision
    );
    let commit_id = history.commit(&Commit {
        paths: vaults.iter().map(|p| (*p).to_owned()).collect(),
        author: actor.name().to_owned(),
        at,
        message,
    })?;
    journal.complete();
    for (row, path, change) in [
        (source, old, FileChange::Removed),
        (target, new, FileChange::Added),
    ] {
        db::insert_revision(
            &tx,
            &RevisionRow {
                document_id: row.id.clone(),
                revision: row.revision,
                commit_id: commit_id.clone(),
                path: row.path.clone(),
                action: Action::MoveFile.name().to_owned(),
                author: actor.name().to_owned(),
                client: actor.client().to_owned(),
                summary: subject.clone(),
                created_at: row.updated_at.clone(),
            },
        )?;
        db::insert_revision_file(
            &tx,
            &RevisionFileRow {
                document_id: row.id.clone(),
                revision: row.revision,
                path: path.to_owned(),
                main: false,
                change,
                from_path: None,
            },
        )?;
    }
    db::set_history_head(&tx, &commit_id)?;
    tx.commit()?;
    Ok(())
}
