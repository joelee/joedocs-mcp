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
use super::{Chrome, Section};
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
    files: Vec<Link>,
    revisions: Vec<Link>,
    older: bool,
    headings: Vec<Heading>,
    rendered_href: String,
    source_href: String,
    latest_href: String,
}

/// The URL of this page with `file`, `version` and `view`.
fn href(id: &str, file: Option<&str>, version: Option<u64>, source: bool) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    if let Some(file) = file {
        query.append_pair("file", file);
    }
    if let Some(version) = version {
        query.append_pair("version", &version.to_string());
    }
    if source {
        query.append_pair("view", "source");
    }
    let query = query.finish();
    if query.is_empty() {
        format!("/admin/documents/{id}")
    } else {
        format!("/admin/documents/{id}?{query}")
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
    let version = match request.query("version").filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(v.parse::<u64>().map_err(|_| {
            WebError::new(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                format!("\"{v}\" is not a revision number"),
            )
        })?),
    };
    let file = request.query("file").filter(|f| !f.is_empty());
    let source = request.query("view") == Some("source");
    let fetched = match file {
        Some(file) => store.get_file(id, file, version),
        None => store.get(id, version),
    }
    .map_err(|e| refused(state, session, &e))?;
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
        markdown::render(&fetched.markdown, &resolver, &state.options.plantuml_url)
    };
    let fields = front_matter::parse(&fetched.markdown)
        .map(|parsed| parsed.fields)
        .unwrap_or_default();
    let shown_version = fetched.revision.revision;
    let latest = document.revision;
    let pinned = (shown_version != latest).then_some(shown_version);
    let page = DocumentPage {
        title: &document.title,
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
        files: file_links(&document.id, &files, file, pinned),
        revisions: (latest.saturating_sub(PICKER_REVISIONS) + 1..=latest)
            .rev()
            .map(|v| Link {
                href: href(&document.id, file, (v != latest).then_some(v), source),
                label: if v == latest {
                    format!("{v} (latest)")
                } else {
                    v.to_string()
                },
                current: v == shown_version,
            })
            .collect(),
        older: latest > PICKER_REVISIONS,
        headings: rendered.headings,
        rendered_href: href(&document.id, file, pinned, false),
        source_href: href(&document.id, file, pinned, true),
        latest_href: href(&document.id, file, None, source),
        fetched: &fetched,
    };
    let needs_pele = page.needs_pele;
    let mut response = render(StatusCode::OK, &page);
    if needs_pele {
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
    }
    Ok(response)
}

/// The refusal for a document that cannot be shown; an archived one links
/// to the Archive.
fn refused(_state: &WebState, _session: &Session, error: &store::Error) -> WebError {
    let refusal = WebError::from_store(error);
    if matches!(error, store::Error::Archived(_)) {
        let page = super::ErrorPage {
            title: "Archived",
            assets: assets::urls(),
            status: refusal.status.as_u16(),
            message: &refusal.message,
            link: Some(("/admin/archive", "Find it in the Archive")),
        };
        let page = render(refusal.status, &page);
        return refusal.with_page(page);
    }
    refusal
}

/// The files of a multi-file document, main file first.
fn file_links(
    id: &str,
    files: &[FileInfo],
    shown: Option<&str>,
    version: Option<u64>,
) -> Vec<Link> {
    files
        .iter()
        .map(|f| {
            let file = (!f.main).then_some(f.file.as_str());
            Link {
                href: href(id, file, version, false),
                label: if f.main {
                    format!("{} (main file)", f.file)
                } else {
                    f.file.clone()
                },
                current: file == shown,
            }
        })
        .collect()
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
