//! Human-readable diagnostics for `config show` and `config paths`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use toml::{Table, Value};

use super::{Resolved, Source};
use crate::app_id::AppId;

/// Renders the effective configuration as TOML, annotating every key with
/// the layer that supplied it. Secrets are listed as set or unset only.
#[must_use]
pub fn render_show(app: &AppId, resolved: &Resolved, secrets: &[(String, bool)]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Effective configuration for {}", app.name());
    match &resolved.file {
        Some(path) => {
            let _ = writeln!(out, "# Config file: {}", path.display());
        }
        None => out.push_str("# Config file: none (defaults and overrides only)\n"),
    }
    render_table(&resolved.effective, "", &resolved.sources, &mut out);
    if !secrets.is_empty() {
        out.push_str("\n# Secrets (values are never shown)\n");
        for (var, set) in secrets {
            let _ = writeln!(out, "# {var}: {}", if *set { "set" } else { "unset" });
        }
    }
    out
}

/// Renders the config file search order, marking the selected file.
#[must_use]
pub fn render_paths(candidates: &[PathBuf], selected: Option<&PathBuf>) -> String {
    let mut out = String::from("# Config file search order (first existing file wins)\n");
    for path in candidates {
        let marker = if Some(path) == selected {
            "  <- selected"
        } else {
            ""
        };
        let _ = writeln!(out, "{}{marker}", path.display());
    }
    if let Some(path) = selected
        && !candidates.contains(path)
    {
        let _ = writeln!(out, "{}  <- selected (explicit)", path.display());
    }
    out
}

fn render_table(table: &Table, prefix: &str, sources: &BTreeMap<String, Source>, out: &mut String) {
    let (tables, scalars): (Vec<_>, Vec<_>) = table.iter().partition(|(_, value)| value.is_table());
    if !scalars.is_empty() && !prefix.is_empty() {
        let _ = writeln!(out, "\n[{prefix}]");
    }
    for (key, value) in scalars {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let source = sources
            .get(&path)
            .map_or_else(|| "unknown".to_owned(), ToString::to_string);
        let _ = writeln!(out, "{key} = {}  # {source}", inline(value));
    }
    for (key, value) in tables {
        if let Value::Table(child) = value {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            render_table(child, &path, sources, out);
        }
    }
}

fn inline(value: &Value) -> String {
    value.to_string().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CliValue, resolve};
    use crate::env::MapEnv;
    use crate::fs::MemFs;

    #[test]
    fn show_annotates_every_key_with_its_source() {
        let app = AppId::new("my-app");
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "2");
        let fs = MemFs::new().with("config.toml", "[storage]\nhome = \"/srv/docs\"\n");
        let cli = [CliValue {
            key: "log.level".into(),
            flag: "--log-level".into(),
            value: Value::String("debug".into()),
        }];
        let resolved = resolve(&app, None, &cli, &env, &fs).unwrap();
        let text = render_show(&app, &resolved, &[("MY_APP_API_TOKEN".into(), true)]);

        assert!(text.contains("# Config file: config.toml"), "{text}");
        assert!(text.contains("[storage]"), "{text}");
        assert!(
            text.contains("home = \"/srv/docs\"  # file config.toml"),
            "{text}"
        );
        assert!(
            text.contains("max_document_bytes = 2  # env MY_APP__SERVER__MAX_DOCUMENT_BYTES"),
            "{text}"
        );
        assert!(
            text.contains("default_type = \"home\"  # default"),
            "{text}"
        );
        assert!(
            text.contains("level = \"debug\"  # cli --log-level"),
            "{text}"
        );
        assert!(text.contains("# MY_APP_API_TOKEN: set"), "{text}");
    }

    #[test]
    fn show_reports_missing_file_and_unset_secrets() {
        let app = AppId::new("my-app");
        let resolved = resolve(&app, None, &[], &MapEnv::new(), &MemFs::new()).unwrap();
        let text = render_show(&app, &resolved, &[("MY_APP_API_TOKEN".into(), false)]);
        assert!(text.contains("# Config file: none"), "{text}");
        assert!(text.contains("# MY_APP_API_TOKEN: unset"), "{text}");
        let text = render_show(&app, &resolved, &[]);
        assert!(!text.contains("Secrets"), "{text}");
    }

    #[test]
    fn show_renders_top_level_scalars_and_nested_tables() {
        let mut nested = Table::new();
        nested.insert("deep".into(), Value::Boolean(true));
        let mut section = Table::new();
        section.insert("inner".into(), Value::Table(nested));
        let mut table = Table::new();
        table.insert("top".into(), Value::Integer(1));
        table.insert("section".into(), Value::Table(section));
        let mut out = String::new();
        render_table(&table, "", &BTreeMap::new(), &mut out);
        assert!(out.contains("top = 1  # unknown"), "{out}");
        assert!(out.contains("[section.inner]\ndeep = true"), "{out}");
        assert!(!out.contains("[section]\n"), "{out}");
    }

    #[test]
    fn paths_marks_the_selected_file() {
        let candidates = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let text = render_paths(&candidates, Some(&PathBuf::from("/b")));
        assert!(text.contains("/a\n"), "{text}");
        assert!(text.contains("/b  <- selected\n"), "{text}");

        let text = render_paths(&candidates, Some(&PathBuf::from("/explicit")));
        assert!(text.contains("/explicit  <- selected (explicit)"), "{text}");

        let text = render_paths(&candidates, None);
        assert!(!text.contains("selected\n"), "{text}");
    }
}
