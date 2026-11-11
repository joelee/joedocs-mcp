//! The History tab and the diff view: a document's revisions, or one file's
//! across its renames, as `joedocs history` lists them; and the unified
//! diff between two revisions, as `joedocs diff` shows it, with each line
//! classed so added and removed lines stand out.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, render};
use super::document::{href, refused};
use super::documents::number;
use super::{Chrome, Section, link};
use crate::cli::DEFAULT_HISTORY_LIMIT;
use crate::store::{DocumentInfo, RevisionInfo, Store};

/// Context lines around each change, as `joedocs diff` uses by default.
const DEFAULT_CONTEXT: u32 = 3;

/// One revision in the History tab.
pub(crate) struct Row {
    pub(crate) revision: u64,
    pub(crate) at: String,
    pub(crate) action: String,
    pub(crate) author: String,
    pub(crate) client: String,
    pub(crate) summary: String,
    /// The file's name at this revision, for a file's history.
    pub(crate) file: Option<String>,
    /// This revision's view.
    pub(crate) view: String,
    /// Its diff with the revision before, when there is one.
    pub(crate) diff: Option<String>,
}

#[derive(Template)]
#[template(path = "history.html")]
struct HistoryPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    document: &'a DocumentInfo,
    file: Option<&'a str>,
    rows: Vec<Row>,
    total: u64,
    start: u64,
    shown_to: u64,
    previous: Option<String>,
    next: Option<String>,
    view_href: String,
    source_href: String,
}

/// `GET /admin/documents/{id}/history`, for the document or `?file=`.
pub(crate) fn history(
    store: &Store,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let file = request.query("file").filter(|f| !f.is_empty());
    let start = number(request, "start", 1)?;
    let limit = number(request, "limit", DEFAULT_HISTORY_LIMIT)?;
    let page = match file {
        Some(file) => store.history_file(id, file, start, limit),
        None => store.history(id, start, limit),
    }
    .map_err(|e| refused(&e))?;
    let document = &page.document;
    let latest = document.revision;
    let shown = page.revisions.len() as u64;
    let older: Vec<Option<u64>> = page
        .revisions
        .iter()
        .skip(1)
        .map(|r| Some(r.revision))
        .chain([None])
        .collect();
    let rows = page
        .revisions
        .iter()
        .zip(older)
        .enumerate()
        .map(|(i, (revision, older))| {
            let oldest = start - 1 + i as u64 + 1 == page.total;
            row(document, revision, older, oldest, latest)
        })
        .collect();
    let base = format!("/admin/documents/{}/history", document.id);
    let url = |start: u64| {
        let start = (start != 1).then(|| start.to_string());
        let limit = (limit != DEFAULT_HISTORY_LIMIT).then(|| limit.to_string());
        link(
            &base,
            &[
                ("file", file),
                ("start", start.as_deref()),
                ("limit", limit.as_deref()),
            ],
        )
    };
    let view = HistoryPage {
        title: &document.title,
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        document,
        file,
        total: page.total,
        start,
        shown_to: start + shown - u64::from(shown > 0),
        previous: (start > 1).then(|| url(start.saturating_sub(limit).max(1))),
        next: page.next_start.map(url),
        rows,
        view_href: href(&document.id, file, None, false),
        source_href: href(&document.id, file, None, true),
    };
    Ok(render(StatusCode::OK, &view))
}

/// A History row: its view, and its diff with `older` (the next row), or
/// with the revision before it when the next row is on another page.
fn row(
    document: &DocumentInfo,
    revision: &RevisionInfo,
    older: Option<u64>,
    oldest: bool,
    latest: u64,
) -> Row {
    let number = revision.revision;
    let file = revision.file.as_deref();
    let pinned = (number != latest).then_some(number);
    let diff = (!oldest && number > 1).then(|| {
        let from = older.unwrap_or(number - 1).to_string();
        let to = number.to_string();
        link(
            &format!("/admin/documents/{}/diff", document.id),
            &[("from", Some(&from)), ("to", Some(&to)), ("file", file)],
        )
    });
    Row {
        revision: number,
        at: revision.at.clone(),
        action: revision.action.clone(),
        author: revision.author.clone(),
        client: revision.client.clone(),
        summary: revision.summary.clone(),
        file: revision.file.clone(),
        view: href(&document.id, file, pinned, false),
        diff,
    }
}

/// One line of a unified diff, with its class.
pub(crate) struct DiffLine {
    pub(crate) class: &'static str,
    pub(crate) text: String,
}

#[derive(Template)]
#[template(path = "diff.html")]
struct DiffPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    document: &'a DocumentInfo,
    file: Option<&'a str>,
    from: u64,
    to: u64,
    context: u32,
    lines: Vec<DiffLine>,
    files: Vec<String>,
    history_href: String,
    view_href: String,
}

/// `GET /admin/documents/{id}/diff?from=N[&to=M][&file=F][&context=C]`.
pub(crate) fn diff(
    store: &Store,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let file = request.query("file").filter(|f| !f.is_empty());
    if request.query("from").is_none_or(str::is_empty) {
        return Err(WebError::new(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "`from` is required: the older revision to compare",
        ));
    }
    let from = number(request, "from", 1)?;
    let to = match request.query("to").filter(|t| !t.is_empty()) {
        None => None,
        Some(_) => Some(number(request, "to", 1)?),
    };
    let context =
        u32::try_from(number(request, "context", u64::from(DEFAULT_CONTEXT))?).unwrap_or(u32::MAX);
    let document = store.document_info(id).map_err(|e| refused(&e))?;
    let found = match file {
        Some(file) => store.diff_file(id, file, from, to, context),
        None => store.diff(id, from, to, context),
    }
    .map_err(|e| refused(&e))?;
    let page = DiffPage {
        title: &document.title,
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        document: &document,
        file,
        from: found.from,
        to: found.to,
        context,
        lines: classify(&found.diff),
        files: found.files,
        history_href: link(
            &format!("/admin/documents/{}/history", document.id),
            &[("file", file)],
        ),
        view_href: href(&document.id, file, None, false),
    };
    Ok(render(StatusCode::OK, &page))
}

/// The lines of a unified diff, each with the class that marks it. Inside a
/// hunk the line counts of its `@@` header say which lines are content, so
/// a removed line that reads `-- x` is not taken for a `--- ` file header.
fn classify(diff: &str) -> Vec<DiffLine> {
    let (mut old_left, mut new_left) = (0_u64, 0_u64);
    diff.lines()
        .map(|line| {
            let in_hunk = old_left > 0 || new_left > 0;
            let class = match line.as_bytes().first() {
                Some(b'\\') => "diff-note",
                Some(b'+') if in_hunk => {
                    new_left = new_left.saturating_sub(1);
                    "diff-add"
                }
                Some(b'-') if in_hunk => {
                    old_left = old_left.saturating_sub(1);
                    "diff-del"
                }
                _ if in_hunk => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    "diff-ctx"
                }
                _ if line.starts_with("@@") => {
                    (old_left, new_left) = hunk_counts(line);
                    "diff-hunk"
                }
                _ if line.starts_with("--- ") || line.starts_with("+++ ") => "diff-file",
                _ => "diff-ctx",
            };
            DiffLine {
                class,
                text: line.to_owned(),
            }
        })
        .collect()
}

/// The old and new line counts of a hunk header `@@ -a[,b] +c[,d] @@`; a
/// count left out is 1.
fn hunk_counts(header: &str) -> (u64, u64) {
    let mut parts = header.split_whitespace().skip(1);
    let count = |range: Option<&str>| {
        range.map_or(0, |r| {
            r.split_once(',')
                .map_or(Some(1), |(_, n)| n.parse().ok())
                .unwrap_or(0)
        })
    };
    let old = count(parts.next().filter(|p| p.starts_with('-')));
    let new = count(parts.next().filter(|p| p.starts_with('+')));
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::{classify, hunk_counts};

    #[test]
    fn diff_lines_are_classed_by_where_they_stand() {
        let diff = "--- JD-1 revision 1\n+++ JD-1 revision 2\n@@ -1,3 +1,3 @@\n same\n--- a removed rule\n+++ an added rule\n-old\n+new\n\\ No newline at end of file\n@@ -9 +9,0 @@\n-gone\n";
        let classes: Vec<&str> = classify(diff).iter().map(|l| l.class).collect();
        assert_eq!(
            classes,
            [
                "diff-file",
                "diff-file",
                "diff-hunk",
                "diff-ctx",
                "diff-del",
                "diff-add",
                "diff-del",
                "diff-add",
                "diff-note",
                "diff-hunk",
                "diff-del",
            ]
        );
    }

    #[test]
    fn hunk_headers_give_their_line_counts() {
        assert_eq!(hunk_counts("@@ -1,3 +1,4 @@"), (3, 4));
        assert_eq!(hunk_counts("@@ -9 +9,0 @@ heading"), (1, 0));
        assert_eq!(hunk_counts("@@ nonsense"), (0, 0));
    }
}
