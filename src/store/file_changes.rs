//! Changing the sub-files of multi-file documents: replacing, editing,
//! appending to, changing the front matter of and renaming one file, and
//! writing several files in one revision.
//!
//! A change to a file is refused with [`Error::FileConflict`] when that file
//! changed after the revision the client read (or the client's revision is
//! ahead of the document), so agents changing different files of one
//! document do not conflict. Every change checks every file of the
//! document against the index first, and changes nothing on any error.

use std::fs;
use std::path::Path;

use super::documents::{
    Action, Change, Changed, FileTouch, finish, live_row, parse_id, read_verified, sha256,
    tidy_line,
};
use super::editing::{MAX_EDITS, Replacement, appended, apply_edits};
use super::files::{folder, main_stem, read_verified_file, vault_path};
use super::journal::Journal;
use super::{Actor, Error, Settings, Store, begin_write, index_file};
use crate::db::{self, Connection, DocumentRow, FileChange, FileRow};
use crate::document::FilePath;
use crate::document::front_matter::{self, Fields, FileHeader};

/// One file to create or replace with [`Store::write_files`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileWrite {
    /// Path inside the document folder, with or without `.md`.
    pub file: String,
    /// The file's new Markdown body.
    pub content: String,
    /// Front matter keys to set: for a new file its keys, for an existing
    /// one keys merged into its front matter (as `update_document` does).
    pub front_matter: Fields,
}

/// A request to write several files of a multi-file document at once.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteFiles {
    /// Document ID.
    pub document_id: String,
    /// The files, each path at most once.
    pub files: Vec<FileWrite>,
    /// The document revision the client last read, if it gives one.
    pub expected_revision: Option<u64>,
    /// Commit summary.
    pub summary: Option<String>,
}

/// One file [`Store::write_files`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenFile {
    /// Path inside the document folder, without `.md`.
    pub file: String,
    /// Vault-relative path.
    pub path: String,
    /// Whether the file is new.
    pub created: bool,
}

/// The result of [`Store::write_files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenFiles {
    /// Document ID.
    pub id: String,
    /// The new revision.
    pub revision: u64,
    /// The files that changed, in the order given; files whose content was
    /// already the same are left out.
    pub files: Vec<WrittenFile>,
}

/// The editable parts of a sub-file during a change.
#[derive(Debug, Clone, PartialEq)]
struct FileDraft {
    extra: Fields,
    body: String,
}

impl FileDraft {
    fn of(text: &str) -> Option<Self> {
        let parsed = front_matter::parse(text).ok()?;
        Some(Self {
            extra: front_matter::file_extras(&parsed.fields),
            body: parsed.body.to_owned(),
        })
    }

    fn set(&mut self, set: &Fields, remove: &[String], field: &'static str) -> Result<(), Error> {
        front_matter::apply_file(&mut self.extra, set, remove)
            .map_err(|e| Error::invalid(field, e.0))
    }
}

/// Refuses a file change against `expected` when the file changed after it
/// or it is ahead of the document.
fn check_file_revision(
    row: &DocumentRow,
    file: &FileRow,
    expected: Option<u64>,
) -> Result<(), Error> {
    match expected {
        Some(expected) if file.revision > expected || expected > row.revision => {
            Err(Error::FileConflict {
                id: row.id.clone(),
                file: file.path.clone(),
                expected,
                file_revision: file.revision,
                current: row.revision,
            })
        }
        _ => Ok(()),
    }
}

/// Requires a multi-file document, naming `field` when it is not.
fn require_multifile(row: &DocumentRow, field: &'static str) -> Result<(), Error> {
    if row.multifile {
        Ok(())
    } else {
        Err(Error::invalid(
            field,
            format!(
                "{} is a single-file document; leave out file, or ask an administrator to make it multi-file with `joedocs document convert {}`",
                row.id, row.id
            ),
        ))
    }
}

fn parse_path(row: &DocumentRow, file: &str, field: &'static str) -> Result<FilePath, Error> {
    FilePath::parse(file, main_stem(row)).map_err(|message| Error::invalid(field, message))
}

/// Checks the main file and every live sub-file of `row` against the index.
fn verify_all(home: &Path, row: &DocumentRow, live: &[FileRow]) -> Result<(), Error> {
    read_verified(home, row)?;
    for file in live {
        read_verified_file(home, row, file)?;
    }
    Ok(())
}

/// Renders a sub-file at `row`'s revision and checks its size.
fn render(
    row: &DocumentRow,
    now: &str,
    draft: &FileDraft,
    settings: &Settings,
) -> Result<String, Error> {
    let header = FileHeader {
        document: row.id.clone(),
        revision: row.revision,
        updated: now.to_owned(),
    };
    let text = front_matter::render_file(&header, &draft.extra, &draft.body)
        .map_err(|e| Error::invalid("front_matter", e.0))?;
    let size = text.len() as u64;
    if size > settings.max_document_bytes {
        return Err(Error::TooLarge {
            size,
            limit: settings.max_document_bytes,
        });
    }
    Ok(text)
}

/// Records `file` as written at `row`'s revision with `text`, and replaces
/// its search entries. Inserts it when its `id` is 0.
fn save(
    conn: &Connection,
    row: &DocumentRow,
    actor: &Actor,
    file: &mut FileRow,
    text: &str,
) -> Result<Fields, Error> {
    file.revision = row.revision;
    file.updated_at.clone_from(&row.updated_at);
    actor.name().clone_into(&mut file.updated_by);
    actor.client().clone_into(&mut file.client);
    file.content_sha256 = sha256(text.as_bytes());
    if file.id == 0 {
        file.id = db::insert_file(conn, file)?;
    } else {
        db::save_file(conn, file)?;
    }
    let parsed = front_matter::parse(text).map_err(|e| Error::invalid("front_matter", e.0))?;
    db::index_file(conn, file, &parsed.fields, parsed.body)?;
    Ok(parsed.fields)
}

/// Raises `row` to a new revision by `actor` at `now`, without touching its
/// main file.
fn bump(conn: &Connection, row: &mut DocumentRow, actor: &Actor, now: &str) -> Result<(), Error> {
    row.revision += 1;
    now.clone_into(&mut row.updated_at);
    actor.name().clone_into(&mut row.updated_by);
    actor.client().clone_into(&mut row.client);
    db::save_document(conn, row)?;
    Ok(())
}

/// Removes the empty directories left above `file` (vault-relative), up to
/// the document folder.
fn remove_empty_dirs(home: &Path, row: &DocumentRow, file: &str) {
    let top = home.join(folder(row));
    let mut dir = home.join(file).parent().map(Path::to_path_buf);
    while let Some(path) = dir {
        if path == top || !path.starts_with(&top) || fs::remove_dir(&path).is_err() {
            return;
        }
        dir = path.parent().map(Path::to_path_buf);
    }
}

/// A one-file change for [`Store::revise_file`].
struct FileEdit<'a> {
    id: &'a str,
    file: &'a str,
    expected: Option<u64>,
    action: Action,
    summary: Option<String>,
    /// The new path, for a rename.
    new_file: Option<&'a str>,
}

impl Store {
    /// Whether `file` names the main file of multi-file document `id`, so a
    /// tool given `file` acts on the main file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a single-file document, and the errors
    /// of [`Store::get`].
    pub fn names_main_file(&self, id: &str, file: &str) -> Result<bool, Error> {
        let row = live_row(&self.conn, &parse_id(id)?)?;
        require_multifile(&row, "file")?;
        let name = file
            .strip_suffix(".md")
            .or_else(|| file.strip_suffix(".MD"))
            .unwrap_or(file);
        Ok(name.to_lowercase() == main_stem(&row).to_lowercase())
    }

    /// Replaces a sub-file's body and sets front matter keys.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when neither would change,
    /// [`Error::FileConflict`], [`Error::FileNotFound`], and the errors of
    /// [`Store::update`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the arguments of update_document"
    )]
    pub fn update_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        content: &str,
        front_matter: &Fields,
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        let edit = FileEdit {
            id,
            file,
            expected: expected_revision,
            action: Action::Update,
            summary: tidy_line(summary),
            new_file: None,
        };
        self.revise_file(actor, &edit, |draft| {
            let before = draft.clone();
            draft.set(front_matter, &[], "front_matter")?;
            content.clone_into(&mut draft.body);
            unchanged(
                draft,
                &before,
                "content",
                "the content and front matter are unchanged",
            )
        })
    }

    /// Applies exact replacements to a sub-file's body, as
    /// [`Store::edit`] does to a document's.
    ///
    /// # Errors
    ///
    /// As [`Store::edit`] and [`Store::update_file`].
    pub fn edit_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        edits: &[Replacement],
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        if edits.is_empty() || edits.len() > MAX_EDITS {
            return Err(Error::invalid(
                "edits",
                format!("give 1 to {MAX_EDITS} edits"),
            ));
        }
        let edit = FileEdit {
            id,
            file,
            expected: expected_revision,
            action: Action::Edit,
            summary: tidy_line(summary),
            new_file: None,
        };
        self.revise_file(actor, &edit, |draft| {
            let body = apply_edits(&draft.body, edits)?;
            if body == draft.body {
                return Err(Error::invalid(
                    "edits",
                    "the edits leave the body unchanged",
                ));
            }
            draft.body = body;
            Ok(())
        })
    }

    /// Adds `content` to the end of a sub-file's body, after a blank line.
    ///
    /// # Errors
    ///
    /// As [`Store::append`] and [`Store::update_file`].
    pub fn append_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        content: &str,
        expected_revision: Option<u64>,
        summary: Option<&str>,
    ) -> Result<Changed, Error> {
        if content.trim().is_empty() {
            return Err(Error::invalid("content", "must not be empty"));
        }
        let edit = FileEdit {
            id,
            file,
            expected: expected_revision,
            action: Action::Append,
            summary: tidy_line(summary),
            new_file: None,
        };
        self.revise_file(actor, &edit, |draft| {
            draft.body = appended(&draft.body, content);
            Ok(())
        })
    }

    /// Sets and removes a sub-file's front matter keys.
    ///
    /// # Errors
    ///
    /// As [`Store::change_front_matter`] and [`Store::update_file`].
    pub fn change_file_front_matter(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        set: &Fields,
        remove: &[String],
        expected_revision: Option<u64>,
    ) -> Result<Changed, Error> {
        if set.is_empty() && remove.is_empty() {
            return Err(Error::invalid(
                "front_matter",
                "nothing to change; give keys to set or remove",
            ));
        }
        let edit = FileEdit {
            id,
            file,
            expected: expected_revision,
            action: Action::FrontMatter,
            summary: None,
            new_file: None,
        };
        self.revise_file(actor, &edit, |draft| {
            let before = draft.clone();
            draft.set(set, remove, "front_matter")?;
            unchanged(
                draft,
                &before,
                "front_matter",
                "nothing would change; the keys already have these values",
            )
        })
    }

    /// Moves a sub-file to `new_file` within its document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad or unchanged path,
    /// [`Error::AlreadyExists`] when it is taken, and the errors of
    /// [`Store::update_file`].
    pub fn rename_file(
        &mut self,
        actor: &Actor,
        id: &str,
        file: &str,
        new_file: &str,
        expected_revision: Option<u64>,
    ) -> Result<Changed, Error> {
        let edit = FileEdit {
            id,
            file,
            expected: expected_revision,
            action: Action::Rename,
            summary: None,
            new_file: Some(new_file),
        };
        self.revise_file(actor, &edit, |_| Ok(()))
    }

    /// Writes a new revision of one sub-file: `edit` changes its draft (and
    /// `request.new_file` moves it), then the file is rewritten with its new
    /// revision in its front matter.
    fn revise_file(
        &mut self,
        actor: &Actor,
        request: &FileEdit<'_>,
        edit: impl FnOnce(&mut FileDraft) -> Result<(), Error>,
    ) -> Result<Changed, Error> {
        let id = parse_id(request.id)?;
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
        let path = parse_path(&row, request.file, "file")?;
        let live = db::live_files(&tx, &row.id)?;
        let mut file = live
            .iter()
            .find(|f| f.path.to_lowercase() == path.key())
            .cloned()
            .ok_or_else(|| Error::FileNotFound {
                id: row.id.clone(),
                file: path.as_str(),
                at: String::new(),
            })?;
        check_file_revision(&row, &file, request.expected)?;
        verify_all(&paths.home, &row, &live)?;
        let old_vault = vault_path(&row, &file.path);
        let text = read_verified_file(&paths.home, &row, &file)?;
        let mut draft = FileDraft::of(&text).ok_or_else(|| Error::Integrity {
            id: row.id.clone(),
            path: old_vault.clone(),
        })?;
        edit(&mut draft)?;
        let from = match request.new_file {
            Some(new_file) => Some(rename_to(&paths.home, &row, &live, &mut file, new_file)?),
            None => None,
        };

        bump(&tx, &mut row, actor, &now)?;
        let text = render(&row, &now, &draft, settings)?;
        let fields = save(&tx, &row, actor, &mut file, &text)?;
        let new_vault = vault_path(&row, &file.path);
        let mut journal = Journal::default();
        write_moved(&mut journal, &paths.home, &old_vault, &new_vault, &text)?;
        let touched = [FileTouch {
            path: file.path.clone(),
            main: false,
            change: if from.is_some() {
                FileChange::Renamed
            } else {
                FileChange::Modified
            },
            from: from.clone(),
        }];
        let summary = request.summary.clone().unwrap_or_else(|| match &from {
            Some(from) => format!("{from} → {}", file.path),
            None => file.path.clone(),
        });
        let commit = Change {
            row: &row,
            at,
            action: request.action,
            summary: &summary,
            actor,
            paths: &[&old_vault, &new_vault],
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        if old_vault != new_vault {
            remove_empty_dirs(&paths.home, &row, &old_vault);
        }
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(Changed {
            id: row.id,
            revision: row.revision,
            path: new_vault,
            file: Some(file.path),
            front_matter: fields,
        })
    }

    /// Creates or replaces several sub-files of a multi-file document in
    /// one revision and one commit. Every path, limit, size and conflict is
    /// checked before anything is written; on any error nothing changes. A
    /// file given with the content and front matter it already has is not a
    /// change, and a call that changes no file is refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] (naming the entry) for a bad path, the
    /// main file, a path given twice, bad front matter, too many files or no
    /// change; [`Error::FileConflict`] and [`Error::Conflict`] for a stale
    /// or future `expected_revision`; [`Error::AlreadyExists`] for a path
    /// taken by a file the index does not know; and the errors of
    /// [`Store::update`].
    pub fn write_files(
        &mut self,
        actor: &Actor,
        request: &WriteFiles,
    ) -> Result<WrittenFiles, Error> {
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
        require_multifile(&row, "document_id")?;
        let live = db::live_files(&tx, &row.id)?;
        let planned = plan_writes(&paths.home, settings, &row, &live, request)?;
        verify_all(&paths.home, &row, &live)?;

        bump(&tx, &mut row, actor, &now)?;
        let mut rendered = Vec::with_capacity(planned.len());
        for (file, draft) in planned {
            let text = render(&row, &now, &draft, settings)?;
            rendered.push((file, text));
        }
        let mut journal = Journal::default();
        let mut written = Vec::with_capacity(rendered.len());
        let mut touched = Vec::with_capacity(rendered.len());
        let mut vaults = Vec::with_capacity(rendered.len());
        for (mut file, text) in rendered {
            let created = file.id == 0;
            if created {
                now.clone_into(&mut file.created_at);
                actor.name().clone_into(&mut file.created_by);
            }
            save(&tx, &row, actor, &mut file, &text)?;
            let vault = vault_path(&row, &file.path);
            journal.write(&paths.home.join(&vault), text.as_bytes())?;
            touched.push(FileTouch {
                path: file.path.clone(),
                main: false,
                change: if created {
                    FileChange::Added
                } else {
                    FileChange::Modified
                },
                from: None,
            });
            written.push(WrittenFile {
                file: file.path,
                path: vault.clone(),
                created,
            });
            vaults.push(vault);
        }
        let summary =
            tidy_line(request.summary.as_deref()).unwrap_or_else(|| match written.len() {
                1 => "1 file".to_owned(),
                n => format!("{n} files"),
            });
        let changed: Vec<&str> = vaults.iter().map(String::as_str).collect();
        let commit = Change {
            row: &row,
            at,
            action: Action::WriteFiles,
            summary: &summary,
            actor,
            paths: &changed,
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(WrittenFiles {
            id: row.id,
            revision: row.revision,
            files: written,
        })
    }
}

/// Refuses a draft that did not change.
fn unchanged(
    draft: &FileDraft,
    before: &FileDraft,
    field: &'static str,
    message: &'static str,
) -> Result<(), Error> {
    if draft == before {
        Err(Error::invalid(field, message))
    } else {
        Ok(())
    }
}

/// Checks `new_file` as `file`'s new path and moves `file` to it, returning
/// the old path.
fn rename_to(
    home: &Path,
    row: &DocumentRow,
    live: &[FileRow],
    file: &mut FileRow,
    new_file: &str,
) -> Result<String, Error> {
    let path = parse_path(row, new_file, "new_file")?;
    if path.as_str() == file.path {
        return Err(Error::invalid("new_file", "is already the file's path"));
    }
    if live
        .iter()
        .any(|f| f.id != file.id && f.path.to_lowercase() == path.key())
    {
        return Err(Error::AlreadyExists(format!("file {path} in {}", row.id)));
    }
    let case_only = path.key() == file.path.to_lowercase();
    let vault = vault_path(row, &path.as_str());
    if !case_only && home.join(&vault).exists() {
        return Err(Error::AlreadyExists(format!(
            "a file the index does not know at {vault}"
        )));
    }
    Ok(std::mem::replace(&mut file.path, path.as_str()))
}

/// Writes `text` at `new` (vault-relative), moving it from `old` when they
/// differ, as `write_revision` does for main files.
fn write_moved(
    journal: &mut Journal,
    home: &Path,
    old: &str,
    new: &str,
    text: &str,
) -> Result<(), Error> {
    let new_file = home.join(new);
    if old == new {
        return journal.write(&new_file, text.as_bytes());
    }
    let old_file = home.join(old);
    if old.eq_ignore_ascii_case(new) {
        journal.rename(&old_file, &new_file)?;
        journal.write(&new_file, text.as_bytes())
    } else {
        journal.write(&new_file, text.as_bytes())?;
        journal.remove(&old_file)
    }
}

/// Checks every entry of a [`Store::write_files`] request and returns the
/// files that change with their new drafts, in the order given. New files
/// have row ID 0.
fn plan_writes(
    home: &Path,
    settings: &Settings,
    row: &DocumentRow,
    live: &[FileRow],
    request: &WriteFiles,
) -> Result<Vec<(FileRow, FileDraft)>, Error> {
    if request.files.is_empty() || request.files.len() as u64 > settings.max_files {
        return Err(Error::invalid(
            "files",
            format!("give 1 to {} files", settings.max_files),
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
    let mut seen = std::collections::HashSet::new();
    let mut planned = Vec::new();
    let mut new_files = 0_u64;
    for (index, entry) in request.files.iter().enumerate() {
        let n = index + 1;
        let entry_error = |message: String| Error::invalid("files", format!("file {n}: {message}"));
        let stem = entry
            .file
            .strip_suffix(".md")
            .or_else(|| entry.file.strip_suffix(".MD"))
            .unwrap_or(&entry.file);
        if stem.to_lowercase() == main_stem(row).to_lowercase() {
            return Err(entry_error(
                "is the main file; change it with update_document".into(),
            ));
        }
        let path = FilePath::parse(&entry.file, main_stem(row)).map_err(entry_error)?;
        if !seen.insert(path.key()) {
            return Err(entry_error(format!("{path} is given twice")));
        }
        let existing = live.iter().find(|f| f.path.to_lowercase() == path.key());
        let (file, mut draft) = if let Some(existing) = existing {
            check_file_revision(row, existing, request.expected_revision)?;
            let text = read_verified_file(home, row, existing)?;
            let draft = FileDraft::of(&text).ok_or_else(|| Error::Integrity {
                id: row.id.clone(),
                path: vault_path(row, &existing.path),
            })?;
            (existing.clone(), draft)
        } else {
            let vault = vault_path(row, &path.as_str());
            if home.join(&vault).exists() {
                return Err(Error::AlreadyExists(format!(
                    "a file the index does not know at {vault}"
                )));
            }
            new_files += 1;
            let draft = FileDraft {
                extra: Fields::new(),
                body: String::new(),
            };
            (new_row(row, &path), draft)
        };
        let before = (file.id != 0).then(|| draft.clone());
        front_matter::apply_file(&mut draft.extra, &entry.front_matter, &[])
            .map_err(|e| entry_error(e.0))?;
        entry.content.clone_into(&mut draft.body);
        if before.as_ref() != Some(&draft) {
            planned.push((file, draft));
        }
    }
    if live.len() as u64 + new_files > settings.max_files {
        return Err(Error::invalid(
            "documents.max_files",
            format!(
                "{} has {} files besides its main file; {new_files} more would pass the limit of {}",
                row.id,
                live.len(),
                settings.max_files
            ),
        ));
    }
    if planned.is_empty() {
        return Err(Error::invalid(
            "files",
            "nothing would change; every file already has this content and front matter",
        ));
    }
    Ok(planned)
}

/// A row for a new sub-file of `row` at `path`, filled in by [`save`].
fn new_row(row: &DocumentRow, path: &FilePath) -> FileRow {
    FileRow {
        id: 0,
        document_id: row.id.clone(),
        path: path.as_str(),
        revision: 0,
        created_at: String::new(),
        created_by: String::new(),
        updated_at: String::new(),
        updated_by: String::new(),
        client: String::new(),
        content_sha256: Vec::new(),
        archived_at: None,
        archived_by: None,
    }
}
