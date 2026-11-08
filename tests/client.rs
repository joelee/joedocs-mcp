//! `joedocs` as a client of the remote admin API: `--server`/`client.server`,
//! `login`, `logout`, the session file, re-login prompts and remote
//! commands, against an in-process server on a loopback port.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assert_cmd::Command;
use joedocs::Config;
use joedocs::clock::{Clock as _, FixedClock};
use joedocs::history::GitCli;
use joedocs::mcp::{AdminOptions, ServerOptions, Shared, serve};
use joedocs::store::{Paths, Settings, Store};
use predicates::prelude::*;
use secrecy::ExposeSecret as _;
use tempfile::TempDir;

/// A server on its own thread and runtime, so the client (which runs its
/// own runtime) can be called from the test thread.
struct Server {
    root: TempDir,
    url: String,
    clock: Arc<FixedClock>,
    token: String,
    secret: Vec<u8>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
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
        store.add_project("joedocs", "JD", Some("JoeDocs"))?;
        let created = store.create_admin_token("laptop", &joedocs::auth::OsRandom)?;
        let secret = joedocs::totp::base32_decode(created.totp_secret.expose_secret())
            .ok_or("bad base32")?;
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
            token: created.token.expose_secret().to_owned(),
            secret,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    fn code(&self) -> String {
        let step = joedocs::totp::step_at(self.clock.now().as_second());
        joedocs::totp::code(&self.secret, step)
    }

    fn state_home(&self) -> PathBuf {
        self.root.path().join("client-state")
    }

    /// The binary as a client of this server, with the admin token set.
    fn cmd(&self) -> Command {
        let mut cmd = self.bare();
        cmd.args(["--server", &self.url]);
        cmd
    }

    /// The binary with the client environment but no `--server`.
    fn bare(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
        cmd.env_clear()
            .env("HOME", self.root.path())
            .env("XDG_STATE_HOME", self.state_home())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS_ADMIN_TOKEN", &self.token)
            .current_dir(self.root.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
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

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).map_or(0, |m| m.permissions().mode() & 0o777)
}

#[test]
fn login_status_and_logout_through_the_binary() {
    let server = Server::start().unwrap();
    server
        .cmd()
        .args(["login", "--code", &server.code()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logged in to"));
    let file = server.state_home().join("joedocs/sessions.toml");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&server.state_home().join("joedocs")), 0o700);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(
        saved.contains(&server.url) && saved.contains("jds_"),
        "{saved}"
    );

    server
        .cmd()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Projects"))
        .stdout(predicate::str::contains("joedocs"))
        .stdout(predicate::str::contains(format!("server: {}", server.url)))
        .stdout(predicate::str::contains("session expires: "));

    server.cmd().arg("logout").assert().success();
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains(&server.url)
    );
    server
        .cmd()
        .arg("status")
        .assert()
        .code(77)
        .stderr(predicate::str::contains("joedocs login"));
}

#[test]
fn login_failures_exit_77_and_an_unreachable_server_69() {
    let server = Server::start().unwrap();
    server
        .cmd()
        .env_remove("JOEDOCS_ADMIN_TOKEN")
        .args(["login", "--code", &server.code()])
        .assert()
        .code(77)
        .stderr(predicate::str::contains("JOEDOCS_ADMIN_TOKEN"));
    server
        .cmd()
        .args(["login", "--code", "000000"])
        .assert()
        .code(77)
        .stderr(predicate::str::contains("not valid"));
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    server
        .bare()
        .args([
            "--server",
            &format!("http://127.0.0.1:{port}"),
            "login",
            "--code",
            "123456",
        ])
        .assert()
        .code(69)
        .stderr(predicate::str::contains("cannot reach"));
}

#[test]
fn client_server_must_be_https_or_loopback_http() {
    let server = Server::start().unwrap();
    for bad in [
        "http://example.com",
        "ftp://x",
        "https://",
        "https://host/path",
    ] {
        server
            .bare()
            .args(["--server", bad, "status"])
            .assert()
            .code(78)
            .stderr(predicate::str::contains("client.server"));
    }
    // Accepted, then fails to connect or authenticate, but not as config.
    server
        .bare()
        .args(["--server", "https://127.0.0.1:9", "status"])
        .assert()
        .code(predicate::ne(78));
}

/// A terminal that answers with scripted codes and counts the questions.
struct Scripted {
    interactive: bool,
    codes: std::cell::RefCell<Vec<String>>,
    asked: std::cell::Cell<usize>,
}

impl Scripted {
    fn new(interactive: bool, codes: &[String]) -> Self {
        Self {
            interactive,
            codes: std::cell::RefCell::new(codes.iter().rev().cloned().collect()),
            asked: std::cell::Cell::new(0),
        }
    }
}

impl joedocs::client::Prompt for Scripted {
    fn interactive(&self) -> bool {
        self.interactive
    }
    fn ask(&self, _message: &str) -> std::io::Result<String> {
        self.asked.set(self.asked.get() + 1);
        Ok(self.codes.borrow_mut().pop().unwrap_or_default())
    }
}

impl Server {
    /// Runs `args` through the library with `prompt`, as the binary would.
    fn run(&self, prompt: &Scripted, args: &[&str]) -> Result<String, joedocs::Error> {
        use clap::Parser as _;
        let env = joedocs::env::MapEnv::new()
            .with("HOME", self.root.path().to_str().unwrap_or_default())
            .with(
                "XDG_STATE_HOME",
                self.state_home().to_str().unwrap_or_default(),
            )
            .with("JOEDOCS_ADMIN_TOKEN", self.token.clone());
        let fs = joedocs::fs::MemFs::new();
        let ctx = joedocs::Context {
            app: joedocs::AppId::current(),
            env: &env,
            fs: &fs,
            clock: Arc::new(joedocs::clock::SystemClock),
            random: &joedocs::auth::OsRandom,
            services: &joedocs::systemd::Systemctl,
            prompt,
        };
        let mut full = vec!["joedocs", "--server", self.url.as_str()];
        full.extend_from_slice(args);
        let cli = joedocs::Cli::try_parse_from(full)
            .map_err(|e| joedocs::Error::Output(std::io::Error::other(e.to_string())))?;
        let loaded = joedocs::load(&cli, &ctx)?;
        let mut out = Vec::new();
        joedocs::execute(&cli, &loaded, &ctx, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

#[test]
fn an_expired_session_prompts_for_a_code_and_retries_once() {
    let server = Server::start().unwrap();
    let quiet = Scripted::new(true, &[]);
    server
        .run(&quiet, &["login", "--code", &server.code()])
        .unwrap();
    assert_eq!(quiet.asked.get(), 0);

    server.clock.advance(8 * 3600);
    let terminal = Scripted::new(true, &[server.code()]);
    let text = server.run(&terminal, &["status"]).unwrap();
    assert!(text.contains("Projects"), "{text}");
    assert_eq!(terminal.asked.get(), 1);
    let saved = std::fs::read_to_string(server.state_home().join("joedocs/sessions.toml")).unwrap();
    // A working session again, without another prompt.
    let again = Scripted::new(true, &[]);
    server.run(&again, &["status"]).unwrap();
    assert_eq!(again.asked.get(), 0);
    assert!(saved.contains("jds_"));
}

#[test]
fn a_wrong_code_prompts_again_up_to_three_times() {
    let server = Server::start().unwrap();
    let terminal = Scripted::new(true, &["000000".into(), server.code()]);
    server.run(&terminal, &["status"]).unwrap();
    assert_eq!(terminal.asked.get(), 2);

    server.clock.advance(8 * 3600);
    let terminal = Scripted::new(true, &["000000".into(), "111111".into(), "222222".into()]);
    let err = server.run(&terminal, &["status"]).unwrap_err();
    assert_eq!(terminal.asked.get(), 3);
    assert_eq!(err.exit_code(), 77, "{err}");
}

#[test]
fn no_prompt_without_a_terminal_a_token_or_when_locked() {
    let server = Server::start().unwrap();
    let pipe = Scripted::new(false, &[server.code()]);
    let err = server.run(&pipe, &["status"]).unwrap_err();
    assert_eq!((err.exit_code(), pipe.asked.get()), (77, 0));
    assert!(err.to_string().contains("joedocs login"), "{err}");

    for _ in 0..5 {
        let _ = server.run(&pipe, &["login", "--code", "000000"]);
    }
    let terminal = Scripted::new(true, &[server.code()]);
    let err = server.run(&terminal, &["status"]).unwrap_err();
    assert_eq!((err.exit_code(), terminal.asked.get()), (77, 0));
    assert!(err.to_string().contains("locked"), "{err}");

    let mut cmd = server.cmd();
    cmd.env_remove("JOEDOCS_ADMIN_TOKEN")
        .arg("status")
        .assert()
        .code(77)
        .stderr(predicate::str::contains("JOEDOCS_ADMIN_TOKEN"));
}
