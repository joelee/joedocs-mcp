//! The Overview: what `joedocs status` reports, the projects and the latest
//! audit entries, each linking to its workspace.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebState, render};
use super::{Chrome, Section};
use crate::db::AuditRow;
use crate::status::{self, Health, StatusReport};
use crate::store::{AuditRequest, Store};

/// How many audit entries the Overview shows.
const RECENT: u64 = 10;

#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    report: &'a StatusReport,
    health: String,
    health_ok: bool,
    vault: String,
    state: String,
    database: String,
    history: String,
    audit: Vec<AuditRow>,
}

/// `GET /admin/`.
pub(crate) fn page(
    store: &Store,
    state: &WebState,
    session: &Session,
) -> Result<Response, WebError> {
    let report = status::collect_parts(
        store,
        state.config_file.clone(),
        state.bind,
        true,
        true,
        &crate::systemd::Systemctl,
    )
    .map_err(|error| match error {
        crate::Error::Store(error) => WebError::from_store(&error),
        other => {
            tracing::error!(error = %other, "cannot gather the status for the Overview");
            WebError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    })?;
    let audit = store
        .audit_entries(&AuditRequest {
            start: 1,
            limit: RECENT,
            ..AuditRequest::default()
        })
        .map_err(|e| WebError::from_store(&e))?
        .items;
    let (health, health_ok) = match &report.health {
        Health::Ok => ("ok".to_owned(), true),
        Health::Unexpected(first) => (format!("unexpected answer ({first})"), false),
        Health::Unreachable(error) => (format!("not reachable ({error})"), false),
    };
    let size = |d: &status::DirSize| format!("{} in {} files", status::human(d.bytes), d.files);
    let page = OverviewPage {
        title: "Overview",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Overview, ""),
        health,
        health_ok,
        vault: size(&report.vault),
        state: size(&report.state),
        database: status::human(report.database_bytes),
        history: size(&report.history),
        audit,
        report: &report,
    };
    Ok(render(StatusCode::OK, &page))
}
