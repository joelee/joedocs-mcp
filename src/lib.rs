//! Library crate holding all application logic.
//!
//! `main.rs` is a thin wrapper: it parses arguments, builds a [`Context`]
//! from the real environment and filesystem, calls [`load`] and [`execute`],
//! and maps any [`Error`] to an exit code. Everything here takes its
//! external dependencies as parameters, so it is unit-testable.

pub mod admin;
pub mod app_id;
pub mod auth;
pub mod cli;
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

pub use app_id::AppId;
pub use cli::{Cli, Command, ConfigCommand};
pub use config::{Config, ConfigError, Resolved};

use crate::env::Env;
use crate::fs::FileSystem;
use crate::secrets::Secrets;

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
}

impl Error {
    /// The process exit code for this error.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => EXIT_CONFIG,
            Self::Output(_) => EXIT_IO,
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
/// Returns [`Error::Output`] when writing to `out` fails.
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::fs::MemFs;
    use clap::Parser as _;

    fn run(args: &[&str], env: &MapEnv, fs: &MemFs) -> Result<String, Error> {
        let cli = Cli::try_parse_from(args).unwrap();
        let ctx = Context {
            app: AppId::new("my-app"),
            env,
            fs,
        };
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
        let ctx = Context {
            app: AppId::new("my-app"),
            env: &env,
            fs: &fs,
        };
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
        let ctx = Context {
            app: AppId::new("my-app"),
            env: &env,
            fs: &fs,
        };
        let loaded = load(&cli, &ctx).unwrap();
        assert_eq!(loaded.config(), &Config::default());
    }
}
