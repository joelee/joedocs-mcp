//! The Audit page: the audit log, newest first, filtered and paged as
//! `joedocs audit` does, with quick filters for admin calls, refusals and
//! the last 24 hours.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::documents::number;
use super::{Chrome, Section, link};
use crate::cli::DEFAULT_AUDIT_LIMIT;
use crate::db::AuditRow;
use crate::store::{AuditRequest, Store};

/// The query keys of the filters, in the order links carry them.
const FILTERS: [&str; 7] = [
    "since",
    "until",
    "key",
    "tool",
    "document",
    "outcome",
    "outcome_not",
];

/// A quick filter link.
pub(crate) struct Quick {
    pub(crate) href: String,
    pub(crate) label: &'static str,
}

#[derive(Template)]
#[template(path = "audit.html")]
struct AuditPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    /// The filter form's values, by key.
    values: Vec<(&'static str, &'a str)>,
    quick: Vec<Quick>,
    error: Option<String>,
    rows: Vec<AuditRow>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
    /// The page size, kept in the form when it is not the default.
    limit: Option<u64>,
}

impl AuditPage<'_> {
    /// The form's value for `key`.
    fn value(&self, key: &str) -> &str {
        self.values
            .iter()
            .find(|(k, _)| *k == key)
            .map_or("", |(_, v)| v)
    }
}

/// `GET /admin/audit`.
/// A refused filter shows the page with the store's message and is the
/// error, so the request is audited with its code.
pub(crate) fn page(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let values: Vec<(&'static str, &str)> = FILTERS
        .iter()
        .map(|key| (*key, request.query(key).unwrap_or("").trim()))
        .collect();
    let given = |key: &str| {
        values
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let url = |start: u64, limit: u64| {
        let start = (start != 1).then(|| start.to_string());
        let limit = (limit != DEFAULT_AUDIT_LIMIT).then(|| limit.to_string());
        let mut pairs: Vec<(&str, Option<&str>)> =
            values.iter().map(|(k, v)| (*k, Some(*v))).collect();
        pairs.push(("start", start.as_deref()));
        pairs.push(("limit", limit.as_deref()));
        link("/admin/audit", &pairs)
    };
    let mut page = AuditPage {
        title: "Audit",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Audit, ""),
        quick: quick_filters(store),
        error: None,
        rows: Vec::new(),
        total: 0,
        start: 1,
        shown_to: 0,
        previous: None,
        next: None,
        limit: None,
        values: values.clone(),
    };
    let numbers = number(request, "start", 1).and_then(|start| {
        number(request, "limit", DEFAULT_AUDIT_LIMIT).map(|limit| (start, limit))
    });
    let (start, limit) = match numbers {
        Ok(numbers) => numbers,
        Err(refused) => {
            page.error = Some(refused.message.clone());
            let shown = render(refused.status, &page);
            return Err(refused.with_page(shown));
        }
    };
    page.limit = (limit != DEFAULT_AUDIT_LIMIT).then_some(limit);
    let found = store.audit_entries(&AuditRequest {
        since: given("since"),
        until: given("until"),
        key: given("key"),
        tool: given("tool"),
        document: given("document"),
        outcome: given("outcome"),
        outcome_not: given("outcome_not"),
        start,
        limit,
    });
    match found {
        Ok(found) => {
            let shown = found.items.len() as u64;
            page.total = found.total;
            page.start = start;
            page.shown_to = start + shown - u64::from(shown > 0);
            page.previous = (start > 1).then(|| url(start.saturating_sub(limit).max(1), limit));
            page.next = found.next_start.map(|next| url(next, limit));
            page.rows = found.items;
            Ok(render(StatusCode::OK, &page))
        }
        Err(error) => {
            let refused = WebError::from_store(&error);
            page.error = Some(refused.message.clone());
            let shown = render(refused.status, &page);
            Err(refused.with_page(shown))
        }
    }
}

/// Admin calls, refusals, and the last 24 hours by the store's clock.
fn quick_filters(store: &Store) -> Vec<Quick> {
    let day_ago = store
        .now()
        .parse::<jiff::Timestamp>()
        .ok()
        .and_then(|now| now.checked_sub(jiff::SignedDuration::from_hours(24)).ok())
        .map(crate::clock::format_timestamp);
    let mut quick = vec![
        Quick {
            href: link("/admin/audit", &[("tool", Some("admin:*"))]),
            label: "Admin calls",
        },
        Quick {
            href: link("/admin/audit", &[("outcome_not", Some("ok"))]),
            label: "Refusals",
        },
    ];
    if let Some(since) = day_ago {
        quick.push(Quick {
            href: link("/admin/audit", &[("since", Some(&since))]),
            label: "Last 24 hours",
        });
    }
    quick
}
