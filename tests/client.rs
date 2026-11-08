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

/// Fills a store: project `joedocs`, JD-1 at revision 2, JD-2 archived,
/// and an API key `laptop`.
fn seed(store: &mut Store) -> Result<(), Box<dyn std::error::Error>> {
    use joedocs::store::{Actor, NewDocument};
    let actor = Actor::new("laptop", "test");
    store.add_project("joedocs", "JD", Some("JoeDocs"))?;
    for title in ["Plan", "Old"] {
        store.create(
            &actor,
            NewDocument {
                project: "joedocs".into(),
                title: title.into(),
                content: "first\n".into(),
                ..NewDocument::default()
            },
        )?;
    }
    store.append(&actor, "JD-1", "second\n", Some(1), None)?;
    store.archive(&actor, "JD-2", "done")?;
    store.create_key(
        "laptop",
        &["joedocs:write".parse()?],
        &joedocs::auth::OsRandom,
    )?;
    Ok(())
}

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
        seed(&mut store)?;
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

/// Replaces what differs between two stores set up alike: timestamps,
/// 12-hex-digit IDs and tokens.
fn normalize(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-');
    let classify = |w: &str| -> String {
        let b = w.as_bytes();
        let timestamp = b.len() == 20 && b[4] == b'-' && b[10] == b'T' && b[19] == b'Z';
        if timestamp {
            "<time>".into()
        } else if ["jd_", "jda_", "jds_"].iter().any(|p| w.starts_with(p)) && w.len() > 60 {
            "<token>".into()
        } else if w.len() == 12
            && w.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            "<id>".into()
        } else {
            w.to_owned()
        }
    };
    let mut out = String::new();
    let mut current = String::new();
    for c in text.chars() {
        if word(c) {
            current.push(c);
        } else {
            out.push_str(&classify(&current));
            current.clear();
            out.push(c);
        }
    }
    out.push_str(&classify(&current));
    // Column padding follows the widths of the values replaced above.
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A second store, set up like the server's, run locally.
struct Local {
    dir: TempDir,
}

impl Local {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let paths = Paths {
            home: dir.path().join("vault"),
            state_dir: dir.path().join("state"),
        };
        let git = GitCli::new(paths.git_dir(), &paths.home);
        Store::init(&paths, &git)?;
        let mut store = Store::open(
            paths,
            Settings::from_config(&Config::default()),
            Box::new(git),
            Arc::new(FixedClock::at(1_793_000_000)),
        )?;
        seed(&mut store)?;
        Ok(Self { dir })
    }

    fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
        cmd.env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("JOEDOCS__STORAGE__HOME", self.dir.path().join("vault"))
            .env("JOEDOCS__STORAGE__STATE_DIR", self.dir.path().join("state"))
            .current_dir(self.dir.path());
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }
}

/// `(exit code, normalized stdout, normalized stderr)` of a run.
fn outcome(mut cmd: Command, args: &[&str]) -> std::io::Result<(Option<i32>, String, String)> {
    let output = cmd.args(args).output()?;
    Ok((
        output.status.code(),
        normalize(&String::from_utf8_lossy(&output.stdout)),
        normalize(&String::from_utf8_lossy(&output.stderr)),
    ))
}

#[test]
fn remote_commands_print_what_local_commands_print() {
    let server = Server::start().unwrap();
    let local = Local::new().unwrap();
    server
        .cmd()
        .args(["login", "--code", &server.code()])
        .assert()
        .success();
    for name in ["out-local.md", "out-remote.md"] {
        let _ = std::fs::remove_file(server.root.path().join(name));
    }
    std::fs::write(server.root.path().join("vault/stray.md"), "x").unwrap();
    std::fs::write(local.dir.path().join("vault/stray.md"), "x").unwrap();
    let steps: Vec<Vec<&str>> = vec![
        vec!["project", "list"],
        vec!["project", "add", "other", "--key", "OT", "--title", "Other"],
        vec!["project", "add", "other", "--key", "OT"],
        vec!["project", "list"],
        vec!["key", "list"],
        vec!["key", "create", "ci", "--grant", "joedocs:read"],
        vec!["key", "grant", "ci", "other:write"],
        vec!["key", "rotate", "ci"],
        vec!["key", "revoke", "ci"],
        vec!["key", "revoke", "nobody"],
        vec!["key", "list"],
        vec!["archive", "list"],
        vec!["archive", "list", "--project", "joedocs"],
        vec!["get", "JD-1"],
        vec!["get", "JD-1", "--version", "1"],
        vec!["get", "JD-1", "--content-only"],
        vec!["get", "JD-1", "--front-matter-only"],
        vec!["get", "JD-2"],
        vec!["get", "JD-99"],
        vec!["archive", "restore", "JD-2"],
        vec!["archive", "list"],
        vec!["check"],
        vec!["reindex"],
    ];
    for step in &steps {
        let local_run = outcome(local.cmd(), step).unwrap();
        let remote_run = outcome(server.cmd(), step).unwrap();
        assert_eq!(remote_run, local_run, "joedocs {}", step.join(" "));
    }
    assert_eq!(
        outcome(local.cmd(), &["check"]).unwrap().0,
        Some(1),
        "the stray file is a problem"
    );

    // -f writes the same file.
    let local_file = local.dir.path().join("out.md");
    let remote_file = server.root.path().join("out.md");
    local
        .cmd()
        .args(["get", "JD-1", "-f", local_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout("");
    server
        .cmd()
        .args(["get", "JD-1", "-f", remote_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout("");
    assert_eq!(
        normalize(&std::fs::read_to_string(remote_file).unwrap()),
        normalize(&std::fs::read_to_string(local_file).unwrap())
    );
}

#[test]
fn a_remote_key_works_and_each_call_is_audited() {
    use secrecy::SecretString;
    let server = Server::start().unwrap();
    server
        .cmd()
        .args(["login", "--code", &server.code()])
        .assert()
        .success();
    let audited = || -> usize {
        let conn = rusqlite::Connection::open(server.root.path().join("state/joedocs.db")).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE tool LIKE 'admin:%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let before = audited();
    let created = server
        .cmd()
        .args(["key", "create", "ci", "--grant", "joedocs:read"])
        .assert()
        .success();
    let token = String::from_utf8_lossy(&created.get_output().stdout)
        .lines()
        .last()
        .unwrap()
        .to_owned();
    server.cmd().args(["project", "list"]).assert().success();
    server.cmd().arg("check").assert().success();
    assert_eq!(audited(), before + 3);

    let store = Store::open(
        Paths {
            home: server.root.path().join("vault"),
            state_dir: server.root.path().join("state"),
        },
        Settings::from_config(&Config::default()),
        Box::new(GitCli::new(
            server.root.path().join("state/history.git"),
            server.root.path().join("vault"),
        )),
        Arc::new(joedocs::clock::SystemClock),
    )
    .unwrap();
    let key = store.authenticate(&SecretString::from(token)).unwrap();
    assert_eq!(key.map(|k| k.name).as_deref(), Some("ci"));
}

#[test]
fn a_remote_backup_downloads_privately_and_restores() {
    let server = Server::start().unwrap();
    server
        .cmd()
        .args(["login", "--code", &server.code()])
        .assert()
        .success();
    let file = server.root.path().join("remote.tgz");
    server
        .cmd()
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backed up 1 document"));
    assert_eq!(mode(&file), 0o600);
    server
        .cmd()
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("already exists"));
    let restored = server.root.path().join("restored");
    let local = Local::new().unwrap();
    local
        .cmd()
        .args(["backup", "--restore", "--state-dir"])
        .arg(restored.join("state"))
        .arg("--content-dir")
        .arg(restored.join("vault"))
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("No problems found."));
}

#[test]
fn local_only_commands_refuse_a_server() {
    let server = Server::start().unwrap();
    for args in [
        vec!["init"],
        vec!["serve"],
        vec!["systemd", "install", "--dry-run"],
        vec!["admin-token", "list"],
        vec!["agents"],
        vec!["backup", "--restore", "x.tgz"],
    ] {
        server
            .cmd()
            .args(&args)
            .assert()
            .code(65)
            .stderr(predicate::str::contains("runs only on the server"));
    }
    // `config` always describes this machine.
    server.cmd().args(["config", "show"]).assert().success();
}

#[test]
fn commands_retry_after_a_new_code_without_repeating_a_change() {
    let server = Server::start().unwrap();
    let quiet = Scripted::new(true, &[]);
    server
        .run(&quiet, &["login", "--code", &server.code()])
        .unwrap();
    server.clock.advance(8 * 3600);
    let terminal = Scripted::new(true, &[server.code()]);
    let listed = server.run(&terminal, &["project", "list"]).unwrap();
    assert!(listed.contains("joedocs"), "{listed}");
    assert_eq!(terminal.asked.get(), 1);

    server.clock.advance(8 * 3600);
    let terminal = Scripted::new(true, &[server.code()]);
    server
        .run(
            &terminal,
            &["key", "create", "once", "--grant", "joedocs:read"],
        )
        .unwrap();
    assert_eq!(terminal.asked.get(), 1);
    let keys = server
        .run(&Scripted::new(true, &[]), &["key", "list"])
        .unwrap();
    assert_eq!(keys.matches("once").count(), 1, "{keys}");
}
