//! The Account and Users pages (0.5.1): a signed-in user changes their
//! password and revokes their devices, and manages other users with the
//! `joedocs user` commands. An admin token's session gets `403
//! user_required` on both pages.
//!
//! Changing a password, creating a user and resetting one hash a password,
//! so those posts run in three phases outside the dispatcher's store lock
//! (JD-36 C-04), checking the session and CSRF token under the lock before
//! and after the hash.

use std::sync::Arc;

use askama::Template;
use axum::http::StatusCode;
use axum::response::Response;
use secrecy::{ExposeSecret as _, SecretString};

use super::super::assets::{self, Urls};
use super::super::{
    Session, WebError, WebRequest, WebState, redirect, render, signed_in, write_audit,
};
use super::{Chrome, Section, link};
use crate::auth::OsRandom;
use crate::db::UserRow;
use crate::password;
use crate::store::{Admin, AdminRefusal, Error, Store, UserState};

/// One device on the Account page.
pub(crate) struct DeviceLine {
    pub(crate) name: String,
    /// The name, percent-encoded for the revoke form's path.
    pub(crate) path: String,
    pub(crate) created: String,
    pub(crate) last_used: Option<String>,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    user: &'a str,
    enrolled: Option<String>,
    devices: Vec<DeviceLine>,
    device_days: u32,
    notice: Option<String>,
    error: Option<String>,
}

/// One user on the Users page, with the confirm dialogs of its actions as
/// `(path action, heading, explanation, button)`.
pub(crate) struct UserLine {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) enrolled: Option<String>,
    pub(crate) last_login: Option<String>,
    pub(crate) devices: usize,
    pub(crate) created: String,
    pub(crate) you: bool,
    /// The Manage menu's buttons, as `(action, class, label)`; none for a
    /// removed user.
    pub(crate) buttons: Vec<(&'static str, &'static str, &'static str)>,
    pub(crate) actions: Vec<(&'static str, String, String, String)>,
}

#[derive(Template)]
#[template(path = "users.html")]
struct UsersPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    users: Vec<UserLine>,
    notice: Option<String>,
    error: Option<String>,
    name: String,
    open: bool,
}

#[derive(Template)]
#[template(path = "user_password.html")]
struct PasswordPage<'a> {
    title: &'a str,
    assets: Urls,
    chrome: Chrome<'a>,
    message: String,
    user: &'a str,
    until: &'a str,
    next: &'a str,
    password: &'a str,
}

/// What a user action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserAction {
    Disable,
    Enable,
    Unlock,
    ResetPassword,
    ResetTotp,
}

impl UserAction {
    /// The action named in a `/admin/users/{name}/{action}` path.
    pub(crate) fn of(action: &str) -> Option<Self> {
        Some(match action {
            "disable" => Self::Disable,
            "enable" => Self::Enable,
            "unlock" => Self::Unlock,
            "reset-password" => Self::ResetPassword,
            "reset-totp" => Self::ResetTotp,
            _ => return None,
        })
    }

    /// The audit tool, as the admin API names it.
    pub(crate) fn operation(self) -> &'static str {
        match self {
            Self::Disable => "admin:users.disable",
            Self::Enable => "admin:users.enable",
            Self::Unlock => "admin:users.unlock",
            Self::ResetPassword => "admin:users.reset_password",
            Self::ResetTotp => "admin:users.reset_totp",
        }
    }

    /// Whether it gives out a new one-time password (so hashes one).
    pub(crate) fn hashes(self) -> bool {
        matches!(self, Self::ResetPassword | Self::ResetTotp)
    }

    /// Whether the last user who can sign in is protected from it.
    fn guarded(self) -> bool {
        matches!(self, Self::Disable | Self::ResetPassword | Self::ResetTotp)
    }
}

/// `403 user_required`: an admin token's session on a page only users use.
fn user_required() -> WebError {
    WebError::new(
        StatusCode::FORBIDDEN,
        "user_required",
        "This page needs a signed-in user, not an admin token: sign out, then sign in with your user name and password.",
    )
}

/// The signed-in user of `session`, or `403 user_required`.
fn user_of(session: &Session) -> Result<&UserRow, WebError> {
    session.admin.user().ok_or_else(user_required)
}

/// `GET /admin/account`, with the outcome of the last action when the URL
/// names it.
pub(crate) fn account(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    let user = user_of(session)?;
    let notice = if request.query("changed").is_some() {
        Some("Your password is changed; your other devices and sessions have ended.".to_owned())
    } else {
        request
            .query("revoked")
            .map(|name| format!("Revoked the device {name} and ended its sessions."))
    };
    account_page(store, state, session, user, StatusCode::OK, notice, None)
}

fn account_page(
    store: &Store,
    state: &WebState,
    session: &Session,
    user: &UserRow,
    status: StatusCode,
    notice: Option<String>,
    error: Option<String>,
) -> Result<Response, WebError> {
    let devices = store
        .devices_of(user)
        .map_err(|e| WebError::from_store(&e))?
        .into_iter()
        .filter(|d| d.revoked_at.is_none())
        .map(|d| DeviceLine {
            path: form_urlencoded::byte_serialize(d.name.as_bytes()).collect(),
            name: d.name,
            created: d.created_at,
            last_used: d.last_used_at,
        })
        .collect();
    Ok(render(
        status,
        &AccountPage {
            title: "Your account",
            assets: assets::urls(),
            chrome: Chrome::new(session, Section::Account, ""),
            user: &user.name,
            enrolled: user.totp_enrolled_at.clone(),
            devices,
            device_days: state.device_days,
            notice,
            error,
        },
    ))
}

/// `POST /admin/account/devices/{name}/revoke`.
pub(crate) fn revoke_device(
    store: &mut Store,
    session: &Session,
    name: &str,
) -> Result<Response, WebError> {
    let user = user_of(session)?.clone();
    store
        .revoke_device_of(&user, name)
        .map_err(|e| super::refused_with_link(&e, "/admin/account", "Back to your account"))?;
    Ok(redirect(&link(
        "/admin/account",
        &[("revoked", Some(name))],
    )))
}

/// `GET /admin/users`, with the outcome of the last action when the URL
/// names it.
pub(crate) fn users(
    store: &Store,
    request: &WebRequest,
    session: &Session,
) -> Result<Response, WebError> {
    user_of(session)?;
    let notice = request.query("done").map(str::to_owned);
    users_page(store, session, StatusCode::OK, notice, None, "")
}

fn users_page(
    store: &Store,
    session: &Session,
    status: StatusCode,
    notice: Option<String>,
    error: Option<String>,
    name: &str,
) -> Result<Response, WebError> {
    let me = session.admin.name().to_owned();
    let users = store
        .users()
        .map_err(|e| WebError::from_store(&e))?
        .into_iter()
        .map(|listed| line(listed, &me))
        .collect();
    let open = error.is_some() && !name.is_empty();
    Ok(render(
        status,
        &UsersPage {
            title: "Users",
            assets: assets::urls(),
            chrome: Chrome::new(session, Section::Users, ""),
            users,
            notice,
            error,
            name: name.to_owned(),
            open,
        },
    ))
}

fn line(listed: crate::store::UserListing, me: &str) -> UserLine {
    let name = listed.row.name.clone();
    let (state, locked) = match &listed.state {
        UserState::SetupPending { until } => (format!("waiting for setup until {until}"), false),
        UserState::SetupExpired => ("setup expired: reset the password".to_owned(), false),
        UserState::Active => ("active".to_owned(), false),
        UserState::SignInLocked { until } => (format!("sign-in locked until {until}"), true),
        UserState::Disabled => ("disabled".to_owned(), false),
        UserState::Removed { at } => (format!("removed {at}"), false),
    };
    let disabled = matches!(listed.state, UserState::Disabled);
    let removed = listed.row.removed_at.is_some();
    let mut buttons = Vec::new();
    if !removed {
        buttons.push(if disabled {
            ("enable", "secondary", "Enable")
        } else {
            ("disable", "danger", "Disable")
        });
        if locked {
            buttons.push(("unlock", "secondary", "Unlock"));
        }
        buttons.push(("reset-password", "secondary", "Reset password"));
        buttons.push(("reset-totp", "danger", "Reset authenticator"));
    }
    let mut actions = Vec::new();
    let mut add = |action: &'static str, heading: String, body: &str, label: String| {
        actions.push((action, heading, body.to_owned(), label));
    };
    if disabled {
        add(
            "enable",
            format!("Enable {name}?"),
            "They can sign in again; their old sessions and devices stay ended.",
            format!("Enable {name}"),
        );
    } else {
        add(
            "disable",
            format!("Disable {name}?"),
            "Sign-in is refused, and their sessions and devices end at once.",
            format!("Disable {name}"),
        );
    }
    if locked {
        add(
            "unlock",
            format!("Unlock {name}?"),
            "Clears the lock after failed sign-ins.",
            format!("Unlock {name}"),
        );
    }
    add(
        "reset-password",
        format!("Reset the password of {name}?"),
        "A new one-time password is shown once; their authenticator is kept, and their sessions and devices end.",
        format!("Reset the password of {name}"),
    );
    add(
        "reset-totp",
        format!("Reset the authenticator of {name}?"),
        "Their authenticator is cleared and a new one-time password is shown once; they set up a new authenticator at the next sign-in, and their sessions and devices end.",
        format!("Reset the authenticator of {name}"),
    );
    UserLine {
        you: name == me,
        name,
        state,
        enrolled: listed.row.totp_enrolled_at,
        last_login: listed.row.last_login_at,
        devices: listed.devices,
        created: listed.row.created_at,
        actions: if removed { Vec::new() } else { actions },
        buttons,
    }
}

/// `POST /admin/users/{name}/disable`, `enable` or `unlock` (the actions
/// that hash nothing).
pub(crate) fn act(
    store: &mut Store,
    session: &Session,
    name: &str,
    action: UserAction,
) -> Result<Response, WebError> {
    user_of(session)?;
    last_user_guard(store, session, name, action)?;
    let done = match action {
        UserAction::Disable => store
            .disable_user(name)
            .map(|_| format!("Disabled user {name}: sign-in is refused, and their sessions and devices have ended.")),
        UserAction::Enable => store
            .enable_user(name)
            .map(|_| format!("Enabled user {name}; they sign in again.")),
        UserAction::Unlock => store
            .unlock_user(name)
            .map(|_| format!("Cleared the sign-in lock on {name}.")),
        UserAction::ResetPassword | UserAction::ResetTotp => {
            return Err(WebError::new(StatusCode::NOT_FOUND, "not_found", "no such page"));
        }
    };
    let message =
        done.map_err(|e| super::refused_with_link(&e, "/admin/users", "Back to Users"))?;
    Ok(redirect(&link("/admin/users", &[("done", Some(&message))])))
}

/// Refuses `action` on the last user who can sign in.
fn last_user_guard(
    store: &Store,
    session: &Session,
    name: &str,
    action: UserAction,
) -> Result<(), WebError> {
    if !action.guarded() {
        return Ok(());
    }
    let last = store
        .is_last_sign_in_user(name)
        .map_err(|e| WebError::from_store(&e))?;
    if !last {
        return Ok(());
    }
    let message = format!(
        "{name} is the last user who can sign in, so it cannot be disabled or reset here; run `joedocs user ...` on the server instead."
    );
    let page = users_page(
        store,
        session,
        StatusCode::BAD_REQUEST,
        None,
        Some(message.clone()),
        "",
    )?;
    Err(WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", message).with_page(page))
}

/// `POST /admin/users`: creates a user with a one-time password.
pub(crate) async fn create(state: Arc<WebState>, request: WebRequest) -> Response {
    let name = request.form("name").unwrap_or("").trim().to_owned();
    phased(
        state,
        request,
        "admin:users.create",
        |_, _, _| Ok(()),
        |()| crate::admin::prepare_password(&OsRandom),
        move |store, session, _, prepared| {
            let (password, hash) = prepared.map_err(|e| WebError::from_store(&e))?;
            match store.create_user(&name, &hash, &OsRandom) {
                Ok(created) => Ok(reveal(
                    session,
                    &name,
                    &created.until,
                    &password,
                    format!("Created user {name}."),
                    "set up an authenticator and choose their own password",
                )),
                Err(error) => {
                    let refusal = WebError::from_store(&error);
                    let message = refusal.message.clone();
                    let page =
                        users_page(store, session, refusal.status, None, Some(message), &name)?;
                    Err(refusal.with_page(page))
                }
            }
        },
    )
    .await
}

/// `POST /admin/users/{name}/reset-password` or `reset-totp`.
pub(crate) async fn reset(
    state: Arc<WebState>,
    request: WebRequest,
    name: String,
    action: UserAction,
) -> Response {
    let checked = name.clone();
    phased(
        state,
        request,
        action.operation(),
        move |store, session, _| last_user_guard(store, session, &checked, action),
        |()| crate::admin::prepare_password(&OsRandom),
        move |store, session, _, prepared| {
            last_user_guard(store, session, &name, action)?;
            let (password, hash) = prepared.map_err(|e| WebError::from_store(&e))?;
            let (reset, message, next) = if action == UserAction::ResetTotp {
                (
                    store.reset_totp(&name, &hash),
                    format!("Cleared the authenticator of {name} and ended their sessions and devices."),
                    "set up a new authenticator and choose their own password",
                )
            } else {
                (
                    store.reset_password(&name, &hash),
                    format!("Reset the password of {name} and ended their sessions and devices; their authenticator is kept."),
                    "choose their own password",
                )
            };
            let reset = reset.map_err(|e| super::refused_with_link(&e, "/admin/users", "Back to Users"))?;
            Ok(reveal(session, &name, &reset.until, &password, message, next))
        },
    )
    .await
}

fn reveal(
    session: &Session,
    user: &str,
    until: &str,
    password: &SecretString,
    message: String,
    next: &str,
) -> Response {
    render(
        StatusCode::OK,
        &PasswordPage {
            title: "One-time password",
            assets: assets::urls(),
            chrome: Chrome::new(session, Section::Users, ""),
            message,
            user,
            until,
            next,
            password: password.expose_secret(),
        },
    )
}

/// `POST /admin/account/password`: the current password, a code and the
/// new password twice.
pub(crate) async fn change_password(state: Arc<WebState>, request: WebRequest) -> Response {
    let current = SecretString::from(request.form("password").unwrap_or("").to_owned());
    let code = request.form("code").unwrap_or("").trim().to_owned();
    let new_password = SecretString::from(request.form("new_password").unwrap_or("").to_owned());
    let again = request.form("new_password_again").unwrap_or("").to_owned();
    let page_state = Arc::clone(&state);
    let finish_state = Arc::clone(&state);
    phased(
        state,
        request,
        "admin:me.password",
        move |store, session, user| {
            let problem = if new_password.expose_secret() == again {
                password::check_new(&user.name, &new_password)
                    .err()
                    .map(|refusal| refusal.to_string())
            } else {
                Some("The two new passwords differ; type the same one twice.".to_owned())
            };
            if let Some(problem) = problem {
                let page = account_page(
                    store,
                    &page_state,
                    session,
                    user,
                    StatusCode::BAD_REQUEST,
                    None,
                    Some(problem.clone()),
                )?;
                return Err(
                    WebError::new(StatusCode::BAD_REQUEST, "invalid_argument", problem)
                        .with_page(page),
                );
            }
            let phc = store
                .password_hash_of(user)
                .map_err(|e| WebError::from_store(&e))?;
            Ok((phc, current, new_password))
        },
        |(phc, current, new_password)| {
            let hashed = password::verify(&current, &phc)
                .matches
                .then(|| password::hash(&new_password, &OsRandom));
            (phc, hashed)
        },
        move |store, session, user, (phc, hashed)| {
            let wrong = |store: &Store, message: &str, state: &WebState| -> WebError {
                let page = account_page(
                    store,
                    state,
                    session,
                    user,
                    StatusCode::UNAUTHORIZED,
                    None,
                    Some(message.to_owned()),
                );
                let refusal =
                    WebError::from_store(&Error::AdminRefused(AdminRefusal::Unauthorized));
                match page {
                    Ok(page) => refusal.with_page(page),
                    Err(error) => error,
                }
            };
            let Some(hashed) = hashed else {
                store
                    .record_sign_in_failure(&user.name)
                    .map_err(|e| WebError::from_store(&e))?;
                return Err(wrong(
                    store,
                    "Your current password was not right.",
                    &finish_state,
                ));
            };
            let hash = hashed.map_err(|e| WebError::from_store(&Error::from(e)))?;
            let keep = store
                .user_web_session(&session.token)
                .map_err(|e| WebError::from_store(&e))?;
            match store.change_password(user, &code, &phc, &hash, &keep) {
                Ok(()) => Ok(redirect("/admin/account?changed=1")),
                Err(Error::AdminRefused(AdminRefusal::Unauthorized)) => Err(wrong(
                    store,
                    "That code was not accepted; try the next one.",
                    &finish_state,
                )),
                Err(error) => Err(WebError::from_store(&error)),
            }
        },
    )
    .await
}

/// A three-phase post by a signed-in user: `check` under the lock, `hash`
/// through the hashing gate without it, then `finish` under the lock with
/// the session checked again; one audit row with `tool`.
async fn phased<A, B, C, H, F>(
    state: Arc<WebState>,
    request: WebRequest,
    tool: &'static str,
    check: C,
    hash: H,
    finish: F,
) -> Response
where
    A: Send + 'static,
    B: Send + 'static,
    C: FnOnce(&Store, &Session, &UserRow) -> Result<A, WebError> + Send + 'static,
    H: FnOnce(A) -> B + Send + 'static,
    F: FnOnce(&mut Store, &Session, &UserRow, B) -> Result<Response, WebError> + Send + 'static,
{
    let (first_state, first_request) = (Arc::clone(&state), request.clone());
    let checked = tokio::task::spawn_blocking(move || {
        let store = first_state
            .shared
            .store()
            .lock()
            .map_err(|_| internal_refusal())?;
        let reply = signed_in_user(&store, &first_state, &first_request, check);
        reply.map_err(|refusal| {
            let (who, error) = *refusal;
            write_audit(&store, who.as_ref(), tool, error.code);
            error
        })
    })
    .await
    .unwrap_or_else(|_| Err(internal_refusal()));
    let (checked, who) = match checked {
        Ok(checked) => checked,
        Err(error) => return error.into_response(),
    };
    let Ok(hashed) = state.shared.hashing().run(move || hash(checked)).await else {
        let shared = Arc::clone(&state.shared);
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(store) = shared.store().lock() {
                write_audit(&store, Some(&who), tool, "busy");
            }
        })
        .await;
        let mut busy = WebError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            password::Busy.to_string(),
        )
        .into_response();
        busy.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("1"),
        );
        return busy;
    };
    tokio::task::spawn_blocking(move || {
        let Ok(mut store) = state.shared.store().lock() else {
            return internal_refusal().into_response();
        };
        let session = match signed_in(&store, &state, &request) {
            Ok(session) => session,
            Err(refusal) => {
                let (who, error) = *refusal;
                write_audit(&store, who.as_ref(), tool, error.code);
                return error.into_response();
            }
        };
        let Some(user) = session.admin.user().cloned() else {
            write_audit(&store, Some(&session.admin), tool, "user_required");
            return user_required().into_response();
        };
        let result = finish(&mut store, &session, &user, hashed);
        let outcome = result.as_ref().map_or_else(|e| e.code, |_| "ok");
        write_audit(&store, Some(&Admin::User(user)), tool, outcome);
        result.unwrap_or_else(WebError::into_response)
    })
    .await
    .unwrap_or_else(|_| internal_refusal().into_response())
}

/// A refused request and who made it, when known.
type Refusal = Box<(Option<Admin>, WebError)>;

/// `check`'s result and the signed-in user who asked, or the refusal and
/// who made the request.
fn signed_in_user<A>(
    store: &Store,
    state: &WebState,
    request: &WebRequest,
    check: impl FnOnce(&Store, &Session, &UserRow) -> Result<A, WebError>,
) -> Result<(A, Admin), Refusal> {
    let session = signed_in(store, state, request)?;
    let Some(user) = session.admin.user().cloned() else {
        return Err(Box::new((Some(session.admin.clone()), user_required())));
    };
    match check(store, &session, &user) {
        Ok(checked) => Ok((checked, Admin::User(user))),
        Err(error) => Err(Box::new((Some(Admin::User(user)), error))),
    }
}

fn internal_refusal() -> WebError {
    WebError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use secrecy::ExposeSecret as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use crate::auth::OsRandom;
    use crate::clock::Clock as _;
    use crate::mcp::{ServerOptions, Shared};
    use crate::store::{SignInRequest, SignedIn, Store};

    /// A store with `alice` set up and signed in on the web; her session
    /// cookie value.
    fn alice_on_the_web(dir: &std::path::Path) -> (Store, String) {
        let paths = crate::store::Paths {
            home: dir.join("vault"),
            state_dir: dir.join("state"),
        };
        let git = crate::history::GitCli::new(paths.git_dir(), &paths.home);
        Store::init(&paths, &git).unwrap();
        let clock = Arc::new(crate::clock::FixedClock::at(1_793_000_000));
        let mut store = Store::open(
            paths,
            crate::store::Settings::from_config(&crate::Config::default()),
            Box::new(git),
            Arc::clone(&clock) as Arc<dyn crate::clock::Clock>,
        )
        .unwrap();
        store
            .create_user("alice", "$argon2id$one-time", &OsRandom)
            .unwrap();
        let ok = crate::password::Verified {
            matches: true,
            needs_rehash: false,
        };
        let web = SignInRequest {
            code: None,
            device: None,
            session_hours: 8,
        };
        let begin = store.sign_in_begin("alice").unwrap();
        let Ok(SignedIn::Setup {
            ticket,
            totp: Some(totp),
            ..
        }) = store.sign_in_finish(&begin, ok, None, &web, &OsRandom)
        else {
            panic!("setup first");
        };
        let secret = crate::totp::base32_decode(totp.secret.expose_secret()).unwrap();
        let code = crate::totp::code(&secret, crate::totp::step_at(clock.now().as_second()));
        let request = SignInRequest {
            code: Some(&code),
            ..web
        };
        let Ok(SignedIn::Session { session, .. }) =
            store.setup_finish(&ticket, "$argon2id$chosen", &request, &OsRandom)
        else {
            panic!("a web session");
        };
        let session = session.expose_secret().to_owned();
        (store, session)
    }

    #[tokio::test]
    async fn a_busy_page_says_when_to_retry_and_is_audited_with_the_user() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, session) = alice_on_the_web(dir.path());
        let mut config = crate::Config::default();
        config.admin.enabled = true;
        config.admin.web_ui = true;
        let shared = Shared::new(store);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let options = ServerOptions::from_config(&config);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                crate::mcp::serve(listener, shared, &options, async {
                    stopped.await.ok();
                })
                .await
            }
        });
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = Arc::new(std::sync::Mutex::new(wait));
        let mut held = Vec::new();
        for _ in 0..crate::password::HASHING_PERMITS {
            let (shared, wait) = (Arc::clone(&shared), Arc::clone(&wait));
            held.push(tokio::spawn(async move {
                shared
                    .hashing()
                    .run(move || {
                        let _ = wait.lock().map(|w| w.recv());
                    })
                    .await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let session_token = secrecy::SecretString::from(session.clone());
        let body = format!(
            "name=bob&csrf={}",
            crate::web::session::csrf_token(&session_token)
        );
        let host = format!("127.0.0.1:{}", addr.port());
        let request = format!(
            "POST /admin/users HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nCookie: joedocs_session={session}\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\ncontent-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let reply = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(reply.starts_with("http/1.1 429"), "{reply}");
        assert!(reply.contains("\r\nretry-after: 1\r\n"), "{reply}");
        for _ in 0..crate::password::HASHING_PERMITS {
            release.send(()).unwrap();
        }
        for task in held {
            task.await.unwrap().unwrap();
        }
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
        let rows: Vec<Option<String>> =
            rusqlite::Connection::open(dir.path().join("state/joedocs.db"))
                .unwrap()
                .prepare(
                    "SELECT user_name FROM audit_log WHERE tool = 'admin:users.create' AND outcome = 'busy'",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
        assert_eq!(rows, [Some("alice".to_owned())]);
    }
}
