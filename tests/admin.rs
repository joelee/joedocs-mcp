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

/// A store under `root` with one project, one document and one key; returns
/// the key's token.
fn populated(root: &Path) -> String {
    use joedocs::store::{Actor, NewDocument};
    cmd(root).arg("init").assert().success();
    cmd(root)
        .args(["project", "add", "joedocs", "--key", "JD"])
        .assert()
        .success();
    let created = cmd(root)
        .args(["key", "create", "laptop", "--grant", "joedocs:write"])
        .assert()
        .success();
    let token = String::from_utf8(created.get_output().stdout.clone())
        .unwrap_or_default()
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    if let Ok(mut store) = library_store(root) {
        let actor = Actor::new("laptop", "test");
        let _ = store.create(
            &actor,
            NewDocument {
                project: "joedocs".into(),
                title: "Plan".into(),
                content: "first\n".into(),
                ..NewDocument::default()
            },
        );
        let _ = store.append(&actor, "JD-1", "second\n", Some(1), None);
    }
    token
}

/// `cmd` pointed at another vault and state directory.
fn cmd_at(root: &Path, home: &Path, state: &Path) -> Command {
    let mut command = cmd(root);
    command
        .env("JOEDOCS__STORAGE__HOME", home)
        .env("JOEDOCS__STORAGE__STATE_DIR", state);
    command
}

#[test]
fn restore_round_trips_a_backup_into_empty_directories() {
    use secrecy::SecretString;
    let root = TempDir::new().unwrap();
    let token = populated(root.path());
    let file = root.path().join("b.tgz");
    cmd(root.path())
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .success();

    let home = root.path().join("restored/vault");
    let state = root.path().join("restored/state");
    std::fs::create_dir_all(&home).unwrap(); // empty is fine
    cmd(root.path())
        .args(["backup", "--restore", "--content-dir"])
        .arg(&home)
        .arg("--state-dir")
        .arg(&state)
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("Restored 1 document"))
        .stdout(predicate::str::contains("No problems found."))
        .stdout(predicate::str::contains("state_dir ="));

    cmd_at(root.path(), &home, &state)
        .arg("check")
        .assert()
        .success();
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&state),
            0o700,
            "the restored state directory is private"
        );
        assert_eq!(mode(&state.join("joedocs.db")), 0o600);
    }
    let original = cmd(root.path()).args(["get", "JD-1"]).assert().success();
    cmd_at(root.path(), &home, &state)
        .args(["get", "JD-1"])
        .assert()
        .success()
        .stdout(String::from_utf8(original.get_output().stdout.clone()).unwrap());

    let before = library_store(root.path())
        .unwrap()
        .history("JD-1", 1, 50)
        .unwrap();
    let restored = {
        use std::sync::Arc;
        let paths = joedocs::store::Paths {
            home: home.clone(),
            state_dir: state.clone(),
        };
        let git = joedocs::history::GitCli::new(paths.git_dir(), &paths.home);
        joedocs::store::Store::open(
            paths,
            joedocs::store::Settings::from_config(&joedocs::Config::default()),
            Box::new(git),
            Arc::new(joedocs::clock::FixedClock::at(1_793_000_000)),
        )
        .unwrap()
    };
    assert_eq!(restored.history("JD-1", 1, 50).unwrap(), before);
    let key = restored
        .authenticate(&SecretString::from(token))
        .unwrap()
        .expect("the key still authenticates");
    assert_eq!(key.name, "laptop");
}

#[test]
fn restore_uses_the_configured_paths_and_refuses_non_empty_ones() {
    let root = TempDir::new().unwrap();
    populated(root.path());
    let file = root.path().join("b.tgz");
    cmd(root.path())
        .args(["backup", file.to_str().unwrap()])
        .assert()
        .success();
    // The configured store is not empty.
    cmd(root.path())
        .args(["backup", "--restore"])
        .arg(&file)
        .assert()
        .code(65)
        .stderr(predicate::str::contains("is not empty"));
    // Configured paths that do not exist yet.
    let other = TempDir::new().unwrap();
    cmd_at(
        root.path(),
        &other.path().join("vault"),
        &other.path().join("state"),
    )
    .args(["backup", "--restore"])
    .arg(&file)
    .assert()
    .success()
    .stdout(predicate::str::contains("state_dir =").not());
    // --state-dir without --restore is a usage error.
    cmd(root.path())
        .args(["backup", "--state-dir", "/tmp/x"])
        .arg(&file)
        .assert()
        .code(2);
}

/// A gzip tar with `manifest` first, then `entry` with raw header fields.
fn crafted(
    path: &Path,
    manifest: &str,
    name: &[u8],
    kind: tar::EntryType,
    link: Option<&str>,
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    builder.append_data(&mut header, "manifest.json", manifest.as_bytes())?;
    let mut header = tar::Header::new_gnu();
    if let Some(gnu) = header.as_gnu_mut() {
        gnu.name[..name.len()].copy_from_slice(name);
    }
    header.set_entry_type(kind);
    header.set_mode(0o644);
    if let Some(link) = link {
        header.set_link_name(link)?;
    }
    let data: &[u8] = if kind == tar::EntryType::Regular {
        b"x"
    } else {
        b""
    };
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder.append(&header, data)?;
    builder.into_inner()?.finish()?;
    Ok(())
}

/// A manifest, an entry name, its type, its link target, and the expected error text.
type HostileCase<'a> = (String, &'a [u8], tar::EntryType, Option<&'a str>, &'a str);

#[test]
fn restore_refuses_unknown_formats_newer_schemas_and_hostile_entries() {
    let root = TempDir::new().unwrap();
    let manifest = |format: u32, schema: u32| {
        format!(
            r#"{{"format":{format},"joedocs_version":"9.9.9","schema_version":{schema},"created_at":"2026-11-08T00:00:00Z","history_head":null,"documents":0,"vault":"/v","state_dir":"/s"}}"#
        )
    };
    let good = manifest(1, 1);
    let cases: Vec<HostileCase<'_>> = vec![
        (
            manifest(2, 1),
            b"content/a.md",
            tar::EntryType::Regular,
            None,
            "format 2",
        ),
        (
            manifest(1, 99),
            b"content/a.md",
            tar::EntryType::Regular,
            None,
            "schema version 99",
        ),
        (
            good.clone(),
            b"/etc/evil",
            tar::EntryType::Regular,
            None,
            "/etc/evil",
        ),
        (
            good.clone(),
            b"content/../../evil",
            tar::EntryType::Regular,
            None,
            "content/../../evil",
        ),
        (
            good.clone(),
            b"elsewhere/a.md",
            tar::EntryType::Regular,
            None,
            "elsewhere/a.md",
        ),
        (
            good.clone(),
            b"content/link",
            tar::EntryType::Symlink,
            Some("/etc/passwd"),
            "content/link",
        ),
        (
            good.clone(),
            b"content/hard",
            tar::EntryType::Link,
            Some("content/a.md"),
            "content/hard",
        ),
        (
            good.clone(),
            b"content/dev",
            tar::EntryType::Char,
            None,
            "content/dev",
        ),
    ];
    for (index, (manifest, name, kind, link, expected)) in cases.into_iter().enumerate() {
        let file = root.path().join(format!("bad{index}.tgz"));
        crafted(&file, &manifest, name, kind, link).unwrap();
        let home = root.path().join(format!("r{index}/vault"));
        let state = root.path().join(format!("r{index}/state"));
        cmd(root.path())
            .args(["backup", "--restore", "--content-dir"])
            .arg(&home)
            .arg("--state-dir")
            .arg(&state)
            .arg(&file)
            .assert()
            .code(65)
            .stderr(predicate::str::contains(expected));
        for dir in [&home, &state] {
            let empty = std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none());
            assert!(empty, "{} left files behind for {expected}", dir.display());
        }
        assert!(!root.path().join("evil").exists());
    }
}

#[test]
fn admin_tokens_are_created_with_a_totp_secret_listed_and_revoked() {
    let root = TempDir::new().unwrap();
    cmd(root.path()).arg("init").assert().success();
    let created = cmd(root.path())
        .args(["admin-token", "create", "laptop"])
        .assert()
        .success();
    let text = String::from_utf8(created.get_output().stdout.clone()).unwrap();
    let token = text.lines().last().unwrap();
    assert!(
        token.starts_with("jda_") && token.len() == 4 + 12 + 1 + 64,
        "{text}"
    );
    let uri = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("otpauth://totp/JoeDocs:laptop?secret="))
        .unwrap_or_else(|| panic!("no otpauth URI in:\n{text}"));
    assert!(
        uri.ends_with("&issuer=JoeDocs&algorithm=SHA1&digits=6&period=30"),
        "{uri}"
    );
    let secret = uri
        .trim_start_matches("otpauth://totp/JoeDocs:laptop?secret=")
        .split('&')
        .next()
        .unwrap();
    assert_eq!(secret.len(), 32, "20 bytes in base32 without padding");
    assert!(
        text.lines().any(|l| l.trim() == secret),
        "the secret on its own line:\n{text}"
    );

    cmd(root.path())
        .args(["admin-token", "create", "laptop"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("already exists"));
    let list = cmd(root.path())
        .args(["admin-token", "list"])
        .assert()
        .success();
    let listed = String::from_utf8(list.get_output().stdout.clone()).unwrap();
    assert!(
        listed.contains("laptop") && listed.contains("active"),
        "{listed}"
    );
    assert!(
        !listed.contains(secret) && !listed.contains("jda_"),
        "{listed}"
    );
    cmd(root.path())
        .args(["admin-token", "revoke", "laptop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Revoked admin token laptop"));
    cmd(root.path())
        .args(["admin-token", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("revoked"));
    cmd(root.path())
        .args(["admin-token", "revoke", "nobody"])
        .assert()
        .code(65);
}

/// Rewrites the backup `from` into `to`, dropping entries for which `keep`
/// is false and adding `extra` entries at the end.
fn rewrite_archive(
    from: &Path,
    to: &Path,
    keep: impl Fn(&str) -> bool,
    extra: &[(&str, &[u8])],
) -> std::io::Result<()> {
    use std::io::Read as _;
    let mut reader = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(from)?));
    let mut writer = tar::Builder::new(flate2::write::GzEncoder::new(
        std::fs::File::create(to)?,
        flate2::Compression::default(),
    ));
    for entry in reader.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        if !keep(&name) {
            continue;
        }
        let mut header = entry.header().clone();
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        writer.append_data(&mut header, &name, data.as_slice())?;
    }
    for (name, data) in extra {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        writer.append_data(&mut header, name, *data)?;
    }
    writer.into_inner()?.finish()?;
    Ok(())
}

#[test]
fn a_restore_that_cannot_open_the_store_leaves_its_targets_as_they_were() {
    let root = TempDir::new().unwrap();
    populated(root.path());
    let good = root.path().join("good.tgz");
    cmd(root.path())
        .args(["backup", good.to_str().unwrap()])
        .assert()
        .success();
    let bad_db = root.path().join("bad-db.tgz");
    rewrite_archive(
        &good,
        &bad_db,
        |n| n != "state/joedocs.db",
        &[("state/joedocs.db", b"not a database")],
    )
    .unwrap();
    let no_history = root.path().join("no-history.tgz");
    rewrite_archive(
        &good,
        &no_history,
        |n| !n.starts_with("state/history.git"),
        &[],
    )
    .unwrap();

    for (index, archive) in [&bad_db, &no_history].into_iter().enumerate() {
        // Absent targets end up absent.
        let absent = root.path().join(format!("absent{index}"));
        cmd(root.path())
            .args(["backup", "--restore", "--state-dir"])
            .arg(absent.join("state"))
            .arg("--content-dir")
            .arg(absent.join("vault"))
            .arg(archive)
            .assert()
            .failure()
            .code(predicate::ne(1));
        assert!(
            !absent.join("state").exists() && !absent.join("vault").exists(),
            "{}",
            archive.display()
        );
        // Empty targets end up empty, and a good archive restores into them.
        let empty = root.path().join(format!("empty{index}"));
        for dir in ["state", "vault"] {
            std::fs::create_dir_all(empty.join(dir)).unwrap();
        }
        let restore = |file: &Path| {
            let mut command = cmd(root.path());
            command
                .args(["backup", "--restore", "--state-dir"])
                .arg(empty.join("state"))
                .arg("--content-dir")
                .arg(empty.join("vault"))
                .arg(file);
            command
        };
        restore(archive).assert().failure().code(predicate::ne(1));
        for dir in ["state", "vault"] {
            assert_eq!(
                std::fs::read_dir(empty.join(dir)).unwrap().count(),
                0,
                "{dir} not emptied"
            );
        }
        restore(&good).assert().success();
    }

    // A store that opens but has problems keeps its data and exits 1.
    let stray = root.path().join("stray.tgz");
    rewrite_archive(&good, &stray, |_| true, &[("content/stray.md", b"x")]).unwrap();
    let kept = root.path().join("kept");
    cmd(root.path())
        .args(["backup", "--restore", "--state-dir"])
        .arg(kept.join("state"))
        .arg("--content-dir")
        .arg(kept.join("vault"))
        .arg(&stray)
        .assert()
        .code(1);
    assert!(kept.join("state/joedocs.db").is_file());
    assert!(kept.join("vault/stray.md").is_file());
}

#[test]
fn restore_refuses_an_archive_whose_gzip_checksum_or_length_is_wrong() {
    let root = TempDir::new().unwrap();
    populated(root.path());
    let good = root.path().join("good.tgz");
    cmd(root.path())
        .args(["backup", good.to_str().unwrap()])
        .assert()
        .success();
    let bytes = std::fs::read(&good).unwrap();
    // The gzip trailer is the CRC-32 of the data, then its length.
    let trailer = bytes.len() - 8;
    let mut bad_crc = bytes.clone();
    bad_crc[trailer] ^= 0xff;
    let mut bad_length = bytes.clone();
    bad_length[trailer + 4] ^= 0xff;
    let truncated = bytes[..trailer].to_vec();

    for (index, damaged) in [bad_crc, bad_length, truncated].into_iter().enumerate() {
        let file = root.path().join(format!("damaged{index}.tgz"));
        std::fs::write(&file, damaged).unwrap();
        let target = root.path().join(format!("r{index}"));
        cmd(root.path())
            .args(["backup", "--restore", "--state-dir"])
            .arg(target.join("state"))
            .arg("--content-dir")
            .arg(target.join("vault"))
            .arg(&file)
            .assert()
            .code(65)
            .stderr(predicate::str::contains("damaged"));
        assert!(
            !target.join("state").exists() && !target.join("vault").exists(),
            "damaged{index} left files behind"
        );
    }
}
