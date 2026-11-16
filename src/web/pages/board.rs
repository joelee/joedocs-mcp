//! The Board (JD-47): the live documents of the chosen projects as cards, in
//! a column per status and a row per type, per project, or one row. It only
//! reads. The page script (`board.js`) fetches the grid again
//! (`?fragment=1`) when the change stream says something changed.

use std::collections::HashMap;

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, WebState, render};
use super::{Chrome, Section};
use crate::db::Sort;
use crate::store::{
    DocumentInfo, ListRequest, MAX_LIST_LIMIT, MAX_SEARCH_LIMIT, SearchRequest, Store,
};

/// The longest `attention` reason a card shows, in characters.
const REASON_CHARS: usize = 80;

/// The most opening text a preview shows, in characters.
const PREVIEW_CHARS: usize = 700;

/// The card preview: the document's facts and its opening text.
#[derive(Template)]
#[template(path = "board_preview.html")]
struct Preview {
    id: String,
    title: String,
    project: String,
    doc_type: String,
    status: String,
    revision: u64,
    updated: String,
    updated_by: String,
    attention: Option<String>,
    paragraphs: Vec<String>,
    cut: bool,
}

#[derive(Template)]
#[template(path = "board.html")]
struct BoardPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    projects: Vec<Choice>,
    types: Vec<Choice>,
    statuses: Vec<Choice>,
    view: View,
    refresh_ms: u32,
    preview_ms: u32,
    /// `?debug=1`: show the page script's counters (developer use).
    debug: bool,
    grid: BoardGrid,
}

/// A checkbox in the filter band.
struct Choice {
    value: String,
    label: String,
    checked: bool,
}

/// How the grid is grouped and sorted, and whether only lit cards show.
#[derive(Debug, Clone, PartialEq, Eq)]
struct View {
    attention_only: bool,
    /// `type`, `project` or `none`.
    group: &'static str,
    /// `updated`, `created` or `id`.
    sort: &'static str,
    /// `desc` or `asc`.
    dir: &'static str,
}

impl View {
    fn of(request: &WebRequest) -> Self {
        let pick = |key: &str, allowed: &[&'static str]| -> &'static str {
            request
                .query(key)
                .and_then(|v| allowed.iter().find(|a| **a == v).copied())
                .unwrap_or(allowed[0])
        };
        Self {
            attention_only: request.query("attention") == Some("1"),
            group: pick("group", &["type", "project", "none"]),
            sort: pick("sort", &["updated", "created", "id"]),
            dir: pick("dir", &["desc", "asc"]),
        }
    }
}

/// The grid, also sent alone to the page script.
#[derive(Template)]
#[template(path = "board_grid.html")]
struct BoardGrid {
    /// The store's change marker when the grid was read, for the stream.
    marker: String,
    columns: Vec<Column>,
    rows: Vec<Row>,
    /// Whether rows have titles (not when ungrouped).
    titled: bool,
}

struct Column {
    status: String,
    count: usize,
}

struct Row {
    key: String,
    label: String,
    cells: Vec<Cell>,
}

struct Cell {
    status: String,
    cards: Vec<Card>,
}

struct Card {
    id: String,
    title: String,
    status: String,
    doc_type: String,
    revision: u64,
    created: String,
    updated: String,
    updated_by: String,
    /// The `attention` reason, when the card is lit.
    attention: Option<String>,
    /// For an approved plan being built: its state and current step.
    building: Option<Building>,
    /// The key of the row the card sits in.
    row: String,
}

/// `GET /admin/board`: `project`, `type` and `status` (any number each;
/// none means every project, every type, and every status that is not
/// closed), `attention=1`, `group`, `sort`, `dir`; `fragment=1` for the
/// grid alone.
pub(crate) fn page(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let board = &state.options.board;
    let all_projects = store.projects().map_err(|e| WebError::from_store(&e))?;
    let settings = store.settings();
    let chosen = |key: &str, all: &[String], default: &dyn Fn(&str) -> bool| -> Vec<String> {
        let asked = request.query_all(key);
        all.iter()
            .filter(|v| {
                if asked.is_empty() {
                    default(v)
                } else {
                    asked.contains(&v.as_str())
                }
            })
            .cloned()
            .collect()
    };
    let project_names: Vec<String> = all_projects.iter().map(|p| p.name.clone()).collect();
    let projects = chosen("project", &project_names, &|_| true);
    let types = chosen("type", &settings.types, &|_| true);
    let statuses = chosen("status", &settings.statuses, &|s| {
        !board.closed_statuses.iter().any(|c| c == s)
    });
    let view = View::of(request);
    let grid = grid(
        store,
        &projects,
        &types,
        &statuses,
        &board.closed_statuses,
        &view,
    )?;
    if request.query("fragment") == Some("1") {
        return Ok(render(StatusCode::OK, &grid));
    }
    let choices = |all: &[String], picked: &[String], label: &dyn Fn(&str) -> String| {
        all.iter()
            .map(|v| Choice {
                value: v.clone(),
                label: label(v),
                checked: picked.contains(v),
            })
            .collect()
    };
    let titles: HashMap<&str, &str> = all_projects
        .iter()
        .map(|p| (p.name.as_str(), p.title.as_str()))
        .collect();
    let page = BoardPage {
        title: "Board",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Board, ""),
        projects: choices(&project_names, &projects, &|name| {
            titles
                .get(name)
                .filter(|t| !t.is_empty())
                .map_or_else(|| name.to_owned(), |t| (*t).to_owned())
        }),
        types: choices(&settings.types, &types, &str::to_owned),
        statuses: choices(&settings.statuses, &statuses, &str::to_owned),
        view,
        refresh_ms: board.refresh_ms,
        preview_ms: board.preview_ms,
        debug: request.query("debug") == Some("1"),
        grid,
    };
    Ok(render(StatusCode::OK, &page))
}

/// `GET /admin/board/preview/{id}`: the facts and the opening text of a
/// live document, with code and diagrams left out: nothing is drawn, and
/// nothing is sent to a diagram server.
pub(crate) fn preview(store: &Store, id: &str) -> Result<Response, WebError> {
    let fetched = store.get(id, None).map_err(|e| WebError::from_store(&e))?;
    let attention = attention_reason(&fetched.markdown);
    let (_, body) = split_front_matter(&fetched.markdown);
    let d = fetched.document;
    let (mut paragraphs, cut) = opening_text(body);
    if paragraphs.first().is_some_and(|p| p == &d.title) {
        paragraphs.remove(0);
    }
    let page = Preview {
        id: d.id,
        title: d.title,
        project: d.project,
        doc_type: d.doc_type,
        status: d.status,
        revision: d.revision,
        updated: d.updated,
        updated_by: d.updated_by,
        attention,
        paragraphs,
        cut,
    };
    Ok(render(StatusCode::OK, &page))
}

/// The opening paragraphs of a Markdown body as plain text, at most
/// [`PREVIEW_CHARS`] characters, and whether it was cut. Headings and list
/// items are paragraphs of their own, without their marks; quotes and
/// callouts lose theirs; a fenced block is `[diagram]` (Mermaid, `PlantUML`)
/// or `[code]`.
fn opening_text(body: &str) -> (Vec<String>, bool) {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut fence: Option<&str> = None;
    let flush = |current: &mut String, paragraphs: &mut Vec<String>| {
        let text = current.trim();
        if !text.is_empty() {
            paragraphs.push(text.to_owned());
        }
        current.clear();
    };
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            flush(&mut current, &mut paragraphs);
            let lang = trimmed.trim_start_matches(['`', '~']).trim();
            let label = if matches!(lang, "mermaid" | "plantuml" | "puml") {
                "[diagram]"
            } else {
                "[code]"
            };
            paragraphs.push(label.to_owned());
            fence = Some(open);
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut current, &mut paragraphs);
            continue;
        }
        let (own_line, text) = strip_marks(trimmed);
        if own_line {
            flush(&mut current, &mut paragraphs);
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(text);
        if own_line {
            flush(&mut current, &mut paragraphs);
        }
    }
    flush(&mut current, &mut paragraphs);
    let mut left = PREVIEW_CHARS;
    let mut kept = Vec::new();
    let mut cut = false;
    for paragraph in paragraphs {
        if left == 0 {
            cut = true;
            break;
        }
        let count = paragraph.chars().count();
        if count > left {
            kept.push(paragraph.chars().take(left).collect());
            cut = true;
            break;
        }
        left -= count;
        kept.push(paragraph);
    }
    (kept, cut)
}

/// A line without its heading, list, quote or callout marks, and whether
/// it stands on its own (a heading or a list item).
fn strip_marks(line: &str) -> (bool, &str) {
    if line.starts_with('#') {
        return (true, line.trim_start_matches('#').trim_start());
    }
    for mark in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(mark) {
            return (
                true,
                rest.strip_prefix("[ ] ")
                    .or_else(|| rest.strip_prefix("[x] "))
                    .unwrap_or(rest),
            );
        }
    }
    if let Some((number, rest)) = line.split_once(". ")
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        return (true, rest);
    }
    let text = line.trim_start_matches(['>', ' ']);
    let text = text
        .strip_prefix("[!")
        .and_then(|t| t.split_once(']'))
        .map_or(text, |(_, rest)| {
            rest.trim_start_matches(['-', '+']).trim_start()
        });
    (false, text)
}

/// `KEY-12` as (`KEY`, 12), so IDs sort by their number.
fn id_order(id: &str) -> (String, u64) {
    match id.rsplit_once('-') {
        Some((key, n)) => (key.to_owned(), n.parse().unwrap_or(0)),
        None => (id.to_owned(), 0),
    }
}

fn grid(
    store: &Store,
    projects: &[String],
    types: &[String],
    statuses: &[String],
    closed: &[String],
    view: &View,
) -> Result<BoardGrid, WebError> {
    // The marker first: a write while the cards are read leaves the page
    // behind its marker, so the stream announces it (an extra refresh at
    // worst). Read last, it could name a revision the cards do not show.
    let marker = store
        .change_marker()
        .map_err(|e| WebError::from_store(&e))?
        .unwrap_or_default();
    let lit = attention(store, projects)?;
    let building = building(store, projects)?;
    let mut cards = Vec::new();
    for project in projects {
        let listing = store
            .list(&ListRequest {
                project: project.clone(),
                doc_type: None,
                status: None,
                sort: Sort::UpdatedDesc,
                start: 1,
                limit: MAX_LIST_LIMIT,
            })
            .map_err(|e| WebError::from_store(&e))?;
        for d in listing.items {
            if !statuses.contains(&d.status) || !types.contains(&d.doc_type) {
                continue;
            }
            // A closed card is never lit: the work is over.
            let reason = (!closed.contains(&d.status))
                .then(|| lit.get(&d.id).cloned())
                .flatten();
            if view.attention_only && reason.is_none() {
                continue;
            }
            let built = (d.status == "approved")
                .then(|| building.get(&d.id).cloned())
                .flatten();
            cards.push(card(d, reason, built, view.group));
        }
    }
    sort_cards(&mut cards, view);
    let columns = statuses
        .iter()
        .map(|s| Column {
            status: s.clone(),
            count: cards.iter().filter(|c| &c.status == s).count(),
        })
        .collect();
    let keys: Vec<(String, String)> = match view.group {
        "project" => projects.iter().map(|p| (p.clone(), p.clone())).collect(),
        "none" => vec![("all".to_owned(), String::new())],
        _ => types.iter().map(|t| (t.clone(), t.clone())).collect(),
    };
    let mut rows: Vec<Row> = keys
        .into_iter()
        .filter(|(key, _)| cards.iter().any(|c| &c.row == key))
        .map(|(key, label)| Row {
            key,
            label,
            cells: statuses
                .iter()
                .map(|status| Cell {
                    status: status.clone(),
                    cards: Vec::new(),
                })
                .collect(),
        })
        .collect();
    for card in cards {
        let cell = rows
            .iter_mut()
            .find(|r| r.key == card.row)
            .and_then(|r| r.cells.iter_mut().find(|c| c.status == card.status));
        if let Some(cell) = cell {
            cell.cards.push(card);
        }
    }
    Ok(BoardGrid {
        marker,
        columns,
        rows,
        titled: view.group != "none",
    })
}

fn sort_cards(cards: &mut [Card], view: &View) {
    match view.sort {
        "created" => cards.sort_by(|a, b| {
            a.created
                .cmp(&b.created)
                .then_with(|| id_order(&a.id).cmp(&id_order(&b.id)))
        }),
        "id" => cards.sort_by_key(|c| id_order(&c.id)),
        _ => cards.sort_by(|a, b| {
            a.updated
                .cmp(&b.updated)
                .then_with(|| id_order(&a.id).cmp(&id_order(&b.id)))
        }),
    }
    if view.dir == "desc" {
        cards.reverse();
    }
}

fn card(
    d: DocumentInfo,
    attention: Option<String>,
    building: Option<Building>,
    group: &str,
) -> Card {
    let row = match group {
        "project" => d.project.clone(),
        "none" => "all".to_owned(),
        _ => d.doc_type.clone(),
    };
    Card {
        id: d.id,
        title: d.title,
        status: d.status,
        doc_type: d.doc_type,
        revision: d.revision,
        created: d.created,
        updated: d.updated,
        updated_by: d.updated_by,
        attention,
        building,
        row,
    }
}

/// Live documents in `projects` matching one front matter condition, main
/// files only, with their front matter key `key` when it is a string.
fn matching(
    store: &Store,
    projects: &[String],
    doc_type: Option<&str>,
    key: &str,
    condition: Value,
) -> Result<Vec<String>, WebError> {
    let mut filters = Map::new();
    filters.insert(key.to_owned(), condition);
    let mut ids = Vec::new();
    let mut start = 1;
    loop {
        let found = store
            .search(&SearchRequest {
                projects: projects.to_vec(),
                doc_type: doc_type.map(str::to_owned),
                query: String::new(),
                filters: filters.clone(),
                start,
                limit: MAX_SEARCH_LIMIT,
                document_id: None,
            })
            .map_err(|e| WebError::from_store(&e))?;
        ids.extend(
            found
                .items
                .into_iter()
                .filter(|h| h.file.is_none())
                .map(|h| h.document.id),
        );
        match found.next_start {
            Some(next) => start = next,
            None => return Ok(ids),
        }
    }
}

/// Documents whose main file has a non-empty `attention` text, with the
/// text cut to [`REASON_CHARS`].
fn attention(store: &Store, projects: &[String]) -> Result<HashMap<String, String>, WebError> {
    let mut lit = HashMap::new();
    for id in matching(store, projects, None, "attention", json!({"exists": true}))? {
        let Ok(fetched) = store.get(&id, None) else {
            continue;
        };
        if let Some(reason) = attention_reason(&fetched.markdown) {
            lit.insert(id, reason);
        }
    }
    Ok(lit)
}

/// A plan's build, as its builder records it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Building {
    /// `implementation_status`: one of [`BUILDING_STATES`].
    state: String,
    /// `current_step` when it is a string, cut to [`STEP_CHARS`].
    step: Option<String>,
}

/// The `implementation_status` values of a plan being built: building its
/// steps, stopped on a blocker, paused for the user, or building an
/// amendment's steps.
const BUILDING_STATES: [&str; 4] = ["in-progress", "blocked", "waiting-for-user", "fixing"];

impl Building {
    /// How the mark names a state other than plain building.
    fn phrase(&self) -> Option<&'static str> {
        match self.state.as_str() {
            "blocked" => Some("blocked"),
            "waiting-for-user" => Some("waiting for you"),
            "fixing" => Some("fixing"),
            _ => None,
        }
    }
}

/// The longest `current_step` a card shows, in characters.
const STEP_CHARS: usize = 24;

/// Plans whose builder has started and not finished, with their state and
/// current step.
fn building(store: &Store, projects: &[String]) -> Result<HashMap<String, Building>, WebError> {
    let mut found = HashMap::new();
    // Without a `plans` type there is nothing to mark, and the search
    // would refuse the unknown type.
    if !store.settings().types.iter().any(|t| t == "plans") {
        return Ok(found);
    }
    for state in BUILDING_STATES {
        for id in matching(
            store,
            projects,
            Some("plans"),
            "implementation_status",
            json!(state),
        )? {
            let step = store
                .get(&id, None)
                .ok()
                .and_then(|fetched| split_front_matter(&fetched.markdown).0)
                .and_then(|front| {
                    let step = front.get("current_step")?.as_str()?.trim().to_owned();
                    (!step.is_empty()).then(|| step.chars().take(STEP_CHARS).collect())
                });
            found.insert(
                id,
                Building {
                    state: state.to_owned(),
                    step,
                },
            );
        }
    }
    Ok(found)
}

/// The front matter of a file and its body.
fn split_front_matter(markdown: &str) -> (Option<Value>, &str) {
    let Some(rest) = markdown.strip_prefix("---\n") else {
        return (None, markdown);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (None, markdown);
    };
    (serde_saphyr::from_str(&rest[..end]).ok(), &rest[end + 5..])
}

/// The `attention` value of a file when it is a non-empty string, cut to
/// [`REASON_CHARS`] characters.
fn attention_reason(markdown: &str) -> Option<String> {
    let (front, _) = split_front_matter(markdown);
    let front = front?;
    let text = front.get("attention")?.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(REASON_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_by_project_key_then_number() {
        let mut ids = vec!["JD-10", "AB-3", "JD-2", "JD-1"];
        ids.sort_by_key(|id| id_order(id));
        assert_eq!(ids, ["AB-3", "JD-1", "JD-2", "JD-10"]);
    }

    #[test]
    fn opening_text_keeps_prose_and_marks_code_and_diagrams() {
        let body = "# Title\n\nOne\ntwo.\n\n1. first\n2. second\n- [ ] task\n\n~~~puml\nx\n~~~\n";
        let (text, cut) = opening_text(body);
        assert_eq!(
            text,
            ["Title", "One two.", "first", "second", "task", "[diagram]"]
        );
        assert!(!cut);
        let (text, cut) = opening_text(&"a".repeat(PREVIEW_CHARS + 5));
        assert_eq!(text[0].chars().count(), PREVIEW_CHARS);
        assert!(cut);
    }

    #[test]
    fn only_a_non_empty_attention_string_lights_a_card() {
        let file = |front: &str| format!("---\nid: JD-1\n{front}\n---\n# Body\n");
        assert_eq!(
            attention_reason(&file("attention: Decide")),
            Some("Decide".to_owned())
        );
        assert_eq!(attention_reason(&file("attention: \"  \"")), None);
        assert_eq!(attention_reason(&file("attention: false")), None);
        assert_eq!(attention_reason(&file("other: 1")), None);
        let long = "x".repeat(100);
        assert_eq!(
            attention_reason(&file(&format!("attention: {long}"))).map(|r| r.chars().count()),
            Some(REASON_CHARS)
        );
        assert_eq!(attention_reason("no front matter"), None);
    }
}
