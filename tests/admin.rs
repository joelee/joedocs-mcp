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
