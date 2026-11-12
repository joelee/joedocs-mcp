//! End-to-end tests of `joedocs user ...` on the server, run through the
//! compiled binary against a temporary vault and real git (JD-37 STEP-06).

use std::path::Path;

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

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn initialised() -> TestResult<TempDir> {
    let root = TempDir::new()?;
    cmd(root.path()).arg("init").assert().success();
    Ok(root)
}

fn stdout(root: &Path, args: &[&str]) -> TestResult<String> {
    let out = cmd(root).args(args).assert().success().get_output().clone();
    Ok(String::from_utf8(out.stdout)?)
}

/// The last line of `text`: where a one-time password is printed.
fn last_line(text: &str) -> &str {
    text.lines().last().unwrap_or_default()
}

fn list_row(root: &Path, name: &str) -> TestResult<String> {
    Ok(stdout(root, &["user", "list"])?
        .lines()
        .find(|l| l.starts_with(&format!("{name} ")))
        .unwrap_or_default()
        .to_owned())
}

#[test]
fn users_are_created_with_a_one_time_password_alone_on_the_last_line() {
    let root = initialised().unwrap();
    let out = stdout(root.path(), &["user", "create", "alice"]).unwrap();
    assert!(out.starts_with("Created user alice.\n"), "{out}");
    assert!(out.contains("72 hours"), "{out}");
    let password = last_line(&out);
    assert_eq!(password.len(), 23, "{password}");
    assert!(
        password
            .chars()
            .all(|c| c == '-' || c.is_ascii_uppercase() || c.is_ascii_digit()),
        "{password}"
    );
    let second = stdout(root.path(), &["user", "create", "bob"]).unwrap();
    assert_ne!(last_line(&second), password);
    let row = list_row(root.path(), "alice").unwrap();
    assert!(row.contains("setup-pending until"), "{row}");
    assert!(
        !stdout(root.path(), &["user", "list"])
            .unwrap()
            .contains(password)
    );
}

#[test]
fn bad_and_taken_names_are_refused() {
    let root = initialised().unwrap();
    cmd(root.path())
        .args(["user", "create", "Alice"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("lower-case"));
    stdout(root.path(), &["user", "create", "alice"]).unwrap();
    cmd(root.path())
        .args(["user", "create", "alice"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("already exists"));
    cmd(root.path())
        .args(["user", "disable", "nobody"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("user nobody"));
}

#[test]
fn users_are_listed_with_their_state_and_never_a_secret() {
    let root = initialised().unwrap();
    assert_eq!(
        stdout(root.path(), &["user", "list"]).unwrap(),
        "No users.\n"
    );
    stdout(root.path(), &["user", "create", "alice"]).unwrap();
    let list = stdout(root.path(), &["user", "list"]).unwrap();
    let header = list.lines().next().unwrap();
    for column in ["NAME", "STATE", "TOTP", "LAST LOGIN", "DEVICES", "CREATED"] {
        assert!(header.contains(column), "{header}");
    }
    let row = list_row(root.path(), "alice").unwrap();
    assert!(row.contains(" - "), "no authenticator yet: {row}");
    assert!(row.contains("never"), "{row}");
    assert!(!list.contains("argon2"), "{list}");
}

#[test]
fn disable_enable_and_unlock_change_the_state() {
    let root = initialised().unwrap();
    stdout(root.path(), &["user", "create", "alice"]).unwrap();
    let out = stdout(root.path(), &["user", "disable", "alice"]).unwrap();
    assert!(out.contains("Disabled user alice"), "{out}");
    assert!(list_row(root.path(), "alice").unwrap().contains("disabled"));
    let out = stdout(root.path(), &["user", "enable", "alice"]).unwrap();
    assert!(out.contains("Enabled user alice"), "{out}");
    assert!(!list_row(root.path(), "alice").unwrap().contains("disabled"));
    let out = stdout(root.path(), &["user", "unlock", "alice"]).unwrap();
    assert!(out.contains("alice"), "{out}");
}

#[test]
fn resets_print_a_new_one_time_password() {
    let root = initialised().unwrap();
    let first = stdout(root.path(), &["user", "create", "alice"]).unwrap();
    let out = stdout(root.path(), &["user", "reset-password", "alice"]).unwrap();
    assert!(out.contains("alice"), "{out}");
    let reset = last_line(&out);
    assert_eq!(reset.len(), 23);
    assert_ne!(reset, last_line(&first));
    let out = stdout(root.path(), &["user", "reset-totp", "alice"]).unwrap();
    assert!(out.contains("authenticator"), "{out}");
    assert_eq!(last_line(&out).len(), 23);
    assert!(
        list_row(root.path(), "alice")
            .unwrap()
            .contains("setup-pending")
    );
}

#[test]
fn removed_users_keep_their_name_taken() {
    let root = initialised().unwrap();
    stdout(root.path(), &["user", "create", "alice"]).unwrap();
    let out = stdout(root.path(), &["user", "remove", "alice"]).unwrap();
    assert!(out.contains("Removed user alice"), "{out}");
    assert!(list_row(root.path(), "alice").unwrap().contains("removed"));
    cmd(root.path())
        .args(["user", "create", "alice"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("already exists"));
    cmd(root.path())
        .args(["user", "enable", "alice"])
        .assert()
        .code(65)
        .stderr(predicate::str::contains("removed"));
}
