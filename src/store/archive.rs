//! Archived documents: listing them and bringing them back (admin only).

use super::documents::{
    Action, Change, Changed, DocumentInfo, choose_path, finish, integrity, parse_id,
    revision_with_content, write_revision,
};
use super::{Actor, Error, Store, begin_write, index_file};
use crate::clock::format_timestamp;
use crate::db;
use crate::document::front_matter;

/// An archived document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedInfo {
    /// The document as it was archived.
    pub document: DocumentInfo,
    /// When it was archived.
    pub archived_at: String,
    /// Key name that archived it.
    pub archived_by: String,
    /// Why.
    pub reason: String,
}

impl Store {
    /// Archived documents, most recently archived first, of one project or
    /// all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProjectNotFound`] and database errors.
    pub fn archived(&self, project: Option<&str>) -> Result<Vec<ArchivedInfo>, Error> {
        let project_id = project
            .map(|name| {
                db::find_project(&self.conn, name)?
                    .filter(|p| p.name == name)
                    .map(|p| p.id)
                    .ok_or_else(|| Error::ProjectNotFound(name.to_owned()))
            })
            .transpose()?;
        Ok(db::archived_documents(&self.conn, project_id)?
            .into_iter()
            .map(|row| ArchivedInfo {
                document: DocumentInfo::from(&row),
                archived_at: row.archived_at.unwrap_or_default(),
                archived_by: row.archived_by.unwrap_or_default(),
                reason: row.archive_reason.unwrap_or_default(),
            })
            .collect())
    }

    /// Brings an archived document back as it was before it was archived,
    /// at its old path or, if that is taken, with its ID appended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`], [`Error::Invalid`] when the document is
    /// not archived, and storage errors.
    pub fn unarchive(&mut self, actor: &Actor, id: &str) -> Result<Changed, Error> {
        let id = parse_id(id)?.to_string();
        let at = self.clock.now();
        let now = format_timestamp(at);
        let Self {
            paths,
            settings,
            conn,
            history,
            ..
        } = self;
        let _lock = begin_write(paths, conn, history.as_ref())?;
        let tx = conn.transaction()?;
        let mut row = db::get_document(&tx, &id)?.ok_or_else(|| Error::NotFound(id.clone()))?;
        if row.archived_at.is_none() {
            return Err(Error::invalid(
                "document_id",
                format!("{id} is not archived"),
            ));
        }
        let before = revision_with_content(&tx, &row, row.revision - 1, "document_id")?;
        let bytes = history.show(&before.commit_id, &before.path)?;
        let text = String::from_utf8(bytes).map_err(|_| integrity(&row))?;
        let parsed = front_matter::parse(&text).map_err(|_| integrity(&row))?;
        let extra = front_matter::extras(&parsed.fields);

        let (path, files) = place(&tx, &paths.home, history.as_ref(), &row, &before)?;
        row.path = path;
        row.revision += 1;
        row.main_revision = row.revision;
        row.updated_at = now;
        actor.name().clone_into(&mut row.updated_by);
        actor.client().clone_into(&mut row.client);
        row.archived_at = None;
        row.archived_by = None;
        row.archive_reason = None;
        let path = row.path.clone();
        let (row, fields, journal) = write_revision(
            &tx,
            &paths.home,
            settings,
            row,
            &extra,
            parsed.body,
            Some(&path),
        )?;
        let mut pending = super::folders::Pending {
            journal,
            touched: super::documents::main_touch(&row, crate::db::FileChange::Added),
            paths: vec![row.path.clone()],
        };
        let scope = super::folders::Scope {
            conn: &tx,
            home: &paths.home,
            settings,
            row: &row,
            actor,
        };
        super::folders::add_files(&scope, files.into_values().collect(), &mut pending)?;
        let super::folders::Pending {
            journal,
            touched,
            paths: changed,
        } = pending;
        let changed_paths: Vec<&str> = changed.iter().map(String::as_str).collect();
        let changed = Changed {
            id: row.id.clone(),
            revision: row.revision,
            path: row.path.clone(),
            file: None,
            front_matter: fields,
        };
        let commit = Change {
            row: &row,
            at,
            action: Action::Unarchive,
            summary: "",
            actor,
            paths: &changed_paths,
            files: &touched,
        };
        finish(tx, history.as_ref(), journal, &commit)?;
        index_file::refresh_document(conn, &paths.home, settings, &row);
        Ok(changed)
    }
}

/// Where an archived document comes back (as `choose_path` picks it, never
/// a folder named like one of its returning files), and, for a multi-file
/// document, the files that come back with it.
fn place(
    tx: &crate::db::Connection,
    home: &std::path::Path,
    history: &dyn crate::history::History,
    row: &crate::db::DocumentRow,
    before: &crate::db::RevisionRow,
) -> Result<(String, super::folders::FilesThen), Error> {
    let files = if row.multifile {
        super::folders::files_then(tx, history, row, before)?
    } else {
        std::collections::BTreeMap::new()
    };
    let own = super::documents::top_level_stems(files.values().map(|(p, _)| p.as_str()));
    let layout = if row.multifile {
        super::documents::Layout::Folder(&own)
    } else {
        super::documents::Layout::Single
    };
    let path = choose_path(
        tx,
        home,
        &row.project,
        &row.doc_type,
        &row.title,
        &row.id,
        None,
        &layout,
    )?;
    Ok((path, files))
}
