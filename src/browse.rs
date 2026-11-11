//! The admin commands that view and find documents (`list`, `search`,
//! `history`, `diff`) and read the audit log (`audit`): from their
//! arguments to store requests, the JSON replies the admin API sends, and
//! the tables the command line prints.
//!
//! Local runs and client mode share this module, so they print the same:
//! a local run turns the store's answer into the JSON reply the admin API
//! would send, and both render that reply. The replies have the shapes of
//! the matching MCP tool results.

use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Error;
use crate::cli::{AuditArgs, DEFAULT_SEARCH_LIMIT, DiffArgs, HistoryArgs, ListArgs, SearchArgs};
use crate::mcp::tools::{
    DiffOut, DocumentOut, HistoryOut, ListDocumentsOut, RevisionOut, SearchDocumentsOut,
    SearchHitOut,
};
use crate::store::{self, AuditRequest, ListRequest, SearchRequest, SearchResults, Store};

/// `joedocs list`: one page of a project's live documents, as the
/// `list_documents` result.
///
/// # Errors
///
/// Returns [`Error::Store`] for an unknown project, a bad type, status,
/// start or limit, and database errors.
pub fn list(store: &Store, args: &ListArgs) -> Result<Value, Error> {
    let listing = store.list(&ListRequest {
        project: args.project.clone(),
        doc_type: args.doc_type.clone(),
        status: args.status.clone(),
        sort: args.sort.sort(),
        start: args.start,
        limit: args.limit,
    })?;
    to_json(&ListDocumentsOut {
        items: listing.items.into_iter().map(DocumentOut::from).collect(),
        total: listing.total,
        next_start: listing.next_start,
    })
}

/// What `joedocs search` asks for: the body of `POST /admin/v1/search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    /// One project; `None` or `"*"` for every project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// One document type.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    /// Words to find.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Front matter conditions, as `search_documents` takes them.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub filters: Map<String, Value>,
    /// Only this document's files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// 1-based position of the first result.
    #[serde(default = "one")]
    pub start: u64,
    /// Most results.
    #[serde(default = "default_search_limit")]
    pub limit: u64,
}

fn one() -> u64 {
    1
}

fn default_search_limit() -> u64 {
    DEFAULT_SEARCH_LIMIT
}

impl SearchSpec {
    /// The search the command line asks for: `--status`, each `--filter`
    /// and `--filters` merged into one filter object.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument [`Error::Store`] for `--filter` text
    /// without `=`, `--filters` that is not a JSON object, and a key given
    /// twice.
    pub fn from_args(args: &SearchArgs) -> Result<Self, Error> {
        let mut filters = match &args.filters {
            None => Map::new(),
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(map)) => map,
                Ok(_) => return Err(invalid("filters", "--filters must be a JSON object")),
                Err(e) => return Err(invalid("filters", format!("--filters is not JSON: {e}"))),
            },
        };
        let mut add = |key: &str, value: &str| {
            if filters.contains_key(key) {
                return Err(invalid("filter", format!("the key `{key}` is given twice")));
            }
            filters.insert(key.to_owned(), Value::String(value.to_owned()));
            Ok(())
        };
        if let Some(status) = &args.status {
            add("status", status)?;
        }
        for pair in &args.filter {
            let (key, value) = pair.split_once('=').ok_or_else(|| {
                invalid(
                    "filter",
                    format!("--filter takes KEY=VALUE, not \"{pair}\""),
                )
            })?;
            add(key.trim(), value)?;
        }
        let query = args.query.join(" ");
        Ok(Self {
            project: args.project.clone(),
            doc_type: args.doc_type.clone(),
            query: (!query.trim().is_empty()).then_some(query),
            filters,
            document_id: args.document.clone(),
            start: args.start,
            limit: args.limit,
        })
    }
}

fn invalid(field: &'static str, message: impl Into<String>) -> Error {
    Error::Store(store::Error::invalid(field, message))
}

/// `joedocs search`: one page of hits, as the `search_documents` result. The
/// administrator reads every project.
///
/// # Errors
///
/// Returns [`Error::Store`] for an unknown project, a bad type, query,
/// filter, start or limit, and database errors.
pub fn search(store: &Store, spec: &SearchSpec) -> Result<Value, Error> {
    let results = search_results(store, spec)?;
    to_json(&SearchDocumentsOut {
        items: results.items.into_iter().map(SearchHitOut::from).collect(),
        total: results.total,
        next_start: results.next_start,
    })
}

/// [`search`] before it becomes JSON, for the web interface.
///
/// # Errors
///
/// As [`search`].
pub fn search_results(store: &Store, spec: &SearchSpec) -> Result<SearchResults, Error> {
    let projects = match spec.project.as_deref() {
        None | Some("*") => store.projects()?.into_iter().map(|p| p.name).collect(),
        Some(name) => {
            store
                .project(name)?
                .ok_or_else(|| store::Error::ProjectNotFound(name.to_owned()))?;
            vec![name.to_owned()]
        }
    };
    Ok(store.search(&SearchRequest {
        projects,
        doc_type: spec.doc_type.clone(),
        query: spec.query.clone().unwrap_or_default(),
        filters: spec.filters.clone(),
        start: spec.start,
        limit: spec.limit,
        document_id: spec.document_id.clone(),
    })?)
}

/// `joedocs history`: one page of a document's revisions, or of one file's,
/// as the `get_document_history` result.
///
/// # Errors
///
/// Returns [`Error::Store`] for an unknown or archived document or file, a
/// bad start or limit, and database errors.
pub fn history(store: &Store, args: &HistoryArgs) -> Result<Value, Error> {
    let page = match &args.part {
        Some(file) => store.history_file(&args.document_id, file, args.start, args.limit)?,
        None => store.history(&args.document_id, args.start, args.limit)?,
    };
    to_json(&HistoryOut {
        document: page.document.into(),
        revisions: page.revisions.into_iter().map(RevisionOut::from).collect(),
        total: page.total,
        next_start: page.next_start,
    })
}

/// `joedocs diff`: the `diff_document` result.
///
/// # Errors
///
/// Returns [`Error::Store`] for an unknown or archived document or file, a
/// revision that does not exist, a bad context, and storage errors.
pub fn diff(store: &Store, args: &DiffArgs) -> Result<Value, Error> {
    let id = &args.document_id;
    let multifile = store.document_info(id)?.multifile;
    let diff = match &args.part {
        Some(file) => store.diff_file(id, file, args.from, args.to, args.context)?,
        None => store.diff(id, args.from, args.to, args.context)?,
    };
    to_json(&DiffOut {
        document_id: diff.id,
        from: diff.from,
        to: diff.to,
        changed: !diff.diff.is_empty(),
        diff: diff.diff,
        files: multifile.then_some(diff.files),
    })
}

/// One audit entry in an `audit` reply.
#[derive(Debug, Serialize)]
struct AuditEntryOut {
    at: String,
    key_id: String,
    key_name: String,
    client: String,
    tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_id: Option<String>,
    outcome: String,
}

/// A page of audit entries: `{"items", "total", "next_start"}`.
#[derive(Debug, Serialize)]
struct AuditOut {
    items: Vec<AuditEntryOut>,
    total: u64,
    next_start: Option<u64>,
}

/// `joedocs audit`: one page of the audit log, newest first.
///
/// # Errors
///
/// Returns [`Error::Store`] for a bad time, document ID, start or limit,
/// and database errors.
pub fn audit(store: &Store, args: &AuditArgs) -> Result<Value, Error> {
    let page = store.audit_entries(&AuditRequest {
        since: args.since.clone(),
        until: args.until.clone(),
        key: args.key.clone(),
        tool: args.tool.clone(),
        document: args.document.clone(),
        outcome: args.outcome.clone(),
        start: args.start,
        limit: args.limit,
    })?;
    to_json(&AuditOut {
        items: page
            .items
            .into_iter()
            .map(|row| AuditEntryOut {
                at: row.at,
                key_id: row.api_key_id,
                key_name: row.key_name,
                client: row.client,
                tool: row.tool,
                document_id: row.document_id,
                outcome: row.outcome,
            })
            .collect(),
        total: page.total,
        next_start: page.next_start,
    })
}

fn to_json(reply: &impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(reply).map_err(|e| Error::Output(std::io::Error::other(e)))
}

/// Whether `reply` has the shape of a page (`items`, `total`): what the
/// client checks before printing a reply from a server.
#[must_use]
pub fn is_page(reply: &Value) -> bool {
    reply["items"].is_array() && reply["total"].is_u64()
}

/// Whether `reply` has the shape of a `history` reply.
#[must_use]
pub fn is_history(reply: &Value) -> bool {
    reply["revisions"].is_array() && reply["total"].is_u64() && reply["document"].is_object()
}

/// Whether `reply` has the shape of a `diff` reply.
#[must_use]
pub fn is_diff(reply: &Value) -> bool {
    reply["diff"].is_string()
        && reply["changed"].is_boolean()
        && reply["from"].is_u64()
        && reply["to"].is_u64()
}

/// Prints `reply`, as pretty JSON when `json`, otherwise as `render` does.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn print(
    reply: &Value,
    json: bool,
    render: fn(&Value, &mut dyn Write) -> Result<(), Error>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if json {
        let text = serde_json::to_string_pretty(reply)
            .map_err(|e| Error::Output(std::io::Error::other(e)))?;
        writeln!(out, "{text}")?;
        Ok(())
    } else {
        render(reply, out)
    }
}

/// Writes a `list` reply as a table, the paging line, or `No documents.`
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render_list(reply: &Value, out: &mut dyn Write) -> Result<(), Error> {
    let items = items(reply);
    if items.is_empty() {
        writeln!(out, "No documents.")?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|doc| {
            let files = doc["files"]
                .as_u64()
                .map_or_else(|| "-".to_owned(), |n| n.to_string());
            vec![
                text(doc, "id"),
                text(doc, "type"),
                text(doc, "status"),
                number(doc, "revision"),
                files,
                text(doc, "updated"),
                text(doc, "title"),
            ]
        })
        .collect();
    crate::admin::write_table(
        out,
        &["ID", "TYPE", "STATUS", "REV", "FILES", "UPDATED", "TITLE"],
        &rows,
    )?;
    write_paging(reply, items.len(), out)
}

/// Writes a `search` reply as a table, each hit followed by its snippet
/// (indented) when there is one, then the paging line; or `No matches.`
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render_search(reply: &Value, out: &mut dyn Write) -> Result<(), Error> {
    let items = items(reply);
    if items.is_empty() {
        writeln!(out, "No matches.")?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|hit| {
            let file = hit["file"].as_str().map_or_else(|| "-".to_owned(), escape);
            vec![
                text(hit, "id"),
                file,
                text(hit, "type"),
                text(hit, "status"),
                text(hit, "updated"),
                text(hit, "title"),
            ]
        })
        .collect();
    let mut table = Vec::new();
    crate::admin::write_table(
        &mut table,
        &["ID", "FILE", "TYPE", "STATUS", "UPDATED", "TITLE"],
        &rows,
    )?;
    let table = String::from_utf8_lossy(&table);
    let mut lines = table.lines();
    if let Some(header) = lines.next() {
        writeln!(out, "{header}")?;
    }
    for (line, hit) in lines.zip(items) {
        writeln!(out, "{line}")?;
        if let Some(snippet) = hit["snippet"].as_str() {
            writeln!(out, "    {}", escape(snippet))?;
        }
    }
    write_paging(reply, items.len(), out)
}

/// Writes a `history` reply as a table, then the paging line.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render_history(reply: &Value, out: &mut dyn Write) -> Result<(), Error> {
    let revisions = reply["revisions"].as_array().map_or(&[][..], Vec::as_slice);
    let rows: Vec<Vec<String>> = revisions
        .iter()
        .map(|rev| {
            vec![
                number(rev, "revision"),
                text(rev, "at"),
                text(rev, "action"),
                text(rev, "author"),
                text(rev, "client"),
                text(rev, "summary"),
            ]
        })
        .collect();
    crate::admin::write_table(
        out,
        &["REV", "TIME", "ACTION", "AUTHOR", "CLIENT", "SUMMARY"],
        &rows,
    )?;
    write_paging(reply, revisions.len(), out)
}

/// Writes a `diff` reply: the unified diff as it is, or a line saying
/// nothing changed.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render_diff(reply: &Value, out: &mut dyn Write) -> Result<(), Error> {
    let diff = reply["diff"].as_str().unwrap_or_default();
    if diff.is_empty() {
        writeln!(
            out,
            "No changes between revisions {} and {}.",
            number(reply, "from"),
            number(reply, "to")
        )?;
    } else {
        out.write_all(diff.as_bytes())?;
        if !diff.ends_with('\n') {
            writeln!(out)?;
        }
    }
    Ok(())
}

/// Writes an `audit` reply as a table, then the paging line; or
/// `No audit entries.`
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn render_audit(reply: &Value, out: &mut dyn Write) -> Result<(), Error> {
    let items = items(reply);
    if items.is_empty() {
        writeln!(out, "No audit entries.")?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|entry| {
            let document = entry["document_id"]
                .as_str()
                .map_or_else(|| "-".to_owned(), escape);
            vec![
                text(entry, "at"),
                text(entry, "key_name"),
                text(entry, "client"),
                text(entry, "tool"),
                document,
                text(entry, "outcome"),
            ]
        })
        .collect();
    crate::admin::write_table(
        out,
        &["TIME", "KEY", "CLIENT", "TOOL", "DOCUMENT", "OUTCOME"],
        &rows,
    )?;
    write_paging(reply, items.len(), out)
}

fn items(reply: &Value) -> &[Value] {
    reply["items"].as_array().map_or(&[], Vec::as_slice)
}

/// The text of `key` in `item`, with control characters escaped.
fn text(item: &Value, key: &str) -> String {
    escape(item[key].as_str().unwrap_or_default())
}

fn number(item: &Value, key: &str) -> String {
    item[key]
        .as_u64()
        .map_or_else(String::new, |n| n.to_string())
}

/// `Showing 1-20 of 57; next page: --start 21` when there is a next page.
fn write_paging(reply: &Value, shown: usize, out: &mut dyn Write) -> Result<(), Error> {
    let (Some(next), Some(total)) = (reply["next_start"].as_u64(), reply["total"].as_u64()) else {
        return Ok(());
    };
    let first = next.saturating_sub(shown as u64);
    let last = next.saturating_sub(1);
    writeln!(
        out,
        "Showing {first}-{last} of {total}; next page: --start {next}"
    )?;
    Ok(())
}

/// `text` with every control character (C0, DEL and C1) escaped, so text
/// written through MCP cannot move the cursor, change colours or forge
/// rows: `\n`, `\r` and `\t`, and `\u{1b}` for the others.
#[must_use]
pub fn escape(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rendered(reply: &Value) -> String {
        let mut out = Vec::new();
        render_list(reply, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(
            escape("a\u{1b}[31mb\nc\rd\te\u{7f}f\u{85}g"),
            "a\\u{1b}[31mb\\nc\\rd\\te\\u{7f}f\\u{85}g"
        );
        assert_eq!(escape("plain «text» é"), "plain «text» é");
    }

    #[test]
    fn a_list_reply_renders_as_a_table_with_a_paging_line() {
        let reply = json!({
            "items": [
                {"id": "P-1", "type": "plans", "status": "draft", "revision": 3,
                 "updated": "2026-11-10T00:00:00Z", "title": "One\u{1b}"},
                {"id": "P-2", "type": "notes", "status": "done", "revision": 1,
                 "updated": "2026-11-09T00:00:00Z", "title": "Two", "multifile": true, "files": 4},
            ],
            "total": 5,
            "next_start": 4,
        });
        assert_eq!(
            rendered(&reply),
            "ID   TYPE   STATUS  REV  FILES  UPDATED               TITLE\n\
             P-1  plans  draft   3    -      2026-11-10T00:00:00Z  One\\u{1b}\n\
             P-2  notes  done    1    4      2026-11-09T00:00:00Z  Two\n\
             Showing 2-3 of 5; next page: --start 4\n"
        );
        assert_eq!(
            rendered(&json!({"items": [], "total": 0})),
            "No documents.\n"
        );
    }

    fn args(extra: &[&str]) -> SearchArgs {
        use clap::Parser as _;
        let mut all = vec!["joedocs", "search"];
        all.extend_from_slice(extra);
        match crate::Cli::try_parse_from(all).unwrap().command {
            crate::Command::Search(args) => args,
            other => panic!("{other:?}"),
        }
    }

    fn message(error: &Error) -> String {
        error.to_string()
    }

    #[test]
    fn search_filters_merge_and_refuse_duplicates() {
        let spec = SearchSpec::from_args(&args(&[
            "two",
            "words",
            "-s",
            "draft",
            "-F",
            "kind=bug",
            "--filters",
            r#"{"severity": {"in": ["major"]}}"#,
        ]))
        .unwrap();
        assert_eq!(spec.query.as_deref(), Some("two words"));
        assert_eq!(
            Value::Object(spec.filters),
            json!({"status": "draft", "kind": "bug", "severity": {"in": ["major"]}})
        );
        assert_eq!(SearchSpec::from_args(&args(&[])).unwrap().query, None);
        for (extra, needle) in [
            (vec!["-s", "a", "-F", "status=b"], "`status` is given twice"),
            (vec!["-F", "k=a", "-F", "k=b"], "`k` is given twice"),
            (
                vec!["--filters", r#"{"k": 1}"#, "-F", "k=b"],
                "`k` is given twice",
            ),
            (vec!["-F", "kind"], "--filter takes KEY=VALUE"),
            (vec!["--filters", "[1]"], "--filters must be a JSON object"),
            (vec!["--filters", "{x"], "--filters is not JSON"),
        ] {
            let error = SearchSpec::from_args(&args(&extra)).unwrap_err();
            assert!(message(&error).contains(needle), "{extra:?}: {error}");
            assert_eq!(error.exit_code(), crate::EXIT_DATA);
        }
    }

    #[test]
    fn a_search_spec_is_the_admin_api_body() {
        let spec = SearchSpec::from_args(&args(&["x", "-p", "p", "-n", "5"])).unwrap();
        let body = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            body,
            json!({"project": "p", "query": "x", "start": 1, "limit": 5})
        );
        assert_eq!(serde_json::from_value::<SearchSpec>(body).unwrap(), spec);
        let defaults: SearchSpec = serde_json::from_value(json!({})).unwrap();
        assert_eq!((defaults.start, defaults.limit), (1, DEFAULT_SEARCH_LIMIT));
        assert!(serde_json::from_value::<SearchSpec>(json!({"nope": 1})).is_err());
    }

    #[test]
    fn a_search_reply_renders_hits_with_their_snippets() {
        let reply = json!({
            "items": [
                {"id": "P-1", "type": "plans", "status": "draft", "updated": "2026-11-10T00:00:00Z",
                 "title": "One", "score": 1.5, "snippet": "a «zebra»\nhere"},
                {"id": "P-2", "type": "notes", "status": "draft", "updated": "2026-11-10T00:00:00Z",
                 "title": "Two", "file": "pages/zoo"},
            ],
            "total": 2,
            "next_start": null,
        });
        let mut out = Vec::new();
        render_search(&reply, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "ID   FILE       TYPE   STATUS  UPDATED               TITLE\n\
             P-1  -          plans  draft   2026-11-10T00:00:00Z  One\n    a «zebra»\\nhere\n\
             P-2  pages/zoo  notes  draft   2026-11-10T00:00:00Z  Two\n"
        );
        let mut out = Vec::new();
        render_search(&json!({"items": [], "total": 0}), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No matches.\n");
    }

    #[test]
    fn history_and_diff_replies_render() {
        let reply = json!({
            "document": {"id": "P-1"},
            "revisions": [
                {"revision": 2, "at": "2026-11-10T00:00:00Z", "action": "edit",
                 "author": "laptop", "client": "cli", "summary": "Fix\ttypo"},
                {"revision": 1, "at": "2026-11-09T00:00:00Z", "action": "create",
                 "author": "laptop", "client": "cli", "summary": "Create P-1"},
            ],
            "total": 3,
            "next_start": 3,
        });
        let mut out = Vec::new();
        render_history(&reply, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "REV  TIME                  ACTION  AUTHOR  CLIENT  SUMMARY\n\
             2    2026-11-10T00:00:00Z  edit    laptop  cli     Fix\\ttypo\n\
             1    2026-11-09T00:00:00Z  create  laptop  cli     Create P-1\n\
             Showing 1-2 of 3; next page: --start 3\n"
        );
        assert!(is_history(&reply));
        assert!(!is_history(&json!({"revisions": [], "total": 0})));

        let mut out = Vec::new();
        let diff = json!({"from": 1, "to": 2, "changed": true, "diff": "-a\n+b"});
        render_diff(&diff, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "-a\n+b\n");
        let mut out = Vec::new();
        let same = json!({"from": 2, "to": 2, "changed": false, "diff": ""});
        render_diff(&same, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "No changes between revisions 2 and 2.\n"
        );
        assert!(is_diff(&diff) && is_diff(&same));
        assert!(!is_diff(&json!({"diff": "x"})));
    }

    #[test]
    fn json_is_pretty_with_a_newline_and_pages_are_recognised() {
        let reply = json!({"items": [], "total": 0});
        let mut out = Vec::new();
        print(&reply, true, render_list, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\n  \"items\": [],\n  \"total\": 0\n}\n"
        );
        assert!(is_page(&reply));
        assert!(!is_page(&json!({"items": 1, "total": 0})));
        assert!(!is_page(&json!({"output": "x"})));
    }
}
