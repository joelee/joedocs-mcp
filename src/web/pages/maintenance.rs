//! The Maintenance page: `check` with the advice the command prints and
//! the problem count; `reindex` after a confirmation, with its report; a
//! backup download (handled in `web::backup`, which shares the admin API's
//! code); and the commands that run only on the server.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, render};
use super::{Chrome, Section};
use crate::store::Store;

/// The deployment guide, for the commands that run only on the server.
const DEPLOYMENT_GUIDE: &str = "https://github.com/joelee/joedocs-mcp/blob/main/docs/deployment.md";

/// What `check` found.
pub(crate) struct CheckResult {
    pub(crate) problems: usize,
    /// The report as `joedocs check` prints it.
    pub(crate) text: String,
}

#[derive(Template)]
#[template(path = "maintenance.html")]
struct MaintenancePage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    /// Live and archived documents, which a backup holds.
    documents: u64,
    check: Option<CheckResult>,
    /// The report as `joedocs reindex` prints it.
    reindex: Option<String>,
    guide: &'static str,
}

/// `GET /admin/maintenance`.
pub(crate) fn page(store: &Store, session: &Session) -> Result<Response, WebError> {
    show(store, session, None, None)
}

/// `POST /admin/maintenance/check`.
pub(crate) fn check(store: &Store, session: &Session) -> Result<Response, WebError> {
    let report = store.check().map_err(|e| WebError::from_store(&e))?;
    let mut text = Vec::new();
    crate::admin::report_check(&report, store.paths(), &mut text)
        .map_err(|e| WebError::from_crate(&e))?;
    let result = CheckResult {
        problems: report.problems(),
        text: String::from_utf8_lossy(&text).into_owned(),
    };
    show(store, session, Some(result), None)
}

/// `POST /admin/maintenance/reindex`.
pub(crate) fn reindex(store: &mut Store, session: &Session) -> Result<Response, WebError> {
    let report = store.reindex().map_err(|e| WebError::from_store(&e))?;
    let mut text = Vec::new();
    crate::admin::report_reindex(&report, &mut text).map_err(|e| WebError::from_crate(&e))?;
    let text = String::from_utf8_lossy(&text).into_owned();
    show(store, session, None, Some(text))
}

fn show(
    store: &Store,
    session: &Session,
    check: Option<CheckResult>,
    reindex: Option<String>,
) -> Result<Response, WebError> {
    let summary = store.summary().map_err(|e| WebError::from_store(&e))?;
    let page = MaintenancePage {
        title: "Maintenance",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Maintenance, ""),
        documents: summary.projects.iter().map(|p| p.live + p.archived).sum(),
        check,
        reindex,
        guide: DEPLOYMENT_GUIDE,
    };
    Ok(render(StatusCode::OK, &page))
}
