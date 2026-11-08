//! `INDEX.md`: a generated table of a project's live documents, written in
//! the project folder so a person can find and open a document by its ID,
//! and one in each multi-file document's folder listing its files.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use super::{Error, Settings, Store, lock};
use crate::db::{self, Connection, DocumentRow, ProjectRow};

/// The file name, in each project folder.
pub(crate) const FILE_NAME: &str = "INDEX.md";

/// The rules in the history's `info/exclude` that keep every project's
/// `INDEX.md`, and every multi-file document folder's, out of `git status`
/// (they are never staged either way).
const EXCLUDE_RULES: [&str; 2] = ["/*/INDEX.md", "/*/*/*/INDEX.md"];

/// Adds the [`EXCLUDE_RULES`] to `<git_dir>/info/exclude` unless they are
/// there, keeping the file's other lines.
///
/// # Errors
///
/// Returns I/O errors.
pub(super) fn exclude_from_history(git_dir: &Path) -> Result<(), Error> {
    let dir = git_dir.join("info");
    let path = dir.join("exclude");
    let current = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(Error::io("read", &path)(error)),
    };
    let missing: Vec<&str> = EXCLUDE_RULES
        .into_iter()
        .filter(|rule| !current.lines().any(|line| line.trim() == *rule))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for rule in missing {
        text.push_str(rule);
        text.push('\n');
    }
    fs::create_dir_all(&dir).map_err(Error::io("create", &dir))?;
    fs::write(&path, text).map_err(Error::io("write", &path))
}

/// Rewrites every project's `INDEX.md` (see [`refresh`]). The caller holds
/// the write lock.
pub(super) fn refresh_all(conn: &Connection, home: &Path, settings: &Settings) {
    if !settings.index_file {
        return;
    }
    match db::list_projects(conn) {
        Ok(projects) => {
            for project in projects {
                refresh(conn, home, settings, &project.name);
            }
        }
        Err(error) => tracing::warn!(%error, "could not list projects to write {FILE_NAME}"),
    }
    match db::live_documents(conn) {
        Ok(rows) => {
            for row in rows.iter().filter(|row| row.multifile) {
                refresh_folder(conn, home, settings, row);
            }
        }
        Err(error) => tracing::warn!(%error, "could not list documents to write {FILE_NAME}"),
    }
}

/// The vault-relative paths of the `INDEX.md` files that are missing or
/// differ from what the store would write; empty when `index_file` is off.
///
/// # Errors
///
/// Returns database errors.
pub(super) fn stale(
    conn: &Connection,
    home: &Path,
    settings: &Settings,
) -> Result<Vec<String>, Error> {
    if !settings.index_file {
        return Ok(Vec::new());
    }
    let mut stale = Vec::new();
    for project in db::list_projects(conn)? {
        let text = expected(conn, &project.name)?;
        let path = format!("{}/{FILE_NAME}", project.name);
        if fs::read_to_string(home.join(&path)).ok().as_deref() != Some(text.as_str()) {
            stale.push(path);
        }
    }
    for row in db::live_documents(conn)?.iter().filter(|row| row.multifile) {
        let text = folder_expected(conn, row)?;
        let path = folder_index_path(row);
        if fs::read_to_string(home.join(&path)).ok().as_deref() != Some(text.as_str()) {
            stale.push(path);
        }
    }
    Ok(stale)
}

/// The vault path of a multi-file document's folder `INDEX.md`.
pub(super) fn folder_index_path(row: &DocumentRow) -> String {
    format!("{}/{FILE_NAME}", super::files::folder(row))
}

/// Rewrites the project `INDEX.md` of `row`, and its folder `INDEX.md` if it
/// is a multi-file document (see [`refresh`]).
pub(super) fn refresh_document(
    conn: &Connection,
    home: &Path,
    settings: &Settings,
    row: &DocumentRow,
) {
    refresh(conn, home, settings, &row.project);
    if row.multifile {
        refresh_folder(conn, home, settings, row);
    }
}

/// Rewrites a multi-file document's folder `INDEX.md` when
/// `settings.index_file` is on; failures are logged, as in [`refresh`].
fn refresh_folder(conn: &Connection, home: &Path, settings: &Settings, row: &DocumentRow) {
    if !settings.index_file {
        return;
    }
    let path = folder_index_path(row);
    let written = folder_expected(conn, row)
        .and_then(|text| write_text(&home.join(super::files::folder(row)), &text));
    if let Err(error) = written {
        tracing::warn!(document = %row.id, %error, "could not write {path}");
    }
}

/// The text a multi-file document's folder `INDEX.md` should hold: its ID
/// and title, its revision, and a table of its live files with an Obsidian
/// link to each, the revision at which it last changed and when.
///
/// # Errors
///
/// Returns database errors.
pub(super) fn folder_expected(conn: &Connection, row: &DocumentRow) -> Result<String, Error> {
    let folder = super::files::main_stem(row);
    let date = |time: &str| time.get(..10).unwrap_or(time).to_owned();
    let main_time = db::get_revision(conn, &row.id, row.main_revision)?
        .map(|r| r.created_at)
        .unwrap_or_default();
    let mut out = format!(
        "# {} {}\n\n{NOTICE}\n\nRevision: {}\n\n| File | Revision | Updated |\n| --- | --- | --- |\n",
        escape(&row.id),
        escape(&row.title),
        row.revision
    );
    let mut line = |path: &str, revision: u64, updated: &str| {
        let _ = writeln!(
            out,
            "| [[{folder}/{path}\\|{path}]] | {revision} | {} |",
            date(updated)
        );
    };
    line(folder, row.main_revision, &main_time);
    for file in db::live_files(conn, &row.id)? {
        line(&file.path, file.revision, &file.updated_at);
    }
    Ok(out)
}

impl Store {
    /// Rewrites every project's `INDEX.md` under the write lock, as `serve`
    /// does when it starts. Failures are logged, not returned.
    pub fn refresh_index_files(&self) {
        match lock(&self.paths) {
            Ok(_lock) => refresh_all(&self.conn, &self.paths.home, &self.settings),
            Err(error) => {
                tracing::warn!(%error, "could not take the write lock to write {FILE_NAME}");
            }
        }
    }
}

/// Rewrites `project`'s `INDEX.md` when `settings.index_file` is on. A
/// failure is logged, not returned: the document change that led here has
/// already been committed, and the next write, `serve` or `reindex`
/// repairs the file.
pub(super) fn refresh(conn: &Connection, home: &Path, settings: &Settings, project: &str) {
    if !settings.index_file {
        return;
    }
    if let Err(error) = write(conn, home, project) {
        tracing::warn!(%project, %error, "could not write {project}/{FILE_NAME}");
    }
}

/// The text `project`'s `INDEX.md` should hold.
///
/// # Errors
///
/// Returns database errors, and [`Error::ProjectNotFound`].
pub(super) fn expected(conn: &Connection, project: &str) -> Result<String, Error> {
    let row = db::find_project(conn, project)?
        .ok_or_else(|| Error::ProjectNotFound(project.to_owned()))?;
    let mut entries = Vec::new();
    for doc in db::live_documents_in(conn, row.id)? {
        let mut entry = Entry::from_row(&row, &doc);
        if doc.multifile {
            entry.files = Some(db::count_live_files(conn, &doc.id)?);
        }
        entries.push(entry);
    }
    Ok(render(&row, &entries))
}

/// Writes `project`'s `INDEX.md` unless it already holds the right text,
/// through a temporary file renamed into place.
fn write(conn: &Connection, home: &Path, project: &str) -> Result<(), Error> {
    let text = expected(conn, project)?;
    write_text(&home.join(project), &text)
}

/// Writes `text` to `dir/INDEX.md` unless it already holds it, through a
/// temporary file renamed into place.
fn write_text(dir: &Path, text: &str) -> Result<(), Error> {
    let path = dir.join(FILE_NAME);
    if fs::read_to_string(&path).is_ok_and(|current| current == text) {
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
    // A dot name, so `check` and Obsidian skip it if a crash leaves it.
    let temp = dir.join(format!(".{FILE_NAME}.tmp"));
    fs::write(&temp, text).map_err(Error::io("write", &temp))?;
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(Error::io("write", &path)(error));
    }
    Ok(())
}

/// One document's row in the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    /// Sequence number within the project, used to sort by ID.
    pub(super) seq: u64,
    /// Document ID, e.g. `JD-42`.
    pub(super) id: String,
    /// Title.
    pub(super) title: String,
    /// Document type.
    pub(super) doc_type: String,
    /// Status.
    pub(super) status: String,
    /// Path relative to the project folder, e.g. `plans/Plan.md`.
    pub(super) path: String,
    /// Date of the latest revision, `YYYY-MM-DD`.
    pub(super) updated: String,
    /// For a multi-file document, how many sub-files it has.
    pub(super) files: Option<u64>,
}

impl Entry {
    /// The entry for a live document of `project`.
    pub(super) fn from_row(project: &ProjectRow, row: &DocumentRow) -> Self {
        let prefix = format!("{}/", project.name);
        let path = row.path.strip_prefix(&prefix).unwrap_or(&row.path);
        Self {
            seq: row.seq,
            id: row.id.clone(),
            title: row.title.clone(),
            doc_type: row.doc_type.clone(),
            status: row.status.clone(),
            path: path.to_owned(),
            updated: row
                .updated_at
                .get(..10)
                .unwrap_or(&row.updated_at)
                .to_owned(),
            files: None,
        }
    }
}

/// The line under the heading.
const NOTICE: &str =
    "Generated by JoeDocs after every change. Do not edit: changes are overwritten.";

/// The text of `INDEX.md` for `project` and its live documents, sorted by
/// the number in their IDs whatever order `entries` is in.
pub(super) fn render(project: &ProjectRow, entries: &[Entry]) -> String {
    let name = if project.title.is_empty() {
        &project.name
    } else {
        &project.title
    };
    let mut out = format!("# {} ({})\n\n{NOTICE}\n\n", escape(name), project.key);
    if entries.is_empty() {
        out.push_str("No documents yet.\n");
        return out;
    }
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|entry| entry.seq);
    out.push_str("| ID | Title | Type | Status | Updated |\n| --- | --- | --- | --- | --- |\n");
    for entry in sorted {
        let files = match entry.files {
            Some(1) => " (+1 file)".to_owned(),
            Some(n) => format!(" (+{n} files)"),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "| {} | [{}]({}){files} | {} | {} | {} |",
            escape(&entry.id),
            escape(&entry.title),
            percent_encode(&entry.path),
            escape(&entry.doc_type),
            escape(&entry.status),
            escape(&entry.updated),
        );
    }
    out
}

/// `text` with the Markdown characters that could end a table cell or a
/// link, or start emphasis or code, escaped with a backslash.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '|' | '[' | ']' | '*' | '_' | '`' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `path` with every byte except unreserved characters and `/`
/// percent-encoded, so it works as a link target in any Markdown viewer.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(title: &str) -> ProjectRow {
        ProjectRow {
            id: 1,
            name: "joedocs".into(),
            key: "JD".into(),
            title: title.into(),
            created_at: "2026-11-01T00:00:00Z".into(),
            documents: 0,
        }
    }

    fn entry(seq: u64, title: &str, path: &str) -> Entry {
        Entry {
            seq,
            id: format!("JD-{seq}"),
            title: title.into(),
            doc_type: "plans".into(),
            status: "draft".into(),
            path: path.into(),
            updated: "2026-11-07".into(),
            files: None,
        }
    }

    #[test]
    fn documents_are_sorted_by_the_number_in_their_id() {
        let entries = [
            entry(10, "Ten", "plans/Ten.md"),
            entry(2, "Two", "plans/Two.md"),
            entry(11, "Eleven", "plans/Eleven.md"),
        ];
        let text = render(&project("JoeDocs"), &entries);
        let ids: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("| JD-"))
            .map(|rest| rest.split(' ').next().unwrap_or(""))
            .collect();
        assert_eq!(ids, ["2", "10", "11"], "{text}");
    }

    #[test]
    fn the_layout_has_a_heading_a_notice_and_the_table() {
        let text = render(&project("JoeDocs"), &[entry(1, "Plan", "plans/Plan.md")]);
        assert_eq!(
            text,
            "# JoeDocs (JD)\n\
             \n\
             Generated by JoeDocs after every change. Do not edit: changes are overwritten.\n\
             \n\
             | ID | Title | Type | Status | Updated |\n\
             | --- | --- | --- | --- | --- |\n\
             | JD-1 | [Plan](plans/Plan.md) | plans | draft | 2026-11-07 |\n"
        );
    }

    #[test]
    fn links_are_relative_and_percent_encoded() {
        let text = render(
            &project("JoeDocs"),
            &[entry(42, "Plan (JD-42)", "plans/Plan (JD-42).md")],
        );
        assert!(
            text.contains("[Plan (JD-42)](plans/Plan%20%28JD-42%29.md)"),
            "{text}"
        );
        let text = render(&project("JoeDocs"), &[entry(3, "Café", "notes/Café.md")]);
        assert!(text.contains("](notes/Caf%C3%A9.md)"), "{text}");
    }

    #[test]
    fn markdown_characters_in_titles_cannot_break_the_table_or_link() {
        let text = render(
            &project("JoeDocs"),
            &[entry(5, r"a | b [c] \d", "notes/a b c d.md")],
        );
        assert!(
            text.contains(r"| [a \| b \[c\] \\d](notes/a%20b%20c%20d.md) |"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_project_says_so() {
        let text = render(&project("JoeDocs"), &[]);
        assert_eq!(
            text,
            "# JoeDocs (JD)\n\
             \n\
             Generated by JoeDocs after every change. Do not edit: changes are overwritten.\n\
             \n\
             No documents yet.\n"
        );
    }

    #[test]
    fn a_project_without_a_title_uses_its_name() {
        let text = render(&project(""), &[]);
        assert!(text.starts_with("# joedocs (JD)\n"), "{text}");
    }

    #[test]
    fn entries_take_the_path_inside_the_project_and_the_date() {
        let row = DocumentRow {
            id: "JD-7".into(),
            project_id: 1,
            project: "joedocs".into(),
            seq: 7,
            doc_type: "reviews".into(),
            title: "Review".into(),
            path: "joedocs/reviews/Review.md".into(),
            status: "review".into(),
            revision: 3,
            created_at: "2026-11-01T10:00:00Z".into(),
            created_by: "laptop".into(),
            updated_at: "2026-11-07T19:36:19Z".into(),
            updated_by: "laptop".into(),
            client: "claude-code".into(),
            content_sha256: Vec::new(),
            archived_at: None,
            archived_by: None,
            archive_reason: None,
            multifile: false,
            main_revision: 3,
        };
        let entry = Entry::from_row(&project("JoeDocs"), &row);
        assert_eq!(
            entry,
            Entry {
                seq: 7,
                id: "JD-7".into(),
                title: "Review".into(),
                doc_type: "reviews".into(),
                status: "review".into(),
                path: "reviews/Review.md".into(),
                updated: "2026-11-07".into(),
                files: None,
            }
        );
    }
}
