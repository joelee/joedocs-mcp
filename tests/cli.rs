//! End-to-end tests that run the compiled binary.
//!
//! Every test clears the environment and runs in a fresh temporary directory
//! so nothing from the developer's machine (real `~/.config`, `/etc`, a
//! local `.env` or `config.toml`) can leak in.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const NAME: &str = env!("CARGO_PKG_NAME");

/// Upper-case environment prefix derived from the package name.
fn prefix() -> String {
    NAME.to_ascii_uppercase().replace('-', "_")
}

/// A command with a cleared environment, `HOME` pointing at `home`, and the
/// working directory set to `home`. `LLVM_PROFILE_FILE` is preserved so
/// coverage still records the child process.
fn cmd(home: &Path) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!();
    cmd.env_clear().env("HOME", home).current_dir(home);
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    cmd
}

/// Writes `~/.config/<name>/config.toml` under `home`. Helpers return
/// `Result` because `unwrap()` is only allowed inside `#[test]` functions.
fn write_user_config(home: &Path, contents: &str) -> std::io::Result<()> {
    let dir = home.join(".config").join(NAME);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("config.toml"), contents)
}

#[test]
fn no_command_prints_help_and_fails() {
    let home = TempDir::new().unwrap();
    cmd(home.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"))
        .stderr(predicate::str::contains("config"));
}

#[test]
fn prints_version() {
    let home = TempDir::new().unwrap();
    cmd(home.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn reads_user_config_from_home() {
    let home = TempDir::new().unwrap();
    write_user_config(home.path(), "[storage]\nhome = \"/srv/from-home\"\n").unwrap();
    cmd(home.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "home = \"/srv/from-home\"  # file ",
        ));
}

#[test]
fn xdg_config_home_wins_over_home() {
    let home = TempDir::new().unwrap();
    write_user_config(home.path(), "[storage]\nhome = \"/srv/from-home\"\n").unwrap();
    let xdg = home.path().join("xdg");
    fs::create_dir_all(xdg.join(NAME)).unwrap();
    fs::write(
        xdg.join(NAME).join("config.toml"),
        "[storage]\nhome = \"/srv/from-xdg\"\n",
    )
    .unwrap();
    cmd(home.path())
        .env("XDG_CONFIG_HOME", &xdg)
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("home = \"/srv/from-xdg\""));
}

#[test]
fn explicit_config_flag_is_used() {
    let home = TempDir::new().unwrap();
    let file = home.path().join("custom.toml");
    fs::write(&file, "[documents]\ndefault_type = \"plans\"\n").unwrap();
    cmd(home.path())
        .arg("--config")
        .arg(&file)
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "default_type = \"plans\"  # file ",
        ));
}

#[test]
fn missing_explicit_config_exits_with_ex_config() {
    let home = TempDir::new().unwrap();
    cmd(home.path())
        .args(["--config", "nope.toml", "config", "show"])
        .assert()
        .code(78)
        .stderr(predicate::str::contains(
            "nope.toml (from --config) does not exist",
        ));
}

#[test]
fn rule_violation_exits_with_ex_config_naming_the_variable() {
    let home = TempDir::new().unwrap();
    let var = format!("{}__DOCUMENTS__DEFAULT_TYPE", prefix());
    cmd(home.path())
        .env(&var, "memo")
        .args(["config", "show"])
        .assert()
        .code(78)
        .stderr(predicate::str::contains(format!(
            "invalid documents.default_type (from env {var})"
        )));
}

#[test]
fn unknown_override_exits_with_ex_config() {
    let home = TempDir::new().unwrap();
    let var = format!("{}__SERVER__COLOUR", prefix());
    cmd(home.path())
        .env(&var, "red")
        .args(["config", "show"])
        .assert()
        .code(78)
        .stderr(predicate::str::contains(var));
}

#[test]
fn dotenv_supplies_values_but_process_env_wins() {
    let home = TempDir::new().unwrap();
    let var = format!("{}__STORAGE__HOME", prefix());
    fs::write(home.path().join(".env"), format!("{var}=/srv/dotenv\n")).unwrap();
    cmd(home.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("home = \"/srv/dotenv\""));
    cmd(home.path())
        .env(&var, "/srv/process")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("home = \"/srv/process\""));
}

#[test]
fn malformed_dotenv_exits_with_ex_config() {
    let home = TempDir::new().unwrap();
    fs::write(home.path().join(".env"), "NOT VALID LINE\n").unwrap();
    cmd(home.path())
        .args(["config", "show"])
        .assert()
        .code(78)
        .stderr(predicate::str::contains("cannot read .env"));
}

#[test]
fn config_show_reports_sources_and_hides_secrets() {
    let home = TempDir::new().unwrap();
    write_user_config(home.path(), "[storage]\nhome = \"/srv/docs\"\n").unwrap();
    let token_var = format!("{}_API_TOKEN", prefix());
    cmd(home.path())
        .env(&token_var, "do-not-print-me")
        .env(format!("{}__SERVER__MAX_DOCUMENT_BYTES", prefix()), "3")
        .args(["--log-level", "warn", "config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("home = \"/srv/docs\"  # file "))
        .stdout(predicate::str::contains(format!(
            "max_document_bytes = 3  # env {}__SERVER__MAX_DOCUMENT_BYTES",
            prefix()
        )))
        .stdout(predicate::str::contains(
            "level = \"warn\"  # cli --log-level",
        ))
        .stdout(predicate::str::contains(format!("# {token_var}: set")))
        .stdout(predicate::str::contains("do-not-print-me").not());
}

#[test]
fn logs_have_no_colour_codes_when_stderr_is_not_a_terminal() {
    let home = TempDir::new().unwrap();
    // assert_cmd pipes stderr, as the journal and `brew services` do.
    let output = cmd(home.path())
        .args(["--log-level", "debug", "config", "show"])
        .assert()
        .success();
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("running command"), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr:?}");
}

#[test]
fn config_paths_lists_search_order() {
    let home = TempDir::new().unwrap();
    write_user_config(home.path(), "").unwrap();
    cmd(home.path())
        .args(["config", "paths"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("/etc/{NAME}/config.toml")))
        .stdout(predicate::str::contains("config.toml  <- selected"));
}

#[test]
fn debug_logging_goes_to_stderr() {
    let home = TempDir::new().unwrap();
    cmd(home.path())
        .args(["--log-level", "debug", "config", "paths"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search order"))
        .stderr(predicate::str::contains("running command"));
}
