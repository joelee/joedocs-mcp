//! The Archive page: archived documents, filtered by project and type as
//! `joedocs archive list` does, and restoring one as `archive restore`
//! does, by the signed-in admin.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, redirect, render};
use super::documents::Choice;
use super::{Chrome, Section, actor, link, refused_with_link};
use crate::store::{ArchivedInfo, Store};

#[derive(Template)]
#[template(path = "archive.html")]
struct ArchivePage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    projects: Vec<Choice>,
    types: Vec<Choice>,
    /// What the last action did, when the URL says.
    notice: Option<String>,
    results: ArchiveResults,
}

/// The results region, also sent alone to htmx when a filter changes.
#[derive(Template)]
#[template(path = "archive_results.html")]
struct ArchiveResults {
    items: Vec<ArchivedInfo>,
    /// The session's CSRF token, for the restore forms.
    csrf: String,
}

/// `GET /admin/archive`, with `project` and `type`.
pub(crate) fn list(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let project = request.query("project").filter(|p| !p.is_empty());
    let doc_type = request.query("type").filter(|t| !t.is_empty());
    let items = store
        .archived(project, doc_type)
        .map_err(|e| WebError::from_store(&e))?;
    let names: Vec<String> = store
        .projects()
        .map_err(|e| WebError::from_store(&e))?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let choices = |values: &[String], current: Option<&str>, all: &str| -> Vec<Choice> {
        std::iter::once(Choice {
            value: String::new(),
            label: all.to_owned(),
            selected: current.is_none(),
        })
        .chain(values.iter().map(|v| Choice {
            value: v.clone(),
            label: v.clone(),
            selected: current == Some(v.as_str()),
        }))
        .collect()
    };
    let results = ArchiveResults {
        items,
        csrf: session.csrf.clone(),
    };
    if request.wants_fragment() {
        return Ok(render(StatusCode::OK, &results));
    }
    let page = ArchivePage {
        title: "Archive",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Archive, ""),
        projects: choices(&names, project, "All projects"),
        types: choices(&store.settings().types, doc_type, "All types"),
        notice: notice(request),
        results,
    };
    Ok(render(StatusCode::OK, &page))
}

/// The notice for `done=archived` and the archived `id`.
fn notice(request: &WebRequest) -> Option<String> {
    if request.query("done")? != "archived" {
        return None;
    }
    let id = request
        .query("id")?
        .parse::<crate::document::DocumentId>()
        .ok()?;
    Some(format!(
        "Archived {id}. Restore it from the list below to bring it back as a new revision."
    ))
}

/// `POST /admin/archive/{id}/restore`: back to the document, which reports
/// the new revision.
pub(crate) fn restore(
    store: &mut Store,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let restored = store
        .unarchive(&actor(session), id)
        .map_err(|e| refused_with_link(&e, "/admin/archive", "Back to the Archive"))?;
    Ok(redirect(&link(
        &format!("/admin/documents/{}", restored.id),
        &[("done", Some("unarchived"))],
    )))
}
