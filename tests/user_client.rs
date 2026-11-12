//! `joedocs auth ...` and remote `joedocs user ...` (0.5.1): the first
//! sign-in with TOTP setup at a terminal, code-only renewals with the stored
//! device, which credential a remote command uses, the device file's modes,
//! and a revoked device being forgotten.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use joedocs::Config;
use joedocs::auth::OsRandom;
use joedocs::clock::{Clock as _, FixedClock};
use joedocs::history::GitCli;
use joedocs::mcp::{AdminOptions, ServerOptions, Shared, serve};
use joedocs::store::{Paths, Settings, Store};
use secrecy::ExposeSecret as _;
use tempfile::TempDir;

const PASSWORD: &str = "a long passphrase of my own";

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A server on its own thread and runtime, so the client (which runs its
/// own runtime) can be called from the test thread.
struct Server {
    root: TempDir,
    url: String,
    clock: Arc<FixedClock>,
    initial: String,
    token: String,
    /// The admin token's TOTP secret.
    token_secret: Vec<u8>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> TestResult<Self> {
        let root = TempDir::new()?;
        let paths = Paths {
            home: root.path().join("vault"),
            state_dir: root.path().join("state"),
        };
        let git = GitCli::new(paths.git_dir(), &paths.home);
        Store::init(&paths, &git)?;
        let clock = Arc::new(FixedClock::at(1_793_000_000));
        let mut store = Store::open(
            paths,
            Settings::from_config(&Config::default()),
            Box::new(git),
            Arc::clone(&clock) as Arc<dyn joedocs::clock::Clock>,
        )?;
        let password = joedocs::password::initial_password(&OsRandom)?;
        let hash = joedocs::password::hash(&password, &OsRandom)?;
        store.create_user("alice", &hash, &OsRandom)?;
        let token = store.create_admin_token("ops", &OsRandom)?;
        let token_secret =
            joedocs::totp::base32_decode(token.totp_secret.expose_secret()).ok_or("bad base32")?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Runtime::new() else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    return;
                };
                let mut options = ServerOptions::from_config(&Config::default());
                options.admin = Some(AdminOptions {
                    session_hours: 8,
                    config_file: None,
                });
                let _ = serve(listener, Shared::new(store), &options, async {
                    stopped.await.ok();
                })
                .await;
            });
        });
        Ok(Self {
            root,
            url,
            clock,
            initial: password.expose_secret().to_owned(),
            token: token.token.expose_secret().to_owned(),
            token_secret,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    fn state_home(&self) -> PathBuf {
        self.root.path().join("client-state")
    }

    /// Runs `args` through the library with `prompt`, as the binary would,
    /// with `JOEDOCS_ADMIN_TOKEN` set when `token`.
    fn run(&self, prompt: &Terminal, token: bool, args: &[&str]) -> Result<String, joedocs::Error> {
        self.run_at(&self.url, prompt, token, args)
    }

    /// As [`Server::run`], against the server at `url`.
    fn run_at(
        &self,
        url: &str,
        prompt: &Terminal,
        token: bool,
        args: &[&str],
    ) -> Result<String, joedocs::Error> {
        use clap::Parser as _;
        let mut env = joedocs::env::MapEnv::new()
            .with("HOME", self.root.path().to_str().unwrap_or_default())
            .with(
                "XDG_STATE_HOME",
                self.state_home().to_str().unwrap_or_default(),
            );
        if token {
            env = env.with("JOEDOCS_ADMIN_TOKEN", self.token.clone());
        }
        let fs = joedocs::fs::MemFs::new();
        let ctx = joedocs::Context {
            app: joedocs::AppId::current(),
            env: &env,
            fs: &fs,
            clock: Arc::new(joedocs::clock::SystemClock),
            random: &OsRandom,
            services: &Host,
            prompt,
        };
        let mut full = vec!["joedocs", "--server", url];
        full.extend_from_slice(args);
        let cli = joedocs::Cli::try_parse_from(full)
            .map_err(|e| joedocs::Error::Output(std::io::Error::other(e.to_string())))?;
        let loaded = joedocs::load(&cli, &ctx)?;
        let mut out = Vec::new();
        joedocs::execute(&cli, &loaded, &ctx, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Alice's first sign-in at a terminal, setting up TOTP and her own
    /// password. Returns the terminal, which knows her TOTP secret.
    fn first_sign_in(&self) -> TestResult<Terminal> {
        let terminal = Terminal::new(Arc::clone(&self.clock), &self.initial);
        let out = self.run(&terminal, false, &["--user", "alice", "auth", "login"])?;
        assert!(
            out.starts_with(&format!(
                "Signed in to {} as alice on the device phoenix;",
                self.url
            )),
            "{out}"
        );
        assert!(terminal.secret.borrow().is_some(), "the secret was shown");
        self.clock.advance(30);
        PASSWORD.clone_into(&mut terminal.password.borrow_mut());
        Ok(terminal)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// The host, as `joedocs auth login` names the device.
struct Host;

impl joedocs::systemd::ServiceManager for Host {
    fn executable(&self) -> std::io::Result<PathBuf> {
        Ok(PathBuf::from("/usr/bin/joedocs"))
    }
    fn systemctl(&self, _args: &[&str]) -> std::io::Result<joedocs::systemd::Ran> {
        Err(std::io::Error::other("no systemctl in tests"))
    }
    fn host_name(&self) -> std::io::Result<String> {
        Ok("phoenix\n".to_owned())
    }
}

/// A terminal that answers each question by what it asks, reads the TOTP
/// secret from the setup text shown to it, and records what it was asked.
struct Terminal {
    clock: Arc<FixedClock>,
    interactive: bool,
    password: RefCell<String>,
    secret: RefCell<Option<Vec<u8>>>,
    asked: RefCell<Vec<String>>,
    shown: RefCell<Vec<String>>,
}

impl Terminal {
    fn new(clock: Arc<FixedClock>, password: &str) -> Self {
        Self {
            clock,
            interactive: true,
            password: RefCell::new(password.to_owned()),
            secret: RefCell::new(None),
            asked: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
        }
    }

    fn quiet(clock: Arc<FixedClock>) -> Self {
        Self {
            interactive: false,
            ..Self::new(clock, "")
        }
    }

    fn code(&self) -> String {
        self.secret
            .borrow()
            .as_ref()
            .map_or_else(String::new, |secret| {
                joedocs::totp::code(secret, joedocs::totp::step_at(self.clock.now().as_second()))
            })
    }
}

impl joedocs::client::Prompt for Terminal {
    fn interactive(&self) -> bool {
        self.interactive
    }
    fn ask(&self, message: &str) -> std::io::Result<String> {
        self.asked.borrow_mut().push(message.to_owned());
        Ok(
            if message.contains("press Enter") && self.secret.borrow().is_none() {
                String::new()
            } else {
                self.code()
            },
        )
    }
    fn ask_secret(&self, message: &str) -> std::io::Result<String> {
        self.asked.borrow_mut().push(message.to_owned());
        Ok(if message.contains("new password") {
            PASSWORD.to_owned()
        } else {
            self.password.borrow().clone()
        })
    }
    fn show(&self, text: &str) {
        if let Some(line) = text.lines().find(|l| l.trim_start().starts_with("Secret:")) {
            let base32 = line.trim_start().trim_start_matches("Secret:").trim();
            *self.secret.borrow_mut() = joedocs::totp::base32_decode(base32);
        }
        self.shown.borrow_mut().push(text.to_owned());
    }
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).map_or(0, |m| m.permissions().mode() & 0o777)
}

#[test]
fn the_first_sign_in_sets_up_totp_with_a_terminal_qr_code_then_only_a_code_is_needed() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let shown = terminal.shown.borrow().join("\n");
    assert!(shown.contains("otpauth://totp/JoeDocs:alice?"), "{shown}");
    assert!(
        shown.contains('█') || shown.contains('▀'),
        "a QR code is drawn"
    );
    let asked = terminal.asked.borrow().join("\n");
    assert!(asked.contains("Choose a new password"), "{asked}");
    // The device file holds the device, privately.
    let devices = server.state_home().join("joedocs/devices.toml");
    assert_eq!(mode(&devices), 0o600);
    assert_eq!(mode(&server.state_home().join("joedocs")), 0o700);
    let stored = std::fs::read_to_string(&devices).unwrap();
    assert!(
        stored.contains("jdd_") && !stored.contains(PASSWORD),
        "{stored}"
    );
    // Remote commands use alice's device and session.
    let status = server
        .run(&terminal, false, &["--user", "alice", "auth", "status"])
        .unwrap();
    assert!(
        status.contains("as alice") && status.contains("device: phoenix"),
        "{status}"
    );
    // After the session ends, a code alone renews it.
    server.clock.advance(8 * 3600);
    terminal.asked.borrow_mut().clear();
    let list = server
        .run(&terminal, false, &["--user", "alice", "user", "list"])
        .unwrap();
    assert!(list.contains("alice"), "{list}");
    let asked = terminal.asked.borrow().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("Session expired. Code for"), "{asked:?}");
}

#[test]
fn renewing_without_a_terminal_needs_a_stored_device_and_a_code() {
    let server = Server::start().unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let refused = server
        .run(&quiet, false, &["--user", "alice", "auth", "login"])
        .unwrap_err();
    assert_eq!(refused.exit_code(), 77, "{refused}");
    assert!(
        refused.to_string().contains("once at a terminal"),
        "{refused}"
    );
    let terminal = server.first_sign_in().unwrap();
    let code = terminal.code();
    let out = server
        .run(
            &quiet,
            false,
            &["--user", "alice", "auth", "login", "--code", &code],
        )
        .unwrap();
    assert!(out.contains("Signed in to"), "{out}");
    assert!(quiet.asked.borrow().is_empty());
}

#[test]
fn remote_commands_pick_their_credential_as_p05_says() {
    let server = Server::start().unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    // No user and no token: nothing to sign in with.
    let none = server.run(&quiet, false, &["user", "list"]).unwrap_err();
    assert_eq!(none.exit_code(), 77, "{none}");
    let terminal = server.first_sign_in().unwrap();
    // The only stored user, when no token is set.
    let only = server.run(&quiet, false, &["user", "list"]).unwrap();
    assert!(only.contains("alice"), "{only}");
    // The admin token wins without `--user`: tokens cannot manage users.
    let token = server.run(&quiet, true, &["user", "list"]).unwrap_err();
    assert!(token.to_string().contains("signed-in user"), "{token}");
    assert_eq!(token.exit_code(), 77);
    // `--user` wins over the token.
    let user = server
        .run(&quiet, true, &["--user", "alice", "user", "list"])
        .unwrap();
    assert!(user.contains("alice"), "{user}");
    // A remote `user create` prints the one-time password last.
    let created = server
        .run(&terminal, false, &["user", "create", "bob"])
        .unwrap();
    assert!(created.starts_with("Created user bob."), "{created}");
    assert_eq!(
        created.lines().last().unwrap_or_default().len(),
        23,
        "{created}"
    );
    // `user remove` runs only on the server.
    let remove = server
        .run(&quiet, false, &["user", "remove", "bob"])
        .unwrap_err();
    assert!(
        remove.to_string().contains("runs only on the server"),
        "{remove}"
    );
}

#[test]
fn a_revoked_device_is_forgotten_and_signing_out_keeps_or_forgets_it() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let devices = server.state_home().join("joedocs/devices.toml");
    let listed = server
        .run(&terminal, false, &["--user", "alice", "auth", "devices"])
        .unwrap();
    assert!(listed.contains("phoenix (this one)"), "{listed}");
    let out = server
        .run(&terminal, false, &["--user", "alice", "auth", "logout"])
        .unwrap();
    assert!(out.starts_with("Signed out of"), "{out}");
    assert!(
        std::fs::read_to_string(&devices).unwrap().contains("jdd_"),
        "the device stays"
    );
    // Revoking this device forgets it here.
    let code = terminal.code();
    server
        .run(
            &terminal,
            false,
            &["--user", "alice", "auth", "login", "--code", &code],
        )
        .unwrap();
    let revoked = server
        .run(
            &terminal,
            false,
            &["--user", "alice", "auth", "devices", "revoke", "phoenix"],
        )
        .unwrap();
    assert!(revoked.contains("Revoked the device phoenix"), "{revoked}");
    assert!(!std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
    // Without a terminal, a command now says to sign in again.
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let refused = server
        .run(&quiet, false, &["--user", "alice", "auth", "status"])
        .unwrap_err();
    assert_eq!(refused.exit_code(), 77, "{refused}");
}

#[test]
fn a_device_the_server_no_longer_accepts_is_forgotten_on_first_refusal() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    // The device lapses after 30 unused days.
    server.clock.advance(31 * 86_400);
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let refused = server
        .run(&quiet, false, &["--user", "alice", "auth", "status"])
        .unwrap_err();
    assert_eq!(refused.exit_code(), 77, "{refused}");
    let devices = server.state_home().join("joedocs/devices.toml");
    assert!(!std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
    // At a terminal, signing in again with the password works.
    let again = server
        .run(&terminal, false, &["--user", "alice", "auth", "login"])
        .unwrap();
    assert!(again.contains("Signed in to"), "{again}");
}

#[test]
fn changing_the_password_asks_for_the_current_one_a_code_and_the_new_one_twice() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let out = server
        .run(&terminal, false, &["--user", "alice", "auth", "password"])
        .unwrap();
    assert!(out.contains("Changed your password"), "{out}");
    let asked = terminal.asked.borrow().join("\n");
    assert!(
        asked.contains("Current password") && asked.contains("Repeat the new password"),
        "{asked}"
    );
}

#[test]
fn signing_in_never_logs_a_password_code_or_credential() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let code = terminal.code();
    let output = assert_cmd::cargo::cargo_bin_cmd!()
        .env_clear()
        .env("HOME", server.root.path())
        .env("XDG_STATE_HOME", server.state_home())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(
            "LLVM_PROFILE_FILE",
            std::env::var_os("LLVM_PROFILE_FILE").unwrap_or_default(),
        )
        .args([
            "--log-level",
            "trace",
            "--server",
            &server.url,
            "--user",
            "alice",
        ])
        .args(["auth", "login", "--code", &code])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}{stderr}");
    let stored = std::fs::read_to_string(server.state_home().join("joedocs/devices.toml")).unwrap();
    let credential = stored
        .lines()
        .find_map(|l| l.strip_prefix("credential = "))
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned();
    assert!(credential.starts_with("jdd_"));
    for secret in [
        code.as_str(),
        credential.as_str(),
        PASSWORD,
        server.initial.as_str(),
    ] {
        assert!(
            !stderr.contains(secret) && !stdout.contains(secret),
            "{secret}"
        );
    }
}

// --- Review fixes (0.5.1, JD-39) --------------------------------------------

/// A server that answers every request with `status` and the JSON `body`.
fn fake_server(status: &'static str, body: &'static str) -> TestResult<String> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut seen = Vec::new();
            let mut buf = [0_u8; 4096];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if seen.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Ok(url)
}

/// Stores a made-up device of alice's for the server at `url`.
fn store_device(server: &Server, url: &str) -> TestResult<PathBuf> {
    let dir = server.state_home().join("joedocs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("devices.toml");
    std::fs::write(
        &path,
        format!(
            "[[device]]\nserver = \"{url}\"\nuser = \"alice\"\ndevice = \"phoenix\"\ncredential = \"jdd_0011_2233\"\nsession = \"jdu_4455_6677\"\nexpires_at = \"2099-01-01T00:00:00Z\"\n"
        ),
    )?;
    Ok(path)
}

#[test]
fn forgetting_a_device_with_an_expired_session_renews_it_then_revokes_the_device() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let devices = server.state_home().join("joedocs/devices.toml");
    let saved = std::fs::read_to_string(&devices).unwrap();
    server.clock.advance(9 * 3600);
    terminal.asked.borrow_mut().clear();
    let out = server
        .run(
            &terminal,
            false,
            &["--user", "alice", "auth", "logout", "--forget-device"],
        )
        .unwrap();
    assert_eq!(
        out,
        format!("Signed out and forgot this device on {}.\n", server.url)
    );
    assert!(!std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
    let asked = terminal.asked.borrow().join("\n");
    assert!(asked.contains("Session expired. Code for"), "{asked}");
    assert!(!asked.contains("Password"), "never a full sign-in: {asked}");
    // The server revoked it: the old credential is refused as a device.
    std::fs::write(&devices, saved).unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let refused = server
        .run(&quiet, false, &["--user", "alice", "auth", "status"])
        .unwrap_err();
    assert!(refused.to_string().contains("has ended"), "{refused}");
}

#[test]
fn forgetting_a_device_without_a_terminal_or_on_a_server_error_keeps_it() {
    let server = Server::start().unwrap();
    server.first_sign_in().unwrap();
    let devices = server.state_home().join("joedocs/devices.toml");
    server.clock.advance(9 * 3600);
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let refused = server
        .run(
            &quiet,
            false,
            &["--user", "alice", "auth", "logout", "--forget-device"],
        )
        .unwrap_err();
    assert_eq!(refused.exit_code(), 77, "{refused}");
    assert!(std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
    // A server error is reported, and the device stays.
    let url = fake_server(
        "500 Internal Server Error",
        r#"{"error": {"code": "internal", "message": "something broke"}}"#,
    )
    .unwrap();
    let path = store_device(&server, &url).unwrap();
    let failed = server
        .run_at(
            &url,
            &quiet,
            false,
            &["--user", "alice", "auth", "logout", "--forget-device"],
        )
        .unwrap_err();
    assert_ne!(failed.exit_code(), 0);
    assert!(failed.to_string().contains("something broke"), "{failed}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("jdd_0011_2233")
    );
}

#[test]
fn forgetting_a_device_the_server_already_refuses_succeeds() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let devices = server.state_home().join("joedocs/devices.toml");
    let saved = std::fs::read_to_string(&devices).unwrap();
    server
        .run(
            &terminal,
            false,
            &["--user", "alice", "auth", "devices", "revoke", "phoenix"],
        )
        .unwrap();
    std::fs::write(&devices, saved).unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let out = server
        .run(
            &quiet,
            false,
            &["--user", "alice", "auth", "logout", "--forget-device"],
        )
        .unwrap();
    assert_eq!(
        out,
        format!("Signed out and forgot this device on {}.\n", server.url)
    );
    assert!(!std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
}

#[test]
fn a_backup_with_a_refused_device_signs_in_again_or_says_how_and_leaves_no_file() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    let devices = server.state_home().join("joedocs/devices.toml");
    let saved = std::fs::read_to_string(&devices).unwrap();
    // The device lapses after 30 unused days.
    server.clock.advance(31 * 86_400);
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let first = server.root.path().join("first.tgz");
    let refused = server
        .run(
            &quiet,
            false,
            &["--user", "alice", "backup", first.to_str().unwrap()],
        )
        .unwrap_err();
    assert_eq!(refused.exit_code(), 77, "{refused}");
    assert!(refused.to_string().contains("has ended"), "{refused}");
    assert!(!first.exists(), "no partial backup");
    assert!(!std::fs::read_to_string(&devices).unwrap().contains("jdd_"));
    // At a terminal, the backup signs in again and then downloads.
    std::fs::write(&devices, saved).unwrap();
    let second = server.root.path().join("second.tgz");
    let out = server
        .run(
            &terminal,
            false,
            &["--user", "alice", "backup", second.to_str().unwrap()],
        )
        .unwrap();
    assert!(second.is_file(), "{out}");
    let stored = std::fs::read_to_string(&devices).unwrap();
    assert!(stored.contains("device = \"phoenix\""), "{stored}");
}

#[test]
fn signing_in_again_after_a_refused_device_keeps_the_device_name() {
    let server = Server::start().unwrap();
    let terminal = server.first_sign_in().unwrap();
    server.clock.advance(31 * 86_400);
    let status = server
        .run(&terminal, false, &["--user", "alice", "auth", "status"])
        .unwrap();
    assert!(status.contains("device: phoenix"), "{status}");
}

#[test]
fn a_server_url_ending_in_a_slash_still_finds_the_only_stored_user() {
    let server = Server::start().unwrap();
    server.first_sign_in().unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let slashed = format!("{}/", server.url);
    let listed = server
        .run_at(&slashed, &quiet, false, &["user", "list"])
        .unwrap();
    assert!(listed.contains("alice"), "{listed}");
}

#[test]
fn an_admin_token_command_does_not_read_the_device_file() {
    let server = Server::start().unwrap();
    let quiet = Terminal::quiet(Arc::clone(&server.clock));
    let code = joedocs::totp::code(
        &server.token_secret,
        joedocs::totp::step_at(server.clock.now().as_second()),
    );
    server
        .run(&quiet, true, &["login", "--code", &code])
        .unwrap();
    let dir = server.state_home().join("joedocs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("devices.toml"), "this is [not toml").unwrap();
    let status = server.run(&quiet, true, &["status"]).unwrap();
    assert!(
        status.contains(&format!("server: {}", server.url)),
        "{status}"
    );
}

#[test]
fn auth_commands_refuse_a_malformed_success_reply() {
    let server = Server::start().unwrap();
    let terminal = Terminal::new(Arc::clone(&server.clock), PASSWORD);
    for body in ["this is not json", "{}"] {
        let url = fake_server("200 OK", body).unwrap();
        store_device(&server, &url).unwrap();
        for args in [
            &["--user", "alice", "auth", "status"][..],
            &["--user", "alice", "auth", "password"][..],
            &["--user", "alice", "auth", "devices", "revoke", "laptop"][..],
        ] {
            let refused = server.run_at(&url, &terminal, false, args).unwrap_err();
            assert_eq!(refused.exit_code(), 70, "{args:?} with {body}: {refused}");
        }
    }
}
