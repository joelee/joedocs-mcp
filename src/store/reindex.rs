//! Rebuilding the index (documents, revisions and search) from the history.
//!
//! Every revision is one commit whose trailers name the document, the
//! revision, the action and the client (`docs/plan.md` section 6), and each
//! document's latest content holds the rest of its metadata in front
//! matter. So the whole index can be replayed from the history. Projects
//! and API keys live only in the database; a project missing from it is
//! recreated from the documents' front matter.

use std::fs;

use indexmap::IndexMap;

use super::documents::sha256;
use super::{Error, Store, lock};
use crate::clock::format_timestamp;
use crate::db::{self, Connection, DocumentRow, ProjectRow, RevisionRow};
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
    /// Commit and path of the latest revision that has content.
    content: Option<(String, String)>,
    /// Time, key name and reason, if the latest revision archived it.
    archived: Option<(String, String, String)>,
}

/// A document ready to insert.
struct Rebuilt {
    row: DocumentRow,
    key: String,
    revisions: Vec<RevisionRow>,
    fields: Fields,
    body: String,
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
            conn,
            history,
            ..
        } = self;
        let _lock = lock(paths)?;
        let head = history.head()?;
        let mut report = Reindexed::default();
        let mut replays: IndexMap<String, Replay> = IndexMap::new();
        for commit in history.log()? {
            if commit.trailer(DOCUMENT).is_some() {
                replay(&mut replays, &commit)?;
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
        for document in documents {
            insert(&tx, document, &mut report)?;
        }
        match &head {
            Some(head) => db::set_history_head(&tx, head)?,
            None => db::clear_history_head(&tx)?,
        }
        db::set_index_format(&tx)?;
        tx.commit()?;

        report.drifted = drifted(conn, &paths.home)?;
        Ok(report)
    }
}

/// Adds one commit to its document's replay.
fn replay(replays: &mut IndexMap<String, Replay>, commit: &Logged) -> Result<(), Error> {
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
    let doc = replays.entry(id.clone()).or_default();
    let expected = doc.revisions.len() as u64 + 1;
    if revision != expected {
        return Err(bad(
            &commit.id,
            format!("{id} revision {revision} found where revision {expected} was expected"),
        ));
    }
    let archive = action == "archive";
    let path = commit
        .changes
        .iter()
        .find(|(change, _)| (*change == Change::Deleted) == archive)
        .map(|(_, path)| path.clone())
        .ok_or_else(|| bad(&commit.id, format!("no file for {id}")))?;
    let at = format_timestamp(commit.at);
    if archive {
        let reason = commit
            .subject
            .strip_prefix(&format!("Archive {id}: "))
            .unwrap_or(&commit.subject);
        doc.archived = Some((at.clone(), commit.author.clone(), reason.to_owned()));
    } else {
        doc.archived = None;
        doc.content = Some((commit.id.clone(), path.clone()));
    }
    doc.revisions.push(RevisionRow {
        document_id: id,
        revision,
        commit_id: commit.id.clone(),
        path,
        action: action.to_owned(),
        author: commit.author.clone(),
        client: commit.trailer(CLIENT).unwrap_or("unknown").to_owned(),
        summary: commit.subject.clone(),
        created_at: at,
    });
    Ok(())
}

/// Reads a document's latest content and builds its index row.
fn rebuild(history: &dyn History, id: &str, replay: Replay) -> Result<Rebuilt, Error> {
    let Replay {
        revisions,
        content,
        archived,
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
    };
    let body = parsed.body.to_owned();
    Ok(Rebuilt {
        row,
        key: parsed_id.key().to_owned(),
        revisions,
        fields: parsed.fields,
        body,
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
    for row in db::live_documents(conn)? {
        let same =
            fs::read(home.join(&row.path)).is_ok_and(|bytes| sha256(&bytes) == row.content_sha256);
        if !same {
            drifted.push((row.id, row.path));
        }
    }
    Ok(drifted)
}
