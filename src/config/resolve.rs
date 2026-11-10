//! Config resolution as a pure function of its inputs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::{Config, ConfigError, Source};
use crate::app_id::AppId;
use crate::env::Env;
use crate::fs::FileSystem;

/// A value supplied by a command-line flag.
#[derive(Debug, Clone, PartialEq)]
pub struct CliValue {
    /// Dotted config key, e.g. `log.level`.
    pub key: String,
    /// The flag as the user would type it, e.g. `--log-level`.
    pub flag: String,
    /// The value, already typed by the CLI parser.
    pub value: Value,
}

/// The outcome of resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The typed, validated configuration.
    pub config: Config,
    /// The config file that was used, if any.
    pub file: Option<PathBuf>,
    /// The effective configuration as a TOML table.
    pub effective: Table,
    /// Where each dotted key's value came from.
    pub sources: BTreeMap<String, Source>,
}

/// Lists the automatic lookup locations in order, for `<APP>` named `app`.
///
/// `XDG_CONFIG_HOME` is used only when set to an absolute path, as the XDG
/// Base Directory specification requires; `HOME` only when non-empty.
#[must_use]
pub fn candidate_paths(app: &AppId, env: &dyn Env) -> Vec<PathBuf> {
    let file = Path::new(app.name()).join("config.toml");
    let mut paths = Vec::with_capacity(4);
    if let Some(xdg) = env.non_empty("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            paths.push(xdg.join(&file));
        }
    }
    if let Some(home) = env.non_empty("HOME") {
        paths.push(PathBuf::from(home).join(".config").join(&file));
    }
    paths.push(Path::new("/etc").join(&file));
    paths.push(PathBuf::from("config.toml"));
    paths
}

/// Selects the config file: the first source that exists wins.
///
/// # Errors
///
/// Returns [`ConfigError::NotFound`] when `--config` or the
/// `<PREFIX>_CONFIG_FILE` variable names a file that does not exist.
pub fn select_file(
    app: &AppId,
    config_arg: Option<&Path>,
    env: &dyn Env,
    fs: &dyn FileSystem,
) -> Result<Option<PathBuf>, ConfigError> {
    let explicit = config_arg
        .map(|path| (path.to_path_buf(), "--config".to_owned()))
        .or_else(|| {
            let var = app.config_file_var();
            env.non_empty(&var).map(|path| (PathBuf::from(path), var))
        });
    if let Some((path, origin)) = explicit {
        return if fs.is_file(&path) {
            Ok(Some(path))
        } else {
            Err(ConfigError::NotFound { path, origin })
        };
    }
    Ok(candidate_paths(app, env)
        .into_iter()
        .find(|path| fs.is_file(path)))
}

/// Resolves the effective configuration.
///
/// # Errors
///
/// Returns a [`ConfigError`] naming the file, variable, or key at fault.
pub fn resolve(
    app: &AppId,
    config_arg: Option<&Path>,
    cli: &[CliValue],
    env: &dyn Env,
    fs: &dyn FileSystem,
) -> Result<Resolved, ConfigError> {
    let defaults = to_table(&Config::default())?;
    let mut effective = defaults.clone();
    let mut sources = BTreeMap::new();
    record_leaves(&defaults, "", &Source::Default, &mut sources);

    let file = select_file(app, config_arg, env, fs)?;
    if let Some(path) = &file {
        let text = fs.read_to_string(path).map_err(|error| ConfigError::Read {
            path: path.clone(),
            error,
        })?;
        let table: Table = toml::from_str(&text).map_err(|error| ConfigError::Parse {
            path: path.clone(),
            message: error.message().to_owned(),
        })?;
        merge(
            &mut effective,
            &table,
            "",
            &Source::File(path.clone()),
            &mut sources,
        );
    }

    for (var, key, value) in env_overrides(app, env, &defaults)? {
        set_path(&mut effective, &key, value);
        sources.insert(key.join("."), Source::Env(var));
    }

    for cli_value in cli {
        let key: Vec<String> = cli_value.key.split('.').map(str::to_owned).collect();
        set_path(&mut effective, &key, cli_value.value.clone());
        sources.insert(cli_value.key.clone(), Source::Cli(cli_value.flag.clone()));
    }

    let config = deserialize(&effective).map_err(|message| ConfigError::Invalid {
        context: file
            .as_ref()
            .map(|path| format!(" in {}", path.display()))
            .unwrap_or_default(),
        message,
    })?;
    config
        .validate()
        .map_err(|violation| ConfigError::InvalidValue {
            key: violation.key.to_owned(),
            origin: sources
                .get(violation.key)
                .cloned()
                .unwrap_or(Source::Default),
            message: violation.message,
        })?;

    Ok(Resolved {
        config,
        file,
        effective,
        sources,
    })
}

/// Parses and validates every `<PREFIX>__…` variable, in name order.
fn env_overrides(
    app: &AppId,
    env: &dyn Env,
    defaults: &Table,
) -> Result<Vec<(String, Vec<String>, Value)>, ConfigError> {
    let prefix = app.override_prefix();
    let mut overrides = Vec::new();
    for (var, raw) in env.vars() {
        let Some(rest) = var.strip_prefix(&prefix) else {
            continue;
        };
        let key: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
        if key.iter().any(String::is_empty) {
            return Err(ConfigError::UnknownOverride { var });
        }
        let Some(template) = lookup(defaults, &key) else {
            return Err(ConfigError::UnknownOverride { var });
        };
        let value = parse_override(&var, &raw, template)?;

        // Validate this override on its own so a schema error names the
        // variable rather than surfacing later without context.
        let mut probe = defaults.clone();
        set_path(&mut probe, &key, value.clone());
        if let Err(message) = deserialize(&probe) {
            return Err(ConfigError::InvalidOverride { var, message });
        }
        overrides.push((var, key, value));
    }
    Ok(overrides)
}

/// Converts a raw override string to a value of the same TOML type as the
/// key's default.
fn parse_override(var: &str, raw: &str, template: &Value) -> Result<Value, ConfigError> {
    if template.is_str() {
        return Ok(Value::String(raw.to_owned()));
    }
    if template.is_table() {
        return Err(ConfigError::InvalidOverride {
            var: var.to_owned(),
            message: "refers to a table; override its keys individually".to_owned(),
        });
    }
    if raw.trim().is_empty() {
        return Err(ConfigError::EmptyOverride {
            var: var.to_owned(),
        });
    }
    let invalid = |message: String| ConfigError::InvalidOverride {
        var: var.to_owned(),
        message,
    };
    let parsed = parse_toml_value(raw).map_err(invalid)?;
    match (template, parsed) {
        (Value::Float(_), Value::Integer(int)) => {
            #[expect(
                clippy::cast_precision_loss,
                reason = "integer literals for float keys follow TOML's own lossy promotion"
            )]
            let float = int as f64;
            Ok(Value::Float(float))
        }
        (template, parsed) if template.same_type(&parsed) => Ok(parsed),
        (template, parsed) => Err(invalid(format!(
            "expected {}, got {}",
            template.type_str(),
            parsed.type_str()
        ))),
    }
}

/// Parses a single TOML value such as `true`, `42` or `["a", "b"]`.
fn parse_toml_value(raw: &str) -> Result<Value, String> {
    let table: Table = toml::from_str(&format!("value = {raw}"))
        .map_err(|error| format!("not a valid TOML value: {}", error.message()))?;
    if table.len() != 1 {
        return Err("not a single TOML value".to_owned());
    }
    table
        .get("value")
        .cloned()
        .ok_or_else(|| "not a single TOML value".to_owned())
}

fn to_table(config: &Config) -> Result<Table, ConfigError> {
    Table::try_from(config).map_err(|error| ConfigError::Defaults(error.to_string()))
}

fn deserialize(table: &Table) -> Result<Config, String> {
    Value::Table(table.clone())
        .try_into()
        .map_err(|error: toml::de::Error| error.message().to_owned())
}

fn lookup<'a>(table: &'a Table, key: &[String]) -> Option<&'a Value> {
    let (first, rest) = key.split_first()?;
    let value = table.get(first)?;
    if rest.is_empty() {
        Some(value)
    } else {
        lookup(value.as_table()?, rest)
    }
}

fn set_path(table: &mut Table, key: &[String], value: Value) {
    let Some((first, rest)) = key.split_first() else {
        return;
    };
    if rest.is_empty() {
        table.insert(first.clone(), value);
        return;
    }
    let entry = table
        .entry(first.clone())
        .or_insert_with(|| Value::Table(Table::new()));
    if !entry.is_table() {
        *entry = Value::Table(Table::new());
    }
    if let Value::Table(child) = entry {
        set_path(child, rest, value);
    }
}

fn merge(
    base: &mut Table,
    overlay: &Table,
    prefix: &str,
    source: &Source,
    sources: &mut BTreeMap<String, Source>,
) {
    for (key, value) in overlay {
        let path = join(prefix, key);
        if let (Some(Value::Table(base_child)), Value::Table(overlay_child)) =
            (base.get_mut(key), value)
        {
            merge(base_child, overlay_child, &path, source, sources);
            continue;
        }
        base.insert(key.clone(), value.clone());
        if let Value::Table(child) = value {
            record_leaves(child, &path, source, sources);
        } else {
            sources.insert(path, source.clone());
        }
    }
}

fn record_leaves(
    table: &Table,
    prefix: &str,
    source: &Source,
    sources: &mut BTreeMap<String, Source>,
) {
    for (key, value) in table {
        let path = join(prefix, key);
        if let Value::Table(child) = value {
            record_leaves(child, &path, source, sources);
        } else {
            sources.insert(path, source.clone());
        }
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LogLevel;
    use crate::env::MapEnv;
    use crate::fs::MemFs;

    fn app() -> AppId {
        AppId::new("my-app")
    }

    fn resolve_with(env: &MapEnv, fs: &MemFs) -> Result<Resolved, ConfigError> {
        resolve(&app(), None, &[], env, fs)
    }

    // --- file selection -------------------------------------------------

    #[test]
    fn candidate_paths_follow_documented_order() {
        let env = MapEnv::new()
            .with("XDG_CONFIG_HOME", "/xdg")
            .with("HOME", "/home/u");
        assert_eq!(
            candidate_paths(&app(), &env),
            vec![
                PathBuf::from("/xdg/my-app/config.toml"),
                PathBuf::from("/home/u/.config/my-app/config.toml"),
                PathBuf::from("/etc/my-app/config.toml"),
                PathBuf::from("config.toml"),
            ]
        );
    }

    #[test]
    fn empty_or_relative_xdg_and_empty_home_are_skipped() {
        let env = MapEnv::new()
            .with("XDG_CONFIG_HOME", "relative")
            .with("HOME", "");
        assert_eq!(
            candidate_paths(&app(), &env),
            vec![
                PathBuf::from("/etc/my-app/config.toml"),
                PathBuf::from("config.toml"),
            ]
        );
        let env = MapEnv::new().with("XDG_CONFIG_HOME", "");
        assert_eq!(candidate_paths(&app(), &env).len(), 2);
    }

    #[test]
    fn config_arg_wins_over_everything() {
        let env = MapEnv::new()
            .with("MY_APP_CONFIG_FILE", "/from-env.toml")
            .with("HOME", "/home/u");
        let fs = MemFs::new()
            .with("/from-arg.toml", "")
            .with("/from-env.toml", "")
            .with("/home/u/.config/my-app/config.toml", "");
        let selected = select_file(&app(), Some(Path::new("/from-arg.toml")), &env, &fs);
        assert_eq!(selected.unwrap(), Some(PathBuf::from("/from-arg.toml")));
    }

    #[test]
    fn missing_config_arg_is_an_error() {
        let err = select_file(
            &app(),
            Some(Path::new("/nope.toml")),
            &MapEnv::new(),
            &MemFs::new(),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::NotFound { ref origin, .. } if origin == "--config"));
    }

    #[test]
    fn config_file_var_wins_over_lookup_and_must_exist() {
        let env = MapEnv::new()
            .with("MY_APP_CONFIG_FILE", "/from-env.toml")
            .with("HOME", "/home/u");
        let fs = MemFs::new()
            .with("/from-env.toml", "")
            .with("/home/u/.config/my-app/config.toml", "");
        assert_eq!(
            select_file(&app(), None, &env, &fs).unwrap(),
            Some(PathBuf::from("/from-env.toml"))
        );
        let err = select_file(&app(), None, &env, &MemFs::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::NotFound { ref origin, .. } if origin == "MY_APP_CONFIG_FILE")
        );
    }

    #[test]
    fn empty_config_file_var_is_ignored() {
        let env = MapEnv::new().with("MY_APP_CONFIG_FILE", "");
        assert_eq!(
            select_file(&app(), None, &env, &MemFs::new()).unwrap(),
            None
        );
    }

    #[test]
    fn first_existing_lookup_path_wins() {
        let env = MapEnv::new()
            .with("XDG_CONFIG_HOME", "/xdg")
            .with("HOME", "/home/u");
        let fs = MemFs::new()
            .with("/home/u/.config/my-app/config.toml", "")
            .with("/etc/my-app/config.toml", "")
            .with("config.toml", "");
        assert_eq!(
            select_file(&app(), None, &env, &fs).unwrap(),
            Some(PathBuf::from("/home/u/.config/my-app/config.toml"))
        );
        let fs = MemFs::new().with("config.toml", "");
        assert_eq!(
            select_file(&app(), None, &env, &fs).unwrap(),
            Some(PathBuf::from("config.toml"))
        );
    }

    // --- layering ---------------------------------------------------------

    #[test]
    fn no_file_yields_defaults() {
        let resolved = resolve_with(&MapEnv::new(), &MemFs::new()).unwrap();
        assert_eq!(resolved.config, Config::default());
        assert_eq!(resolved.file, None);
        assert!(resolved.sources.values().all(|s| *s == Source::Default));
        assert_eq!(resolved.sources.len(), 16);
    }

    #[test]
    fn file_values_override_defaults_and_files_are_not_merged() {
        let env = MapEnv::new().with("HOME", "/home/u");
        let fs = MemFs::new()
            .with(
                "/home/u/.config/my-app/config.toml",
                "[storage]\nhome = \"/srv/docs\"\n",
            )
            .with("config.toml", "[server]\nmax_document_bytes = 9\n");
        let resolved = resolve_with(&env, &fs).unwrap();
        assert_eq!(resolved.config.storage.home, PathBuf::from("/srv/docs"));
        assert_eq!(
            resolved.config.server.max_document_bytes, 1_048_576,
            "second file must be ignored"
        );
        assert_eq!(
            resolved.sources["storage.home"],
            Source::File(PathBuf::from("/home/u/.config/my-app/config.toml"))
        );
        assert_eq!(
            resolved.sources["server.max_document_bytes"],
            Source::Default
        );
    }

    #[test]
    fn env_overrides_beat_file_and_cli_beats_env() {
        let env = MapEnv::new()
            .with("MY_APP__LOG__LEVEL", "debug")
            .with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "3");
        let fs = MemFs::new().with("config.toml", "[log]\nlevel = \"warn\"\n");
        let cli = [CliValue {
            key: "log.level".into(),
            flag: "--log-level".into(),
            value: Value::String("trace".into()),
        }];
        let resolved = resolve(&app(), None, &cli, &env, &fs).unwrap();
        assert_eq!(resolved.config.log.level, LogLevel::Trace);
        assert_eq!(resolved.config.server.max_document_bytes, 3);
        assert_eq!(
            resolved.sources["log.level"],
            Source::Cli("--log-level".into())
        );
        assert_eq!(
            resolved.sources["server.max_document_bytes"],
            Source::Env("MY_APP__SERVER__MAX_DOCUMENT_BYTES".into())
        );
    }

    #[test]
    fn unknown_key_in_file_is_rejected_with_file_context() {
        let fs = MemFs::new().with("config.toml", "[server]\ncolour = \"red\"\n");
        let err = resolve_with(&MapEnv::new(), &fs).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("in config.toml"), "{text}");
        assert!(text.contains("colour"), "{text}");
    }

    #[test]
    fn invalid_toml_is_a_parse_error() {
        let fs = MemFs::new().with("config.toml", "this is = = not toml");
        let err = resolve_with(&MapEnv::new(), &fs).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn unreadable_file_is_a_read_error() {
        struct ListedButUnreadable;
        impl FileSystem for ListedButUnreadable {
            fn is_file(&self, _: &Path) -> bool {
                true
            }
            fn read_to_string(&self, _: &Path) -> std::io::Result<String> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "denied",
                ))
            }
        }
        let err = resolve(&app(), None, &[], &MapEnv::new(), &ListedButUnreadable).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    #[test]
    fn file_table_replacing_scalar_is_reported_as_invalid() {
        let fs = MemFs::new().with("config.toml", "server = 5\n");
        let err = resolve_with(&MapEnv::new(), &fs).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
    }

    // --- env override parsing ------------------------------------------------

    #[test]
    fn overrides_parse_each_type() {
        let env = MapEnv::new()
            .with("MY_APP__STORAGE__HOME", "/srv/docs ça va")
            .with("MY_APP__SERVER__BIND", "0.0.0.0:9000")
            .with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "2")
            .with("MY_APP__DOCUMENTS__TYPES", r#"["home", "plans"]"#);
        let resolved = resolve_with(&env, &MemFs::new()).unwrap();
        assert_eq!(
            resolved.config.storage.home,
            PathBuf::from("/srv/docs ça va")
        );
        assert_eq!(resolved.config.server.bind, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(resolved.config.server.max_document_bytes, 2);
        assert_eq!(resolved.config.documents.types, vec!["home", "plans"]);
    }

    #[test]
    fn index_file_is_on_by_default_and_a_boolean_override_turns_it_off() {
        let resolved = resolve_with(&MapEnv::new(), &MemFs::new()).unwrap();
        assert!(resolved.config.documents.index_file);
        let env = MapEnv::new().with("MY_APP__DOCUMENTS__INDEX_FILE", "false");
        let resolved = resolve_with(&env, &MemFs::new()).unwrap();
        assert!(!resolved.config.documents.index_file);
        let env = MapEnv::new().with("MY_APP__DOCUMENTS__INDEX_FILE", "no");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidOverride { var: ref v, .. } if v == "MY_APP__DOCUMENTS__INDEX_FILE"),
            "{err}"
        );
    }

    #[test]
    fn empty_string_override_is_allowed_for_string_keys() {
        let env = MapEnv::new().with("MY_APP__STORAGE__HOME", "");
        let resolved = resolve_with(&env, &MemFs::new()).unwrap();
        assert_eq!(resolved.config.storage.home, PathBuf::new());
    }

    #[test]
    fn override_variable_names_are_case_insensitive_after_the_prefix() {
        let env = MapEnv::new().with("MY_APP__server__Max_Document_Bytes", "4");
        let resolved = resolve_with(&env, &MemFs::new()).unwrap();
        assert_eq!(resolved.config.server.max_document_bytes, 4);
    }

    #[test]
    fn unparseable_bind_address_names_the_variable() {
        let env = MapEnv::new().with("MY_APP__SERVER__BIND", "localhost");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidOverride { var: ref v, .. } if v == "MY_APP__SERVER__BIND"),
            "{err}"
        );
    }

    #[test]
    fn rule_violations_name_the_key_and_its_source() {
        let env = MapEnv::new().with("MY_APP__DOCUMENTS__DEFAULT_TYPE", "memo");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid documents.default_type (from env MY_APP__DOCUMENTS__DEFAULT_TYPE): \"memo\" is not one of documents.types"
        );

        let fs = MemFs::new().with("config.toml", "[storage]\nhome = \"docs\"\n");
        let err = resolve_with(&MapEnv::new(), &fs).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid storage.home (from file config.toml): \"docs\" must be an absolute path"
        );
    }

    #[test]
    fn unrelated_variables_are_ignored() {
        let env = MapEnv::new()
            .with("MY_APP_CONFIG_FILE", "")
            .with("MY_APP_ADMIN_TOKEN", "secret")
            .with("OTHER__SERVER__MAX_DOCUMENT_BYTES", "x");
        assert!(resolve_with(&env, &MemFs::new()).is_ok());
    }

    #[test]
    fn unknown_override_is_rejected() {
        for var in [
            "MY_APP__SERVER__COLOUR",
            "MY_APP__NOPE",
            "MY_APP__",
            "MY_APP__SERVER____BIND",
        ] {
            let env = MapEnv::new().with(var, "1");
            let err = resolve_with(&env, &MemFs::new()).unwrap_err();
            assert!(
                matches!(err, ConfigError::UnknownOverride { var: ref v } if v == var),
                "{var}: {err}"
            );
        }
    }

    #[test]
    fn table_override_is_rejected() {
        let env = MapEnv::new().with("MY_APP__SERVER", "x");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(err.to_string().contains("refers to a table"), "{err}");
    }

    #[test]
    fn empty_override_for_non_string_is_rejected() {
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "  ");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyOverride { .. }), "{err}");
    }

    #[test]
    fn wrong_type_override_is_rejected() {
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "true");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid value for MY_APP__SERVER__MAX_DOCUMENT_BYTES: expected integer, got boolean"
        );
    }

    #[test]
    fn unparseable_override_is_rejected() {
        let env = MapEnv::new().with("MY_APP__DOCUMENTS__TYPES", "[unterminated");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(err.to_string().contains("not a valid TOML value"), "{err}");
    }

    #[test]
    fn override_cannot_smuggle_extra_keys() {
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "1\nother = 2");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(err.to_string().contains("not a single TOML value"), "{err}");
    }

    #[test]
    fn out_of_range_override_names_the_variable() {
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "-1");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidOverride { var: ref v, .. } if v == "MY_APP__SERVER__MAX_DOCUMENT_BYTES"),
            "{err}"
        );
    }

    #[test]
    fn invalid_enum_override_names_the_variable() {
        let env = MapEnv::new().with("MY_APP__LOG__LEVEL", "loud");
        let err = resolve_with(&env, &MemFs::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidOverride { var: ref v, .. } if v == "MY_APP__LOG__LEVEL"),
            "{err}"
        );
    }

    #[test]
    fn integer_promotes_to_float_template() {
        let value = parse_override("V", "3", &Value::Float(1.5)).unwrap();
        assert_eq!(value, Value::Float(3.0));
        let value = parse_override("V", "true", &Value::Boolean(false)).unwrap();
        assert_eq!(value, Value::Boolean(true));
    }

    #[test]
    fn set_path_replaces_scalars_with_tables_when_needed() {
        let mut table = Table::new();
        table.insert("a".into(), Value::Integer(1));
        set_path(&mut table, &["a".into(), "b".into()], Value::Integer(2));
        set_path(&mut table, &[], Value::Integer(3));
        assert_eq!(
            lookup(&table, &["a".into(), "b".into()]),
            Some(&Value::Integer(2))
        );
    }
}
