//! The admin API's user routes (0.5.1): password sign-in with TOTP setup,
//! device renewal and sign-out, and the signed-in user's own account and
//! devices.
//!
//! Sign-in, setup and a password change hash passwords. They run in three
//! phases so the store lock is never held while hashing (JD-36 C-04): read
//! under the lock, hash through the server-wide [`crate::password::Gate`]
//! (at most two at once, `429 busy` beyond), then finish and audit under the
//! lock. Every other route here runs under the lock as the token routes do.

use std::io::Write as _;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};

use super::{ApiError, ApiState, Call, Refused, UserRoute, authorized, refused, run, write_audit};
use crate::auth::{DEVICE_PREFIX, OsRandom};
use crate::cli::UserCommand;
use crate::password::{self, Verified};
use crate::store::{Admin, SignInRequest, SignedIn, Store, UserSession};

/// The default name of a device that names none.
const DEFAULT_DEVICE: &str = "device";

/// Whether the bearer credential of `call` is a device credential.
pub(super) fn has_device(call: &Call) -> bool {
    call.token
        .as_ref()
        .is_some_and(|t| t.expose_secret().starts_with(DEVICE_PREFIX))
}

/// A user's session from the device credential and session of `call`, or
/// the refusal and the device's user (when the credential is genuine).
pub(super) fn authenticate(
    store: &Store,
    call: &Call,
) -> Result<(Admin, UserSession), Box<Refused>> {
    let Some(credential) = call.token.as_ref() else {
        return Err(refused(None, ApiError::unauthorized()));
    };
    let owner = store
        .device_owner(credential)
        .ok()
        .flatten()
        .map(Admin::User);
    let Some(session) = call.session.as_ref() else {
        return Err(refused(owner, ApiError::no_session()));
    };
    store
        .user_api_session(credential, session, call.device_days)
        .map(|checked| (Admin::User(checked.user.clone()), checked))
        .map_err(|e| refused(owner, ApiError::from(&e)))
}

/// `POST /admin/v1/session` with a device credential and `{"code"}`: a new
/// API session for the device's user.
pub(super) fn renew(
    store: &mut Store,
    state: &ApiState,
    call: &Call,
    body: &[u8],
) -> (Option<Admin>, Result<Response, ApiError>) {
    let Some(credential) = call.token.clone() else {
        return (None, Err(ApiError::unauthorized()));
    };
    let owner = store
        .device_owner(&credential)
        .ok()
        .flatten()
        .map(Admin::User);
    let Some(code) = string(body, "code") else {
        return (owner, Err(argument("send {\"code\": \"123456\"}")));
    };
    let request = SignInRequest {
        code: Some(&code),
        device: None,
        session_hours: state.session_hours,
    };
    let result = store
        .device_session(&credential, &code, &request, call.device_days, &OsRandom)
        .map_err(|e| ApiError::from(&e))
        .map(|signed| session_reply(&signed));
    (owner, result)
}

/// `DELETE /admin/v1/session` with a device credential: ends the session
/// (even an expired one).
pub(super) fn sign_out(store: &Store, call: &Call) -> (Option<Admin>, Result<Response, ApiError>) {
    let owner = call
        .token
        .as_ref()
        .and_then(|t| store.device_owner(t).ok().flatten())
        .map(Admin::User);
    if owner.is_none() {
        return (
            None,
            Err(ApiError::from(&crate::store::Error::AdminRefused(
                crate::store::AdminRefusal::DeviceInvalid,
            ))),
        );
    }
    let result = match &call.session {
        None => Err(ApiError::no_session()),
        Some(session) => store
            .end_user_session(session, false)
            .map(|_| StatusCode::NO_CONTENT.into_response())
            .map_err(|e| ApiError::from(&e)),
    };
    (owner, result)
}

/// `POST /admin/v1/auth/login` with `{"user", "password", "code", "device"}`:
/// signs in with a password and a TOTP code, giving a device credential and
/// a session, or a setup ticket when TOTP setup or a new password is due.
pub(super) async fn sign_in(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    let fields = (
        string(&body, "user"),
        string(&body, "password").map(SecretString::from),
    );
    let (Some(name), Some(password)) = fields else {
        let wrong = argument("send {\"user\", \"password\", \"code\", \"device\"}");
        return refuse_now(state, call, None, wrong).await;
    };
    let code = string(&body, "code");
    let device = string(&body, "device").unwrap_or_else(|| DEFAULT_DEVICE.to_owned());
    if let Err(forbidden) = call.allowed.clone() {
        return refuse_now(state, call, None, forbidden).await;
    }
    let begin_name = name.clone();
    let phase_one = under_lock(&state, move |store| store.sign_in_begin(&begin_name)).await;
    let begin = match phase_one {
        Ok(Ok(found)) => found,
        Ok(Err(error)) => return refuse_now(state, call, None, ApiError::from(&error)).await,
        Err(failed) => return failed.into_response(),
    };
    let who = begin.user().cloned().map(Admin::User);
    if let Some(locked) = begin.locked() {
        let error = ApiError::from(&crate::store::Error::AdminRefused(locked.clone()));
        return refuse_now(state, call, who, error).await;
    }
    let phc = begin.phc().map(str::to_owned);
    let checked = hash_off_lock(&state, move || {
        let stored = phc.as_deref().unwrap_or_else(|| password::dummy_hash());
        let verified = password::verify(&password, stored);
        let rehash = (verified.matches && verified.needs_rehash)
            .then(|| password::hash(&password, &OsRandom).ok())
            .flatten();
        (verified, rehash)
    })
    .await;
    let (verified, rehash): (Verified, Option<String>) = match checked {
        Ok(done) => done,
        Err(busy) => return refuse_now(state, call, who, busy).await,
    };
    let hours = state.session_hours;
    finish(state, call, who, move |store| {
        let request = SignInRequest {
            code: code.as_deref(),
            device: Some(&device),
            session_hours: hours,
        };
        store.sign_in_finish(&begin, verified, rehash.as_deref(), &request, &OsRandom)
    })
    .await
}

/// `POST /admin/v1/auth/setup` with `{"ticket", "code", "password",
/// "device"}`: confirms a new authenticator (when one is being set up) and
/// sets the user's own password, giving a device credential and a session.
pub(super) async fn setup(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    let fields = (
        string(&body, "ticket").map(SecretString::from),
        string(&body, "password").map(SecretString::from),
    );
    let (Some(ticket), Some(new_password)) = fields else {
        let wrong = argument("send {\"ticket\", \"code\", \"password\", \"device\"}");
        return refuse_now(state, call, None, wrong).await;
    };
    let code = string(&body, "code");
    let device = string(&body, "device").unwrap_or_else(|| DEFAULT_DEVICE.to_owned());
    if let Err(forbidden) = call.allowed.clone() {
        return refuse_now(state, call, None, forbidden).await;
    }
    let begin_ticket = ticket.clone();
    let phase_one = under_lock(&state, move |store| store.setup_begin(&begin_ticket)).await;
    let user = match phase_one {
        Ok(Ok(found)) => found.user,
        Ok(Err(error)) => return refuse_now(state, call, None, ApiError::from(&error)).await,
        Err(failed) => return failed.into_response(),
    };
    let who = Some(Admin::User(user.clone()));
    if let Err(refusal) = password::check_new(&user.name, &new_password) {
        return refuse_now(state, call, who, argument(refusal.to_string())).await;
    }
    let hashed = hash_off_lock(&state, move || password::hash(&new_password, &OsRandom)).await;
    let new_hash = match hashed {
        Ok(Ok(hash)) => hash,
        Ok(Err(error)) => {
            let error = ApiError::from(&crate::store::Error::from(error));
            return refuse_now(state, call, who, error).await;
        }
        Err(busy) => return refuse_now(state, call, who, busy).await,
    };
    let hours = state.session_hours;
    finish(state, call, who, move |store| {
        let request = SignInRequest {
            code: code.as_deref(),
            device: Some(&device),
            session_hours: hours,
        };
        store.setup_finish(&ticket, &new_hash, &request, &OsRandom)
    })
    .await
}

/// `GET /admin/v1/me`: the signed-in user and their session.
pub(super) async fn me(state: Arc<ApiState>, call: Call) -> Response {
    run(state, call, |store, _, call| {
        let (who, session) = match signed_in_user(store, call) {
            Ok(found) => found,
            Err(refusal) => return Ok((refusal.who, Err(refusal.error))),
        };
        let user = &session.user;
        let reply = json!({
            "user": user.name,
            "expires_at": session.expires_at,
            "totp_enrolled_at": user.totp_enrolled_at,
            "last_login_at": user.last_login_at,
            "device": device_name(store, &session),
        });
        Ok((Some(who), Ok(axum::Json(reply).into_response())))
    })
    .await
}

/// `GET /admin/v1/me/devices`: the signed-in user's devices, as text.
pub(super) async fn devices(state: Arc<ApiState>, call: Call) -> Response {
    run(state, call, |store, _, call| {
        let (who, session) = match signed_in_user(store, call) {
            Ok(found) => found,
            Err(refusal) => return Ok((refusal.who, Err(refusal.error))),
        };
        let result = store
            .devices_of(&session.user)
            .map_err(|e| ApiError::from(&e))
            .and_then(|devices| {
                super::text(|out| {
                    crate::admin::write_devices(out, &devices, session.device_id.as_deref())
                })
            });
        Ok((Some(who), result))
    })
    .await
}

/// `POST /admin/v1/me/devices/{name}/revoke`: revokes one of the signed-in
/// user's devices and its sessions.
pub(super) async fn revoke_device(state: Arc<ApiState>, call: Call, name: String) -> Response {
    run(state, call, move |store, _, call| {
        let (who, session) = match signed_in_user(store, call) {
            Ok(found) => found,
            Err(refusal) => return Ok((refusal.who, Err(refusal.error))),
        };
        let result = store
            .revoke_device_of(&session.user, &name)
            .map_err(|e| ApiError::from(&e))
            .and_then(|_| {
                super::text(|out| {
                    writeln!(out, "Revoked the device {name} and ended its sessions.")
                        .map_err(crate::Error::from)
                })
            });
        Ok((Some(who), result))
    })
    .await
}

/// `POST /admin/v1/me/password` with `{"password", "code", "new_password"}`:
/// changes the signed-in user's password. Every other device and session of
/// theirs ends.
pub(super) async fn change_password(state: Arc<ApiState>, call: Call, body: Bytes) -> Response {
    let fields = (
        string(&body, "password").map(SecretString::from),
        string(&body, "code"),
        string(&body, "new_password").map(SecretString::from),
    );
    let (Some(current), Some(code), Some(new_password)) = fields else {
        return authorized(state, call, |_, _, _| {
            Err(argument("send {\"password\", \"code\", \"new_password\"}"))
        })
        .await;
    };
    let phase_one = {
        let call = call.clone();
        under_lock(&state, move |store| {
            if let Err(forbidden) = &call.allowed {
                return Err(refused(None, forbidden.clone()));
            }
            let (who, session) = signed_in_user(store, &call)?;
            let phc = store
                .password_hash_of(&session.user)
                .map_err(|e| refused(Some(who.clone()), ApiError::from(&e)))?;
            Ok((who, session, phc))
        })
        .await
    };
    let (who, session, phc) = match phase_one {
        Ok(Ok(found)) => found,
        Ok(Err(refusal)) => {
            let Refused { who, error } = *refusal;
            return refuse_now(state, call, who, error).await;
        }
        Err(failed) => return failed.into_response(),
    };
    if let Err(refusal) = password::check_new(&session.user.name, &new_password) {
        return refuse_now(state, call, Some(who), argument(refusal.to_string())).await;
    }
    let checked = hash_off_lock(&state, move || {
        password::verify(&current, &phc)
            .matches
            .then(|| password::hash(&new_password, &OsRandom))
    })
    .await;
    let new_hash = match checked {
        Ok(Some(Ok(hash))) => Some(hash),
        Ok(Some(Err(error))) => {
            let error = ApiError::from(&crate::store::Error::from(error));
            return refuse_now(state, call, Some(who), error).await;
        }
        Ok(None) => None,
        Err(busy) => return refuse_now(state, call, Some(who), busy).await,
    };
    let user = session.user.clone();
    finish_text(state, call, Some(who), move |store| match new_hash {
        None => {
            store.record_sign_in_failure(&user.name)?;
            Err(crate::store::AdminRefusal::Unauthorized.into())
        }
        Some(hash) => store
            .change_password(&user, &code, &hash, &session)
            .map(|()| {
                "Changed your password; your other devices and sessions have ended.".to_owned()
            }),
    })
    .await
}

/// `/admin/v1/users...`: the `joedocs user` commands, for a signed-in user,
/// replying with the text the server's command prints. A new one-time
/// password is hashed without the store lock, after the caller is checked.
/// The last user who can sign in cannot be disabled or reset from here.
pub(super) async fn users(
    state: Arc<ApiState>,
    call: Call,
    route: UserRoute,
    body: Bytes,
) -> Response {
    let command = match route {
        UserRoute::List => Some(UserCommand::List),
        UserRoute::Create => string(&body, "name").map(|name| UserCommand::Create { name }),
        UserRoute::Disable(name) => Some(UserCommand::Disable { name }),
        UserRoute::Enable(name) => Some(UserCommand::Enable { name }),
        UserRoute::Unlock(name) => Some(UserCommand::Unlock { name }),
        UserRoute::ResetPassword(name) => Some(UserCommand::ResetPassword { name }),
        UserRoute::ResetTotp(name) => Some(UserCommand::ResetTotp { name }),
    };
    let prepared = match &command {
        Some(command) if crate::admin::needs_password(command) => {
            match prepare(&state, &call).await {
                Ok(prepared) => Some(prepared),
                Err(reply) => return *reply,
            }
        }
        _ => None,
    };
    run(state, call, move |store, _, call| {
        let who = match signed_in_user(store, call) {
            Ok((who, _)) => who,
            Err(refusal) => return Ok((refusal.who, Err(refusal.error))),
        };
        let Some(command) = command else {
            return Ok((Some(who), Err(argument("send {\"name\": \"alice\"}"))));
        };
        if let Some(name) = guarded(&command) {
            let last = store
                .is_last_sign_in_user(name)
                .map_err(|e| ApiError::from(&e))?;
            if last {
                let refusal = argument(format!(
                    "{name} is the last user who can sign in, so it cannot be disabled or reset remotely; run `joedocs user ...` on the server instead"
                ));
                return Ok((Some(who), Err(refusal)));
            }
        }
        let result = super::text(|out| {
            crate::admin::user_prepared(store, &command, prepared, &OsRandom, out)
        });
        Ok((Some(who), result))
    })
    .await
}

/// The user a command would lock out, for the last-user rule.
fn guarded(command: &UserCommand) -> Option<&str> {
    match command {
        UserCommand::Disable { name }
        | UserCommand::ResetPassword { name }
        | UserCommand::ResetTotp { name } => Some(name),
        _ => None,
    }
}

/// A new one-time password, hashed without the store lock, once the caller
/// is known to be a signed-in user (so an anonymous request costs no hash).
async fn prepare(
    state: &Arc<ApiState>,
    call: &Call,
) -> Result<crate::admin::Prepared, Box<Response>> {
    let check = {
        let call = call.clone();
        under_lock(state, move |store| {
            if let Err(forbidden) = &call.allowed {
                return Err(refused(None, forbidden.clone()));
            }
            signed_in_user(store, &call).map(|_| ())
        })
        .await
    };
    match check {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => {
            let Refused { who, error } = *refusal;
            return Err(Box::new(
                refuse_now(Arc::clone(state), call.clone(), who, error).await,
            ));
        }
        Err(failed) => return Err(Box::new(failed.into_response())),
    }
    match hash_off_lock(state, || crate::admin::prepare_password(&OsRandom)).await {
        Ok(Ok(prepared)) => Ok(prepared),
        Ok(Err(error)) => {
            let error = ApiError::from(&error);
            Err(Box::new(
                refuse_now(Arc::clone(state), call.clone(), None, error).await,
            ))
        }
        Err(busy) => Err(Box::new(
            refuse_now(Arc::clone(state), call.clone(), None, busy).await,
        )),
    }
}

/// The signed-in user of `call` and their session, refusing an admin
/// token with `403 user_required`.
fn signed_in_user(store: &Store, call: &Call) -> Result<(Admin, UserSession), Box<Refused>> {
    if !has_device(call) {
        let who = call
            .token
            .as_ref()
            .and_then(|t| store.admin_token_for(t).ok().flatten())
            .map(Admin::Token);
        return Err(refused(who, user_required()));
    }
    authenticate(store, call)
}

/// `403 user_required`: an admin token on a route that only users may use.
pub(super) fn user_required() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "user_required",
        "this needs a signed-in user, not an admin token: run `joedocs auth login --user NAME`",
    )
}

fn device_name(store: &Store, session: &UserSession) -> Option<String> {
    let id = session.device_id.as_deref()?;
    store
        .devices_of(&session.user)
        .ok()?
        .into_iter()
        .find(|d| d.id == id)
        .map(|d| d.name)
}

/// The reply to a finished sign-in or setup.
fn session_reply(signed: &SignedIn) -> Response {
    let reply = match signed {
        SignedIn::Session {
            user,
            session,
            expires_at,
            device,
        } => {
            let mut reply = json!({
                "session": session.expose_secret(),
                "expires_at": expires_at,
                "user": user.name,
                "name": user.name,
            });
            if let Some(device) = device {
                reply["device"] = json!({
                    "name": device.name,
                    "credential": device.credential.expose_secret(),
                });
            }
            reply
        }
        SignedIn::Setup {
            user,
            ticket,
            expires_at,
            totp,
        } => json!({
            "user": user.name,
            "setup": {
                "ticket": ticket.expose_secret(),
                "expires_at": expires_at,
                "totp": totp.as_ref().map(|t| json!({
                    "secret": t.secret.expose_secret(),
                    "uri": t.uri.expose_secret(),
                })),
            },
        }),
    };
    axum::Json(reply).into_response()
}

fn argument(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_argument", message)
}

/// `429 busy`: both hashing places stayed taken.
fn busy() -> ApiError {
    let mut error = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "busy",
        password::Busy.to_string(),
    );
    error.retry_after = Some(1);
    error
}

/// The text field `name` of a JSON body, if present.
fn string(body: &[u8], name: &str) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get(name)?
        .as_str()
        .map(str::to_owned)
}

/// Runs `work` with the store, on a blocking thread.
async fn under_lock<T, F>(state: &Arc<ApiState>, work: F) -> Result<T, ApiError>
where
    F: FnOnce(&mut Store) -> T + Send + 'static,
    T: Send + 'static,
{
    let shared = Arc::clone(&state.shared);
    tokio::task::spawn_blocking(move || {
        let mut store = shared.store.lock().map_err(|_| super::internal())?;
        Ok(work(&mut store))
    })
    .await
    .unwrap_or_else(|_| Err(super::internal()))
}

/// Runs `work` (password hashing) through the hashing gate, never under
/// the store lock.
async fn hash_off_lock<T, F>(state: &Arc<ApiState>, work: F) -> Result<T, ApiError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(test)]
    let work = {
        let hook = state.during_hash.clone();
        let shared = Arc::clone(&state.shared);
        move || {
            if let Some(hook) = hook {
                hook(&shared);
            }
            work()
        }
    };
    state.shared.hashing().run(work).await.map_err(|_| busy())
}

/// Audits a refusal under the lock and replies with it.
async fn refuse_now(
    state: Arc<ApiState>,
    call: Call,
    who: Option<Admin>,
    error: ApiError,
) -> Response {
    let audited = under_lock(&state, move |store| {
        write_audit(store, who.as_ref(), &call, error.code);
        error
    })
    .await;
    audited.unwrap_or_else(|e| e).into_response()
}

/// Phase 3 of a sign-in or setup: runs `work` under the lock, audits it,
/// and replies with the session or setup ticket.
async fn finish<F>(state: Arc<ApiState>, call: Call, who: Option<Admin>, work: F) -> Response
where
    F: FnOnce(&mut Store) -> Result<SignedIn, crate::store::Error> + Send + 'static,
{
    let done = under_lock(&state, move |store| {
        let result = work(store);
        let who = match &result {
            Ok(SignedIn::Session { user, .. } | SignedIn::Setup { user, .. }) => {
                Some(Admin::User(user.clone()))
            }
            Err(_) => who,
        };
        let reply = result
            .map_err(|e| ApiError::from(&e))
            .map(|signed| session_reply(&signed));
        let outcome = reply.as_ref().map_or_else(|e| e.code, |_| "ok");
        write_audit(store, who.as_ref(), &call, outcome);
        reply
    })
    .await;
    match done {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) | Err(error) => error.into_response(),
    }
}

/// Like [`finish`], for work that replies with text (`{"output"}`).
async fn finish_text<F>(state: Arc<ApiState>, call: Call, who: Option<Admin>, work: F) -> Response
where
    F: FnOnce(&mut Store) -> Result<String, crate::store::Error> + Send + 'static,
{
    let done = under_lock(&state, move |store| {
        let reply = work(store)
            .map_err(|e| ApiError::from(&e))
            .and_then(|line| {
                super::text(|out| writeln!(out, "{line}").map_err(crate::Error::from))
            });
        let outcome = reply.as_ref().map_or_else(|e| e.code, |_| "ok");
        write_audit(store, who.as_ref(), &call, outcome);
        reply
    })
    .await;
    match done {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) | Err(error) => error.into_response(),
    }
}
