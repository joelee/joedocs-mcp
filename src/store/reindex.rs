//! Rebuilding the index (documents, revisions and search) from the history.
//!
//! Every revision is one commit whose trailers name the document, the
//! revision, the action and the client (`docs/plan.md` section 6), and each
//! document's latest content holds the rest of its metadata in front
//! matter. So the whole index can be replayed from the history. Projects
//! and API keys live only in the database; a project missing from it is
//! recreated from the documents' front matter.

use std::collections::HashSet;
use std::fs;

use indexmap::IndexMap;

use super::admin_files::{TARGET_DOCUMENT, TARGET_FILE, TARGET_REVISION};
use super::documents::{FILE_FROM_TRAILER, FILE_TRAILER, sha256};
use super::{Error, Store, index_file, lock};
use crate::clock::format_timestamp;
use crate::db::{
    self, Connection, DocumentRow, FileChange, FileRow, ProjectRow, RevisionFileRow, RevisionRow,
};
use crate::document::DocumentId;
use crate::document::front_matter::{self, Fields};
use crate::history::{Change, History, Logged};

const DOCUMENT: &str = "JoeDocs-Document";
const REVISION: &str = "JoeDocs-Revision";
const ACTION: &str = "JoeDocs-Action";
const CLIENT: &str = "JoeDocs-Client";

/// What [`Store::reindex`] rebuilt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reindexed {
    /// Live documents.
    pub documents: u64,
    /// Archived documents.
    pub archived: u64,
    /// Revisions of all documents.
    pub revisions: u64,
    /// History commits that name no document, which were ignored.
    pub skipped_commits: u64,
    /// Projects that were missing from the database and were recreated, as
    /// `(name, key)`.
    pub projects_created: Vec<(String, String)>,
    /// Live documents whose vault file differs from the history, as
    /// `(id, path)`. Writes to them are refused until this is resolved.
    pub drifted: Vec<(String, String)>,
}

/// One document's revisions, as replayed so far.
#[derive(Default)]
struct Replay {
    revisions: Vec<RevisionRow>,
    /// Commit and path of the latest revision that has content (of the
    /// main file, for a multi-file document).
    content: Option<(String, String)>,
    /// Time, key name and reason, if the latest revision archived it.
    archived: Option<(String, String, String)>,
    /// Whether its commits name files (`JoeDocs-File`).
    multifile: bool,
    /// The revision at which the main file last changed.
    main_revision: u64,
    /// Live sub-files by case-folded path.
    files: IndexMap<String, ReplayFile>,
    /// Archived sub-files.
    gone: Vec<ReplayFile>,
    /// The file changes of every revision.
    changes: Vec<RevisionFileRow>,
}

/// One sub-file, as replayed so far.
struct ReplayFile {
    /// Position among every sub-file created, in history order, so the
    /// rebuilt rows get the IDs the live store gave them.
    order: usize,
    path: String,
    created_at: String,
    created_by: String,
    revision: u64,
    updated_at: String,
    updated_by: String,
    client: String,
    /// Commit and vault path of its latest content.
    content: (String, String),
    /// Time and key name, if archived.
    archived: Option<(String, String)>,
}

/// A document ready to insert.
struct Rebuilt {
    row: DocumentRow,
    key: String,
    revisions: Vec<RevisionRow>,
    fields: Fields,
    body: String,
    files: Vec<RebuiltFile>,
    changes: Vec<RevisionFileRow>,
}

/// A sub-file ready to insert, with its search entries when live.
struct RebuiltFile {
    order: usize,
    row: FileRow,
    indexed: Option<(Fields, String)>,
}

fn bad(commit: &str, message: impl Into<String>) -> Error {
    Error::Rebuild {
        commit: commit.to_owned(),
        message: message.into(),
    }
}

impl Store {
    /// Whether the index is behind the history or was built by a build
    /// with another index format, so [`Store::reindex`] should run.
    ///
    /// # Errors
    ///
    /// Returns database and history errors.
    pub fn needs_reindex(&self) -> Result<bool, Error> {
        Ok(db::history_head(&self.conn)? != self.history.head()?
            || !db::index_format_current(&self.conn)?)
    }

    /// Rebuilds every document, revision and search entry from the
    /// history, in one transaction, then compares the vault with it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Rebuild`] when a commit cannot be replayed, and
    /// storage errors. On error the index is unchanged.
    pub fn reindex(&mut self) -> Result<Reindexed, Error> {
        let Self {
            paths,
            settings,
            conn,
            history,
            ..
        } = self;
        let _lock = lock(paths)?;
        let head = history.head()?;
        let mut report = Reindexed::default();
        let mut replays: IndexMap<String, Replay> = IndexMap::new();
        let mut created = 0;
        for commit in history.log()? {
            if commit.trailer(DOCUMENT).is_some() {
                replay(&mut replays, &commit, &mut created)?;
            } else {
                report.skipped_commits += 1;
            }
        }
        let documents = replays
            .into_iter()
            .map(|(id, replay)| rebuild(history.as_ref(), &id, replay))
            .collect::<Result<Vec<_>, _>>()?;

        let tx = conn.transaction()?;
        db::clear_documents(&tx)?;
        let mut files = Vec::new();
        let mut changes = Vec::new();
        for mut document in documents {
            files.append(&mut document.files);
            changes.append(&mut document.changes);
            insert(&tx, document, &mut report)?;
        }
        // In the order the live store created them, so they get its IDs.
        files.sort_by_key(|file| file.order);
        for file in files {
            let mut row = file.row;
            row.id = db::insert_file(&tx, &row)?;
            if let Some((fields, body)) = file.indexed {
                db::index_file(&tx, &row, &fields, &body)?;
            }
        }
        for change in &changes {
            db::insert_revision_file(&tx, change)?;
        }
        match &head {
            Some(head) => db::set_history_head(&tx, head)?,
            None => db::clear_history_head(&tx)?,
        }
        db::set_index_format(&tx)?;
        tx.commit()?;

        index_file::refresh_all(conn, &paths.home, settings);
        report.drifted = drifted(conn, &paths.home)?;
        Ok(report)
    }
}

/// Adds one commit to its document's replay. `created` counts the sub-files
/// created so far.
fn replay(
    replays: &mut IndexMap<String, Replay>,
    commit: &Logged,
    created: &mut usize,
) -> Result<(), Error> {
    let trailer = |key| {
        commit
            .trailer(key)
            .ok_or_else(|| bad(&commit.id, format!("no {key} trailer")))
    };
    let id: DocumentId = trailer(DOCUMENT)?
        .parse()
        .map_err(|e: crate::document::InvalidId| bad(&commit.id, e.to_string()))?;
    let id = id.to_string();
    let revision: u64 = trailer(REVISION)?
        .parse()
        .map_err(|_| bad(&commit.id, "the revision is not a number"))?;
    let action = trailer(ACTION)?;
    check_next(replays, commit, &id, revision)?;
    if commit.trailer(TARGET_DOCUMENT).is_some() {
        return replay_move(replays, commit, &id, revision, action);
    }
    let doc = replays.entry(id.clone()).or_default();
    let archive = action == "archive";
    let at = format_timestamp(commit.at);
    let client = commit.trailer(CLIENT).unwrap_or("unknown").to_owned();
    let files = file_trailers(commit)?;
    let path = if files.is_empty() {
        if doc.multifile {
            return Err(bad(
                &commit.id,
                format!("{id} is a multi-file document, but the commit names no {FILE_TRAILER}"),
            ));
        }
        commit
            .changes
            .iter()
            .find(|(change, _)| (*change == Change::Deleted) == archive)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| bad(&commit.id, format!("no file for {id}")))?
    } else {
        let step = Step {
            commit,
            id: &id,
            revision,
            at: &at,
            client: &client,
        };
        replay_files(doc, &step, &files, created)?
    };
    if archive {
        let reason = commit
            .subject
            .strip_prefix(&format!("Archive {id}: "))
            .unwrap_or(&commit.subject);
        doc.archived = Some((at.clone(), commit.author.clone(), reason.to_owned()));
    } else {
        doc.archived = None;
        if !doc.multifile {
            doc.content = Some((commit.id.clone(), path.clone()));
        }
    }
    doc.revisions.push(RevisionRow {
        document_id: id,
        revision,
        commit_id: commit.id.clone(),
        path,
        action: action.to_owned(),
        author: commit.author.clone(),
        client,
        summary: commit.subject.clone(),
        created_at: at,
    });
    Ok(())
}

/// Refuses `revision` of `id` unless it is the next one.
fn check_next(
    replays: &mut IndexMap<String, Replay>,
    commit: &Logged,
    id: &str,
    revision: u64,
) -> Result<(), Error> {
    let expected = replays.entry(id.to_owned()).or_default().revisions.len() as u64 + 1;
    if revision == expected {
        Ok(())
    } else {
        Err(bad(
            &commit.id,
            format!("{id} revision {revision} found where revision {expected} was expected"),
        ))
    }
}

/// Replays a file's move from document `id` to the document its
/// `JoeDocs-Target-*` trailers name: one commit, a revision of each.
fn replay_move(
    replays: &mut IndexMap<String, Replay>,
    commit: &Logged,
    id: &str,
    revision: u64,
    action: &str,
) -> Result<(), Error> {
    let fail = |message: String| bad(&commit.id, message);
    let trailer = |key| {
        commit
            .trailer(key)
            .ok_or_else(|| bad(&commit.id, format!("no {key} trailer")))
    };
    let target: DocumentId = trailer(TARGET_DOCUMENT)?
        .parse()
        .map_err(|e: crate::document::InvalidId| fail(e.to_string()))?;
    let target = target.to_string();
    let target_revision: u64 = trailer(TARGET_REVISION)?
        .parse()
        .map_err(|_| fail("the target revision is not a number".into()))?;
    check_next(replays, commit, &target, target_revision)?;
    let old = trailer(FILE_TRAILER)?;
    let new = trailer(TARGET_FILE)?;
    let at = format_timestamp(commit.at);
    let client = commit.trailer(CLIENT).unwrap_or("unknown").to_owned();
    let main_path = |doc_id: &str, replays: &IndexMap<String, Replay>| -> Result<String, Error> {
        replays
            .get(doc_id)
            .and_then(|doc| doc.revisions.last())
            .map(|r| r.path.clone())
            .filter(|path| is_main_path(path))
            .ok_or_else(|| fail(format!("{doc_id} is not a multi-file document")))
    };
    let source_main = main_path(id, replays)?;
    let target_main = main_path(&target, replays)?;
    let folder = |path: &str| path.rsplit_once('/').map_or("", |(f, _)| f).to_owned();
    let from_vault = format!("{}/{old}", folder(&source_main));
    let to_vault = format!("{}/{new}", folder(&target_main));
    let changed = |change: Change, path: &str| {
        commit
            .changes
            .iter()
            .any(|(c, p)| *c == change && p == path)
    };
    if commit.changes.len() != 2
        || !changed(Change::Deleted, &from_vault)
        || !changed(Change::Added, &to_vault)
    {
        return Err(fail(format!(
            "a move must delete {from_vault} and add {to_vault}, and nothing else"
        )));
    }
    let strip = |name: &str| -> Result<String, Error> {
        name.strip_suffix(".md")
            .map(str::to_owned)
            .ok_or_else(|| fail(format!("{name} does not end in .md")))
    };
    let (old, new) = (strip(old)?, strip(new)?);
    let mut file = replays
        .get_mut(id)
        .and_then(|doc| doc.files.shift_remove(&old.to_lowercase()))
        .ok_or_else(|| fail(format!("{id} has no file {old} to move")))?;
    new.clone_into(&mut file.path);
    file.revision = target_revision;
    at.clone_into(&mut file.updated_at);
    commit.author.clone_into(&mut file.updated_by);
    client.clone_into(&mut file.client);
    file.content = (commit.id.clone(), to_vault);
    for (doc_id, doc_revision, path, change, main) in [
        (id, revision, &old, FileChange::Removed, &source_main),
        (
            target.as_str(),
            target_revision,
            &new,
            FileChange::Added,
            &target_main,
        ),
    ] {
        let doc = replays.entry(doc_id.to_owned()).or_default();
        doc.changes.push(RevisionFileRow {
            document_id: doc_id.to_owned(),
            revision: doc_revision,
            path: path.clone(),
            main: false,
            change,
            from_path: None,
        });
        doc.revisions.push(RevisionRow {
            document_id: doc_id.to_owned(),
            revision: doc_revision,
            commit_id: commit.id.clone(),
            path: main.clone(),
            action: action.to_owned(),
            author: commit.author.clone(),
            client: client.clone(),
            summary: commit.subject.clone(),
            created_at: at.clone(),
        });
    }
    let key = new.to_lowercase();
    replays.entry(target).or_default().files.insert(key, file);
    Ok(())
}

/// The `JoeDocs-File` trailers of `commit` in order, each with the
/// `JoeDocs-File-From` that follows it, if any.
fn file_trailers(commit: &Logged) -> Result<Vec<(String, Option<String>)>, Error> {
    let mut files: Vec<(String, Option<String>)> = Vec::new();
    for (key, value) in &commit.trailers {
        if key == FILE_TRAILER {
            files.push((value.clone(), None));
        } else if key == FILE_FROM_TRAILER {
            match files.last_mut() {
                Some((_, from @ None)) => *from = Some(value.clone()),
                _ => {
                    return Err(bad(
                        &commit.id,
                        format!("{FILE_FROM_TRAILER} without a {FILE_TRAILER} before it"),
                    ));
                }
            }
        }
    }
    Ok(files)
}

/// Whether `path` is the main file of a multi-file document's folder:
/// `<project>/<type>/<name>/<name>.md`.
fn is_main_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() == 4 && parts[3].strip_suffix(".md") == Some(parts[2])
}

/// The commit being replayed, for [`replay_files`].
struct Step<'a> {
    commit: &'a Logged,
    id: &'a str,
    revision: u64,
    at: &'a str,
    client: &'a str,
}

/// Replays a multi-file document's commit: each `JoeDocs-File` names a file
/// inside the folder that the commit adds, changes or removes, and every
/// path the commit changes must be named. Returns the main file's path.
fn replay_files(
    doc: &mut Replay,
    step: &Step<'_>,
    files: &[(String, Option<String>)],
    created: &mut usize,
) -> Result<String, Error> {
    let commit = step.commit;
    let fail = |message: String| bad(&commit.id, message);
    doc.multifile = true;
    let previous = doc.revisions.last().map(|r| r.path.clone());
    let main_path = main_path_after(commit, files, previous.as_deref())
        .ok_or_else(|| fail(format!("no main file for {} in the commit", step.id)))?;
    let folder = main_path.rsplit_once('/').map_or("", |(folder, _)| folder);
    let folder_name = folder.rsplit_once('/').map_or(folder, |(_, name)| name);
    // The folder before this commit, when the document moved.
    let old_folder = previous
        .as_deref()
        .filter(|path| is_main_path(path))
        .map(|path| path.rsplit_once('/').map_or("", |(folder, _)| folder))
        .filter(|old| *old != folder)
        .map(str::to_owned);
    let mut named: HashSet<String> = HashSet::new();
    // Converting a single-file document moves its file into the folder.
    if let Some(single) = previous.as_deref().filter(|path| !is_main_path(path)) {
        named.insert(single.to_owned());
        doc.changes
            .extend(super::files::backfill_main(&doc.revisions));
    }
    for (file, from) in files {
        let rel = file
            .strip_suffix(".md")
            .ok_or_else(|| fail(format!("{FILE_TRAILER} {file} does not end in .md")))?;
        let main = rel == folder_name;
        let vault = format!("{folder}/{file}");
        let kind = commit
            .changes
            .iter()
            .find(|(_, path)| *path == vault)
            .map(|(change, _)| *change)
            .ok_or_else(|| {
                fail(format!(
                    "{FILE_TRAILER} {file} names a file the commit does not change"
                ))
            })?;
        named.insert(vault.clone());
        let from_rel = match from {
            Some(from) => {
                // The main file moves with its folder; other files move
                // within it.
                let from_folder = match (&old_folder, main) {
                    (Some(old), true) => old.as_str(),
                    _ => folder,
                };
                named.insert(format!("{from_folder}/{from}"));
                Some(
                    from.strip_suffix(".md")
                        .ok_or_else(|| {
                            fail(format!("{FILE_FROM_TRAILER} {from} does not end in .md"))
                        })?
                        .to_owned(),
                )
            }
            None => None,
        };
        let change = match (kind, &from_rel) {
            (Change::Deleted, _) => FileChange::Removed,
            (Change::Added, Some(_)) => FileChange::Renamed,
            (Change::Added, None) => FileChange::Added,
            (Change::Modified, _) => FileChange::Modified,
        };
        if main {
            doc.main_revision = step.revision;
            if kind != Change::Deleted {
                doc.content = Some((commit.id.clone(), vault));
            }
        } else {
            replay_file(doc, step, rel, change, from_rel.as_deref(), vault, created)?;
        }
        doc.changes.push(RevisionFileRow {
            document_id: step.id.to_owned(),
            revision: step.revision,
            path: rel.to_owned(),
            main,
            change,
            from_path: from_rel,
        });
    }
    if let Some(old) = &old_folder {
        move_with_folder(doc, commit, old, folder, &mut named)?;
    }
    if let Some((_, path)) = commit
        .changes
        .iter()
        .find(|(_, path)| !named.contains(path))
    {
        return Err(fail(format!(
            "it changes {path}, which no {FILE_TRAILER} trailer names"
        )));
    }
    Ok(main_path)
}

/// The main file's path after `commit`: where a main file it adds (and
/// names in a trailer) is, else where the main file was before
/// (`previous`), if that was a folder's main file.
fn main_path_after(
    commit: &Logged,
    files: &[(String, Option<String>)],
    previous: Option<&str>,
) -> Option<String> {
    let added = commit
        .changes
        .iter()
        .filter(|(change, path)| *change != Change::Deleted && is_main_path(path))
        .map(|(_, path)| path.as_str())
        .find(|path| {
            let name = path.rsplit_once('/').map_or(*path, |(_, name)| name);
            files.iter().any(|(file, _)| file == name)
        });
    added
        .or(previous.filter(|path| is_main_path(path)))
        .map(str::to_owned)
}

/// Moves the live sub-files of a document whose folder moved from `old` to
/// `new` in `commit`: each must be deleted from the old folder and added to
/// the new one, unchanged. Their content is now at the new path.
fn move_with_folder(
    doc: &mut Replay,
    commit: &Logged,
    old: &str,
    new: &str,
    named: &mut HashSet<String>,
) -> Result<(), Error> {
    let has = |change: Change, path: &str| {
        commit
            .changes
            .iter()
            .any(|(c, p)| *c == change && p == path)
    };
    for file in doc.files.values_mut() {
        let from = format!("{old}/{}.md", file.path);
        let to = format!("{new}/{}.md", file.path);
        if named.contains(&to) {
            continue;
        }
        if !has(Change::Deleted, &from) || !has(Change::Added, &to) {
            return Err(bad(
                &commit.id,
                format!(
                    "the folder moved from {old} to {new}, but {} did not move with it",
                    file.path
                ),
            ));
        }
        named.insert(from);
        named.insert(to.clone());
        file.content = (commit.id.clone(), to);
    }
    Ok(())
}

/// Applies one sub-file's change to the replay.
fn replay_file(
    doc: &mut Replay,
    step: &Step<'_>,
    rel: &str,
    change: FileChange,
    from: Option<&str>,
    vault: String,
    created: &mut usize,
) -> Result<(), Error> {
    let fail = |message: String| bad(&step.commit.id, message);
    let key = rel.to_lowercase();
    let author = &step.commit.author;
    let mut file = match (change, from) {
        (FileChange::Added, _) => {
            if doc.files.contains_key(&key) {
                return Err(fail(format!("{} already has a file {rel}", step.id)));
            }
            *created += 1;
            ReplayFile {
                order: *created,
                path: rel.to_owned(),
                created_at: step.at.to_owned(),
                created_by: author.clone(),
                revision: 0,
                updated_at: String::new(),
                updated_by: String::new(),
                client: String::new(),
                content: (String::new(), String::new()),
                archived: None,
            }
        }
        (FileChange::Renamed, Some(from)) => doc
            .files
            .shift_remove(&from.to_lowercase())
            .ok_or_else(|| fail(format!("{} has no file {from} to rename", step.id)))?,
        _ => doc
            .files
            .shift_remove(&key)
            .ok_or_else(|| fail(format!("{} has no file {rel}", step.id)))?,
    };
    rel.clone_into(&mut file.path);
    file.revision = step.revision;
    step.at.clone_into(&mut file.updated_at);
    author.clone_into(&mut file.updated_by);
    step.client.clone_into(&mut file.client);
    if change == FileChange::Removed {
        file.archived = Some((step.at.to_owned(), author.clone()));
        doc.gone.push(file);
    } else {
        file.content = (step.commit.id.clone(), vault);
        doc.files.insert(key, file);
    }
    Ok(())
}

/// Reads a document's latest content and builds its index row.
fn rebuild(history: &dyn History, id: &str, replay: Replay) -> Result<Rebuilt, Error> {
    let Replay {
        revisions,
        content,
        archived,
        multifile,
        main_revision,
        files: live,
        gone,
        changes,
    } = replay;
    let (Some((commit, path)), Some(last)) = (content, revisions.last()) else {
        let commit = revisions.first().map_or("(none)", |r| r.commit_id.as_str());
        return Err(bad(commit, format!("{id} has no revision with content")));
    };
    let bytes = history.show(&commit, &path)?;
    let text =
        String::from_utf8(bytes).map_err(|_| bad(&commit, format!("{path} is not UTF-8")))?;
    let parsed =
        front_matter::parse(&text).map_err(|e| bad(&commit, format!("{path}: {}", e.0)))?;
    let field = |key: &str| {
        parsed
            .fields
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| bad(&commit, format!("{path} has no {key} in its front matter")))
    };
    if field("id")? != id {
        return Err(bad(&commit, format!("{path} does not have id {id}")));
    }
    let parsed_id: DocumentId = id
        .parse()
        .map_err(|e: crate::document::InvalidId| bad(&commit, e.to_string()))?;
    let (archived_at, archived_by, archive_reason) = match archived {
        Some((at, by, reason)) => (Some(at), Some(by), Some(reason)),
        None => (None, None, None),
    };
    let row = DocumentRow {
        id: id.to_owned(),
        project_id: 0,
        project: field("project")?,
        seq: parsed_id.seq(),
        doc_type: field("type")?,
        title: field("title")?,
        path: last.path.clone(),
        status: field("status")?,
        revision: last.revision,
        created_at: field("created")?,
        created_by: field("created_by")?,
        updated_at: last.created_at.clone(),
        updated_by: last.author.clone(),
        client: last.client.clone(),
        content_sha256: sha256(text.as_bytes()),
        archived_at,
        archived_by,
        archive_reason,
        multifile,
        main_revision: if multifile {
            main_revision
        } else {
            last.revision
        },
    };
    let body = parsed.body.to_owned();
    let mut files = Vec::with_capacity(live.len() + gone.len());
    for (file, is_live) in live
        .into_values()
        .map(|f| (f, true))
        .chain(gone.into_iter().map(|f| (f, false)))
    {
        files.push(rebuild_file(history, id, file, is_live)?);
    }
    Ok(Rebuilt {
        row,
        key: parsed_id.key().to_owned(),
        revisions,
        fields: parsed.fields,
        body,
        files,
        changes,
    })
}

/// Reads a sub-file's latest content and builds its row, with its search
/// entries when it is live.
fn rebuild_file(
    history: &dyn History,
    id: &str,
    file: ReplayFile,
    live: bool,
) -> Result<RebuiltFile, Error> {
    let (commit, path) = &file.content;
    let bytes = history.show(commit, path)?;
    let content_sha256 = sha256(&bytes);
    let indexed = if live {
        let text =
            String::from_utf8(bytes).map_err(|_| bad(commit, format!("{path} is not UTF-8")))?;
        let parsed =
            front_matter::parse(&text).map_err(|e| bad(commit, format!("{path}: {}", e.0)))?;
        Some((parsed.fields, parsed.body.to_owned()))
    } else {
        None
    };
    let (archived_at, archived_by) = match file.archived {
        Some((at, by)) => (Some(at), Some(by)),
        None => (None, None),
    };
    Ok(RebuiltFile {
        order: file.order,
        row: FileRow {
            id: 0,
            document_id: id.to_owned(),
            path: file.path,
            revision: file.revision,
            created_at: file.created_at,
            created_by: file.created_by,
            updated_at: file.updated_at,
            updated_by: file.updated_by,
            client: file.client,
            content_sha256,
            archived_at,
            archived_by,
        },
        indexed,
    })
}

/// Inserts a rebuilt document, its revisions and its search entries,
/// recreating its project if needed.
fn insert(tx: &Connection, document: Rebuilt, report: &mut Reindexed) -> Result<(), Error> {
    let Rebuilt {
        mut row,
        key,
        revisions,
        fields,
        body,
        ..
    } = document;
    let existing = db::find_project(tx, &key)?.filter(|p| p.key == key);
    let project = if let Some(project) = existing {
        project
    } else {
        let project = recreate_project(tx, &row, &key, &revisions[0].commit_id)?;
        report.projects_created.push((project.name.clone(), key));
        project
    };
    row.project_id = project.id;
    row.project = project.name;
    db::insert_document(tx, &row)?;
    report.revisions += revisions.len() as u64;
    for revision in &revisions {
        db::insert_revision(tx, revision)?;
    }
    db::raise_next_seq(tx, project.id, row.seq + 1)?;
    if row.archived_at.is_some() {
        report.archived += 1;
    } else {
        db::index_document(tx, &row, &fields, &body)?;
        report.documents += 1;
    }
    Ok(())
}

/// Registers the project a document names in its front matter, which is
/// missing from the database, with its name as its title.
fn recreate_project(
    tx: &Connection,
    row: &DocumentRow,
    key: &str,
    commit: &str,
) -> Result<ProjectRow, Error> {
    if db::find_project(tx, &row.project)?.is_some() {
        return Err(bad(
            commit,
            format!(
                "{} belongs to project {} with key {key}, but that name has another key",
                row.id, row.project
            ),
        ));
    }
    let id = db::insert_project(tx, &row.project, key, &row.project, &row.created_at)?;
    db::find_project_by_id(tx, id)?.ok_or_else(|| Error::ProjectNotFound(row.project.clone()))
}

/// Live documents whose vault file is missing or not the indexed content.
pub(super) fn drifted(
    conn: &Connection,
    home: &std::path::Path,
) -> Result<Vec<(String, String)>, Error> {
    let mut drifted = Vec::new();
    let same = |path: &str, hash: &[u8]| {
        fs::read(home.join(path)).is_ok_and(|bytes| sha256(&bytes) == hash)
    };
    for row in db::live_documents(conn)? {
        if !same(&row.path, &row.content_sha256) {
            drifted.push((row.id.clone(), row.path.clone()));
        }
        if row.multifile {
            for file in db::live_files(conn, &row.id)? {
                let path = super::files::vault_path(&row, &file.path);
                if !same(&path, &file.content_sha256) {
                    drifted.push((row.id.clone(), path));
                }
            }
        }
    }
    Ok(drifted)
}
