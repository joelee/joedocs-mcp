//! Saving `client.server` into a config file, for `joedocs login`.
//!
//! The file is edited, not rewritten from the schema: comments, key order and
//! other tables stay as they are.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use crate::app_id::AppId;
use crate::env::Env;

/// The file `joedocs login` saves `client.server` in: the config file in use
/// (`in_use`), else `$XDG_CONFIG_HOME/<app>/config.toml` when
/// `XDG_CONFIG_HOME` is absolute, else `$HOME/.config/<app>/config.toml`.
/// `None` when there is no file in use and neither variable helps.
#[must_use]
pub fn client_file(in_use: Option<&Path>, app: &AppId, env: &dyn Env) -> Option<PathBuf> {
    if let Some(path) = in_use {
        return Some(path.to_path_buf());
    }
    let file = Path::new(app.name()).join("config.toml");
    env.non_empty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|xdg| xdg.is_absolute())
        .map(|xdg| xdg.join(&file))
        .or_else(|| {
            env.non_empty("HOME")
                .map(|home| PathBuf::from(home).join(".config").join(&file))
        })
}

/// Sets `[client] server` to `server` in the TOML file at `path`, creating
/// the file (mode `0600`) and its missing directories (mode `0700`) when
/// needed. Everything else in the file is kept.
///
/// # Errors
///
/// Returns the I/O error, or [`io::ErrorKind::InvalidData`] when the file is
/// not TOML or its `client` is not a table.
pub fn set_client_server(path: &Path, server: &str) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = with_client_server(&text, server)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    crate::admin::write_atomically(path, updated.as_bytes())
}

/// `text` with `[client] server` set to `server`.
fn with_client_server(text: &str, server: &str) -> io::Result<String> {
    let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(format!("not valid TOML: {e}")))?;
    let client = doc
        .as_table_mut()
        .entry("client")
        .or_insert_with(toml_edit::table);
    let Some(client) = client.as_table_like_mut() else {
        return Err(invalid("`client` is not a table".into()));
    };
    match client.get_mut("server") {
        // Replace only the value, so comments above the key and after the
        // value stay.
        Some(item) => {
            let decor = item.as_value().map(|v| v.decor().clone());
            *item = toml_edit::value(server);
            if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
                *value.decor_mut() = decor;
            }
        }
        None => {
            client.insert("server", toml_edit::value(server));
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn the_file_in_use_wins_then_xdg_then_home() {
        let app = AppId::current();
        let both = MapEnv::new()
            .with("XDG_CONFIG_HOME", "/xdg")
            .with("HOME", "/home/me");
        assert_eq!(
            client_file(Some(Path::new("/etc/joedocs/config.toml")), &app, &both),
            Some(PathBuf::from("/etc/joedocs/config.toml"))
        );
        assert_eq!(
            client_file(None, &app, &both),
            Some(PathBuf::from("/xdg/joedocs/config.toml"))
        );
        let relative = MapEnv::new()
            .with("XDG_CONFIG_HOME", "xdg")
            .with("HOME", "/home/me");
        assert_eq!(
            client_file(None, &app, &relative),
            Some(PathBuf::from("/home/me/.config/joedocs/config.toml"))
        );
        assert_eq!(client_file(None, &app, &MapEnv::new()), None);
    }

    #[test]
    fn the_server_is_added_or_replaced_and_the_rest_kept() {
        let url = "https://docs.example.ts.net";
        assert_eq!(
            with_client_server("", url).unwrap(),
            format!("[client]\nserver = \"{url}\"\n")
        );
        assert_eq!(
            with_client_server("# top\n[client]\nserver = \"old\" # note\n", url).unwrap(),
            format!("# top\n[client]\nserver = \"{url}\" # note\n")
        );
        assert_eq!(
            with_client_server("client = { server = \"\" }\n", url).unwrap(),
            format!("client = {{ server = \"{url}\" }}\n")
        );
        assert_eq!(
            with_client_server("client.server = \"\"\n", url).unwrap(),
            format!("client.server = \"{url}\"\n")
        );
    }

    #[test]
    fn files_that_are_not_toml_or_whose_client_is_not_a_table_are_refused() {
        for text in ["[client\n", "client = 3\n"] {
            let err = with_client_server(text, "https://x").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{text}");
        }
    }

    #[test]
    fn a_new_file_and_its_directories_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a/joedocs/config.toml");
        set_client_server(&path, "https://x").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("a/joedocs")), 0o700);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[client]\nserver = \"https://x\"\n"
        );
    }
}
