//! Signing in as a user (0.5.1): `joedocs auth login --user NAME`, the
//! device credentials it stores, and renewing a session with a TOTP code.
//!
//! The first sign-in on a machine takes the password and a code and gives a
//! device credential, kept in `devices.toml` beside `sessions.toml` (file
//! `0600`, directory `0700`), one entry per server URL and user. After that
//! a session needs only a code. A password is only ever typed at a
//! terminal, never read from an argument, the environment or a file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Credential, Error, Remote, Saved, write_private};
use crate::env::Env;

/// Wrong answers allowed for one question during sign-in.
const ATTEMPTS: usize = 3;

/// One stored device credential, and its current session.
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// The server's URL.
    pub server: String,
    /// The user.
    pub user: String,
    /// The device's name on the server.
    pub device: String,
    /// The `jdd_...` device credential.
    pub credential: String,
    /// The `jdu_...` session, while there is one.
    #[serde(default)]
    pub session: Option<String>,
    /// When the session expires.
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl std::fmt::Debug for DeviceEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceEntry")
            .field("server", &self.server)
            .field("user", &self.user)
            .field("device", &self.device)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct DevicesFile {
    #[serde(default)]
    device: Vec<DeviceEntry>,
}

/// The device file, `$XDG_STATE_HOME/joedocs/devices.toml` (or
/// `~/.local/state/joedocs/devices.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Devices {
    path: PathBuf,
}

impl Devices {
    /// The device file for this user.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] when neither `XDG_STATE_HOME` (absolute)
    /// nor `HOME` is set.
    pub fn locate(env: &dyn Env) -> Result<Self, Error> {
        let sessions = super::Sessions::locate(env)?;
        let dir = sessions.path().parent().unwrap_or_else(|| Path::new("."));
        Ok(Self {
            path: dir.join("devices.toml"),
        })
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<DevicesFile, Error> {
        match fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| Error::Io {
                action: "read",
                path: self.path.clone(),
                source: io::Error::other(e),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DevicesFile::default()),
            Err(error) => Err(Error::io("read", &self.path)(error)),
        }
    }

    fn save(&self, file: &DevicesFile) -> Result<(), Error> {
        let text = toml::to_string(file).map_err(|e| Error::Io {
            action: "write",
            path: self.path.clone(),
            source: io::Error::other(e),
        })?;
        write_private(&self.path, &text)
    }

    /// The entry for `user` on `server`, if any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read.
    pub fn get(&self, server: &str, user: &str) -> Result<Option<DeviceEntry>, Error> {
        Ok(self
            .load()?
            .device
            .into_iter()
            .find(|e| e.server == server && e.user == user))
    }

    /// The users with a device stored for `server`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read.
    pub fn users(&self, server: &str) -> Result<Vec<String>, Error> {
        Ok(self
            .load()?
            .device
            .into_iter()
            .filter(|e| e.server == server)
            .map(|e| e.user)
            .collect())
    }

    /// Stores `entry`, replacing any for the same server and user.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be written.
    pub fn put(&self, entry: DeviceEntry) -> Result<(), Error> {
        let mut file = self.load()?;
        file.device
            .retain(|e| !(e.server == entry.server && e.user == entry.user));
        file.device.push(entry);
        self.save(&file)
    }

    /// Forgets the device of `user` on `server`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be written.
    pub fn remove(&self, server: &str, user: &str) -> Result<(), Error> {
        let mut file = self.load()?;
        let before = file.device.len();
        file.device
            .retain(|e| !(e.server == server && e.user == user));
        if file.device.len() != before {
            self.save(&file)?;
        }
        Ok(())
    }

    /// Forgets the session of `user` on `server`, keeping the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be written.
    pub fn forget_session(&self, server: &str, user: &str) -> Result<(), Error> {
        if let Some(mut entry) = self.get(server, user)? {
            entry.session = None;
            entry.expires_at = None;
            self.put(entry)?;
        }
        Ok(())
    }
}

/// A finished `joedocs auth login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    /// The user.
    pub user: String,
    /// The device's name on the server.
    pub device: String,
    /// When the session expires.
    pub expires_at: String,
    /// Whether TOTP was set up and a password chosen just now.
    pub set_up: bool,
}

/// What renewing a session with the stored device gave.
enum Renewal {
    /// A new session, for the user.
    Renewed(String, Saved),
    /// The server no longer accepts the device, which was forgotten here.
    DeviceRefused,
}

impl Renewal {
    /// The new session, or the refusal as an error that says to sign in
    /// again with the password.
    fn into_session(self) -> Result<(String, Saved), Error> {
        match self {
            Self::Renewed(user, saved) => Ok((user, saved)),
            Self::DeviceRefused => Err(Error::Auth(
                "this device's sign-in has ended; sign in again with your password (`joedocs auth login`)"
                    .into(),
            )),
        }
    }
}

impl<'a> Remote<'a> {
    /// Prepares calls to `server` as `user`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] when the state files have no home.
    pub fn for_user(
        server: &str,
        user: &str,
        env: &dyn Env,
        prompt: &'a dyn super::Prompt,
    ) -> Result<Self, Error> {
        Self::with_credential(
            server,
            Credential::User(user.to_owned()),
            env,
            prompt,
            super::READ_TIMEOUT,
        )
    }

    fn user(&self) -> Result<&str, Error> {
        match &self.credential {
            Credential::User(user) => Ok(user),
            Credential::Token(_) => Err(Error::Usage(
                "this needs a user: pass --user NAME (or set client.user)".into(),
            )),
        }
    }

    /// `joedocs auth login`: renews the session with the stored device and
    /// a code; with no device stored (or one the server no longer accepts),
    /// signs in with the password and a code at the terminal, setting up
    /// TOTP and a password first when the account needs it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] when the credentials are refused, or when no
    /// device is stored and nobody can be asked for a password.
    pub fn auth_login(&self, code: Option<&str>, device: &str) -> Result<SignedIn, Error> {
        let user = self.user()?.to_owned();
        if let Some(entry) = self.devices.get(&self.server, &user)? {
            match self.renew(code) {
                Ok((_, saved)) => {
                    return Ok(SignedIn {
                        user,
                        device: entry.device,
                        expires_at: saved.expires_at,
                        set_up: false,
                    });
                }
                Err(Error::Auth(why)) if why.contains("sign in again with your password") => {}
                Err(other) => return Err(other),
            }
        }
        if !self.prompt.interactive() {
            return Err(Error::Auth(format!(
                "no device of {user} is signed in to {} here: run `joedocs auth login --user {user}` once at a terminal",
                self.server
            )));
        }
        self.sign_in(code, device)
    }

    /// Renews the session with the stored device and `code`, or codes asked
    /// for at the terminal.
    pub(super) fn renew(&self, code: Option<&str>) -> Result<(String, Saved), Error> {
        match code {
            Some(code) => self.try_renew(code),
            None => self.renew_with_prompts(&format!("Code for {}: ", self.server)),
        }
    }

    fn try_renew(&self, code: &str) -> Result<(String, Saved), Error> {
        self.try_renewal(code).and_then(Renewal::into_session)
    }

    /// Renews the session with the stored device and `code`; a device the
    /// server refuses is forgotten here and reported as such.
    fn try_renewal(&self, code: &str) -> Result<Renewal, Error> {
        let user = self.user()?.to_owned();
        let reply = self.send(
            reqwest::Method::POST,
            "session",
            None,
            Some(&json!({"code": code})),
        )?;
        if self.device_refused(&reply)?.is_some() {
            return Ok(Renewal::DeviceRefused);
        }
        let renewed = reply.into_result()?.json();
        let saved = Saved {
            token: field(&renewed, "session")?,
            expires_at: field(&renewed, "expires_at")?,
        };
        if let Some(mut entry) = self.devices.get(&self.server, &user)? {
            entry.session = Some(saved.token.clone());
            entry.expires_at = Some(saved.expires_at.clone());
            self.devices.put(entry)?;
        }
        Ok(Renewal::Renewed(user, saved))
    }

    /// Renews with codes asked for at the terminal, up to three.
    pub(super) fn renew_with_prompts(&self, message: &str) -> Result<(String, Saved), Error> {
        self.renewal_with_prompts(message)
            .and_then(Renewal::into_session)
    }

    /// As [`Remote::renew_with_prompts`], reporting a device the server
    /// refuses (forgotten here) instead of failing.
    fn renewal_with_prompts(&self, message: &str) -> Result<Renewal, Error> {
        let user = self.user()?.to_owned();
        if self.devices.get(&self.server, &user)?.is_none() {
            return self
                .sign_in_again(None)
                .map(|saved| Renewal::Renewed(user, saved));
        }
        if !self.prompt.interactive() {
            return Err(Error::Auth(format!(
                "no valid session of {user} for {}; run `joedocs auth login --user {user}` (or with `--code CODE`)",
                self.server
            )));
        }
        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            let asked = if attempt == 1 {
                message
            } else {
                "That code was not accepted. Code: "
            };
            let code = self.ask(asked)?;
            match self.try_renewal(&code) {
                Ok(done) => return Ok(done),
                Err(Error::Auth(why)) if why.contains("not valid") => last = Some(why),
                Err(other) => return Err(other),
            }
        }
        Err(Error::Auth(
            last.unwrap_or_else(|| "the code was not accepted".into()),
        ))
    }

    /// When `reply` says the stored device is no longer accepted, forgets
    /// the device and returns its name (for the device that replaces it).
    pub(super) fn device_refused(&self, reply: &super::Reply) -> Result<Option<String>, Error> {
        let Credential::User(user) = &self.credential else {
            return Ok(None);
        };
        let refused =
            reply.status == 401 && reply.json()["error"]["code"].as_str() == Some("device_invalid");
        if !refused {
            return Ok(None);
        }
        let name = self
            .devices
            .get(&self.server, user)?
            .map_or_else(|| "device".to_owned(), |e| e.device);
        self.devices.remove(&self.server, user)?;
        Ok(Some(name))
    }

    /// Signs in again from scratch after the device was refused, when
    /// someone can answer; otherwise says how. The new device is named
    /// `device` (the refused one's name), or after the stored one.
    pub(super) fn sign_in_again(&self, device: Option<&str>) -> Result<Saved, Error> {
        let user = self.user()?.to_owned();
        if !self.prompt.interactive() {
            return Err(Error::Auth(format!(
                "this device's sign-in to {} has ended; run `joedocs auth login --user {user}` at a terminal",
                self.server
            )));
        }
        let device = match device {
            Some(name) => name.to_owned(),
            None => self
                .devices
                .get(&self.server, &user)?
                .map_or_else(|| "device".to_owned(), |e| e.device),
        };
        self.sign_in(None, &device)?;
        self.saved()?
            .ok_or_else(|| Error::Protocol("no session after signing in".into()))
    }

    /// The password sign-in at a terminal, with TOTP setup and a new
    /// password when the account needs them.
    fn sign_in(&self, code: Option<&str>, device: &str) -> Result<SignedIn, Error> {
        let user = self.user()?.to_owned();
        let password = self.ask_secret(&format!("Password for {user} on {}: ", self.server))?;
        let code = match code {
            Some(code) => code.to_owned(),
            None => self.ask(
                "Code from your authenticator (press Enter at your first sign-in, before one is set up): ",
            )?,
        };
        let mut body = json!({"user": user, "password": password, "device": device});
        if !code.is_empty() {
            body["code"] = json!(code);
        }
        let reply = self
            .send(reqwest::Method::POST, "auth/login", None, Some(&body))?
            .into_result()?
            .json();
        if reply.get("setup").is_some() {
            return self.set_up(&user, device, &reply["setup"]);
        }
        self.store_session(&user, &reply, false)
    }

    /// Sets up TOTP (when due) and a new password with the setup ticket.
    fn set_up(&self, user: &str, device: &str, setup: &Value) -> Result<SignedIn, Error> {
        let ticket = field(setup, "ticket")?;
        let totp = setup.get("totp").filter(|t| !t.is_null());
        if let Some(totp) = totp {
            let uri = field(totp, "uri")?;
            self.prompt.show(&format!(
                "Two-factor authentication is required. Add this account to your authenticator app:\n  Secret: {}\n  URI:    {uri}\n{}",
                field(totp, "secret")?,
                terminal_qr(&uri)
            ));
        } else {
            self.prompt
                .show("Your password was reset: choose a new one.");
        }
        let mut code = String::new();
        let mut last = None;
        for _ in 0..ATTEMPTS {
            if totp.is_some() && code.is_empty() {
                code = self.ask("Code from your authenticator: ")?;
            }
            let password = self.new_password()?;
            let mut body = json!({"ticket": ticket, "password": password, "device": device});
            if !code.is_empty() {
                body["code"] = json!(code);
            }
            let reply = self.send(reqwest::Method::POST, "auth/setup", None, Some(&body))?;
            match reply.into_result() {
                Ok(done) => {
                    self.prompt.show("Two-factor authentication is set up.");
                    return self.store_session(user, &done.json(), true);
                }
                Err(Error::Remote {
                    code: kind,
                    message,
                }) if kind == "invalid_argument" => {
                    self.prompt.show(&message);
                    last = Some(Error::Remote {
                        code: kind,
                        message,
                    });
                }
                Err(Error::Auth(why)) if why.contains("not valid") && totp.is_some() => {
                    self.prompt.show("That code was not accepted.");
                    code.clear();
                    last = Some(Error::Auth(why));
                }
                Err(other) => return Err(other),
            }
        }
        Err(last.unwrap_or_else(|| Error::Auth("setup did not finish".into())))
    }

    /// A new password typed twice the same.
    fn new_password(&self) -> Result<String, Error> {
        for _ in 0..ATTEMPTS {
            let first = self.ask_secret("Choose a new password (12 or more characters): ")?;
            let again = self.ask_secret("Repeat the new password: ")?;
            if first == again {
                return Ok(first);
            }
            self.prompt.show("The two passwords differ; try again.");
        }
        Err(Error::Usage(
            "the new password was not typed the same twice".into(),
        ))
    }

    fn store_session(&self, user: &str, reply: &Value, set_up: bool) -> Result<SignedIn, Error> {
        let device = &reply["device"];
        let entry = DeviceEntry {
            server: self.server.clone(),
            user: user.to_owned(),
            device: field(device, "name")?,
            credential: field(device, "credential")?,
            session: Some(field(reply, "session")?),
            expires_at: Some(field(reply, "expires_at")?),
        };
        let signed = SignedIn {
            user: user.to_owned(),
            device: entry.device.clone(),
            expires_at: entry.expires_at.clone().unwrap_or_default(),
            set_up,
        };
        self.devices.put(entry)?;
        Ok(signed)
    }

    /// `joedocs auth logout`: ends the session, and with `forget_device`
    /// also revokes this device and forgets it. Returns whether there was
    /// anything to end.
    ///
    /// The device is forgotten here only once the server has revoked it,
    /// or says it is no longer valid; an expired session is renewed first
    /// with a code (never a full sign-in).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Auth`] when the session cannot be renewed (no
    /// terminal, or the code refused), [`Error::Unreachable`],
    /// [`Error::Io`], and the server's refusal; the device then stays.
    pub fn auth_logout(&self, forget_device: bool) -> Result<bool, Error> {
        let user = self.user()?.to_owned();
        let Some(entry) = self.devices.get(&self.server, &user)? else {
            return Ok(false);
        };
        if !forget_device {
            return self.logout();
        }
        let path = format!("me/devices/{}/revoke", crate::percent_encode(&entry.device));
        let mut reply = self.send(reqwest::Method::POST, &path, entry.session.as_deref(), None)?;
        if reply.is_session_refusal() {
            let message = format!("Session expired. Code for {}: ", self.server);
            match self.renewal_with_prompts(&message)? {
                // Already refused, and forgotten by the renewal: done.
                Renewal::DeviceRefused => return Ok(true),
                Renewal::Renewed(_, fresh) => {
                    reply = self.send(reqwest::Method::POST, &path, Some(&fresh.token), None)?;
                }
            }
        }
        let invalid =
            reply.status == 401 && reply.json()["error"]["code"].as_str() == Some("device_invalid");
        if !invalid {
            reply.into_result()?;
        }
        self.devices.remove(&self.server, &user)?;
        Ok(true)
    }

    /// `joedocs auth password`: asks for the current password, a code and
    /// the new password twice, and returns what the server printed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] without a terminal, and the call's errors.
    pub fn change_password(&self) -> Result<String, Error> {
        self.user()?;
        if !self.prompt.interactive() {
            return Err(Error::Usage(
                "changing your password asks for it at a terminal".into(),
            ));
        }
        let current = self.ask_secret("Current password: ")?;
        let code = self.ask("Code from your authenticator: ")?;
        let new_password = self.new_password()?;
        let body = json!({"password": current, "code": code, "new_password": new_password});
        let reply = self.call(&reqwest::Method::POST, "me/password", Some(&body))?;
        let text: super::TextReply = self.parse(&reply, "command reply")?;
        Ok(text.output)
    }

    /// `joedocs auth devices revoke NAME`: revokes one of your devices, and
    /// forgets it here when it is this one.
    ///
    /// # Errors
    ///
    /// Returns the call's errors.
    pub fn revoke_device(&self, name: &str) -> Result<String, Error> {
        let user = self.user()?.to_owned();
        let path = format!("me/devices/{}/revoke", crate::percent_encode(name));
        let reply = self.call(&reqwest::Method::POST, &path, None)?;
        let text: super::TextReply = self.parse(&reply, "command reply")?;
        if self
            .devices
            .get(&self.server, &user)?
            .is_some_and(|e| e.device == name)
        {
            self.devices.remove(&self.server, &user)?;
        }
        Ok(text.output)
    }

    fn ask(&self, message: &str) -> Result<String, Error> {
        self.prompt.ask(message).map_err(|e| Error::Io {
            action: "read",
            path: PathBuf::from("the terminal"),
            source: e,
        })
    }

    fn ask_secret(&self, message: &str) -> Result<String, Error> {
        self.prompt.ask_secret(message).map_err(|e| Error::Io {
            action: "read",
            path: PathBuf::from("the terminal"),
            source: e,
        })
    }
}

fn field(value: &Value, name: &str) -> Result<String, Error> {
    value[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Protocol(format!("the reply has no {name}")))
}

/// `uri` as a QR code drawn with Unicode blocks, light on dark, for a
/// terminal.
fn terminal_qr(uri: &str) -> String {
    use qrcode::render::unicode::Dense1x2;
    qrcode::QrCode::new(uri.as_bytes()).map_or_else(
        |_| String::new(),
        |code| {
            code.render::<Dense1x2>()
                .dark_color(Dense1x2::Light)
                .light_color(Dense1x2::Dark)
                .quiet_zone(true)
                .build()
        },
    )
}
