//! The API keys page: every key with its grants, creation, last use and
//! state, and the `joedocs key` commands (create, grant, ungrant, rotate,
//! revoke) with their rules and messages. A new or rotated token is shown
//! once, on the reply to that `POST`, and nowhere else: not in a URL, a
//! redirect, the audit log or a log line.

use std::str::FromStr as _;

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;
use secrecy::ExposeSecret as _;

use super::super::assets::{self, Urls};
use super::super::{Session, WebError, WebRequest, redirect, render};
use super::{Chrome, Section, link};
use crate::admin::grant_list;
use crate::auth::{Grant, OsRandom};
use crate::store::{self, CreatedKey, Store};

/// One key in the table.
pub(crate) struct KeyLine {
    pub(crate) name: String,
    pub(crate) id: String,
    pub(crate) created: String,
    pub(crate) last_used: Option<String>,
    pub(crate) revoked: Option<String>,
    /// Each grant as `project:permission`, with its project.
    pub(crate) grants: Vec<(String, String)>,
}

/// One project's permission in the Create key form.
pub(crate) struct GrantChoice {
    pub(crate) project: String,
    /// `""`, `"read"` or `"write"`.
    pub(crate) chosen: String,
}

#[derive(Template)]
#[template(path = "keys.html")]
struct KeysPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    keys: Vec<KeyLine>,
    projects: Vec<String>,
    notice: Option<String>,
    error: Option<String>,
    /// The Create key form: the name typed and a choice per project.
    name: String,
    choices: Vec<GrantChoice>,
    /// Whether the Create key dialog opens with the page (after a refusal).
    open: bool,
}

#[derive(Template)]
#[template(path = "key_token.html")]
struct TokenPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    message: String,
    token: &'a str,
}

/// `GET /admin/keys`, with the outcome of the last action when the URL
/// names it (`updated` or `revoked`).
pub(crate) fn list(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let named = |key: &str| request.query(key).filter(|n| !n.is_empty());
    let find = |name: &str| -> Result<Option<crate::db::KeyRow>, WebError> {
        Ok(store
            .keys()
            .map_err(|e| WebError::from_store(&e))?
            .into_iter()
            .find(|k| k.name == name))
    };
    let notice = if let Some(name) = named("updated") {
        find(name)?.map(|k| format!("API key {} grants: {}", k.name, grant_list(&k.grants)))
    } else if let Some(name) = named("revoked") {
        find(name)?.map(|k| {
            format!(
                "API key {} is revoked (since {}).",
                k.name,
                k.revoked_at.as_deref().unwrap_or("now")
            )
        })
    } else {
        None
    };
    page(
        store,
        session,
        StatusCode::OK,
        notice,
        None,
        Form::default(),
    )
}

/// What a refused Create key form held, to show it again.
#[derive(Default)]
struct Form {
    name: String,
    grants: Vec<(String, String)>,
}

/// `POST /admin/keys`: `name`, and `grant.<project>` = `read` or `write`
/// for each project to grant.
pub(crate) fn create(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let name = request.form("name").unwrap_or("").trim().to_owned();
    let projects = store.projects().map_err(|e| WebError::from_store(&e))?;
    let chosen: Vec<(String, String)> = projects
        .iter()
        .filter_map(|p| {
            let value = request.form(&format!("grant.{}", p.name))?.trim();
            (!value.is_empty()).then(|| (p.name.clone(), value.to_owned()))
        })
        .collect();
    let form = Form {
        name: name.clone(),
        grants: chosen.clone(),
    };
    let grants = match parse_grants(&chosen) {
        Ok(grants) => grants,
        Err(message) => return refuse(store, session, invalid(message), form),
    };
    match store.create_key(&name, &grants, &OsRandom) {
        Ok(created) => {
            let message = format!(
                "Created API key {} (id {}) with grants: {}",
                created.key.name,
                created.key.id,
                grant_list(&created.key.grants)
            );
            Ok(token_page(session, "API key created", message, &created))
        }
        Err(error) => refused(store, session, &error, form),
    }
}

/// `POST /admin/keys/{name}/grant`: `project` and `permission`.
pub(crate) fn grant(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
    name: &str,
) -> Result<Response, WebError> {
    let project = request.form("project").unwrap_or("").trim();
    let permission = request.form("permission").unwrap_or("").trim();
    let grant = match Grant::from_str(&format!("{project}:{permission}")) {
        Ok(grant) => grant,
        Err(message) => {
            return refuse(store, session, invalid(message), Form::default());
        }
    };
    match store.grant(name, &[grant]) {
        Ok(key) => Ok(updated(&key.name)),
        Err(error) => refused(store, session, &error, Form::default()),
    }
}

/// `POST /admin/keys/{name}/ungrant`: `project`.
pub(crate) fn ungrant(
    store: &mut Store,
    request: &WebRequest,
    session: &Session,
    name: &str,
) -> Result<Response, WebError> {
    let project = request.form("project").unwrap_or("").trim().to_owned();
    let projects: Vec<String> = Some(project)
        .filter(|p| !p.is_empty())
        .into_iter()
        .collect();
    match store.ungrant(name, &projects) {
        Ok(key) => Ok(updated(&key.name)),
        Err(error) => refused(store, session, &error, Form::default()),
    }
}

/// `POST /admin/keys/{name}/rotate`: a new token, shown once.
pub(crate) fn rotate(
    store: &mut Store,
    session: &Session,
    name: &str,
) -> Result<Response, WebError> {
    match store.rotate_key(name, &OsRandom) {
        Ok(rotated) => {
            let message = format!(
                "Rotated API key {} (id {}); the old token no longer works.",
                rotated.key.name, rotated.key.id
            );
            Ok(token_page(session, "API key rotated", message, &rotated))
        }
        Err(error) => refused(store, session, &error, Form::default()),
    }
}

/// `POST /admin/keys/{name}/revoke`.
pub(crate) fn revoke(
    store: &mut Store,
    session: &Session,
    name: &str,
) -> Result<Response, WebError> {
    match store.revoke(name) {
        Ok(key) => Ok(redirect(&link(
            "/admin/keys",
            &[("revoked", Some(&key.name))],
        ))),
        Err(error) => refused(store, session, &error, Form::default()),
    }
}

/// The grants chosen in the Create key form, parsed as `--grant` is.
fn parse_grants(chosen: &[(String, String)]) -> Result<Vec<Grant>, String> {
    chosen
        .iter()
        .map(|(project, permission)| Grant::from_str(&format!("{project}:{permission}")))
        .collect()
}

fn updated(name: &str) -> Response {
    redirect(&link("/admin/keys", &[("updated", Some(name))]))
}

/// The reveal-once reply: the token is on this page only.
fn token_page(session: &Session, title: &str, message: String, created: &CreatedKey) -> Response {
    let page = TokenPage {
        title,
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Keys, ""),
        message,
        token: created.token.expose_secret(),
    };
    render(StatusCode::OK, &page)
}

/// The keys page again, with the store's refusal.
fn refused(
    store: &Store,
    session: &Session,
    error: &store::Error,
    form: Form,
) -> Result<Response, WebError> {
    refuse(store, session, WebError::from_store(error), form)
}

/// A request the page refuses itself, as `invalid_argument`.
fn invalid(message: String) -> WebError {
    WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", message)
}

/// The keys page again with `refusal`'s message, kept as the error so the
/// request is audited with its code.
fn refuse(
    store: &Store,
    session: &Session,
    refusal: WebError,
    form: Form,
) -> Result<Response, WebError> {
    let shown = page(
        store,
        session,
        refusal.status,
        None,
        Some(refusal.message.clone()),
        form,
    )?;
    Err(refusal.with_page(shown))
}

fn page(
    store: &Store,
    session: &Session,
    status: StatusCode,
    notice: Option<String>,
    error: Option<String>,
    form: Form,
) -> Result<Response, WebError> {
    let keys = store.keys().map_err(|e| WebError::from_store(&e))?;
    let projects: Vec<String> = store
        .projects()
        .map_err(|e| WebError::from_store(&e))?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let open = error.is_some() && !form.name.is_empty();
    let page = KeysPage {
        title: "API keys",
        assets: assets::urls(),
        chrome: Chrome::new(session, Section::Keys, ""),
        keys: keys
            .into_iter()
            .map(|k| KeyLine {
                grants: k
                    .grants
                    .iter()
                    .map(|g| (g.project.clone(), g.to_string()))
                    .collect(),
                name: k.name,
                id: k.id,
                created: k.created_at,
                last_used: k.last_used_at,
                revoked: k.revoked_at,
            })
            .collect(),
        choices: projects
            .iter()
            .map(|p| GrantChoice {
                project: p.clone(),
                chosen: form
                    .grants
                    .iter()
                    .find(|(name, _)| name == p)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            })
            .collect(),
        projects,
        notice,
        error,
        name: form.name,
        open,
    };
    Ok(render(status, &page))
}
