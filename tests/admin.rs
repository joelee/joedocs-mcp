//! End-to-end tests of the admin commands (`init`, `project`, `key`,
//! `reindex`, `archive`, `check`) run through the compiled binary against a
//! temporary vault and real git.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A command with a cleared environment except `PATH` (to find `git`), the
/// storage paths under `root`, and `LLVM_PROFILE_FILE` for coverage.
fn cmd(root: &Path) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
    cmd.env_clear()
        .env("HOME", root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("JOEDOCS__STORAGE__HOME", root.join("vault"))
        .env("JOEDOCS__STORAGE__STATE_DIR", root.join("state"))
        .current_dir(root);
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    cmd
}

fn vault(root: &Path) -> PathBuf {
    root.join("vault")
}

#[test]
fn init_creates_an_empty_vault_and_separate_state() {
    let root = TempDir::new().unwrap();
    cmd(root.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("Initialised JoeDocs.\n"));
    assert_eq!(std::fs::read_dir(vault(root.path())).unwrap().count(), 0);
    for file in ["joedocs.db", "history.git/HEAD", "joedocs.lock"] {
        assert!(root.path().join("state").join(file).exists(), "{file}");
    }
    cmd(root.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("Already initialised"));
}

#[test]
fn init_refuses_a_directory_with_other_files() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(vault(root.path())).unwrap();
    std::fs::write(vault(root.path()).join("mine.md"), "x").unwrap();
    cmd(root.path())
        .arg("init")
        .assert()
        .code(65)
        .stderr(predicate::str::contains(
            "is not empty and has no JoeDocs history",
        ));
}

#[test]
fn commands_before_init_explain_what_to_do() {
    let root = TempDir::new().unwrap();
    cmd(root.path())
        .args(["project", "list"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("run `joedocs init`"));
    cmd(root.path())
        .env_remove("JOEDOCS__STORAGE__HOME")
        .arg("init")
        .assert()
        .code(78)
        .stderr(predicate::str::contains("storage.home is not set"));
}

#[test]
fn projects_and_keys_can_be_managed() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args([
            "project", "add", "joedocs", "--key", "JD", "--title", "JoeDocs",
        ])
        .assert()
        .success()
        .stdout("Added project joedocs (key JD): JoeDocs\n");
    cmd(root.path())
        .args(["project", "add", "Bad Name", "--key", "BN"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("invalid project"));
    cmd(root.path())
        .args(["project", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("joedocs  JD   0          JoeDocs"));

    let output = cmd(root.path())
        .args(["key", "create", "laptop", "--grant", "joedocs:write"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let token = stdout.lines().last().unwrap();
    assert!(token.starts_with("jd_"), "{stdout}");
    assert_eq!(token.len(), 80);

    let listing = cmd(root.path())
        .args(["key", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listing = String::from_utf8(listing).unwrap();
    assert!(listing.contains("active  joedocs:write"), "{listing}");
    assert!(!listing.contains(token), "tokens are never listed");

    cmd(root.path())
        .args(["key", "grant", "laptop", "other:read"])
        .assert()
        .code(65)
        .stderr("error: project other does not exist\n");

    let output = cmd(root.path())
        .args(["key", "rotate", "laptop"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rotated = String::from_utf8(output).unwrap();
    assert!(
        rotated.starts_with("Rotated API key laptop (id "),
        "{rotated}"
    );
    let new_token = rotated.lines().last().unwrap();
    assert_eq!(new_token.len(), 80);
    assert_ne!(new_token, token);
    assert_eq!(new_token[..16], token[..16], "the key ID is kept");
    let store = library_store(root.path()).unwrap();
    assert!(
        store
            .authenticate(&token.to_owned().into())
            .unwrap()
            .is_none()
    );
    let key = store
        .authenticate(&new_token.to_owned().into())
        .unwrap()
        .unwrap();
    assert_eq!(key.grants.len(), 1);
    drop(store);

    cmd(root.path())
        .args(["key", "revoke", "laptop"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("API key laptop is revoked"));
    cmd(root.path())
        .args(["key", "rotate", "laptop"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains(
            "is revoked; create a new key instead",
        ));
}

/// Opens the store the binary uses under `root`, through the library.
fn library_store(root: &Path) -> Result<joedocs::store::Store, joedocs::store::Error> {
    use std::sync::Arc;
    let paths = joedocs::store::Paths {
        home: vault(root),
        state_dir: root.join("state"),
    };
    let git = joedocs::history::GitCli::new(paths.git_dir(), &paths.home);
    joedocs::store::Store::open(
        paths,
        joedocs::store::Settings::from_config(&joedocs::Config::default()),
        Box::new(git),
        Arc::new(joedocs::clock::FixedClock::at(1_793_000_000)),
    )
}

#[test]
fn reindex_rebuilds_from_the_history_and_reports_problems() {
    use joedocs::store::{Actor, NewDocument};
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    let mut store = library_store(root.path()).unwrap();
    let actor = Actor::new("laptop", "test");
    for title in ["Kept", "Gone"] {
        store
            .create(
                &actor,
                NewDocument {
                    project: "joedocs".into(),
                    title: title.into(),
                    content: "x".into(),
                    ..NewDocument::default()
                },
            )
            .unwrap();
    }
    store.archive(&actor, "JD-2", "test").unwrap();
    drop(store);

    cmd(root.path())
        .arg("reindex")
        .assert()
        .success()
        .stdout("Rebuilt the index from the history: 2 documents (1 archived), 3 revisions.\n");

    std::fs::write(vault(root.path()).join("joedocs/home/Kept.md"), "edited").unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let file = root.path().join(format!("state/joedocs.db{suffix}"));
        std::fs::remove_file(file).unwrap_or_default();
    }
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path()).arg("reindex").assert().success().stdout(
        "Rebuilt the index from the history: 2 documents (1 archived), 3 revisions.\n\
         Recreated project joedocs (key JD), which was missing from the database; \
         its title is its name.\n\
         Warning: these vault files differ from the history, so writes to them are refused:\n  \
         JD-1  joedocs/home/Kept.md\n",
    );
}

#[test]
fn reindex_reports_a_history_it_cannot_replay() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    std::fs::write(vault(root.path()).join("x.md"), "x").unwrap();
    let git =
        joedocs::history::GitCli::new(root.path().join("state/history.git"), vault(root.path()));
    joedocs::history::History::commit(
        &git,
        &joedocs::history::Commit {
            paths: vec!["x.md".into()],
            author: "someone".into(),
            at: jiff::Timestamp::UNIX_EPOCH,
            message: "x\n\nJoeDocs-Document: JD-1\nJoeDocs-Revision: 1\nJoeDocs-Action: create\n"
                .into(),
        },
    )
    .unwrap();
    cmd(root.path())
        .arg("reindex")
        .assert()
        .code(70)
        .stderr(predicate::str::contains(
            "cannot rebuild the index from history commit",
        ));
}

#[test]
fn archived_documents_are_listed_and_restored_and_check_reports_drift() {
    use joedocs::store::{Actor, NewDocument};
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    cmd(root.path())
        .args(["archive", "list"])
        .assert()
        .success()
        .stdout("No archived documents.\n");
    cmd(root.path())
        .arg("check")
        .assert()
        .success()
        .stdout("No problems found.\n");

    let mut store = library_store(root.path()).unwrap();
    let actor = Actor::new("laptop", "test");
    store
        .create(
            &actor,
            NewDocument {
                project: "joedocs".into(),
                title: "Old plan".into(),
                content: "x".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    store.archive(&actor, "JD-1", "superseded").unwrap();
    drop(store);

    cmd(root.path())
        .args(["archive", "list", "--project", "joedocs"])
        .assert()
        .success()
        .stdout(
            "ID    PROJECT  TYPE  TITLE     ARCHIVED              BY      REASON\n\
             JD-1  joedocs  home  Old plan  2026-10-26T07:33:20Z  laptop  superseded\n",
        );
    cmd(root.path())
        .args(["archive", "restore", "jd-1"])
        .assert()
        .success()
        .stdout("Restored JD-1 at revision 3: joedocs/home/Old plan.md\n");
    cmd(root.path())
        .args(["archive", "restore", "JD-1"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("JD-1 is not archived"));
    let log = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(root.path().join("state/history.git"))
        .args(["log", "-1", "--format=%an|%s"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout),
        "admin|Unarchive JD-1\n"
    );

    std::fs::write(
        vault(root.path()).join("joedocs/home/Old plan.md"),
        "edited",
    )
    .unwrap();
    std::fs::write(vault(root.path()).join("stray.md"), "x").unwrap();
    cmd(root.path())
        .arg("check")
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "Vault files that are missing or differ from the history (writes to them are refused):\n  \
             JD-1  joedocs/home/Old plan.md\n",
        ))
        .stdout(predicate::str::contains("restore --source=HEAD -- '<path>'"))
        .stdout(predicate::str::contains(
            "Vault files that belong to no document (move them out of the vault):\n  stray.md\n",
        ))
        .stderr("error: check found 2 problems\n");

    let conn = joedocs::db::open(&root.path().join("state/joedocs.db")).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'index_format'", [])
        .unwrap();
    drop(conn);
    cmd(root.path())
        .arg("check")
        .assert()
        .code(1)
        .stdout(predicate::str::starts_with(
            "The index is behind the history",
        ));
}

#[test]
fn systemd_install_dry_run_shows_the_unit_for_this_binary() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    let output = cmd(root.path())
        .args(["systemd", "install", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(output).unwrap();
    let exe = std::fs::canonicalize(assert_cmd::cargo::cargo_bin!()).unwrap();
    assert!(
        out.contains(&format!("ExecStart=\"{}\" serve\n", exe.display())),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "Environment=\"JOEDOCS__STORAGE__HOME={}\"\n",
            vault(root.path()).display()
        )),
        "{out}"
    );
    assert!(!root.path().join(".config/systemd").exists());
    cmd(root.path())
        .args(["systemd", "install", "--run-as", "joedocs"])
        .assert()
        .code(65)
        .stderr("error: --run-as needs --system; a user unit runs as you\n");
}

#[test]
fn check_reports_a_stale_index_file_and_reindex_rewrites_it() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    let index = vault(root.path()).join("joedocs/INDEX.md");
    assert!(index.is_file());
    std::fs::write(&index, "edited by hand\n").unwrap();
    cmd(root.path())
        .arg("check")
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "INDEX.md files that are missing or out of date (`joedocs reindex` rewrites them):\n  \
             joedocs/INDEX.md\n",
        ))
        .stderr(predicate::str::starts_with("error: check found 1 "));
    cmd(root.path()).arg("reindex").assert().success();
    cmd(root.path())
        .arg("check")
        .assert()
        .success()
        .stdout("No problems found.\n");
}

#[test]
fn get_prints_a_document_or_one_part_of_it() {
    use joedocs::store::{Actor, NewDocument};
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    let mut store = library_store(root.path()).unwrap();
    let actor = Actor::new("laptop", "test");
    store
        .create(
            &actor,
            NewDocument {
                project: "joedocs".into(),
                title: "Plan".into(),
                content: "First body.\n".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    store
        .append(&actor, "JD-1", "Second part.\n", Some(1), None)
        .unwrap();
    let latest = store.get("JD-1", None).unwrap().markdown;
    let first = store.get("JD-1", Some(1)).unwrap().markdown;
    drop(store);
    let split = |text: &str| {
        let rest = text.strip_prefix("---\n").unwrap();
        let end = rest.find("\n---\n").unwrap();
        (rest[..=end].to_owned(), rest[end + 5..].to_owned())
    };
    let (yaml, body) = split(&latest);
    let (first_yaml, first_body) = split(&first);
    assert!(body.contains("Second part."), "{body}");

    let get = |args: &[&str]| {
        let mut command = cmd(root.path());
        command.arg("get").args(args);
        command
    };
    get(&["JD-1"]).assert().success().stdout(latest.clone());
    get(&["JD-1", "--content-only"])
        .assert()
        .success()
        .stdout(body.clone());
    let output = get(&["JD-1", "--front-matter-only"]).assert().success();
    let printed = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert_eq!(printed, yaml);
    assert!(!printed.contains("---"), "{printed}");
    assert!(printed.contains("id: JD-1\n"), "{printed}");
    assert!(printed.contains("revision: 2\n"), "{printed}");

    get(&["JD-1", "--version", "1"])
        .assert()
        .success()
        .stdout(first.clone());
    get(&["JD-1", "--version", "1", "--content-only"])
        .assert()
        .success()
        .stdout(first_body);
    get(&["JD-1", "--version", "1", "--front-matter-only"])
        .assert()
        .success()
        .stdout(first_yaml);
    get(&["JD-1", "--content-only", "--front-matter-only"])
        .assert()
        .code(2);

    for (args, expected) in [
        (vec![], latest.clone()),
        (vec!["--content-only"], body.clone()),
        (vec!["--front-matter-only"], yaml.clone()),
    ] {
        let file = root.path().join("out.md");
        std::fs::write(&file, "old contents").unwrap();
        let mut full = vec!["JD-1", "-f", file.to_str().unwrap()];
        full.extend(args);
        get(&full).assert().success().stdout("");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
    }
    let file = root.path().join("long.md");
    get(&["JD-1", "--file", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout("");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), latest);

    get(&["JD-9"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("JD-9"));
    let mut store = library_store(root.path()).unwrap();
    store.archive(&actor, "JD-1", "done").unwrap();
    drop(store);
    get(&["JD-1"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("joedocs archive restore JD-1"));
}

/// Answers one `GET /health` with `ok`, on a free loopback port.
fn health_responder() -> std::io::Result<std::net::SocketAddr> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut seen = Vec::new();
            let mut byte = [0_u8; 1];
            while !seen.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                seen.push(byte[0]);
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    Ok(addr)
}

#[test]
fn status_describes_the_installation_without_secrets() {
    use joedocs::store::{Actor, NewDocument};
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    cmd(root.path())
        .args(["key", "create", "laptop", "--grant", "joedocs:write"])
        .assert()
        .success();
    let mut store = library_store(root.path()).unwrap();
    let actor = Actor::new("laptop", "test");
    for title in ["One", "Two"] {
        store
            .create(
                &actor,
                NewDocument {
                    project: "joedocs".into(),
                    title: title.into(),
                    content: "x".into(),
                    ..NewDocument::default()
                },
            )
            .unwrap();
    }
    store.archive(&actor, "JD-2", "done").unwrap();
    drop(store);

    // Nothing listens on the bind address.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let output = cmd(root.path())
        .env("JOEDOCS__SERVER__BIND", format!("127.0.0.1:{port}"))
        .env("JOEDOCS_API_TOKEN", "jd_0123456789ab_secretvalue")
        .arg("status")
        .assert()
        .success();
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    for expected in [
        "JoeDocs ",
        &format!("bind: 127.0.0.1:{port}"),
        "health: not reachable",
        &vault(root.path()).display().to_string(),
        &root.path().join("state").display().to_string(),
        "database:",
        "history:",
        "index: current",
        "API keys: 1 active, 0 revoked",
        "joedocs",
    ] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from:\n{text}"
        );
    }
    assert!(
        text.lines()
            .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["joedocs", "JD", "1", "1"]),
        "{text}"
    );
    for secret in ["secretvalue", "jd_", "jda_", "jds_"] {
        assert!(!text.contains(secret), "{secret} leaked:\n{text}");
    }

    let addr = health_responder().unwrap();
    cmd(root.path())
        .env("JOEDOCS__SERVER__BIND", addr.to_string())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("health: ok"));
}

#[test]
fn agents_prints_the_guide_for_this_installation() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    let output = cmd(root.path())
        .env("JOEDOCS__SERVER__BIND", "127.0.0.1:9911")
        .env("JOEDOCS_API_TOKEN", "jd_0123456789ab_secretvalue")
        .env("JOEDOCS_ADMIN_TOKEN", "jda_0123456789ab_secretvalue")
        .arg("agents")
        .assert()
        .success();
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(text.starts_with("# AGENTS.md"), "{text}");
    for expected in [
        vault(root.path()).display().to_string(),
        root.path().join("state").display().to_string(),
        "127.0.0.1:9911".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    ] {
        assert!(text.contains(&expected), "{expected:?} missing");
    }
    assert!(!text.contains("{{"), "unfilled placeholder:\n{text}");
    assert!(!text.contains("secretvalue"), "{text}");
}

/// The entry names in a `.tgz` and the text of `manifest.json`.
fn archive_entries(path: &Path) -> std::io::Result<(Vec<String>, String)> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut names = Vec::new();
    let mut manifest = String::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        if name == "manifest.json" {
            entry.read_to_string(&mut manifest)?;
        }
        names.push(name);
    }
    Ok((names, manifest))
}

#[test]
fn backup_writes_a_private_consistent_archive() {
    use joedocs::store::{Actor, NewDocument};
    use std::os::unix::fs::PermissionsExt as _;
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    cmd(root.path())
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    let mut store = library_store(root.path()).unwrap();
    store
        .create(
            &Actor::new("laptop", "test"),
            NewDocument {
                project: "joedocs".into(),
                title: "Plan".into(),
                content: "x".into(),
                ..NewDocument::default()
            },
        )
        .unwrap();
    let head = store.summary().unwrap().history_head.unwrap();
    drop(store);

    let file = root.path().join("backup.tgz");
    cmd(root.path())
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backed up 1 document"));
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let (names, manifest) = archive_entries(&file).unwrap();
    for expected in [
        "manifest.json",
        "state/joedocs.db",
        "state/history.git/HEAD",
        "content/joedocs/home/Plan.md",
        "content/joedocs/INDEX.md",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing: {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.contains("joedocs.lock")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("backup-staging")),
        "{names:?}"
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["history_head"], head.as_str());
    assert_eq!(manifest["joedocs_version"], env!("CARGO_PKG_VERSION"));
    assert!(manifest["schema_version"].as_u64().unwrap() >= 3);
    let leftovers: Vec<_> = std::fs::read_dir(root.path().join("state"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("backup-staging")
        })
        .collect();
    assert!(leftovers.is_empty(), "staging left behind");

    // An existing file is never replaced.
    cmd(root.path())
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("already exists"));
    assert_eq!(archive_entries(&file).unwrap().0, names);
}

#[test]
fn backup_waits_for_a_writer_holding_the_lock() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("state/joedocs.lock"))
        .unwrap();
    lock.lock().unwrap();
    let file = root.path().join("waited.tgz");
    let mut backup = cmd(root.path());
    backup.args(["backup", file.to_str().unwrap()]);
    let running = std::thread::spawn(move || backup.assert().success());
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(!file.exists(), "backup ran while the lock was held");
    lock.unlock().unwrap();
    running.join().unwrap();
    assert!(file.exists());
}
