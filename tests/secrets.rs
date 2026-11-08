//! The secret scan (`.gitleaks.toml`) must catch API keys in the `jd_...`
//! format, which the built-in gitleaks rules miss, without flagging the
//! documentation's placeholders.
//!
//! The fake keys are assembled at run time, so no key-shaped text is ever in
//! this file or the git history. Needs `gitleaks` on `PATH`, a required tool.

use std::path::Path;
use std::process::Command;

/// Exit status of `gitleaks dir` with the repository's config on a file
/// holding `text`.
fn scan(text: &str) -> Result<Option<i32>, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("client.txt");
    std::fs::write(&file, text)?;
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join(".gitleaks.toml");
    let status = Command::new("gitleaks")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "dir",
            "--no-banner",
            "--redact",
            "--log-level",
            "error",
            "--config",
        ])
        .arg(&config)
        .arg(&file)
        .status()?;
    Ok(status.code())
}

/// A key in the `jd_<12 hex>_<64 hex>` format of `src/auth.rs`, with
/// repeated digits so no other rule could mistake it for a real secret.
fn fake_key(id: &str, secret: &str) -> String {
    format!("jd_{}_{}", id.repeat(6), secret.repeat(32))
}

#[test]
fn a_joedocs_key_is_reported() {
    let header = format!("Authorization: Bearer {}\n", fake_key("ab", "cd"));
    assert_eq!(scan(&header).unwrap(), Some(1), "gitleaks missed {header}");
}

#[test]
fn an_upper_case_joedocs_key_is_reported() {
    let header = format!("\"Authorization\": \"Bearer {}\"\n", fake_key("AB", "CD"));
    assert_eq!(scan(&header).unwrap(), Some(1), "gitleaks missed {header}");
}

#[test]
fn admin_and_session_tokens_are_reported() {
    for prefix in ["jda", "jds"] {
        let token = format!("{prefix}{}", &fake_key("ab", "cd")[2..]);
        let line = format!("JOEDOCS_ADMIN_TOKEN={token}\n");
        assert_eq!(scan(&line).unwrap(), Some(1), "gitleaks missed {line}");
    }
}

#[test]
fn documentation_placeholders_are_not_reported() {
    let placeholders = "\
--header \"Authorization: Bearer jd_...\"
clients send `Authorization: Bearer jd_<id>_<secret>`
API keys: `jd_<12 hex id>_<64 hex secret>`
JOEDOCS_TEST_TOKEN=jd_... just docker-test
admin tokens look like `jda_<12 hex id>_<64 hex secret>`; sessions `jds_...`
";
    assert_eq!(scan(placeholders).unwrap(), Some(0));
}
