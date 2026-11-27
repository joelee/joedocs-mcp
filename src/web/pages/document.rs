//! The document view: one file of a document at one revision, rendered or
//! as stored, with its properties, its other files, its revisions and an
//! "On this page" list.

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::super::assets::{self, Urls};
use super::super::markdown::{self, Heading, Resolver, Target};
use super::super::{Session, WebError, WebRequest, WebState, render};
use super::{Chrome, Section, actor, link, refused_with_link};
use crate::document::front_matter::{self, SERVER_OWNED};
use crate::document::is_project_key;
use crate::store::{self, Fetched, FileInfo, Store};

/// Most revisions the picker links; the History tab has the rest.
const PICKER_REVISIONS: u64 = 30;

/// Links resolved against the store, for the renderer.
struct StoreResolver<'s> {
    store: &'s Store,
    project: &'s str,
    /// The document shown, when it is multi-file: links may name its files.
    folder: Option<&'s str>,
}

impl Resolver for StoreResolver<'_> {
    fn wiki(&self, target: &str) -> Option<Target> {
        if let Ok(Some(id)) = self.store.find_by_title(self.project, target) {
            return Some(Target { id, file: None });
        }
        let folder = self.folder?;
        let file = self.store.find_file(folder, target).ok().flatten()?;
        Some(Target {
            id: folder.to_owned(),
            file: Some(file),
        })
    }

    fn document(&self, id: &str) -> bool {
        self.store.document_info(id).is_ok()
    }
}

/// One shown value of a property.
pub(crate) enum PropValue {
    Text(String),
    Document(String),
    Url(String),
}

/// One front matter key and its values.
pub(crate) struct Property {
    pub(crate) key: String,
    pub(crate) values: Vec<PropValue>,
}

/// A link in a side list, marked when it is the page shown.
pub(crate) struct Link {
    pub(crate) href: String,
    pub(crate) label: String,
    pub(crate) current: bool,
}

#[derive(Template)]
#[template(path = "document.html")]
struct DocumentPage<'a> {
    title: &'a str,
    facts: &'a Facts,
    assets: Urls,
    chrome: Chrome<'a>,
    fetched: &'a Fetched,
    /// The file shown, for a sub-file.
    file: Option<&'a str>,
    source: bool,
    rendered: String,
    needs_pele: bool,
    markdown: &'a str,
    tags: Vec<String>,
    aliases: Vec<String>,
    properties: Vec<Property>,
    /// The Files panel, as a tree flattened for the template.
    files: Vec<FileItem>,
    revisions: Vec<Link>,
    older: bool,
    headings: Vec<Heading>,
    rendered_href: String,
    source_href: String,
    latest_href: String,
    history_href: String,
    download_href: String,
    /// All files as a `.tgz`, for a multi-file document.
    archive_href: Option<String>,
    /// What the last action did, when the URL says.
    notice: Option<String>,
    /// What the Actions menu offers.
    menu: Menu,
    /// The stream of the document's revisions, on its latest revision
    /// only: the page then updates itself (`document.js`).
    live: Option<String>,
}

/// The Actions menu of the page shown.
pub(crate) struct Menu {
    /// The structural action it offers.
    pub(crate) offer: Offer,
    /// Whether it offers to archive the document: on the latest revision
    /// of its main page.
    pub(crate) archive: bool,
}

impl Menu {
    /// Whether the menu has anything to offer.
    pub(crate) fn is_empty(&self) -> bool {
        self.offer == Offer::Nothing && !self.archive
    }
}

/// What the Actions menu offers on the page shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Nothing: an older revision, or a multi-file document's main file.
    Nothing,
    /// Convert, on the latest single-file document.
    Convert,
    /// Move this file, on the latest revision of a multi-file document's
    /// file.
    Move,
}

/// The document's title, status, type and server facts as the page shows
/// them. They come from the stored front matter of the main file shown, so
/// an older revision shows its own. A file of a multi-file document stores
/// none of them, so its page shows the document's current ones, labelled as
/// current on an older revision.
pub(crate) struct Facts {
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) doc_type: String,
    pub(crate) created: String,
    pub(crate) created_by: String,
    pub(crate) updated: String,
    pub(crate) updated_by: String,
    pub(crate) client: String,
    /// Whether these are the current values on an older revision's page.
    pub(crate) current: bool,
}

fn facts(
    document: &crate::store::DocumentInfo,
    fields: &front_matter::Fields,
    sub_file: bool,
    pinned: bool,
) -> Facts {
    // A key the stored front matter lacks falls back to the current value.
    let stored = |key: &str, now: &str| {
        fields
            .get(key)
            .filter(|_| !sub_file)
            .and_then(Value::as_str)
            .unwrap_or(now)
            .to_owned()
    };
    Facts {
        title: stored("title", &document.title),
        status: stored("status", &document.status),
        doc_type: stored("type", &document.doc_type),
        created: stored("created", &document.created),
        created_by: stored("created_by", &document.created_by),
        updated: stored("updated", &document.updated),
        updated_by: stored("updated_by", &document.updated_by),
        client: stored("client", &document.client),
        current: sub_file && pinned,
    }
}

/// The URL of this page with `file`, `version` and `view`.
pub(crate) fn href(id: &str, file: Option<&str>, version: Option<u64>, source: bool) -> String {
    let version = version.map(|v| v.to_string());
    link(
        &format!("/admin/documents/{id}"),
        &[
            ("file", file),
            ("version", version.as_deref()),
            ("view", source.then_some("source")),
        ],
    )
}

/// The `version` the query asks for, if any.
fn version(request: &WebRequest) -> Result<Option<u64>, WebError> {
    match request.query("version").filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v.parse::<u64>().map(Some).map_err(|_| {
            WebError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("\"{v}\" is not a revision number"),
            )
        }),
    }
}

/// `GET /admin/documents/{id}`.
pub(crate) fn view(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let version = version(request)?;
    let file = request.query("file").filter(|f| !f.is_empty());
    let source = request.query("view") == Some("source");
    let fetched = match file {
        Some(file) => store.get_file(id, file, version),
        None => store.get(id, version),
    }
    .map_err(|e| refused(&e))?;
    let document = &fetched.document;
    let files = if document.multifile {
        store
            .list_files(&document.id, version)
            .map_err(|e| WebError::from_store(&e))?
    } else {
        Vec::new()
    };
    let resolver = StoreResolver {
        store,
        project: &document.project,
        folder: document.multifile.then_some(document.id.as_str()),
    };
    let rendered = if source {
        markdown::Rendered::default()
    } else {
        markdown::render(
            &fetched.markdown,
            &resolver,
            state.options.diagrams.server_url(),
        )
    };
    let fields = front_matter::parse(&fetched.markdown)
        .map(|parsed| parsed.fields)
        .unwrap_or_default();
    let shown_version = fetched.revision.revision;
    let latest = document.revision;
    let pinned = (shown_version != latest).then_some(shown_version);
    let pinned_text = pinned.map(|v| v.to_string());
    let facts = facts(document, &fields, file.is_some(), pinned.is_some());
    let page = DocumentPage {
        title: &facts.title,
        facts: &facts,
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Documents, ""),
        file,
        source,
        needs_pele: rendered.needs_pele,
        rendered: rendered.html,
        markdown: &fetched.markdown,
        tags: strings(fields.get("tags")),
        aliases: strings(fields.get("aliases")),
        properties: properties(&fields, &resolver, file.is_some()),
        files: file_tree(&document.id, &files, file, pinned),
        revisions: revision_links(&document.id, file, source, latest, shown_version),
        older: latest > PICKER_REVISIONS,
        headings: rendered.headings,
        rendered_href: href(&document.id, file, pinned, false),
        source_href: href(&document.id, file, pinned, true),
        latest_href: href(&document.id, file, None, source),
        live: pinned
            .is_none()
            .then(|| format!("/admin/documents/{}/stream", document.id)),
        notice: notice(request, document, file),
        menu: Menu {
            archive: pinned.is_none() && file.is_none(),
            offer: match (pinned, document.multifile, file) {
                (None, false, None) => Offer::Convert,
                (None, true, Some(_)) => Offer::Move,
                _ => Offer::Nothing,
            },
        },
        history_href: link(
            &format!("/admin/documents/{}/history", document.id),
            &[("file", file)],
        ),
        download_href: link(
            &format!("/admin/documents/{}/download", document.id),
            &[("file", file), ("version", pinned_text.as_deref())],
        ),
        archive_href: document.multifile.then(|| {
            link(
                &format!("/admin/documents/{}/download", document.id),
                &[
                    ("archive", Some("true")),
                    ("version", pinned_text.as_deref()),
                ],
            )
        }),
        fetched: &fetched,
    };
    let needs_pele = page.needs_pele;
    let response = render(StatusCode::OK, &page);
    Ok(if needs_pele {
        with_pele_policy(response, state)
    } else {
        response
    })
}

/// `response` with the policy a page that loads Pele needs: the usual one
/// plus `style-src-attr 'unsafe-inline'` (Pele styles the SVG it draws).
fn with_pele_policy(mut response: Response, state: &WebState) -> Response {
    let policy = format!(
        "{}{}",
        state.csp.to_str().unwrap_or("default-src 'none'"),
        super::super::PELE_STYLE_ATTRIBUTES
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&policy) {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

/// The refusal for a document that cannot be shown; an archived one links
/// to the Archive.
pub(crate) fn refused(error: &store::Error) -> WebError {
    let refusal = WebError::from_store(error);
    if matches!(error, store::Error::Archived(_)) {
        let page = super::ErrorPage {
            title: "Archived",
            assets: assets::urls(),
            status: refusal.status.as_u16(),
            message: &refusal.message,
            link: Some(("/admin/archive", "Find it in the Archive")),
            reset_view: None,
        };
        let page = render(refusal.status, &page);
        return refusal.with_page(page);
    }
    refusal
}

/// `GET /admin/documents/{id}/download`: the file as `joedocs get -f`
/// writes it, or with `archive=true` every file as `get --archive` does.
pub(crate) fn download(
    store: &Store,
    request: &WebRequest,
    id: &str,
) -> Result<Response, WebError> {
    let version = version(request)?;
    let file = request.query("file").filter(|f| !f.is_empty());
    if request.query("archive") == Some("true") {
        let bytes = crate::admin::folder_archive(store, id, version).map_err(|e| failed(&e))?;
        return Ok(attachment(
            bytes,
            "application/gzip",
            &download_name(id, None, version, "tgz"),
        ));
    }
    let markdown = match file {
        Some(file) => crate::admin::fetch_file(store, id, file, version),
        None => crate::admin::fetch(store, id, version),
    }
    .map_err(|e| failed(&e))?;
    Ok(attachment(
        markdown.into_bytes(),
        "text/markdown; charset=utf-8",
        &download_name(id, file, version, "md"),
    ))
}

/// The refusal for an admin operation that failed.
fn failed(error: &crate::Error) -> WebError {
    match error {
        crate::Error::Store(error) => refused(error),
        crate::Error::Archived(id) => refused(&store::Error::Archived(id.clone())),
        other => {
            tracing::error!(error = %other, "web download failed");
            WebError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    }
}

/// `bytes` as a download named `name`.
fn attachment(bytes: Vec<u8>, content_type: &'static str, name: &str) -> Response {
    use axum::http::{HeaderValue, header};
    use axum::response::IntoResponse as _;
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// A download's file name, ASCII letters, digits, `.`, `_` and `-` only:
/// `JD-3-items-first-r2.md` for file `items/first` of JD-3 at revision 2.
fn download_name(id: &str, file: Option<&str>, version: Option<u64>, extension: &str) -> String {
    let safe = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let mut name = safe(id);
    if let Some(file) = file {
        name.push('-');
        name.push_str(&safe(file));
    }
    match version {
        Some(version) => format!("{name}-r{version}.{extension}"),
        None => format!("{name}.{extension}"),
    }
}

/// The line that reports what an action did, from the `done` (and `from`)
/// the action redirected with. Only these known codes are shown.
fn notice(
    request: &WebRequest,
    document: &crate::store::DocumentInfo,
    file: Option<&str>,
) -> Option<String> {
    let (id, revision) = (&document.id, document.revision);
    let from = request.query("from").filter(|f| !f.is_empty());
    match request.query("done")? {
        "unarchived" => Some(format!(
            "Restored {id} from the archive; it is now at revision {revision}."
        )),
        "converted" => Some(format!(
            "Converted {id} to a multi-file document at revision {revision}."
        )),
        "moved" => {
            let source = from?.parse::<crate::document::DocumentId>().ok()?;
            Some(format!(
                "Moved here from {source}; {id} is now at revision {revision}."
            ))
        }
        "restored" => {
            let version = from?.parse::<u64>().ok()?;
            Some(format!(
                "Restored {} as it was at revision {version}; {id} is now at revision {revision}.",
                file?
            ))
        }
        _ => None,
    }
}

/// A link back to document `id`, for a refused action.
fn back(id: &str) -> String {
    format!("/admin/documents/{id}")
}

/// `POST /admin/documents/{id}/convert`, as `joedocs document convert`.
pub(crate) fn convert(
    store: &mut Store,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let converted = store
        .convert(&actor(session), id)
        .map_err(|e| refused_with_link(&e, &back(id), "Back to the document"))?;
    Ok(super::super::redirect(&link(
        &back(&converted.id),
        &[("done", Some("converted"))],
    )))
}

/// `POST /admin/documents/{id}/archive` with a `reason`, as the MCP tool
/// `archive_document`: then to the Archive, where it can be restored.
pub(crate) fn archive(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let reason = request.form("reason").unwrap_or("");
    let archived = store
        .archive(&actor(session), id, reason)
        .map_err(|e| refused_with_link(&e, &back(id), "Back to the document"))?;
    Ok(super::super::redirect(&link(
        "/admin/archive",
        &[("done", Some("archived")), ("id", Some(&archived.id))],
    )))
}

/// `POST /admin/documents/{id}/move-file` with `file`, `to` and an
/// optional `as`, as `joedocs document move-file`.
pub(crate) fn move_file(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let field = |key: &str| request.form(key).unwrap_or("").trim();
    let as_file = Some(field("as")).filter(|a| !a.is_empty());
    let moved = store
        .move_file(&actor(session), id, field("file"), field("to"), as_file)
        .map_err(|e| refused_with_link(&e, &back(id), "Back to the document"))?;
    Ok(super::super::redirect(&link(
        &back(&moved.target),
        &[
            ("file", Some(&moved.file)),
            ("done", Some("moved")),
            ("from", Some(&moved.source)),
        ],
    )))
}

/// `POST /admin/documents/{id}/restore-file` with `file` and `version`, as
/// `joedocs document restore-file`.
pub(crate) fn restore_file(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
    id: &str,
) -> Result<Response, WebError> {
    let file = request.form("file").unwrap_or("").trim();
    let text = request.form("version").unwrap_or("").trim();
    let version = text.parse::<u64>().map_err(|_| {
        WebError::new(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            format!("`version` must be a revision number, not \"{text}\""),
        )
    })?;
    let restored = store
        .restore_file(&actor(session), id, file, version)
        .map_err(|e| refused_with_link(&e, &back(id), "Back to the document"))?;
    let version = version.to_string();
    Ok(super::super::redirect(&link(
        &back(&restored.id),
        &[
            ("file", restored.file.as_deref()),
            ("done", Some("restored")),
            ("from", Some(&version)),
        ],
    )))
}

/// The newest revisions for the picker, newest first, the one shown
/// marked.
fn revision_links(
    id: &str,
    file: Option<&str>,
    source: bool,
    latest: u64,
    shown: u64,
) -> Vec<Link> {
    (latest.saturating_sub(PICKER_REVISIONS) + 1..=latest)
        .rev()
        .map(|v| Link {
            href: href(id, file, (v != latest).then_some(v), source),
            label: if v == latest {
                format!("{v} (latest)")
            } else {
                v.to_string()
            },
            current: v == shown,
        })
        .collect()
}

/// A directory of the Files panel: its name, its path in the document,
/// how many files it holds (its subdirectories' included), and whether it
/// starts open (it holds the file shown).
pub(crate) struct FileDir {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) count: usize,
    pub(crate) open: bool,
}

/// One entry of the Files panel in the order the template writes it:
/// a directory opens, its files and subdirectories follow, then it closes.
pub(crate) enum FileItem {
    Open(FileDir),
    File(Link),
    Close,
}

/// The files of a directory and its subdirectories, by name.
#[derive(Default)]
struct FileNode {
    files: Vec<Link>,
    dirs: std::collections::BTreeMap<String, FileNode>,
}

impl FileNode {
    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(Self::count).sum::<usize>()
    }

    /// Writes this node's files, then each subdirectory, into `out`.
    fn flatten(self, prefix: &str, shown: Option<&str>, out: &mut Vec<FileItem>) {
        out.extend(self.files.into_iter().map(FileItem::File));
        for (name, dir) in self.dirs {
            let path = format!("{prefix}{name}");
            let inner = format!("{path}/");
            out.push(FileItem::Open(FileDir {
                count: dir.count(),
                open: shown.is_some_and(|s| s.starts_with(&inner)),
                name,
                path,
            }));
            dir.flatten(&inner, shown, out);
            out.push(FileItem::Close);
        }
    }
}

/// The files of a multi-file document as a tree: the main file and the
/// files at the top level first, then each directory with its files and
/// subdirectories, in name order (D-06). A file is labelled by its name in
/// its directory.
fn file_tree(
    id: &str,
    files: &[FileInfo],
    shown: Option<&str>,
    version: Option<u64>,
) -> Vec<FileItem> {
    let mut root = FileNode::default();
    let mut main = Vec::new();
    for f in files {
        let file = (!f.main).then_some(f.file.as_str());
        let link = |label: String| Link {
            href: href(id, file, version, false),
            label,
            current: file == shown,
        };
        if f.main {
            main.push(link(format!("{} (main file)", f.file)));
            continue;
        }
        let mut parts: Vec<&str> = f.file.split('/').collect();
        let name = parts.pop().unwrap_or_default().to_owned();
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_owned()).or_default();
        }
        node.files.push(link(name));
    }
    sort_files(&mut root);
    let mut out: Vec<FileItem> = main.into_iter().map(FileItem::File).collect();
    root.flatten("", shown, &mut out);
    out
}

/// Sorts every directory's files by name.
fn sort_files(node: &mut FileNode) {
    node.files.sort_by(|a, b| a.label.cmp(&b.label));
    for dir in node.dirs.values_mut() {
        sort_files(dir);
    }
}

/// A front matter value as a list of strings (for tags and aliases).
fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().filter_map(scalar).collect(),
        Some(other) => scalar(other).into_iter().collect(),
        None => Vec::new(),
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// The keys the properties panel lists: custom keys for a main file (its
/// server keys are shown as facts), every key but tags and aliases for a
/// sub-file.
fn properties(
    fields: &front_matter::Fields,
    resolver: &dyn Resolver,
    sub_file: bool,
) -> Vec<Property> {
    fields
        .iter()
        .filter(|(key, _)| {
            !matches!(key.as_str(), "tags" | "aliases")
                && (sub_file || !SERVER_OWNED.contains(&key.as_str()) && key.as_str() != "status")
        })
        .map(|(key, value)| Property {
            key: key.clone(),
            values: match value {
                Value::Array(items) => items.iter().map(|v| prop_value(v, resolver)).collect(),
                other => vec![prop_value(other, resolver)],
            },
        })
        .collect()
}

/// A value shown as a document link, an external link or text.
fn prop_value(value: &Value, resolver: &dyn Resolver) -> PropValue {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    };
    let looks_like_id = text.split_once('-').is_some_and(|(key, n)| {
        is_project_key(key) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    });
    if looks_like_id && resolver.document(&text) {
        return PropValue::Document(text);
    }
    let lower = text.to_ascii_lowercase();
    if (lower.starts_with("https://") || lower.starts_with("http://"))
        && !text.contains(char::is_whitespace)
    {
        return PropValue::Url(text);
    }
    PropValue::Text(text)
}

#[cfg(test)]
mod tests {
    use super::{FileItem, download_name, file_tree};
    use crate::store::FileInfo;

    fn info(file: &str, main: bool) -> FileInfo {
        FileInfo {
            file: file.to_owned(),
            main,
            revision: 1,
            updated: String::new(),
        }
    }

    /// The tree as lines: `+dir (count) open|closed`, `-` to close it, and
    /// `label` (with `*` when current) for a file.
    fn outline(items: &[FileItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                FileItem::Open(dir) => format!(
                    "+{} ({}) {}",
                    dir.path,
                    dir.count,
                    if dir.open { "open" } else { "closed" }
                ),
                FileItem::Close => "-".to_owned(),
                FileItem::File(link) => {
                    format!("{}{}", link.label, if link.current { "*" } else { "" })
                }
            })
            .collect()
    }

    #[test]
    fn files_are_grouped_by_directory_after_the_main_and_top_level_files() {
        let files = [
            info("Research", true),
            info("brief", false),
            info("coordinator/log", false),
            info("coordinator/state", false),
            info("plan", false),
            info("sources/S-001-one", false),
            info("sources/deep/S-003-three", false),
            info("sources/S-002-two", false),
        ];
        let tree = file_tree("JD-7", &files, Some("sources/deep/S-003-three"), None);
        assert_eq!(
            outline(&tree),
            [
                "Research (main file)",
                "brief",
                "plan",
                "+coordinator (2) closed",
                "log",
                "state",
                "-",
                "+sources (3) open",
                "S-001-one",
                "S-002-two",
                "+sources/deep (1) open",
                "S-003-three*",
                "-",
                "-",
            ]
        );
        let FileItem::File(link) = &tree[10 + 1] else {
            panic!("a file");
        };
        assert_eq!(
            link.href,
            "/admin/documents/JD-7?file=sources%2Fdeep%2FS-003-three"
        );
    }

    #[test]
    fn the_main_file_shown_leaves_every_directory_closed() {
        let files = [
            info("Backlog", true),
            info("items/one", false),
            info("items", false),
        ];
        let tree = file_tree("JD-3", &files, None, Some(4));
        assert_eq!(
            outline(&tree),
            [
                "Backlog (main file)*",
                "items",
                "+items (1) closed",
                "one",
                "-"
            ]
        );
        let FileItem::File(main) = &tree[0] else {
            panic!("a file");
        };
        assert_eq!(main.href, "/admin/documents/JD-3?version=4");
    }

    #[test]
    fn download_names_are_plain_ascii() {
        assert_eq!(download_name("JD-1", None, None, "md"), "JD-1.md");
        assert_eq!(
            download_name("JD-3", Some("items/first"), Some(2), "md"),
            "JD-3-items-first-r2.md"
        );
        assert_eq!(
            download_name("JD-3", Some("caf\u{e9} \"x\";\r\n"), None, "md"),
            "JD-3-caf---x----.md"
        );
        assert_eq!(download_name("JD-3", None, Some(4), "tgz"), "JD-3-r4.tgz");
    }
}
