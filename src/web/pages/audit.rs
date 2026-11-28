//! The Audit page: the audit log, newest first, filtered and paged as
//! `joedocs audit` does, with quick filters for admin calls, refusals, the
//! last 24 hours, and everything but the Board's own rows.

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
const FILTERS: [&str; 8] = [
    "since",
    "until",
    "key",
    "tool",
    "document",
    "outcome",
    "outcome_not",
    "tool_not",
];

/// The tools of the Board's page loads, refreshes and streams: the most
/// frequent rows while a Board is open (JD-47 REQ-11).
const BOARD_TOOLS: &str = "admin:documents.board*";

/// A quick filter, shown as a pill: a link to a preset view, lit when the
/// filters in force are exactly its own.
pub(crate) struct Quick {
    pub(crate) href: String,
    pub(crate) label: &'static str,
    pub(crate) current: bool,
}

/// A filter that leaves rows out (`outcome_not`, `tool_not`), shown as a
/// chip whose x is a link to the same view without it.
pub(crate) struct Chip {
    pub(crate) facet: &'static str,
    pub(crate) text: String,
    /// The address without this filter.
    pub(crate) remove: String,
    /// The x's accessible name.
    pub(crate) label: String,
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
    chips: Vec<Chip>,
    /// The page size, kept in the form when it is not the default.
    limit: Option<u64>,
    results: AuditResults,
}

/// The results region, also sent alone to htmx when a filter changes.
#[derive(Template)]
#[template(path = "audit_results.html")]
struct AuditResults {
    error: Option<String>,
    rows: Vec<AuditRow>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
}

impl AuditPage<'_> {
    /// The page, or for htmx only its results region.
    fn reply(&self, status: StatusCode, fragment: bool) -> Response {
        if fragment {
            render(status, &self.results)
        } else {
            render(status, self)
        }
    }

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
        quick: quick_filters(store, &values),
        chips: Vec::new(),
        limit: None,
        values: values.clone(),
        results: AuditResults {
            error: None,
            rows: Vec::new(),
            total: 0,
            start: 1,
            shown_to: 0,
            previous: None,
            next: None,
        },
    };
    let fragment = request.wants_fragment();
    let numbers = number(request, "start", 1).and_then(|start| {
        number(request, "limit", DEFAULT_AUDIT_LIMIT).map(|limit| (start, limit))
    });
    let (start, limit) = match numbers {
        Ok(numbers) => numbers,
        Err(refused) => {
            page.results.error = Some(refused.message.clone());
            let shown = page.reply(refused.status, fragment);
            return Err(refused.with_page(shown));
        }
    };
    page.limit = (limit != DEFAULT_AUDIT_LIMIT).then_some(limit);
    page.chips = chips(&values, page.limit);
    let found = store.audit_entries(&AuditRequest {
        since: given("since"),
        until: given("until"),
        key: given("key"),
        tool: given("tool"),
        document: given("document"),
        outcome: given("outcome"),
        outcome_not: given("outcome_not"),
        tool_not: given("tool_not"),
        start,
        limit,
    });
    match found {
        Ok(found) => {
            let shown = found.items.len() as u64;
            let results = &mut page.results;
            results.total = found.total;
            results.start = start;
            results.shown_to = start + shown - u64::from(shown > 0);
            results.previous = (start > 1).then(|| url(start.saturating_sub(limit).max(1), limit));
            results.next = found.next_start.map(|next| url(next, limit));
            results.rows = found.items;
            Ok(page.reply(StatusCode::OK, fragment))
        }
        Err(error) => {
            let refused = WebError::from_store(&error);
            page.results.error = Some(refused.message.clone());
            let shown = page.reply(refused.status, fragment);
            Err(refused.with_page(shown))
        }
    }
}

/// The chips of the filters that leave rows out, each removed by a link
/// that keeps every other filter and the page size.
fn chips(values: &[(&'static str, &str)], limit: Option<u64>) -> Vec<Chip> {
    let limit = limit.map(|l| l.to_string());
    let without = |key: &str| {
        let mut pairs: Vec<(&str, Option<&str>)> = values
            .iter()
            .filter(|(k, _)| *k != key)
            .map(|(k, v)| (*k, Some(*v)))
            .collect();
        pairs.push(("limit", limit.as_deref()));
        link("/admin/audit", &pairs)
    };
    let value = |key: &str| {
        values
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .filter(|v| !v.is_empty())
    };
    let mut chips = Vec::new();
    if let Some(outcome) = value("outcome_not") {
        chips.push(Chip {
            facet: "Outcome",
            text: format!("not {outcome}"),
            remove: without("outcome_not"),
            label: format!("Remove the filter: outcomes other than {outcome}"),
        });
    }
    if let Some(tool) = value("tool_not") {
        chips.push(Chip {
            facet: "Tool",
            text: format!("not {tool}"),
            remove: without("tool_not"),
            label: format!("Remove the filter: leaving out tools matching {tool}"),
        });
    }
    chips
}

/// All, admin calls, refusals, everything but the Board's rows, and the
/// last 24 hours by the store's clock; the one matching the filters in
/// force (`values`) exactly is current.
fn quick_filters(store: &Store, values: &[(&'static str, &str)]) -> Vec<Quick> {
    let day_ago = store
        .now()
        .parse::<jiff::Timestamp>()
        .ok()
        .and_then(|now| now.checked_sub(jiff::SignedDuration::from_hours(24)).ok())
        .map(crate::clock::format_timestamp);
    let in_force: Vec<(&str, &str)> = values
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (*k, *v))
        .collect();
    let quick = |label: &'static str, pair: Option<(&str, &str)>| Quick {
        href: link(
            "/admin/audit",
            &pair
                .map(|(k, v)| (k, Some(v)))
                .into_iter()
                .collect::<Vec<_>>(),
        ),
        label,
        current: in_force == pair.into_iter().collect::<Vec<_>>(),
    };
    let mut quicks = vec![
        quick("All", None),
        quick("Admin calls", Some(("tool", "admin:*"))),
        quick("Refusals", Some(("outcome_not", "ok"))),
        quick("Leave out Board", Some(("tool_not", BOARD_TOOLS))),
    ];
    if let Some(since) = day_ago {
        quicks.push(quick("Last 24 hours", Some(("since", &since))));
    }
    quicks
}
