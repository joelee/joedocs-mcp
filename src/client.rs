//! `joedocs` as a client of a server's admin API (`client.server` or
//! `--server`): `login` and `logout`, the session file, and remote calls
//! that log in again when a session has expired.
//!
//! Every call sends the admin token (`JOEDOCS_ADMIN_TOKEN`) and the session,
//! so a copied session file is useless without the token. HTTPS uses rustls
//! with the ring provider and the operating system's root certificates.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead as _, IsTerminal as _, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::env::Env;
use crate::mcp::SESSION_HEADER;

#[path = "client_user.rs"]
mod user;
pub use user::{DeviceEntry, Devices, SignedIn};

/// Exit code when the admin API refuses the credentials (`EX_NOPERM`).
pub const EXIT_NOPERM: u8 = 77;

/// How long a call may wait for the next bytes from the server.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Code prompts allowed for one re-login.
const ATTEMPTS: usize = 3;

/// Asks the user for a TOTP code.
pub trait Prompt {
    /// Whether a person can answer: standard input and standard error are
    /// both terminals.
    fn interactive(&self) -> bool;

    /// Shows `message` and reads one line.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal cannot be read.
    fn ask(&self, message: &str) -> io::Result<String>;

    /// Shows `message` and reads one line without echoing it (a password).
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal cannot be read.
    fn ask_secret(&self, message: &str) -> io::Result<String> {
        self.ask(message)
    }

    /// Shows `text` to the person at the terminal, on standard error (TOTP
    /// setup instructions; never a command's result).
    fn show(&self, text: &str) {
        let _ = writeln!(io::stderr(), "{text}");
    }
}

/// The real prompt, on standard error and standard input.
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    fn ask(&self, message: &str) -> io::Result<String> {
        let mut stderr = io::stderr().lock();
        write!(stderr, "{message}")?;
        stderr.flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim().to_owned())
    }

    fn ask_secret(&self, message: &str) -> io::Result<String> {
        rpassword::prompt_password(message)
    }
}

/// Client failures, each with its exit code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server could not be reached.
    #[error("cannot reach {url}: {reason}")]
    Unreachable {
        /// The server.
        url: String,
        /// Why.
        reason: String,
    },
    /// The admin API refused the credentials, or there is no session and no
    /// way to ask for a code.
    #[error("{0}")]
    Auth(String),
    /// The server refused the request.
    #[error("{message}")]
    Remote {
        /// The API error code, e.g. `not_found`.
        code: String,
        /// The server's message.
        message: String,
    },
    /// The command cannot run this way.
    #[error("{0}")]
    Usage(String),
    /// A local file could not be read or written.
    #[error("cannot {action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// The server's reply was not understood.
    #[error("unexpected reply from the server: {0}")]
    Protocol(String),
    /// The server is too busy checking other sign-ins: try again shortly.
    #[error("{0}")]
    Busy(String),
}

impl Error {
    /// The process exit code: `77` for refused credentials, `69` for an
    /// unreachable server, `65` for refused requests, and the local
    /// command's code otherwise.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Unreachable { .. } | Self::Busy(_) => crate::EXIT_UNAVAILABLE,
            Self::Auth(_) => EXIT_NOPERM,
            Self::Usage(_) => crate::EXIT_DATA,
            Self::Remote { code, .. } => match code.as_str() {
                "problems" => crate::EXIT_PROBLEMS,
                "invalid_argument" | "not_found" | "already_exists" | "conflict" | "archived" => {
                    crate::EXIT_DATA
                }
                _ => crate::EXIT_SOFTWARE,
            },
            Self::Io { .. } => crate::EXIT_IO,
            Self::Protocol(_) => crate::EXIT_SOFTWARE,
        }
    }

    fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

/// A saved session.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    /// The `jds_...` session token.
    pub token: String,
    /// When it expires (UTC, RFC 3339).
    pub expires_at: String,
}

impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SessionsFile {
    #[serde(default)]
    sessions: BTreeMap<String, Saved>,
}

/// The session file, `$XDG_STATE_HOME/joedocs/sessions.toml` (or
/// `~/.local/state/joedocs/sessions.toml`): directory 0700, file 0600,
/// one entry per server URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sessions {
    path: PathBuf,
}

impl Sessions {
    /// The session file for this user.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] when neither `XDG_STATE_HOME` (absolute)
    /// nor `HOME` is set.
    pub fn locate(env: &dyn Env) -> Result<Self, Error> {
        let base = env
            .non_empty("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                env.non_empty("HOME")
                    .map(|home| PathBuf::from(home).join(".local/state"))
            })
            .ok_or_else(|| {
                Error::Usage("set HOME or XDG_STATE_HOME to keep the admin session".into())
            })?;
        Ok(Self {
            path: base.join("joedocs").join("sessions.toml"),
        })
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<SessionsFile, Error> {
        match fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| Error::Io {
                action: "read",
                path: self.path.clone(),
                source: io::Error::other(e),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SessionsFile::default()),
            Err(error) => Err(Error::io("read", &self.path)(error)),
        }
    }

    fn save(&self, file: &SessionsFile) -> Result<(), Error> {
        let text = toml::to_string(file).map_err(|e| Error::Io {
            action: "write",
            path: self.path.clone(),
            source: io::Error::other(e),
        })?;
        write_private(&self.path, &text)
    }

    /// The saved session for `server`, if any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read.
    pub fn get(&self, server: &str) -> Result<Option<Saved>, Error> {
        Ok(self.load()?.sessions.remove(server))
    }

    /// Saves the session for `server`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be written.
    pub fn put(&self, server: &str, saved: Saved) -> Result<(), Error> {
        let mut file = self.load()?;
        file.sessions.insert(server.to_owned(), saved);
        self.save(&file)
    }

    /// Forgets the session for `server`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be written.
    pub fn remove(&self, server: &str) -> Result<(), Error> {
        let mut file = self.load()?;
        if file.sessions.remove(server).is_some() {
            self.save(&file)?;
        }
        Ok(())
    }
}

/// Writes `text` to `path` through a new temporary file in the same
/// directory: the directory is made `0700`, the file `0600`.
fn write_private(path: &Path, text: &str) -> Result<(), Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(Error::io("create", dir))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(Error::io("protect", dir))?;
    let name = path
        .file_name()
        .map_or_else(|| "state".into(), |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temp);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(Error::io("create", &temp))?;
    out.write_all(text.as_bytes())
        .map_err(Error::io("write", &temp))?;
    drop(out);
    fs::rename(&temp, path).map_err(Error::io("write", path))
}

/// A reply from the admin API.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The HTTP status.
    pub status: u16,
    /// The `Location` of a redirect, if any.
    pub location: Option<String>,
    /// The body.
    pub body: Vec<u8>,
}

/// The reply to `POST /admin/v1/session`.
#[derive(Deserialize)]
struct LoginReply {
    session: String,
    expires_at: String,
    name: String,
}

/// The reply of a text command: what the command printed on the server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TextReply {
    /// The command's output.
    pub output: String,
    /// `check` problems (exit `1` when not `0`).
    #[serde(default)]
    pub problems: u64,
}

/// The reply to `GET /admin/v1/documents/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DocumentReply {
    /// The whole document, as `get_document` returns it.
    pub markdown: String,
    /// For a multi-file document, its other files.
    #[serde(default)]
    pub files: Option<Vec<crate::admin::ListedFile>>,
}

impl Reply {
    /// The body as JSON (`null` when it is not JSON).
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn error_code(&self) -> Option<String> {
        self.json()["error"]["code"].as_str().map(str::to_owned)
    }

    fn error_message(&self) -> String {
        self.json()["error"]["message"]
            .as_str()
            .map_or_else(|| format!("HTTP {}", self.status), str::to_owned)
    }

    fn is_session_refusal(&self) -> bool {
        self.status == 401
            && matches!(
                self.error_code().as_deref(),
                Some("no_session" | "session_expired")
            )
    }

    /// The reply as a result: success, or the error it carries.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] for refused credentials and
    /// [`Error::Remote`] for other refusals.
    pub fn into_result(self) -> Result<Self, Error> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        if (300..400).contains(&self.status) {
            // Never followed: a redirect could carry the session or a code
            // to another origin, or down to plain HTTP.
            return Err(Error::Protocol(format!(
                "the server answered with a redirect (HTTP {}) to {}; set client.server to the final URL",
                self.status,
                self.location.as_deref().unwrap_or("an unknown location")
            )));
        }
        let message = self.error_message();
        match (self.status, self.error_code()) {
            (429, Some(code)) if code == "busy" => Err(Error::Busy(message)),
            (401 | 429, _) => Err(Error::Auth(message)),
            (403, Some(code)) if code == "user_required" => Err(Error::Auth(message)),
            (_, Some(code)) => Err(Error::Remote { code, message }),
            (status, None) => Err(Error::Protocol(format!("HTTP {status}"))),
        }
    }
}

/// What a remote call proves who it is with.
#[derive(Clone)]
pub enum Credential {
    /// An admin token (`JOEDOCS_ADMIN_TOKEN`, deprecated since 0.5.1), with
    /// its sessions in `sessions.toml`.
    Token(SecretString),
    /// A user, with the device credential and session `joedocs auth login`
    /// stored in `devices.toml`.
    User(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("Token([REDACTED])"),
            Self::User(name) => f.debug_tuple("User").field(name).finish(),
        }
    }
}

/// A connection to one server's admin API.
pub struct Remote<'a> {
    server: String,
    credential: Credential,
    sessions: Sessions,
    devices: Devices,
    prompt: &'a dyn Prompt,
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
}

impl std::fmt::Debug for Remote<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl<'a> Remote<'a> {
    /// Prepares calls to `server` with `token`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] when there is no admin token, and
    /// [`Error::Usage`] when the session file has no home.
    pub fn new(
        server: &str,
        token: Option<&SecretString>,
        env: &dyn Env,
        prompt: &'a dyn Prompt,
    ) -> Result<Self, Error> {
        Self::with_timeout(server, token, env, prompt, READ_TIMEOUT)
    }

    /// Like [`Remote::new`], giving up when the server sends nothing for
    /// `read_timeout` (between any two reads, so a long download that keeps
    /// coming is not cut off).
    ///
    /// # Errors
    ///
    /// As [`Remote::new`].
    pub fn with_timeout(
        server: &str,
        token: Option<&SecretString>,
        env: &dyn Env,
        prompt: &'a dyn Prompt,
        read_timeout: Duration,
    ) -> Result<Self, Error> {
        let token = token.cloned().ok_or_else(|| {
            Error::Auth(
                "export JOEDOCS_ADMIN_TOKEN with your admin token (printed once by `joedocs admin-token create` on the server), or sign in as a user with `joedocs auth login --user NAME`"
                    .into(),
            )
        })?;
        Self::with_credential(server, Credential::Token(token), env, prompt, read_timeout)
    }

    /// Prepares calls to `server` with `credential`, giving up when the
    /// server sends nothing for `read_timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] when the state files have no home.
    pub fn with_credential(
        server: &str,
        credential: Credential,
        env: &dyn Env,
        prompt: &'a dyn Prompt,
        read_timeout: Duration,
    ) -> Result<Self, Error> {
        let (http, runtime) = http_client(read_timeout)?;
        Ok(Self {
            server: server.trim_end_matches('/').to_owned(),
            credential,
            sessions: Sessions::locate(env)?,
            devices: Devices::locate(env)?,
            prompt,
            runtime,
            http,
        })
    }

    /// The bearer credential of every call: the admin token, or the user's
    /// device credential when one is stored.
    fn bearer(&self) -> Result<Option<String>, Error> {
        Ok(match &self.credential {
            Credential::Token(token) => Some(token.expose_secret().to_owned()),
            Credential::User(user) => self
                .devices
                .get(&self.server, user)?
                .map(|entry| entry.credential),
        })
    }

    /// The server's base URL.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The saved session, if any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the session file cannot be read.
    pub fn saved(&self) -> Result<Option<Saved>, Error> {
        match &self.credential {
            Credential::Token(_) => self.sessions.get(&self.server),
            Credential::User(user) => Ok(self.devices.get(&self.server, user)?.and_then(|e| {
                Some(Saved {
                    token: e.session?,
                    expires_at: e.expires_at.unwrap_or_default(),
                })
            })),
        }
    }

    /// Sends one request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unreachable`] when the server cannot be reached.
    pub fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        session: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Error> {
        let url = format!("{}/admin/v1/{path}", self.server);
        let mut request = self.http.request(method, &url);
        if let Some(bearer) = self.bearer()? {
            request = request.bearer_auth(bearer);
        }
        if let Some(session) = session {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let unreachable = |e: reqwest::Error| Error::Unreachable {
            url: self.server.clone(),
            reason: without_url(&e),
        };
        self.runtime.block_on(async {
            let response = request.send().await.map_err(unreachable)?;
            let status = response.status().as_u16();
            let location = location(&response);
            let body = response.bytes().await.map_err(unreachable)?.to_vec();
            Ok(Reply {
                status,
                location,
                body,
            })
        })
    }

    /// Logs in with `code`, or with codes asked for on the terminal (up to
    /// three), and saves the session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] when the code is refused or the token is
    /// locked, or when no code is given and nobody can be asked.
    pub fn login(&self, code: Option<&str>) -> Result<(String, Saved), Error> {
        if let Credential::User(_) = self.credential {
            return self.renew(code);
        }
        match code {
            Some(code) => self.try_code(code),
            None => self.login_with_prompts(&format!("Code for {}: ", self.server)),
        }
    }

    fn try_code(&self, code: &str) -> Result<(String, Saved), Error> {
        let reply = self.send(
            reqwest::Method::POST,
            "session",
            None,
            Some(&json!({"code": code})),
        )?;
        let login: LoginReply = self.parse(&reply.into_result()?, "login reply")?;
        let saved = Saved {
            token: login.session,
            expires_at: login.expires_at,
        };
        self.sessions.put(&self.server, saved.clone())?;
        Ok((login.name, saved))
    }

    /// Reads a success reply as `T`, or fails with a protocol error naming
    /// the server, before anything is printed or written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the body is not `T` as JSON.
    pub fn parse<T: serde::de::DeserializeOwned>(
        &self,
        reply: &Reply,
        what: &str,
    ) -> Result<T, Error> {
        serde_json::from_slice(&reply.body).map_err(|e| {
            Error::Protocol(format!(
                "{} sent something that is not a JoeDocs {what} ({e}); is client.server the JoeDocs server?",
                self.server
            ))
        })
    }

    fn login_with_prompts(&self, message: &str) -> Result<(String, Saved), Error> {
        if !self.prompt.interactive() {
            return Err(Error::Auth(format!(
                "no valid admin session for {}; run `joedocs login` (or `joedocs login --code CODE`)",
                self.server
            )));
        }
        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            let code = self
                .prompt
                .ask(if attempt == 1 {
                    message
                } else {
                    "That code was not accepted. Code: "
                })
                .map_err(|e| Error::Io {
                    action: "read",
                    path: PathBuf::from("the terminal"),
                    source: e,
                })?;
            match self.try_code(&code) {
                Ok(done) => return Ok(done),
                Err(Error::Auth(why)) if why.contains("not valid") => last = Some(why),
                Err(other) => return Err(other),
            }
        }
        Err(Error::Auth(
            last.unwrap_or_else(|| "the code was not accepted".into()),
        ))
    }

    /// Ends the session on the server and forgets it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unreachable`] and [`Error::Io`]; a session the server
    /// no longer knows is simply forgotten.
    pub fn logout(&self) -> Result<bool, Error> {
        let Some(saved) = self.saved()? else {
            return Ok(false);
        };
        let reply = self.send(reqwest::Method::DELETE, "session", Some(&saved.token), None)?;
        match &self.credential {
            Credential::Token(_) => self.sessions.remove(&self.server)?,
            Credential::User(user) => self.devices.forget_session(&self.server, user)?,
        }
        if reply.is_session_refusal() || (200..300).contains(&reply.status) {
            Ok(true)
        } else {
            reply.into_result().map(|_| true)
        }
    }

    /// Calls `path` with the saved session. When the server refuses the
    /// session (or there is none) and someone can answer, asks for a code,
    /// logs in, and sends the request once more. The server checks the
    /// session before it runs anything, so the retry never repeats a change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] for refused credentials, and the request's
    /// own errors.
    pub fn call(
        &self,
        method: &reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, Error> {
        let saved = self.saved()?;
        let reply = self.send(
            method.clone(),
            path,
            saved.as_ref().map(|s| s.token.as_str()),
            body,
        )?;
        if self.device_refused(&reply)? {
            let fresh = self.sign_in_again()?;
            return self
                .send(method.clone(), path, Some(&fresh.token), body)?
                .into_result();
        }
        if !reply.is_session_refusal() {
            return reply.into_result();
        }
        let message = if saved.is_some() {
            format!("Session expired. Code for {}: ", self.server)
        } else {
            format!("Code for {}: ", self.server)
        };
        let fresh = self.relogin(&message)?;
        self.send(method.clone(), path, Some(&fresh.token), body)?
            .into_result()
    }

    /// Opens a new session after the server refused the saved one: with a
    /// TOTP code for the admin token, or the device credential for a user.
    fn relogin(&self, message: &str) -> Result<Saved, Error> {
        match &self.credential {
            Credential::Token(_) => self.login_with_prompts(message).map(|(_, saved)| saved),
            Credential::User(_) => self.renew_with_prompts(message).map(|(_, saved)| saved),
        }
    }
}

/// What a backup download wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Bytes written.
    pub bytes: u64,
    /// Vault symlinks the server left out.
    pub skipped_links: Vec<String>,
}

impl Remote<'_> {
    /// Downloads `path` (the backup) into `dest`, which must not exist,
    /// through a 0600 temporary file renamed into place. Logs in again like
    /// [`Remote::call`] when the session is refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`], [`Error::Io`] and the request's errors.
    pub fn download(&self, path: &str, dest: &Path) -> Result<Downloaded, Error> {
        let saved = self.saved()?;
        let first = self.download_once(path, saved.as_ref().map(|s| s.token.as_str()), dest)?;
        let refused = match first {
            Ok(done) => return Ok(done),
            Err(reply) if reply.is_session_refusal() => reply,
            Err(reply) => {
                return Err(reply
                    .into_result()
                    .err()
                    .unwrap_or_else(|| Error::Protocol("no body".into())));
            }
        };
        drop(refused);
        let message = format!("Session expired. Code for {}: ", self.server);
        let fresh = self.relogin(&message)?;
        match self.download_once(path, Some(&fresh.token), dest)? {
            Ok(done) => Ok(done),
            Err(reply) => Err(reply
                .into_result()
                .err()
                .unwrap_or_else(|| Error::Protocol("no body".into()))),
        }
    }

    fn download_once(
        &self,
        path: &str,
        session: Option<&str>,
        dest: &Path,
    ) -> Result<Result<Downloaded, Reply>, Error> {
        let url = format!("{}/admin/v1/{path}", self.server);
        let mut request = self.http.get(&url);
        if let Some(bearer) = self.bearer()? {
            request = request.bearer_auth(bearer);
        }
        if let Some(session) = session {
            request = request.header(SESSION_HEADER, session);
        }
        let unreachable = |e: reqwest::Error| Error::Unreachable {
            url: self.server.clone(),
            reason: without_url(&e),
        };
        self.runtime.block_on(async {
            let mut response = request.send().await.map_err(unreachable)?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                let location = location(&response);
                let body = response.bytes().await.map_err(unreachable)?.to_vec();
                return Ok(Err(Reply {
                    status,
                    location,
                    body,
                }));
            }
            let gzip = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/gzip"));
            if !gzip {
                return Err(Error::Protocol(format!(
                    "{} did not send a backup archive (application/gzip); is client.server the JoeDocs server?",
                    self.server
                )));
            }
            let skipped_links = response
                .headers()
                .get("joedocs-skipped-links")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| serde_json::from_str(v).ok())
                .unwrap_or_default();
            let dir = dest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = dest
                .file_name()
                .map_or_else(|| "backup".into(), |n| n.to_string_lossy().into_owned());
            let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(Error::io("create", &temp))?;
            let mut bytes = 0_u64;
            let written: Result<(), Error> = async {
                while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
                    file.write_all(&chunk).map_err(Error::io("write", &temp))?;
                    bytes += chunk.len() as u64;
                }
                file.sync_all().map_err(Error::io("write", &temp))
            }
            .await;
            if let Err(error) = written {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
            if fs::symlink_metadata(dest).is_ok() {
                let _ = fs::remove_file(&temp);
                return Err(Error::Usage(format!(
                    "{} already exists; choose another file name",
                    dest.display()
                )));
            }
            fs::rename(&temp, dest).map_err(Error::io("write", dest))?;
            Ok(Ok(Downloaded {
                bytes,
                skipped_links,
            }))
        })
    }
}

/// An HTTP client that never follows redirects, and the runtime to drive it.
fn http_client(
    read_timeout: Duration,
) -> Result<(reqwest::Client, tokio::runtime::Runtime), Error> {
    // ring for every rustls connection in this process; a second install
    // (by another remote in the same process) keeps the first.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder()
        .user_agent(concat!("joedocs-cli/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(read_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    Ok((http, runtime))
}

/// Checks a server URL typed by the user and returns it without a trailing
/// `/`: `https://host[:port]`, or `http://` to `localhost`, `127.0.0.1` or
/// `[::1]`, with no path, credentials, query or fragment (what
/// `client.server` accepts).
///
/// # Errors
///
/// Returns why the URL cannot be used.
pub fn server_url(text: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(text.trim())
        .map_err(|e| format!("`{}` is not a URL ({e})", text.trim()))?;
    let host = url.host_str().unwrap_or_default();
    match url.scheme() {
        "https" => {}
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => {}
        "http" => {
            return Err(format!(
                "{url} is plain http to another machine; use https (http is only for localhost, 127.0.0.1 or [::1])"
            ));
        }
        other => return Err(format!("{url} is not http or https ({other})")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("put no user name or password in the server URL".into());
    }
    if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
        return Err(format!(
            "{url} has a path, query or fragment; give the server's base URL, like https://docs.example.ts.net"
        ));
    }
    let server = url.as_str().trim_end_matches('/').to_owned();
    if crate::config::is_server_url(&server) {
        Ok(server)
    } else {
        Err(format!("{server} cannot be used as client.server"))
    }
}

/// Checks that `server` is a `joedocs serve`: `GET /health` answers `200`
/// with `ok`, without a redirect.
///
/// # Errors
///
/// Returns why the server cannot be used.
pub fn probe_health(server: &str) -> Result<(), String> {
    let (http, runtime) = http_client(READ_TIMEOUT).map_err(|e| e.to_string())?;
    let unreachable = |e: &reqwest::Error| format!("cannot reach {server}: {}", without_url(e));
    runtime.block_on(async {
        let mut response = http
            .get(format!("{server}/health"))
            .send()
            .await
            .map_err(|e| unreachable(&e))?;
        let status = response.status();
        if status.is_redirection() {
            let target = location(&response).map_or_else(String::new, |l| format!(" to {l}"));
            return Err(format!(
                "{server}/health redirects{target}; give the URL it should go to"
            ));
        }
        // `ok` is two bytes; read no more than a little past that.
        let mut body = Vec::new();
        while body.len() <= 16 {
            match response.chunk().await.map_err(|e| unreachable(&e))? {
                Some(chunk) => body.extend_from_slice(&chunk),
                None => break,
            }
        }
        if status == reqwest::StatusCode::OK && body.trim_ascii() == b"ok" {
            Ok(())
        } else {
            Err(format!(
                "{server} does not look like a JoeDocs server (/health answered {status})"
            ))
        }
    })
}

/// Asks for the server URL on the terminal, up to three times, and returns
/// the first one that [`server_url`] accepts and [`probe_health`] reaches.
///
/// # Errors
///
/// Returns [`Error::Usage`] after three unusable answers, and [`Error::Io`]
/// when the terminal cannot be read.
pub fn ask_server(prompt: &dyn Prompt) -> Result<String, Error> {
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let message = if attempt == 1 {
            "Server URL (for example https://docs.example.ts.net): ".to_owned()
        } else {
            format!("{last}.\nServer URL: ")
        };
        let answer = prompt.ask(&message).map_err(|e| Error::Io {
            action: "read",
            path: PathBuf::from("the terminal"),
            source: e,
        })?;
        match server_url(&answer).and_then(|url| probe_health(&url).map(|()| url)) {
            Ok(url) => return Ok(url),
            Err(why) => last = why,
        }
    }
    Err(Error::Usage(format!(
        "no usable server after {ATTEMPTS} tries (last: {last}); set client.server, or pass --server URL"
    )))
}

/// The `Location` header of `response`, if any.
fn location(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// A reqwest error without its URL (which is printed already).
fn without_url(error: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut reason = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        reason = cause.to_string();
        source = cause.source();
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn the_session_file_follows_xdg_state_home_then_home() {
        let env = MapEnv::new()
            .with("XDG_STATE_HOME", "/state")
            .with("HOME", "/home/u");
        assert_eq!(
            Sessions::locate(&env).unwrap().path(),
            Path::new("/state/joedocs/sessions.toml")
        );
        let env = MapEnv::new()
            .with("XDG_STATE_HOME", "relative")
            .with("HOME", "/home/u");
        assert_eq!(
            Sessions::locate(&env).unwrap().path(),
            Path::new("/home/u/.local/state/joedocs/sessions.toml")
        );
        assert!(matches!(
            Sessions::locate(&MapEnv::new()),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn sessions_are_kept_per_server_with_private_modes() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = MapEnv::new().with("XDG_STATE_HOME", dir.path().to_str().unwrap());
        let sessions = Sessions::locate(&env).unwrap();
        assert!(sessions.get("https://a").unwrap().is_none());
        let saved = |t: &str| Saved {
            token: t.into(),
            expires_at: "2026-11-08T20:00:00Z".into(),
        };
        sessions.put("https://a", saved("one")).unwrap();
        sessions.put("https://b", saved("two")).unwrap();
        assert_eq!(sessions.get("https://a").unwrap().unwrap().token, "one");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(sessions.path()), 0o600);
        assert_eq!(mode(sessions.path().parent().unwrap()), 0o700);
        sessions.remove("https://a").unwrap();
        assert!(sessions.get("https://a").unwrap().is_none());
        assert_eq!(sessions.get("https://b").unwrap().unwrap().token, "two");
        assert!(!format!("{:?}", saved("secret")).contains("secret"));
    }

    #[test]
    fn server_urls_are_https_or_loopback_http_without_extras() {
        for (text, want) in [
            (
                "https://docs.example.ts.net/",
                "https://docs.example.ts.net",
            ),
            (
                " HTTPS://Docs.Example.ts.net:8443 ",
                "https://docs.example.ts.net:8443",
            ),
            ("http://127.0.0.1:8750", "http://127.0.0.1:8750"),
            ("http://LOCALHOST:8750/", "http://localhost:8750"),
            ("http://[::1]:8750", "http://[::1]:8750"),
        ] {
            assert_eq!(server_url(text).as_deref(), Ok(want), "{text}");
        }
        for (text, why) in [
            ("docs.example.ts.net", "not a URL"),
            ("http://docs.example.ts.net", "plain http"),
            ("http://10.0.0.2:8750", "plain http"),
            ("http://127.0.0.2:8750", "plain http"),
            (
                "https://docs.example.ts.net/joedocs",
                "path, query or fragment",
            ),
            ("ftp://docs.example.ts.net", "not http or https"),
            ("https://me:pw@docs.example.ts.net", "user name or password"),
            (
                "https://docs.example.ts.net/?a=1",
                "path, query or fragment",
            ),
            (
                "https://docs.example.ts.net/#top",
                "path, query or fragment",
            ),
        ] {
            let err = server_url(text).unwrap_err();
            assert!(err.contains(why), "{text}: {err}");
        }
    }

    #[test]
    fn exit_codes_follow_the_kind_of_failure() {
        let remote = |code: &str| Error::Remote {
            code: code.into(),
            message: String::new(),
        };
        assert_eq!(Error::Auth(String::new()).exit_code(), 77);
        assert_eq!(
            Error::Unreachable {
                url: String::new(),
                reason: String::new()
            }
            .exit_code(),
            69
        );
        assert_eq!(remote("not_found").exit_code(), 65);
        assert_eq!(remote("problems").exit_code(), 1);
        assert_eq!(remote("internal").exit_code(), 70);
        assert_eq!(Error::Usage(String::new()).exit_code(), 65);
    }

    #[test]
    fn replies_turn_into_results() {
        let reply = |status: u16, body: Value| Reply {
            status,
            location: None,
            body: body.to_string().into_bytes(),
        };
        assert!(reply(200, json!({})).into_result().is_ok());
        let err = reply(
            401,
            json!({"error": {"code": "unauthorized", "message": "no"}}),
        )
        .into_result();
        assert!(matches!(err, Err(Error::Auth(m)) if m == "no"));
        let err = reply(
            404,
            json!({"error": {"code": "not_found", "message": "gone"}}),
        )
        .into_result();
        assert!(matches!(err, Err(Error::Remote { code, .. }) if code == "not_found"));
        assert!(matches!(
            reply(502, Value::Null).into_result(),
            Err(Error::Protocol(_))
        ));
        assert!(reply(401, json!({"error": {"code": "session_expired"}})).is_session_refusal());
        assert!(!reply(401, json!({"error": {"code": "unauthorized"}})).is_session_refusal());
    }
}
