//! Rewriting wiki links to a renamed document (`rename_document` with
//! `rewrite_links`): every live file of every other live document in its
//! project whose `[[Old title]]` links resolve to it. The rename and the
//! rewrites are one history commit; each rewritten document gets a
//! `relink` revision, named by a [`RELINKED_TRAILER`] (and, for a
//! multi-file document, a [`RELINKED_FILE_TRAILER`] per file).

use std::collections::HashMap;
use std::fmt::Write as _;

use indexmap::IndexMap;

use super::documents::{Action, Changed, FileTouch, integrity, read_verified, write_revision};
use super::file_changes::{FileDraft, bump, render, save, verify_all};
use super::files::{main_stem, read_verified_file, vault_path};
use super::journal::Journal;
use super::{Actor, Error, Settings, index_file};
use crate::db::{self, Connection, DocumentRow, FileChange, FileRow, RevisionFileRow, RevisionRow};
use crate::document::front_matter::{self, Fields};
use crate::document::links::{replace_ranges, wiki_pages};

/// The trailer naming each document a rename rewrote links in, with its
/// new revision: `JoeDocs-Relinked: JD-7 3`.
pub(super) const RELINKED_TRAILER: &str = "JoeDocs-Relinked";

/// The trailer naming each rewritten file of a multi-file document, its
/// main file included: `JoeDocs-Relinked-File: JD-7 items/one.md`.
pub(super) const RELINKED_FILE_TRAILER: &str = "JoeDocs-Relinked-File";

/// How [`super::Store::rename_with`] renames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenameOptions {
    /// Rewrite the wiki links to the old title in the project's other
    /// live documents.
    pub rewrite_links: bool,
}

/// A document whose links a rename rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relinked {
    /// Its ID.
    pub document_id: String,
    /// Its new revision.
    pub revision: u64,
}

/// What [`super::Store::rename_with`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Renamed {
    /// The renamed document.
    pub changed: Changed,
    /// The documents whose links were rewritten, in document order; empty
    /// without `rewrite_links` or when no link was found.
    pub relinked: Vec<Relinked>,
}

/// The summary of a `relink` revision.
pub(super) fn relink_summary(renamed: &str) -> String {
    format!("Rewrite links to renamed {renamed}")
}

/// A document with links to rewrite, read and checked before anything is
/// written.
pub(super) struct Plan {
    row: DocumentRow,
    /// The main file's front matter and new body, when it has links.
    main: Option<(Fields, String)>,
    /// The sub-files with links, with their new drafts, by path.
    files: Vec<(FileRow, FileDraft)>,
}

/// One rewritten document, for the commit.
pub(super) struct Written {
    /// Its row at the new revision.
    pub(super) row: DocumentRow,
    /// For a multi-file document, the files rewritten.
    pub(super) files: Vec<FileTouch>,
    /// The vault paths written.
    pub(super) vaults: Vec<String>,
}

/// Finds the links to `renamed` (still under its old title) that a rename
/// to `new_title` rewrites, and checks every file of each document that
/// has one against the index. Changes nothing.
///
/// Candidates come from the search index (the old title's words in a
/// row); each is then parsed as the web interface parses it, and a link
/// counts when its page is the old title and resolves to `renamed`.
pub(super) fn plan(
    conn: &Connection,
    home: &std::path::Path,
    renamed: &DocumentRow,
    new_title: &str,
) -> Result<Vec<Plan>, Error> {
    let old = renamed.title.as_str();
    let words: Vec<&str> = old
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut candidates: IndexMap<String, Vec<Option<String>>> = IndexMap::new();
    for (id, file) in db::files_mentioning(conn, renamed.project_id, &renamed.id, &words)? {
        candidates.entry(id).or_default().push(file);
    }
    let mut resolves: HashMap<String, bool> = HashMap::new();
    let mut links_here = |page: &str| -> Result<bool, Error> {
        // `find_by_title` compares titles with SQLite's NOCASE (ASCII).
        if !page.eq_ignore_ascii_case(old) {
            return Ok(false);
        }
        if let Some(known) = resolves.get(page) {
            return Ok(*known);
        }
        let found = db::find_by_title(conn, &renamed.project, page)?.as_deref()
            == Some(renamed.id.as_str());
        resolves.insert(page.to_owned(), found);
        Ok(found)
    };
    let mut plans = Vec::new();
    for (id, files) in candidates {
        let row = db::get_document(conn, &id)?.ok_or_else(|| Error::NotFound(id.clone()))?;
        let live = if row.multifile {
            db::live_files(conn, &row.id)?
        } else {
            Vec::new()
        };
        let mut plan = Plan {
            row,
            main: None,
            files: Vec::new(),
        };
        for file in files {
            match file {
                None => {
                    let text = read_verified(home, &plan.row)?;
                    let parsed = front_matter::parse(&text).map_err(|_| integrity(&plan.row))?;
                    if let Some(body) = rewrite(parsed.body, new_title, &mut links_here)? {
                        plan.main = Some((front_matter::extras(&parsed.fields), body));
                    }
                }
                Some(path) => {
                    let Some(file) = live.iter().find(|f| f.path == path) else {
                        continue;
                    };
                    let text = read_verified_file(home, &plan.row, file)?;
                    let mut draft = FileDraft::of(&text).ok_or_else(|| integrity(&plan.row))?;
                    if let Some(body) = rewrite(&draft.body, new_title, &mut links_here)? {
                        draft.body = body;
                        plan.files.push((file.clone(), draft));
                    }
                }
            }
        }
        if plan.main.is_none() && plan.files.is_empty() {
            continue;
        }
        // Nothing is written over a file someone changed by hand.
        if plan.row.multifile {
            verify_all(home, &plan.row, &live)?;
        } else {
            read_verified(home, &plan.row)?;
        }
        plans.push(plan);
    }
    Ok(plans)
}

/// `body` with the page of each link `links_here` accepts set to
/// `new_title`, or `None` when it accepts none.
fn rewrite(
    body: &str,
    new_title: &str,
    links_here: &mut impl FnMut(&str) -> Result<bool, Error>,
) -> Result<Option<String>, Error> {
    let mut ranges = Vec::new();
    for range in wiki_pages(body) {
        if links_here(&body[range.clone()])? {
            ranges.push(range);
        }
    }
    Ok((!ranges.is_empty()).then(|| replace_ranges(body, &ranges, new_title)))
}

/// Writes each planned document at a new revision by `actor` at `now`:
/// its rewritten files, their index rows and search entries. The returned
/// journal undoes the file writes if it is dropped before the commit.
///
/// # Errors
///
/// A rewritten file over `server.max_document_bytes` is
/// [`Error::Invalid`] on `rewrite_links`, naming the document.
pub(super) fn write(
    conn: &Connection,
    home: &std::path::Path,
    settings: &Settings,
    actor: &Actor,
    now: &str,
    plans: Vec<Plan>,
) -> Result<(Journal, Vec<Written>), Error> {
    let mut journal = Journal::default();
    let mut written = Vec::with_capacity(plans.len());
    for Plan {
        mut row,
        main,
        files,
    } in plans
    {
        let id = row.id.clone();
        let too_large = |error: Error| match error {
            Error::TooLarge { size, limit } => Error::invalid(
                "rewrite_links",
                format!(
                    "rewriting the links in {id} would make a file {size} bytes, over the limit of {limit} (server.max_document_bytes)"
                ),
            ),
            other => other,
        };
        let mut touches = Vec::new();
        let mut vaults = Vec::new();
        if let Some((extra, body)) = main {
            row.revision += 1;
            row.main_revision = row.revision;
            now.clone_into(&mut row.updated_at);
            actor.name().clone_into(&mut row.updated_by);
            actor.client().clone_into(&mut row.client);
            let path = row.path.clone();
            let (new_row, _, main_journal) =
                write_revision(conn, home, settings, row, &extra, &body, Some(&path))
                    .map_err(too_large)?;
            row = new_row;
            journal = journal.then(main_journal);
            vaults.push(path);
            if row.multifile {
                touches.push(FileTouch {
                    path: main_stem(&row).to_owned(),
                    main: true,
                    change: FileChange::Modified,
                    from: None,
                });
            }
        } else {
            bump(conn, &mut row, actor, now)?;
        }
        for (mut file, draft) in files {
            let text = render(&row, now, &draft, settings).map_err(too_large)?;
            save(conn, &row, actor, &mut file, &text)?;
            let vault = vault_path(&row, &file.path);
            journal.write(&home.join(&vault), text.as_bytes())?;
            touches.push(FileTouch {
                path: file.path,
                main: false,
                change: FileChange::Modified,
                from: None,
            });
            vaults.push(vault);
        }
        written.push(Written {
            row,
            files: touches,
            vaults,
        });
    }
    Ok((journal, written))
}

/// Adds the trailers naming each rewritten document and file to a commit
/// message.
pub(super) fn write_trailers(message: &mut String, relinked: &[Written]) {
    for written in relinked {
        let id = &written.row.id;
        let _ = writeln!(message, "{RELINKED_TRAILER}: {id} {}", written.row.revision);
        for touch in &written.files {
            let _ = writeln!(message, "{RELINKED_FILE_TRAILER}: {id} {}.md", touch.path);
        }
    }
}

/// Records the `relink` revision of each rewritten document, in commit
/// `commit_id` of the rename of `renamed`.
pub(super) fn record(
    conn: &Connection,
    relinked: &[Written],
    commit_id: &str,
    actor: &Actor,
    renamed: &str,
) -> Result<(), Error> {
    for written in relinked {
        let row = &written.row;
        db::insert_revision(
            conn,
            &RevisionRow {
                document_id: row.id.clone(),
                revision: row.revision,
                commit_id: commit_id.to_owned(),
                path: row.path.clone(),
                action: Action::Relink.name().to_owned(),
                author: actor.name().to_owned(),
                client: actor.client().to_owned(),
                summary: relink_summary(renamed),
                created_at: row.updated_at.clone(),
            },
        )?;
        for touch in &written.files {
            db::insert_revision_file(
                conn,
                &RevisionFileRow {
                    document_id: row.id.clone(),
                    revision: row.revision,
                    path: touch.path.clone(),
                    main: touch.main,
                    change: touch.change,
                    from_path: None,
                },
            )?;
        }
    }
    Ok(())
}

/// After the commit: rewrites the `INDEX.md` files of each rewritten
/// document, and says which they were.
pub(super) fn refreshed(
    conn: &Connection,
    home: &std::path::Path,
    settings: &Settings,
    relinked: Vec<Written>,
) -> Vec<Relinked> {
    relinked
        .into_iter()
        .map(|written| {
            index_file::refresh_document(conn, home, settings, &written.row);
            Relinked {
                document_id: written.row.id,
                revision: written.row.revision,
            }
        })
        .collect()
}
