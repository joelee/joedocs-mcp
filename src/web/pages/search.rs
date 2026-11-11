//! The Search page: `Store::search` over every project or one. The request
//! is built as `joedocs search` builds it (`browse::SearchSpec::from_args`),
//! so `status`, each `key=value` filter and the JSON filters merge and fail
//! as on the command line. An htmx request for part of the page gets only
//! the results region.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::document::href;
use super::documents::{Choice, number};
use super::{Chrome, Section};
use crate::browse::{self, SearchSpec};
use crate::cli::{DEFAULT_SEARCH_LIMIT, SearchArgs};
use crate::store::{SearchHit, Store};

/// One hit as the page shows it.
pub(crate) struct Hit {
    pub(crate) href: String,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) file: Option<String>,
    pub(crate) project: String,
    pub(crate) doc_type: String,
    pub(crate) status: String,
    pub(crate) updated: String,
    /// The snippet in pieces, each marked as a match or not; the template
    /// escapes every piece.
    pub(crate) snippet: Vec<(bool, String)>,
}

/// The results region, also sent alone to htmx.
#[derive(Template)]
#[template(path = "search_results.html")]
struct Results {
    /// Whether anything was asked for.
    searched: bool,
    error: Option<String>,
    hits: Vec<Hit>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    query: &'a str,
    projects: Vec<Choice>,
    types: Vec<Choice>,
    statuses: Vec<Choice>,
    /// The `key=value` filters, one input each, then an empty one.
    filter_values: Vec<&'a str>,
    filters_json: &'a str,
    /// The page size, kept in the form when it is not the default.
    limit: Option<u64>,
    results: Results,
}

/// What the page asks for, from its query.
struct Asked<'a> {
    query: &'a str,
    project: &'a str,
    doc_type: &'a str,
    status: &'a str,
    filter: Vec<&'a str>,
    filters: &'a str,
}

impl<'a> Asked<'a> {
    fn from(request: &'a WebRequest) -> Self {
        let text = |key| request.query(key).unwrap_or("").trim();
        Self {
            query: text("q"),
            project: text("project"),
            doc_type: text("type"),
            status: text("status"),
            filter: request
                .query_all("filter")
                .into_iter()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .collect(),
            filters: text("filters"),
        }
    }

    fn is_empty(&self) -> bool {
        [
            self.query,
            self.project,
            self.doc_type,
            self.status,
            self.filters,
        ]
        .iter()
        .all(|v| v.is_empty())
            && self.filter.is_empty()
    }

    /// This search from `start`, as a link.
    fn url(&self, start: u64, limit: u64) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (key, value) in [
            ("q", self.query),
            ("project", self.project),
            ("type", self.doc_type),
            ("status", self.status),
        ] {
            if !value.is_empty() {
                query.append_pair(key, value);
            }
        }
        for pair in &self.filter {
            query.append_pair("filter", pair);
        }
        if !self.filters.is_empty() {
            query.append_pair("filters", self.filters);
        }
        if start != 1 {
            query.append_pair("start", &start.to_string());
        }
        if limit != DEFAULT_SEARCH_LIMIT {
            query.append_pair("limit", &limit.to_string());
        }
        format!("/admin/search?{}", query.finish())
    }

    /// The `joedocs search` arguments for this search.
    fn args(&self, start: u64, limit: u64) -> SearchArgs {
        let some = |v: &str| (!v.is_empty()).then(|| v.to_owned());
        SearchArgs {
            query: some(self.query).into_iter().collect(),
            project: some(self.project),
            doc_type: some(self.doc_type),
            status: some(self.status),
            filter: self.filter.iter().map(|f| (*f).to_owned()).collect(),
            filters: some(self.filters),
            document: None,
            start,
            limit,
            json: false,
        }
    }
}

/// `GET /admin/search`.
pub(crate) fn page(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let asked = Asked::from(request);
    let limit = number(request, "limit", DEFAULT_SEARCH_LIMIT);
    let found = number(request, "start", 1).and_then(|start| match &limit {
        Ok(limit) => run(store, &asked, start, *limit),
        Err(refused) => Err(WebError::new(
            refused.status,
            refused.code,
            refused.message.clone(),
        )),
    });
    let (status, results, refused) = match found {
        Ok(results) => (StatusCode::OK, results, None),
        Err(refused) => (
            refused.status,
            refusal(refused.message.clone()),
            Some(refused),
        ),
    };
    if request.wants_fragment() {
        return answer(refused, render(status, &results));
    }
    let settings = store.settings();
    let projects = store.projects().map_err(|e| WebError::from_store(&e))?;
    let page = SearchPage {
        title: "Search",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Search, asked.query),
        query: asked.query,
        projects: choices(
            projects
                .iter()
                .map(|p| (p.name.clone(), format!("{} ({})", p.name, p.key))),
            asked.project,
            "All projects",
        ),
        types: choices(
            settings.types.iter().map(|t| (t.clone(), t.clone())),
            asked.doc_type,
            "All types",
        ),
        statuses: choices(
            settings.statuses.iter().map(|s| (s.clone(), s.clone())),
            asked.status,
            "All statuses",
        ),
        filter_values: asked.filter.iter().copied().chain([""]).collect(),
        filters_json: asked.filters,
        limit: limit.ok().filter(|l| *l != DEFAULT_SEARCH_LIMIT),
        results,
    };
    answer(refused, render(status, &page))
}

/// `response`, or for a refused search the refusal with `response` as its
/// page, so the request is audited with the refusal's code.
fn answer(refused: Option<WebError>, response: Response) -> Result<Response, WebError> {
    match refused {
        Some(refused) => Err(refused.with_page(response)),
        None => Ok(response),
    }
}

/// Runs the search, or says why it cannot.
fn run(store: &Store, asked: &Asked<'_>, start: u64, limit: u64) -> Result<Results, WebError> {
    if asked.is_empty() {
        return Ok(Results::idle());
    }
    let found = SearchSpec::from_args(&asked.args(start, limit))
        .and_then(|spec| browse::search_results(store, &spec));
    let results = match found {
        Ok(results) => results,
        Err(error) => return Err(WebError::from_crate(&error)),
    };
    let shown = results.items.len() as u64;
    Ok(Results {
        searched: true,
        error: None,
        total: results.total,
        start,
        shown_to: start + shown - u64::from(shown > 0),
        previous: (start > 1).then(|| asked.url(start.saturating_sub(limit).max(1), limit)),
        next: results.next_start.map(|next| asked.url(next, limit)),
        hits: results.items.into_iter().map(hit).collect(),
    })
}

impl Results {
    /// Before anything is asked: the region invites a search.
    fn idle() -> Self {
        Self {
            searched: false,
            error: None,
            hits: Vec::new(),
            total: 0,
            start: 1,
            shown_to: 0,
            previous: None,
            next: None,
        }
    }
}

/// Results that show `message` instead of hits.
fn refusal(message: String) -> Results {
    Results {
        searched: true,
        error: Some(message),
        ..Results::idle()
    }
}

fn hit(found: SearchHit) -> Hit {
    let document = found.document;
    Hit {
        href: href(&document.id, found.file.as_deref(), None, false),
        snippet: found.snippet.as_deref().map(pieces).unwrap_or_default(),
        id: document.id,
        title: document.title,
        file: found.file,
        project: document.project,
        doc_type: document.doc_type,
        status: document.status,
        updated: document.updated,
    }
}

/// A snippet split at its `«` and `»` markers: `(true, text)` for a match.
fn pieces(snippet: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = snippet;
    while let Some(open) = rest.find('«') {
        if open > 0 {
            out.push((false, rest[..open].to_owned()));
        }
        let after = &rest[open + '«'.len_utf8()..];
        let (marked, next) = after.split_once('»').unwrap_or((after, ""));
        out.push((true, marked.to_owned()));
        rest = next;
    }
    if !rest.is_empty() {
        out.push((false, rest.to_owned()));
    }
    out
}

/// A picker: "all" first, then `values` as `(value, label)`.
fn choices(
    values: impl Iterator<Item = (String, String)>,
    current: &str,
    all: &str,
) -> Vec<Choice> {
    std::iter::once(Choice {
        value: String::new(),
        label: all.to_owned(),
        selected: current.is_empty(),
    })
    .chain(values.map(|(value, label)| Choice {
        selected: value == current,
        value,
        label,
    }))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::pieces;

    #[test]
    fn snippets_split_at_their_markers() {
        assert_eq!(
            pieces("a «b» c «d»"),
            [
                (false, "a ".to_owned()),
                (true, "b".to_owned()),
                (false, " c ".to_owned()),
                (true, "d".to_owned()),
            ]
        );
        assert_eq!(pieces("plain"), [(false, "plain".to_owned())]);
        assert_eq!(pieces("«open"), [(true, "open".to_owned())]);
        assert_eq!(pieces(""), Vec::<(bool, String)>::new());
    }
}
