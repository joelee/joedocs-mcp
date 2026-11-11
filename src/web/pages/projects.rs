//! The Projects page: every project with its key, title and live and
//! archived document counts, and adding one with the rules and messages of
//! `joedocs project add`.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, redirect, render};
use super::{Chrome, Section, link};
use crate::store::Store;

/// One project in the table.
pub(crate) struct ProjectLine {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) live: u64,
    pub(crate) archived: u64,
}

/// What the Add project form holds.
#[derive(Default)]
pub(crate) struct AddForm {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) title: String,
}

#[derive(Template)]
#[template(path = "projects.html")]
struct ProjectsPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    projects: Vec<ProjectLine>,
    notice: Option<String>,
    error: Option<String>,
    form: AddForm,
    /// Whether the Add project dialog opens with the page (after a refusal).
    open: bool,
}

/// `GET /admin/projects`.
pub(crate) fn list(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let notice = match request.query("added").filter(|n| !n.is_empty()) {
        Some(name) => store
            .project(name)
            .map_err(|e| WebError::from_store(&e))?
            .map(|p| format!("Added project {} (key {}): {}", p.name, p.key, p.title)),
        None => None,
    };
    page(
        store,
        session,
        StatusCode::OK,
        notice,
        None,
        AddForm::default(),
    )
}

/// `POST /admin/projects`: `name`, `key` and an optional `title`.
pub(crate) fn add(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let form = AddForm {
        name: request.form("name").unwrap_or("").trim().to_owned(),
        key: request.form("key").unwrap_or("").trim().to_owned(),
        title: request.form("title").unwrap_or("").trim().to_owned(),
    };
    let title = Some(form.title.as_str()).filter(|t| !t.is_empty());
    match store.add_project(&form.name, &form.key, title) {
        Ok(project) => Ok(redirect(&link(
            "/admin/projects",
            &[("added", Some(&project.name))],
        ))),
        // The form again with the store's message, kept as the error so
        // the request is audited with its code.
        Err(error) => {
            let refused = WebError::from_store(&error);
            let shown = page(
                store,
                session,
                refused.status,
                None,
                Some(refused.message.clone()),
                form,
            )?;
            Err(refused.with_page(shown))
        }
    }
}

fn page(
    store: &Store,
    session: &Session,
    status: StatusCode,
    notice: Option<String>,
    error: Option<String>,
    form: AddForm,
) -> Result<Response, WebError> {
    let projects = store.projects().map_err(|e| WebError::from_store(&e))?;
    let counts = store
        .summary()
        .map_err(|e| WebError::from_store(&e))?
        .projects;
    let archived = |name: &str| {
        counts
            .iter()
            .find(|c| c.name == name)
            .map_or(0, |c| c.archived)
    };
    let page = ProjectsPage {
        title: "Projects",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Projects, ""),
        projects: projects
            .into_iter()
            .map(|p| ProjectLine {
                archived: archived(&p.name),
                live: p.documents,
                name: p.name,
                key: p.key,
                title: p.title,
            })
            .collect(),
        notice,
        open: error.is_some(),
        error,
        form,
    };
    Ok(render(status, &page))
}
