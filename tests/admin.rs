//! End-to-end tests of the admin commands (`init`, `project`, `key`) run
//! through the compiled binary against a temporary vault and real git.

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
    cmd(root.path())
        .args(["key", "revoke", "laptop"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("API key laptop is revoked"));
}
