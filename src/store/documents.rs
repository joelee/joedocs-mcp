//! Document use cases.

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::Transaction;
use sha2::{Digest as _, Sha256};

use super::journal::Journal;
use super::{Actor, Error, Settings, Store, begin_write};
use crate::db::{self, Connection, DocumentRow, ListQuery, RevisionRow, Sort};
use crate::document::front_matter::{self, Fields};
use crate::document::{DocumentId, Header, check_title, file_stem};
use crate::history::{Commit, History};

/// Most items one listing page may return.
pub const MAX_LIST_LIMIT: u64 = 1000;

/// Longest commit summary kept, in characters.
const MAX_SUMMARY_CHARS: usize = 200;

/// A request to create a document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewDocument {
    /// Project name.
    pub project: String,
    /// Document type; `documents.default_type` when `None`.
    pub doc_type: Option<String>,
    /// Status; `documents.default_status` when `None`.
    pub status: Option<String>,
    /// Title.
    pub title: String,
    /// Markdown body, without front matter.
    pub content: String,
    /// Extra front matter: `tags`, `aliases` and custom keys.
    pub front_matter: Fields,
    /// Commit summary; the title when `None`.
    pub summary: Option<String>,
}

/// A request to replace a document's body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Update {
    /// Document ID.
    pub id: String,
    /// New Markdown body.
    pub content: String,
    /// The revision the client last read.
    pub expected_revision: u64,
    /// Front matter keys to set at the same time.
    pub front_matter: Fields,
    /// Commit summary.
    pub summary: Option<String>,
}

/// The result of a document change.
#[derive(Debug, Clone, PartialEq)]
pub struct Changed {
    /// Document ID.
    pub id: String,
    /// The new revision.
    pub revision: u64,
    /// Vault-relative path.
    pub path: String,
    /// The complete front matter now in the file.
    pub front_matter: Fields,
}

/// The result of archiving a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    /// Document ID.
    pub id: String,
    /// The archive revision.
    pub revision: u64,
    /// When it was archived.
    pub archived_at: String,
}

/// A document's index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInfo {
    /// Document ID.
    pub id: String,
    /// Project name.
    pub project: String,
    /// Document type.
    pub doc_type: String,
    /// Title.
    pub title: String,
    /// Status.
    pub status: String,
    /// Latest revision.
    pub revision: u64,
    /// Vault-relative path.
    pub path: String,
    /// Creation time.
    pub created: String,
    /// Creating key name.
    pub created_by: String,
    /// Time of the latest revision.
    pub updated: String,
    /// Key name of the latest revision.
    pub updated_by: String,
    /// Client of the latest revision.
    pub client: String,
}

impl From<&DocumentRow> for DocumentInfo {
    fn from(row: &DocumentRow) -> Self {
        Self {
            id: row.id.clone(),
            project: row.project.clone(),
            doc_type: row.doc_type.clone(),
            title: row.title.clone(),
            status: row.status.clone(),
            revision: row.revision,
            path: row.path.clone(),
            created: row.created_at.clone(),
            created_by: row.created_by.clone(),
            updated: row.updated_at.clone(),
            updated_by: row.updated_by.clone(),
            client: row.client.clone(),
        }
    }
}

/// One revision's history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionInfo {
    /// Revision number.
    pub revision: u64,
    /// What happened, e.g. `update`.
    pub action: String,
    /// Key name.
    pub author: String,
    /// Client name.
    pub client: String,
    /// Commit subject.
    pub summary: String,
    /// Time.
    pub at: String,
    /// History commit.
    pub commit: String,
    /// Vault-relative path at that revision.
    pub path: String,
}

impl From<RevisionRow> for RevisionInfo {
    fn from(row: RevisionRow) -> Self {
        Self {
            revision: row.revision,
            action: row.action,
            author: row.author,
            client: row.client,
            summary: row.summary,
            at: row.created_at,
            commit: row.commit_id,
            path: row.path,
        }
    }
}

/// A document as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The document's current index entry.
    pub document: DocumentInfo,
    /// The revision returned.
    pub revision: RevisionInfo,
    /// The whole file: front matter and body.
    pub markdown: String,
}

/// Which documents to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRequest {
    /// Project name.
    pub project: String,
    /// Only this type; `None` or `"all"` for every type.
    pub doc_type: Option<String>,
    /// Only this status.
    pub status: Option<String>,
    /// Order.
    pub sort: Sort,
    /// 1-based position of the first item.
    pub start: u64,
    /// Most items to return, 1 to [`MAX_LIST_LIMIT`].
    pub limit: u64,
}

/// One page of documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The documents.
    pub items: Vec<DocumentInfo>,
    /// How many documents match in total.
    pub total: u64,
    /// `start` for the next page, if there is one.
    pub next_start: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Create,
    Update,
    Rename,
    FrontMatter,
    Archive,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Rename => "rename",
            Self::FrontMatter => "front_matter",
            Self::Archive => "archive",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Update => "Update",
            Self::Rename => "Rename",
            Self::FrontMatter => "Change front matter of",
            Self::Archive => "Archive",
        }
    }
}

/// The editable parts of a document during a change.
struct Draft {
    title: String,
    status: String,
    extra: Fields,
    body: String,
}

impl Store {
    /// Creates a document at revision 1.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProjectNotFound`], [`Error::Invalid`] for a bad type,
    /// status, title or front matter, [`Error::TooLarge`], and storage
    /// errors.
    pub fn create(&mut self, actor: &Actor, request: NewDocument) -> Result<Changed, Error> {
        let doc_type = request
            .doc_type
            .unwrap_or_else(|| self.settings.default_type.clone());
        check_member("type", &doc_type, &self.settings.types)?;
        let mut status = request
            .status
            .unwrap_or_else(|| self.settings.default_status.clone());
        check_member("status", &status, &self.settings.statuses)?;
        let title = check_title(&request.title).map_err(|m| Error::invalid("title", m))?;
        if request.front_matter.contains_key("status") {
            return Err(Error::invalid(
                "front_matter",
                "\"status\" has its own parameter",
            ));
        }
        let mut extra = Fields::new();
        front_matter::apply(
            &mut status,
            &mut extra,
            &request.front_matter,
            &[],
            &self.settings.statuses,
        )
        .map_err(|e| Error::invalid("front_matter", e.0))?;
        let now = self.now();
        let summary = tidy_line(request.summary.as_deref()).unwrap_or_else(|| title.clone());

        let Self {
            paths,
            settings,
            conn,
            history,
            ..
        } = self;
        let _lock = begin_write(paths, conn, history.as_ref())?;
        let tx = conn.transaction()?;
        let project = db::find_project(&tx, &request.project)?
            .filter(|p| p.name == request.project)
            .ok_or_else(|| Error::ProjectNotFound(request.project.clone()))?;
        let seq = db::allocate_seq(&tx, project.id)?;
        let id = DocumentId::new(&project.key, seq).to_string();
        let path = choose_path(
            &tx,
            &paths.home,
            &project.name,
            &doc_type,
            &title,
            &id,
            None,
        )?;
        let row = DocumentRow {
            id: id.clone(),
            project_id: project.id,
            project: project.name,
            seq,
            doc_type,
            title,
            path,
            status,
            revision: 1,
            created_at: now.clone(),
            created_by: actor.name().to_owned(),
            updated_at: now,
            updated_by: actor.name().to_owned(),
            client: actor.client().to_owned(),
            content_sha256: Vec::new(),
            archived_at: None,
            archived_by: None,
            archive_reason: None,
        };
        let (row, fields, journal) = write_revision(
            &paths.home,
            settings,
            row,
            &extra,
            &request.content,
            None,
            |row| db::insert_document(&tx, row),
        )?;
        let changed = Changed {
            id: row.id.clone(),
            revision: row.revision,
            path: row.path.clone(),
            front_matter: fields,
        };
        let commit = Change {
            row: &row,
            action: Action::Create,
            summary: &summary,
            actor,
            paths: &[&row.path],
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        Ok(changed)
    }

    /// Replaces a document's body (and optionally sets front matter keys).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Conflict`] when `expected_revision` is stale,
    /// [`Error::Integrity`] when the file changed behind the store's back,
    /// and the errors of [`Store::create`].
    pub fn update(&mut self, actor: &Actor, request: Update) -> Result<Changed, Error> {
        let summary = tidy_line(request.summary.as_deref());
        self.revise(
            actor,
            &request.id,
            Some(request.expected_revision),
            Action::Update,
            summary,
            |draft, settings| {
                apply_front_matter(draft, &request.front_matter, &[], settings)?;
                draft.body = request.content;
                Ok(())
            },
        )
    }

    /// Changes a document's title, moving its file and keeping the old
    /// title in `aliases`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad or unchanged title, and the
    /// errors of [`Store::update`].
    pub fn rename(
        &mut self,
        actor: &Actor,
        id: &str,
        title: &str,
        expected_revision: Option<u64>,
    ) -> Result<Changed, Error> {
        let title = check_title(title).map_err(|m| Error::invalid("title", m))?;
        self.revise(
            actor,
            id,
            expected_revision,
            Action::Rename,
            None,
            |draft, _| {
                if draft.title == title {
                    return Err(Error::invalid("title", "is already the document's title"));
                }
                let old_title = std::mem::replace(&mut draft.title, title.clone());
                front_matter::add_alias(&mut draft.extra, &old_title);
                if let Some(serde_json::Value::Array(aliases)) = draft.extra.get_mut("aliases") {
                    aliases.retain(|alias| alias.as_str() != Some(title.as_str()));
                }
                Ok(())
            },
        )
    }

    /// Sets and removes front matter keys.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when nothing would change or a front
    /// matter rule is broken, and the errors of [`Store::update`].
    pub fn change_front_matter(
        &mut self,
        actor: &Actor,
        id: &str,
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
        self.revise(
            actor,
            id,
            expected_revision,
            Action::FrontMatter,
            None,
            |draft, settings| apply_front_matter(draft, set, remove, settings),
        )
    }

    /// Removes a document from the vault and from listings. The history
    /// keeps every revision.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] without a reason, and the errors of
    /// [`Store::update`].
    pub fn archive(&mut self, actor: &Actor, id: &str, reason: &str) -> Result<Archived, Error> {
        let reason =
            tidy_line(Some(reason)).ok_or_else(|| Error::invalid("reason", "must not be empty"))?;
        let id = parse_id(id)?;
        let now = self.now();
        let Self {
            paths,
            conn,
            history,
            ..
        } = self;
        let _lock = begin_write(paths, conn, history.as_ref())?;
        let tx = conn.transaction()?;
        let mut row = live_row(&tx, &id)?;
        read_verified(&paths.home, &row)?;

        row.revision += 1;
        row.updated_at.clone_from(&now);
        actor.name().clone_into(&mut row.updated_by);
        actor.client().clone_into(&mut row.client);
        row.archived_at = Some(now.clone());
        row.archived_by = Some(actor.name().to_owned());
        row.archive_reason = Some(reason.clone());
        db::save_document(&tx, &row)?;

        let file = paths.home.join(&row.path);
        let mut journal = Journal::default();
        journal.remove(&file)?;
        let commit = Change {
            row: &row,
            action: Action::Archive,
            summary: &reason,
            actor,
            paths: &[&row.path],
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        remove_empty_parent(&file);
        Ok(Archived {
            id: row.id,
            revision: row.revision,
            archived_at: now,
        })
    }

    /// Reads a document: the latest revision, or `version`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Archived`], [`Error::Invalid`]
    /// for a revision that does not exist, and storage errors.
    pub fn get(&self, id: &str, version: Option<u64>) -> Result<Fetched, Error> {
        let id = parse_id(id)?;
        let row = live_row(&self.conn, &id)?;
        let wanted = version.unwrap_or(row.revision);
        if wanted == 0 || wanted > row.revision {
            return Err(Error::invalid(
                "version",
                format!("{} has revisions 1 to {}", row.id, row.revision),
            ));
        }
        let revision = db::get_revision(&self.conn, &row.id, wanted)?.ok_or_else(|| {
            Error::invalid("version", format!("revision {wanted} is not indexed"))
        })?;

        let current = (wanted == row.revision)
            .then(|| read_verified(&self.paths.home, &row))
            .transpose();
        let markdown = match current {
            Ok(Some(text)) => text,
            Ok(None) => self.committed_text(&row, &revision)?,
            Err(error) => {
                tracing::warn!(%error, "serving the committed revision instead of the file");
                self.committed_text(&row, &revision)?
            }
        };
        Ok(Fetched {
            document: DocumentInfo::from(&row),
            revision: revision.into(),
            markdown,
        })
    }

    fn committed_text(&self, row: &DocumentRow, revision: &RevisionRow) -> Result<String, Error> {
        let bytes = self.history.show(&revision.commit_id, &revision.path)?;
        String::from_utf8(bytes).map_err(|_| integrity(row))
    }

    /// Lists a project's live documents.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProjectNotFound`], [`Error::Invalid`] for a bad type,
    /// status, start or limit, and database errors.
    pub fn list(&self, request: &ListRequest) -> Result<Listing, Error> {
        let doc_type = request.doc_type.as_deref().filter(|t| *t != "all");
        if let Some(doc_type) = doc_type {
            check_member("type", doc_type, &self.settings.types)?;
        }
        if let Some(status) = &request.status {
            check_member("status", status, &self.settings.statuses)?;
        }
        if request.start == 0 {
            return Err(Error::invalid("start", "must be 1 or more"));
        }
        if request.limit == 0 || request.limit > MAX_LIST_LIMIT {
            return Err(Error::invalid(
                "limit",
                format!("must be 1 to {MAX_LIST_LIMIT}"),
            ));
        }
        let project = db::find_project(&self.conn, &request.project)?
            .filter(|p| p.name == request.project)
            .ok_or_else(|| Error::ProjectNotFound(request.project.clone()))?;
        let offset = request.start - 1;
        let (rows, total) = db::list_documents(
            &self.conn,
            &ListQuery {
                project_id: project.id,
                doc_type,
                status: request.status.as_deref(),
                sort: request.sort,
                offset,
                limit: request.limit,
            },
        )?;
        let shown = offset + rows.len() as u64;
        Ok(Listing {
            items: rows.iter().map(DocumentInfo::from).collect(),
            total,
            next_start: (shown < total).then_some(shown + 1),
        })
    }

    /// Writes a new revision of an existing document: `edit` changes the
    /// draft, then the file is rewritten (and moved if the title changed).
    fn revise(
        &mut self,
        actor: &Actor,
        id: &str,
        expected: Option<u64>,
        action: Action,
        summary: Option<String>,
        edit: impl FnOnce(&mut Draft, &Settings) -> Result<(), Error>,
    ) -> Result<Changed, Error> {
        let id = parse_id(id)?;
        let now = self.now();
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
        if let Some(expected) = expected
            && expected != row.revision
        {
            return Err(Error::Conflict {
                id: row.id,
                expected,
                current: row.revision,
            });
        }
        let text = read_verified(&paths.home, &row)?;
        let parsed = front_matter::parse(&text).map_err(|_| integrity(&row))?;
        let mut draft = Draft {
            title: row.title.clone(),
            status: row.status.clone(),
            extra: front_matter::extras(&parsed.fields),
            body: parsed.body.to_owned(),
        };
        edit(&mut draft, settings)?;

        let old_path = row.path.clone();
        let old_title = std::mem::replace(&mut row.title, draft.title);
        if row.title != old_title {
            row.path = choose_path(
                &tx,
                &paths.home,
                &row.project,
                &row.doc_type,
                &row.title,
                &row.id,
                Some(&old_path),
            )?;
        }
        row.status = draft.status;
        row.revision += 1;
        row.updated_at = now;
        actor.name().clone_into(&mut row.updated_by);
        actor.client().clone_into(&mut row.client);
        let (row, fields, journal) = write_revision(
            &paths.home,
            settings,
            row,
            &draft.extra,
            &draft.body,
            Some(&old_path),
            |row| db::save_document(&tx, row),
        )?;
        let summary = summary.unwrap_or_else(|| match action {
            Action::Rename => format!("{old_title} → {}", row.title),
            _ => String::new(),
        });
        let changed = Changed {
            id: row.id.clone(),
            revision: row.revision,
            path: row.path.clone(),
            front_matter: fields,
        };
        let commit = Change {
            row: &row,
            action,
            summary: &summary,
            actor,
            paths: &[&old_path, &row.path],
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        Ok(changed)
    }
}

/// Renders `row` with `extra` and `body`, checks the size, saves the row
/// with `save`, and writes the file (moving it from `old_path` if given and
/// different). The returned journal undoes the file changes if it is
/// dropped before [`finish`] completes it.
fn write_revision(
    home: &Path,
    settings: &Settings,
    mut row: DocumentRow,
    extra: &Fields,
    body: &str,
    old_path: Option<&str>,
    save: impl FnOnce(&DocumentRow) -> rusqlite::Result<()>,
) -> Result<(DocumentRow, Fields, Journal), Error> {
    let header = header(&row);
    let text = front_matter::render(&header, extra, body)
        .map_err(|e| Error::invalid("front_matter", e.0))?;
    let size = text.len() as u64;
    if size > settings.max_document_bytes {
        return Err(Error::TooLarge {
            size,
            limit: settings.max_document_bytes,
        });
    }
    row.content_sha256 = sha256(text.as_bytes());
    save(&row)?;

    let new_file = home.join(&row.path);
    let mut journal = Journal::default();
    match old_path {
        Some(old) if old != row.path => {
            let old_file = home.join(old);
            if old.eq_ignore_ascii_case(&row.path) {
                journal.rename(&old_file, &new_file)?;
                journal.write(&new_file, text.as_bytes())?;
            } else {
                journal.write(&new_file, text.as_bytes())?;
                journal.remove(&old_file)?;
            }
        }
        _ => journal.write(&new_file, text.as_bytes())?,
    }
    let fields = front_matter::parse(&text)
        .map(|parsed| parsed.fields)
        .unwrap_or_default();
    Ok((row, fields, journal))
}

/// One revision about to be committed.
struct Change<'a> {
    row: &'a DocumentRow,
    action: Action,
    summary: &'a str,
    actor: &'a Actor,
    paths: &'a [&'a str],
}

/// Commits the change to history, then records the revision and the new
/// history head and commits the transaction. If the history commit fails,
/// the vault files are restored and the transaction rolls back.
fn finish(
    tx: Transaction<'_>,
    history: &dyn History,
    journal: Journal,
    change: &Change<'_>,
) -> Result<(), Error> {
    let Change {
        row,
        action,
        summary,
        actor,
        paths,
    } = *change;
    let subject = if summary.is_empty() {
        format!("{} {}", action.verb(), row.id)
    } else {
        format!("{} {}: {summary}", action.verb(), row.id)
    };
    let message = format!(
        "{subject}\n\nJoeDocs-Document: {}\nJoeDocs-Revision: {}\nJoeDocs-Action: {}\nJoeDocs-Client: {}\n",
        row.id,
        row.revision,
        action.name(),
        actor.client(),
    );
    let mut changed: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    changed.dedup();
    let at = jiff::Timestamp::strptime("%Y-%m-%dT%H:%M:%SZ", &row.updated_at)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let commit_id = history.commit(&Commit {
        paths: changed,
        author: actor.name().to_owned(),
        at,
        message,
    })?;
    journal.complete();
    db::insert_revision(
        &tx,
        &RevisionRow {
            document_id: row.id.clone(),
            revision: row.revision,
            commit_id: commit_id.clone(),
            path: row.path.clone(),
            action: action.name().to_owned(),
            author: actor.name().to_owned(),
            client: actor.client().to_owned(),
            summary: subject,
            created_at: row.updated_at.clone(),
        },
    )?;
    db::set_history_head(&tx, &commit_id)?;
    tx.commit()?;
    Ok(())
}

fn header(row: &DocumentRow) -> Header {
    Header {
        id: row.id.clone(),
        title: row.title.clone(),
        project: row.project.clone(),
        doc_type: row.doc_type.clone(),
        status: row.status.clone(),
        revision: row.revision,
        created: row.created_at.clone(),
        created_by: row.created_by.clone(),
        updated: row.updated_at.clone(),
        updated_by: row.updated_by.clone(),
        client: row.client.clone(),
    }
}

fn apply_front_matter(
    draft: &mut Draft,
    set: &Fields,
    remove: &[String],
    settings: &Settings,
) -> Result<(), Error> {
    front_matter::apply(
        &mut draft.status,
        &mut draft.extra,
        set,
        remove,
        &settings.statuses,
    )
    .map_err(|e| Error::invalid("front_matter", e.0))
}

fn parse_id(id: &str) -> Result<DocumentId, Error> {
    id.parse()
        .map_err(|e: crate::document::InvalidId| Error::invalid("document_id", e.to_string()))
}

fn live_row(conn: &Connection, id: &DocumentId) -> Result<DocumentRow, Error> {
    let id = id.to_string();
    let row = db::get_document(conn, &id)?.ok_or_else(|| Error::NotFound(id.clone()))?;
    if row.archived_at.is_some() {
        return Err(Error::Archived(id));
    }
    Ok(row)
}

fn integrity(row: &DocumentRow) -> Error {
    Error::Integrity {
        id: row.id.clone(),
        path: row.path.clone(),
    }
}

/// Reads the document's file and checks it is the one the index recorded.
fn read_verified(home: &Path, row: &DocumentRow) -> Result<String, Error> {
    let file = home.join(&row.path);
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(integrity(row)),
        Err(error) => return Err(Error::io("read", &file)(error)),
    };
    if sha256(&bytes) != row.content_sha256 {
        return Err(integrity(row));
    }
    String::from_utf8(bytes).map_err(|_| integrity(row))
}

/// Picks `<project>/<type>/<stem>.md`, or `<stem> (<id>).md` when that is
/// taken by another document or by a file the index does not know.
fn choose_path(
    conn: &Connection,
    home: &Path,
    project: &str,
    doc_type: &str,
    title: &str,
    id: &str,
    current: Option<&str>,
) -> Result<String, Error> {
    let stem = file_stem(title);
    for candidate in [
        format!("{project}/{doc_type}/{stem}.md"),
        format!("{project}/{doc_type}/{stem} ({id}).md"),
    ] {
        let ours = current.is_some_and(|c| c.eq_ignore_ascii_case(&candidate));
        if !db::path_taken(conn, &candidate, Some(id))? && (ours || !home.join(&candidate).exists())
        {
            return Ok(candidate);
        }
    }
    Err(Error::AlreadyExists(format!(
        "a file for \"{title}\" in {project}/{doc_type}"
    )))
}

fn check_member(field: &'static str, value: &str, allowed: &[String]) -> Result<(), Error> {
    if allowed.iter().any(|a| a == value) {
        Ok(())
    } else {
        Err(Error::invalid(
            field,
            format!("\"{value}\" is not one of: {}", allowed.join(", ")),
        ))
    }
}

/// One line of at most [`MAX_SUMMARY_CHARS`], or `None` when empty.
fn tidy_line(text: Option<&str>) -> Option<String> {
    let line: String = text?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_SUMMARY_CHARS)
        .collect();
    (!line.is_empty()).then_some(line)
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Removes the type directory, then the project directory, if empty.
fn remove_empty_parent(file: &Path) {
    let mut dir = file.parent();
    for _ in 0..2 {
        let Some(path) = dir else { return };
        if fs::remove_dir(path).is_err() {
            return;
        }
        dir = path.parent();
    }
}
