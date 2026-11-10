//! The admin commands that view and find documents (`list`): from their
//! arguments to store requests, the JSON replies the admin API sends, and
//! the tables the command line prints.
//!
//! Local runs and client mode share this module, so they print the same:
//! a local run turns the store's answer into the JSON reply the admin API
//! would send, and both render that reply. The replies have the shapes of
//! the matching MCP tool results.

use std::io::Write;

use serde_json::Value;

use crate::Error;
use crate::cli::ListArgs;
use crate::mcp::tools::{DocumentOut, ListDocumentsOut};
use crate::store::{ListRequest, Store};

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

fn to_json(reply: &impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(reply).map_err(|e| Error::Output(std::io::Error::other(e)))
}

/// Whether `reply` has the shape of a page (`items`, `total`): what the
/// client checks before printing a reply from a server.
#[must_use]
pub fn is_page(reply: &Value) -> bool {
    reply["items"].is_array() && reply["total"].is_u64()
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
