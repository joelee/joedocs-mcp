//! Non-secret configuration: the schema, its defaults, and resolution.
//!
//! Effective configuration is layered, highest precedence first: CLI flags,
//! per-key environment overrides, the selected config file, and built-in
//! defaults. See `docs/configuration.md` for the full rules.

mod resolve;
mod save;
mod show;
mod validate;

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use resolve::{CliValue, Resolved, candidate_paths, resolve, select_file};
pub use save::{client_file, set_client_server};
pub use show::{render_paths, render_show};
pub use validate::Violation;

use crate::app_id::AppId;
use crate::env::Env;

/// The complete non-secret configuration.
///
/// Every field must have a default so the application runs without a config
/// file, and so every key has a known type for environment overrides. Avoid
/// `Option` fields: a `None` default has no type to parse overrides against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Logging settings (`[log]`).
    pub log: LogConfig,
    /// MCP server settings (`[server]`).
    pub server: ServerConfig,
    /// Where documents and server state live (`[storage]`).
    pub storage: StorageConfig,
    /// Allowed document types and statuses (`[documents]`).
    pub documents: DocumentsConfig,
    /// The remote admin API (`[admin]`).
    pub admin: AdminConfig,
    /// This machine as a client of a remote server (`[client]`).
    pub client: ClientConfig,
}

/// Logging settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogConfig {
    /// Minimum level written to stderr.
    pub level: LogLevel,
}

/// Log verbosity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational messages and above.
    #[default]
    Info,
    /// Debug messages and above.
    Debug,
    /// Everything.
    Trace,
}

impl LogLevel {
    /// The equivalent `tracing` filter.
    #[must_use]
    pub fn as_filter(self) -> tracing::level_filters::LevelFilter {
        use tracing::level_filters::LevelFilter;
        match self {
            Self::Error => LevelFilter::ERROR,
            Self::Warn => LevelFilter::WARN,
            Self::Info => LevelFilter::INFO,
            Self::Debug => LevelFilter::DEBUG,
            Self::Trace => LevelFilter::TRACE,
        }
    }

    /// The lowercase name used in config files and on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Default port for the MCP server.
pub const DEFAULT_PORT: u16 = 8750;

/// MCP server settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Address and port the HTTP server listens on.
    pub bind: SocketAddr,
    /// `Host` header values to accept (DNS-rebinding protection), as
    /// `host` or `host:port`. Add the public name when behind a proxy.
    pub allowed_hosts: Vec<String>,
    /// `Origin` header values to accept, such as
    /// `https://docs.example.net`. Requests without an `Origin` are allowed;
    /// any other `Origin` is refused.
    pub allowed_origins: Vec<String>,
    /// Largest accepted document (front matter and body), in bytes.
    pub max_document_bytes: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_PORT)),
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allowed_origins: Vec::new(),
            max_document_bytes: 1024 * 1024,
        }
    }
}

/// Where documents and server state live. Empty paths mean "not set".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageConfig {
    /// The content directory (`$JOEDOCS_HOME`): `<project>/<type>/*.md`.
    pub home: PathBuf,
    /// The state directory holding the database and the history git
    /// directory. Empty means the platform data directory (see
    /// [`StorageConfig::resolve_state_dir`]).
    pub state_dir: PathBuf,
}

impl StorageConfig {
    /// The content directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Missing`] when `storage.home` is not set.
    pub fn require_home(&self, app: &AppId) -> Result<&Path, ConfigError> {
        if self.home.as_os_str().is_empty() {
            return Err(ConfigError::Missing {
                key: "storage.home".to_owned(),
                hint: format!(
                    "set it in config.toml or with {}STORAGE__HOME",
                    app.override_prefix()
                ),
            });
        }
        Ok(&self.home)
    }

    /// The state directory: `storage.state_dir` when set, otherwise
    /// `$XDG_DATA_HOME/<app>` (only when `XDG_DATA_HOME` is absolute), otherwise
    /// `$HOME/.local/share/<app>`, which is also used on macOS.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Missing`] when it is not set and neither
    /// variable gives a usable base directory.
    pub fn resolve_state_dir(&self, app: &AppId, env: &dyn Env) -> Result<PathBuf, ConfigError> {
        if !self.state_dir.as_os_str().is_empty() {
            return Ok(self.state_dir.clone());
        }
        if let Some(xdg) = env.non_empty("XDG_DATA_HOME") {
            let xdg = PathBuf::from(xdg);
            if xdg.is_absolute() {
                return Ok(xdg.join(app.name()));
            }
        }
        if let Some(home) = env.non_empty("HOME") {
            return Ok(PathBuf::from(home)
                .join(".local")
                .join("share")
                .join(app.name()));
        }
        Err(ConfigError::Missing {
            key: "storage.state_dir".to_owned(),
            hint: format!(
                "neither XDG_DATA_HOME nor HOME is set; set it in config.toml or with {}STORAGE__STATE_DIR",
                app.override_prefix()
            ),
        })
    }
}

/// Allowed document types and statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DocumentsConfig {
    /// Document types; each is a folder under every project.
    pub types: Vec<String>,
    /// Allowed values of the `status` front matter key.
    pub statuses: Vec<String>,
    /// Type used when a new document names none; must be in `types`.
    pub default_type: String,
    /// Status used when a new document names none; must be in `statuses`.
    pub default_status: String,
    /// Whether to write `INDEX.md`, a table of the live documents, in
    /// each project folder.
    pub index_file: bool,
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
        Self {
            types: owned(&[
                "home", "plans", "ideas", "issues", "reviews", "research", "specs", "notes",
            ]),
            statuses: owned(&[
                "draft",
                "review",
                "approved",
                "rejected",
                "superseded",
                "done",
            ]),
            default_type: "home".to_owned(),
            default_status: "draft".to_owned(),
            index_file: true,
        }
    }
}

/// The remote admin API, served by `joedocs serve` under `/admin/v1/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdminConfig {
    /// Whether to serve the admin API. Off by default.
    pub enabled: bool,
    /// How long a session opened by `joedocs login` lasts, in hours (1 to
    /// 168).
    pub session_hours: u32,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            session_hours: 8,
        }
    }
}

/// This machine as a client of a server's admin API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClientConfig {
    /// The server's base URL, e.g. `https://docs.example.ts.net`. When set,
    /// admin commands run on that server instead of the local store. Empty
    /// means local.
    pub server: String,
}

/// Where a configuration value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The built-in default.
    Default,
    /// The selected config file.
    File(PathBuf),
    /// A per-key environment override, named by its variable.
    Env(String),
    /// A command-line flag.
    Cli(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File(path) => write!(f, "file {}", path.display()),
            Self::Env(var) => write!(f, "env {var}"),
            Self::Cli(flag) => write!(f, "cli {flag}"),
        }
    }
}

/// Errors raised while resolving configuration. Each names the source at
/// fault so the user can fix it.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A config file named explicitly does not exist.
    #[error("config file {} (from {origin}) does not exist", path.display())]
    NotFound {
        /// The missing path.
        path: PathBuf,
        /// What named it: `--config` or the `<PREFIX>_CONFIG_FILE` variable.
        origin: String,
    },
    /// The selected config file could not be read.
    #[error("cannot read config file {}: {error}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// The selected config file is not valid TOML.
    #[error("cannot parse config file {}: {message}", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },
    /// The merged configuration does not match the schema.
    #[error("invalid configuration{context}: {message}")]
    Invalid {
        /// Optional ` in <file>` suffix.
        context: String,
        /// The schema error.
        message: String,
    },
    /// An override variable does not correspond to a config key.
    #[error("unknown config override {var}: no such key")]
    UnknownOverride {
        /// The variable name.
        var: String,
    },
    /// An override variable's value cannot be used for its key.
    #[error("invalid value for {var}: {message}")]
    InvalidOverride {
        /// The variable name.
        var: String,
        /// Why the value was rejected.
        message: String,
    },
    /// An override for a non-string key is empty.
    #[error("{var} is empty; only string keys accept an empty value")]
    EmptyOverride {
        /// The variable name.
        var: String,
    },
    /// A value has the right type but breaks a rule, such as a default type
    /// that is not in the list of types.
    #[error("invalid {key} (from {origin}): {message}")]
    InvalidValue {
        /// Dotted key at fault, e.g. `documents.default_type`.
        key: String,
        /// The layer that supplied the value.
        origin: Source,
        /// The rule that was broken.
        message: String,
    },
    /// A key that a command needs has no value.
    #[error("{key} is not set: {hint}")]
    Missing {
        /// Dotted key, e.g. `storage.home`.
        key: String,
        /// How to set it.
        hint: String,
    },
    /// The built-in defaults could not be serialised (a bug in the schema).
    #[error("internal error: cannot serialise default configuration: {0}")]
    Defaults(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::env::MapEnv;

    #[test]
    fn defaults_are_deterministic_and_valid() {
        let config = Config::default();
        assert_eq!(config.log.level, LogLevel::Info);
        assert_eq!(config.server.bind, "127.0.0.1:8750".parse().unwrap());
        assert_eq!(config.server.allowed_origins, Vec::<String>::new());
        assert_eq!(config.server.max_document_bytes, 1_048_576);
        assert_eq!(config.storage, StorageConfig::default());
        assert_eq!(config.documents.types.len(), 8);
        assert_eq!(config.documents.default_type, "home");
        assert_eq!(config.documents.default_status, "draft");
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn require_home_errors_until_set() {
        let app = AppId::new("my-app");
        let err = StorageConfig::default().require_home(&app).unwrap_err();
        assert_eq!(
            err.to_string(),
            "storage.home is not set: set it in config.toml or with MY_APP__STORAGE__HOME"
        );
        let storage = StorageConfig {
            home: PathBuf::from("/srv/docs"),
            ..StorageConfig::default()
        };
        assert_eq!(storage.require_home(&app).unwrap(), Path::new("/srv/docs"));
    }

    #[test]
    fn state_dir_prefers_config_then_xdg_then_home() {
        let app = AppId::new("my-app");
        let env = MapEnv::new()
            .with("XDG_DATA_HOME", "/xdg")
            .with("HOME", "/home/u");
        let explicit = StorageConfig {
            state_dir: PathBuf::from("/var/lib/my-app"),
            ..StorageConfig::default()
        };
        assert_eq!(
            explicit.resolve_state_dir(&app, &env).unwrap(),
            PathBuf::from("/var/lib/my-app")
        );

        let unset = StorageConfig::default();
        assert_eq!(
            unset.resolve_state_dir(&app, &env).unwrap(),
            PathBuf::from("/xdg/my-app")
        );
        let relative_xdg = MapEnv::new()
            .with("XDG_DATA_HOME", "relative")
            .with("HOME", "/home/u");
        assert_eq!(
            unset.resolve_state_dir(&app, &relative_xdg).unwrap(),
            PathBuf::from("/home/u/.local/share/my-app")
        );
        let err = unset.resolve_state_dir(&app, &MapEnv::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::Missing { ref key, .. } if key == "storage.state_dir"),
            "{err}"
        );
    }

    #[test]
    fn log_levels_map_to_filters_and_names() {
        use tracing::level_filters::LevelFilter;
        let cases = [
            (LogLevel::Error, LevelFilter::ERROR, "error"),
            (LogLevel::Warn, LevelFilter::WARN, "warn"),
            (LogLevel::Info, LevelFilter::INFO, "info"),
            (LogLevel::Debug, LevelFilter::DEBUG, "debug"),
            (LogLevel::Trace, LevelFilter::TRACE, "trace"),
        ];
        for (level, filter, name) in cases {
            assert_eq!(level.as_filter(), filter);
            assert_eq!(level.as_str(), name);
        }
    }

    #[test]
    fn sources_display_their_origin() {
        assert_eq!(Source::Default.to_string(), "default");
        assert_eq!(
            Source::File(PathBuf::from("/etc/a.toml")).to_string(),
            "file /etc/a.toml"
        );
        assert_eq!(Source::Env("A__B".into()).to_string(), "env A__B");
        assert_eq!(Source::Cli("--x".into()).to_string(), "cli --x");
    }

    #[test]
    fn errors_name_the_source_at_fault() {
        let err = ConfigError::NotFound {
            path: PathBuf::from("/missing.toml"),
            origin: "--config".into(),
        };
        assert_eq!(
            err.to_string(),
            "config file /missing.toml (from --config) does not exist"
        );
        let err = ConfigError::EmptyOverride { var: "A__B".into() };
        assert_eq!(
            err.to_string(),
            "A__B is empty; only string keys accept an empty value"
        );
        let err = ConfigError::InvalidValue {
            key: "documents.default_type".into(),
            origin: Source::Env("A__DOCUMENTS__DEFAULT_TYPE".into()),
            message: "\"x\" is not one of documents.types".into(),
        };
        assert_eq!(
            err.to_string(),
            "invalid documents.default_type (from env A__DOCUMENTS__DEFAULT_TYPE): \"x\" is not one of documents.types"
        );
        let err = ConfigError::Defaults("boom".into());
        assert!(err.to_string().contains("boom"));
    }
}
