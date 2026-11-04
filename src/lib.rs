//! Library crate holding all application logic.
//!
//! `main.rs` is a thin wrapper: it parses arguments, builds a [`Context`]
//! from the real environment, filesystem, clock and random source, calls
//! [`load`] and [`execute`],
//! and maps any [`Error`] to an exit code. Everything here takes its
//! external dependencies as parameters, so it is unit-testable.

pub mod admin;
pub mod app_id;
pub mod auth;
pub mod cli;
pub mod clock;
pub mod config;
pub mod db;
pub mod document;
pub mod env;
pub mod fs;
pub mod history;
pub mod mcp;
pub mod search;
pub mod secrets;
pub mod store;

use std::io::{self, Write};
use std::sync::Arc;

pub use app_id::AppId;
pub use cli::{Cli, Command, ConfigCommand};
pub use config::{Config, ConfigError, Resolved};

use crate::auth::RandomSource;
use crate::clock::Clock;
use crate::env::Env;
use crate::fs::FileSystem;
use crate::history::GitCli;
use crate::secrets::Secrets;
use crate::store::{Paths, Settings, Store};

/// Exit code for invalid requests, such as a bad name or an unknown project
/// (`EX_DATAERR` from `sysexits.h`).
pub const EXIT_DATA: u8 = 65;
/// Exit code for internal failures: database, history or vault errors
/// (`EX_SOFTWARE` from `sysexits.h`).
pub const EXIT_SOFTWARE: u8 = 70;
/// Exit code when the server cannot listen (`EX_UNAVAILABLE` from
/// `sysexits.h`).
pub const EXIT_UNAVAILABLE: u8 = 69;
/// Exit code for configuration errors (`EX_CONFIG` from `sysexits.h`).
pub const EXIT_CONFIG: u8 = 78;
/// Exit code for output errors (`EX_IOERR` from `sysexits.h`).
pub const EXIT_IO: u8 = 74;

/// Errors surfaced to the user.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Configuration could not be resolved.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Writing output failed.
    #[error("cannot write output: {0}")]
    Output(#[from] io::Error),
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] store::Error),
    /// The MCP server could not start or failed while running.
    #[error("cannot serve on {addr}: {source}")]
    Serve {
        /// The configured listen address.
        addr: std::net::SocketAddr,
        /// Cause.
        source: io::Error,
    },
}

impl Error {
    /// The process exit code for this error.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => EXIT_CONFIG,
            Self::Output(_) => EXIT_IO,
            Self::Store(error) if error.is_user_error() => EXIT_DATA,
            Self::Store(_) => EXIT_SOFTWARE,
            Self::Serve { .. } => EXIT_UNAVAILABLE,
        }
    }
}

/// External dependencies, injected so tests never touch the real process.
pub struct Context<'a> {
    /// Application identity (name and environment prefix).
    pub app: AppId,
    /// Environment variables.
    pub env: &'a dyn Env,
    /// Filesystem used for config lookup.
    pub fs: &'a dyn FileSystem,
    /// Wall-clock time.
    pub clock: Arc<dyn Clock>,
    /// Randomness for API keys.
    pub random: &'a dyn RandomSource,
}

/// Configuration and secrets resolved for one run.
#[derive(Debug)]
pub struct Loaded {
    /// The resolved configuration with its provenance.
    pub resolved: Resolved,
    /// Secrets read from the environment.
    pub secrets: Secrets,
}

impl Loaded {
    /// The typed configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.resolved.config
    }
}

/// Resolves configuration and secrets for `cli`.
///
/// # Errors
///
/// Returns [`Error::Config`] when any configuration layer is invalid.
pub fn load(cli: &Cli, ctx: &Context<'_>) -> Result<Loaded, Error> {
    let resolved = config::resolve(
        &ctx.app,
        cli.config.as_deref(),
        &cli.cli_values(),
        ctx.env,
        ctx.fs,
    )?;
    let secrets = Secrets::from_env(&ctx.app, ctx.env);
    Ok(Loaded { resolved, secrets })
}

/// Runs the selected command, writing normal output to `out`.
///
/// # Errors
///
/// Returns [`Error::Config`] when the storage paths are not configured,
/// [`Error::Store`] when a store operation fails, and [`Error::Output`] when
/// writing to `out` fails.
pub fn execute(
    cli: &Cli,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    tracing::debug!(command = ?cli.command, file = ?loaded.resolved.file, "running command");
    match &cli.command {
        Command::Config {
            command: ConfigCommand::Show,
        } => {
            let text =
                config::render_show(&ctx.app, &loaded.resolved, &loaded.secrets.status(&ctx.app));
            out.write_all(text.as_bytes())?;
        }
        Command::Config {
            command: ConfigCommand::Paths,
        } => {
            let candidates = config::candidate_paths(&ctx.app, ctx.env);
            let text = config::render_paths(&candidates, loaded.resolved.file.as_ref());
            out.write_all(text.as_bytes())?;
        }
        Command::Init => {
            let paths = Paths::from_config(loaded.config(), &ctx.app, ctx.env)?;
            let outcome = Store::init(&paths, &GitCli::new(paths.git_dir(), &paths.home))?;
            admin::report_init(&paths, outcome, out)?;
        }
        Command::Serve => {
            let store = open_store(loaded, ctx)?;
            mcp::run(store, loaded.config(), out)?;
        }
        Command::Project { command } => {
            admin::project(&mut open_store(loaded, ctx)?, command, out)?;
        }
        Command::Key { command } => {
            admin::key(&mut open_store(loaded, ctx)?, command, ctx.random, out)?;
        }
    }
    Ok(())
}

/// Opens the configured store, using `git` from `PATH`.
///
/// # Errors
///
/// Returns [`Error::Config`] when the storage paths are not configured and
/// [`Error::Store`] when the store is not initialised or cannot be opened.
pub fn open_store(loaded: &Loaded, ctx: &Context<'_>) -> Result<Store, Error> {
    let config = loaded.config();
    let paths = Paths::from_config(config, &ctx.app, ctx.env)?;
    let history = GitCli::new(paths.git_dir(), &paths.home);
    Ok(Store::open(
        paths,
        Settings::from_config(config),
        Box::new(history),
        Arc::clone(&ctx.clock),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CountingRandom;
    use crate::clock::FixedClock;
    use crate::env::MapEnv;
    use crate::fs::MemFs;
    use clap::Parser as _;

    fn context<'a>(env: &'a MapEnv, fs: &'a MemFs) -> Context<'a> {
        Context {
            app: AppId::new("my-app"),
            env,
            fs,
            clock: Arc::new(FixedClock::at(1_793_000_000)),
            random: &CountingRandom(0),
        }
    }

    fn run(args: &[&str], env: &MapEnv, fs: &MemFs) -> Result<String, Error> {
        let cli = Cli::try_parse_from(args).unwrap();
        let ctx = context(env, fs);
        let loaded = load(&cli, &ctx)?;
        let mut out = Vec::new();
        execute(&cli, &loaded, &ctx, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn config_show_honours_all_layers() {
        let env = MapEnv::new().with("MY_APP__SERVER__MAX_DOCUMENT_BYTES", "2");
        let fs = MemFs::new().with("config.toml", "[storage]\nhome = \"/srv/docs\"\n");
        let out = run(&["app", "--log-level", "warn", "config", "show"], &env, &fs).unwrap();
        assert!(
            out.contains("home = \"/srv/docs\"  # file config.toml"),
            "{out}"
        );
        assert!(
            out.contains("max_document_bytes = 2  # env MY_APP__SERVER__MAX_DOCUMENT_BYTES"),
            "{out}"
        );
        assert!(out.contains("level = \"warn\"  # cli --log-level"), "{out}");
    }

    #[test]
    fn config_show_and_paths_write_reports() {
        let env = MapEnv::new().with("HOME", "/home/u");
        let out = run(&["app", "config", "show"], &env, &MemFs::new()).unwrap();
        assert!(
            out.starts_with("# Effective configuration for my-app"),
            "{out}"
        );
        assert!(out.contains("# MY_APP_API_TOKEN: unset"), "{out}");

        let fs = MemFs::new().with("/etc/my-app/config.toml", "");
        let out = run(&["app", "config", "paths"], &env, &fs).unwrap();
        assert!(
            out.contains("/home/u/.config/my-app/config.toml\n"),
            "{out}"
        );
        assert!(
            out.contains("/etc/my-app/config.toml  <- selected"),
            "{out}"
        );
    }

    #[test]
    fn config_errors_map_to_ex_config() {
        let env = MapEnv::new().with("MY_APP__NOPE", "1");
        let err = run(&["app", "config", "show"], &env, &MemFs::new()).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_CONFIG);
        assert_eq!(
            err.to_string(),
            "unknown config override MY_APP__NOPE: no such key"
        );
    }

    #[test]
    fn output_errors_map_to_ex_ioerr() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let cli = Cli::try_parse_from(["app", "config", "show"]).unwrap();
        let env = MapEnv::new();
        let fs = MemFs::new();
        let ctx = context(&env, &fs);
        let loaded = load(&cli, &ctx).unwrap();
        let err = execute(&cli, &loaded, &ctx, &mut Broken).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_IO);
        assert!(err.to_string().starts_with("cannot write output"), "{err}");

        let cli = Cli::try_parse_from(["app", "config", "paths"]).unwrap();
        let err = execute(&cli, &loaded, &ctx, &mut Broken).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_IO);
    }

    #[test]
    fn loaded_exposes_config() {
        let cli = Cli::try_parse_from(["app", "config", "paths"]).unwrap();
        let env = MapEnv::new();
        let fs = MemFs::new();
        let ctx = context(&env, &fs);
        let loaded = load(&cli, &ctx).unwrap();
        assert_eq!(loaded.config(), &Config::default());
    }

    fn storage_env(dir: &std::path::Path) -> MapEnv {
        MapEnv::new()
            .with("MY_APP__STORAGE__HOME", dir.join("home").to_str().unwrap())
            .with(
                "MY_APP__STORAGE__STATE_DIR",
                dir.join("state").to_str().unwrap(),
            )
    }

    #[test]
    fn admin_commands_run_against_the_configured_store() {
        let dir = tempfile::tempdir().unwrap();
        let env = storage_env(dir.path());
        let fs = MemFs::new();
        let err = run(&["app", "project", "list"], &env, &fs).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_DATA, "{err}");
        assert!(err.to_string().contains("run `joedocs init`"), "{err}");

        let out = run(&["app", "init"], &env, &fs).unwrap();
        assert!(out.starts_with("Initialised JoeDocs.\n  vault: "), "{out}");
        assert!(
            out.ends_with("Next: joedocs project add <name> --key <KEY>\n"),
            "{out}"
        );
        let out = run(&["app", "init"], &env, &fs).unwrap();
        assert!(out.starts_with("Already initialised"), "{out}");

        let out = run(&["app", "project", "list"], &env, &fs).unwrap();
        assert!(out.starts_with("No projects."), "{out}");
        let out = run(
            &[
                "app", "project", "add", "joedocs", "--key", "JD", "--title", "JoeDocs",
            ],
            &env,
            &fs,
        )
        .unwrap();
        assert_eq!(out, "Added project joedocs (key JD): JoeDocs\n");
        let out = run(&["app", "project", "list"], &env, &fs).unwrap();
        assert_eq!(
            out,
            "NAME     KEY  DOCUMENTS  TITLE\njoedocs  JD   0          JoeDocs\n"
        );
        let err = run(
            &["app", "project", "add", "joedocs", "--key", "XX"],
            &env,
            &fs,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), EXIT_DATA);
    }

    #[test]
    fn key_commands_print_the_token_once_and_list_grants() {
        let dir = tempfile::tempdir().unwrap();
        let env = storage_env(dir.path());
        let fs = MemFs::new();
        run(&["app", "init"], &env, &fs).unwrap();
        run(
            &["app", "project", "add", "joedocs", "--key", "JD"],
            &env,
            &fs,
        )
        .unwrap();

        let out = run(&["app", "key", "list"], &env, &fs).unwrap();
        assert!(out.starts_with("No API keys."), "{out}");
        let out = run(
            &["app", "key", "create", "laptop", "--grant", "joedocs:read"],
            &env,
            &fs,
        )
        .unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(
            lines[0],
            "Created API key laptop (id 000102030405) with grants: joedocs:read"
        );
        assert!(lines[3].starts_with("jd_000102030405_000102"), "{out}");

        let out = run(
            &["app", "key", "grant", "laptop", "joedocs:write"],
            &env,
            &fs,
        )
        .unwrap();
        assert_eq!(out, "API key laptop grants: joedocs:write\n");
        let out = run(&["app", "key", "revoke", "laptop"], &env, &fs).unwrap();
        assert_eq!(
            out,
            "API key laptop is revoked (since 2026-10-26T07:33:20Z).\n"
        );
        let out = run(&["app", "key", "list"], &env, &fs).unwrap();
        assert_eq!(
            out,
            "NAME    ID            CREATED               LAST USED  STATUS   GRANTS\n\
             laptop  000102030405  2026-10-26T07:33:20Z  never      revoked  joedocs:write\n"
        );
    }

    #[test]
    fn init_needs_storage_home() {
        let err = run(&["app", "init"], &MapEnv::new(), &MemFs::new()).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_CONFIG);
        assert!(
            err.to_string().starts_with("storage.home is not set"),
            "{err}"
        );
    }

    #[test]
    fn store_faults_map_to_ex_software() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let env = MapEnv::new()
            .with(
                "MY_APP__STORAGE__HOME",
                dir.path().join("home").to_str().unwrap(),
            )
            .with(
                "MY_APP__STORAGE__STATE_DIR",
                file.join("state").to_str().unwrap(),
            );
        let err = run(&["app", "init"], &env, &MemFs::new()).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_SOFTWARE, "{err}");
    }
}
