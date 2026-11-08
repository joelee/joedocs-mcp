//! Library crate holding all application logic of the `joedocs` binary.
//!
//! **This library is internal.** It is published only because the binary
//! is built from it; its API may change in any release. The stable
//! interfaces are the `joedocs` command line, its configuration, and the MCP
//! tools. See the [README](https://github.com/joelee/joedocs-mcp).
//!
//! `main.rs` is a thin wrapper: it parses arguments, builds a [`Context`]
//! from the real environment, filesystem, clock and random source, calls
//! [`load`] and [`execute`],
//! and maps any [`Error`] to an exit code. Everything here takes its
//! external dependencies as parameters, so it is unit-testable.

pub mod admin;
pub mod agents;
pub mod app_id;
pub mod auth;
pub mod backup;
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
pub mod status;
pub mod store;
pub mod systemd;
pub mod totp;

use std::io::{self, Write};
use std::sync::Arc;

pub use app_id::AppId;
pub use cli::{ArchiveCommand, Cli, Command, ConfigCommand, SystemdCommand};
pub use config::{Config, ConfigError, Resolved};

use crate::auth::RandomSource;
use crate::clock::Clock;
use crate::env::Env;
use crate::fs::FileSystem;
use crate::history::GitCli;
use crate::secrets::Secrets;
use crate::store::{Paths, Settings, Store};
use crate::systemd::ServiceManager;

/// Exit code for invalid requests, such as a bad name or an unknown project
/// (`EX_DATAERR` from `sysexits.h`).
pub const EXIT_DATA: u8 = 65;
/// Exit code when `check` finds problems.
pub const EXIT_PROBLEMS: u8 = 1;
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
    /// A systemd unit could not be installed or removed.
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
    /// A backup could not be written or restored.
    #[error(transparent)]
    Backup(#[from] backup::Error),
    /// The document is archived; the admin command can bring it back.
    #[error("document {0} is archived; run `joedocs archive restore {0}` to bring it back")]
    Archived(String),
    /// `check` found problems (already reported on standard output).
    #[error("check found {0} problems")]
    Problems(usize),
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
            Self::Store(error) if error.is_user_error() => EXIT_DATA,
            Self::Backup(error) if error.is_user_error() => EXIT_DATA,
            Self::Systemd(error) if error.is_user_error() => EXIT_DATA,
            Self::Archived(_) => EXIT_DATA,
            Self::Output(_)
            | Self::Systemd(systemd::Error::Output(_))
            | Self::Backup(backup::Error::Io { .. }) => EXIT_IO,
            Self::Store(_) | Self::Backup(_) => EXIT_SOFTWARE,
            Self::Serve { .. } | Self::Systemd(_) => EXIT_UNAVAILABLE,
            Self::Problems(_) => EXIT_PROBLEMS,
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
    /// systemd, for `joedocs systemd`.
    pub services: &'a dyn ServiceManager,
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
            let mut store = open_store(loaded, ctx)?;
            catch_up(&mut store)?;
            store.refresh_index_files();
            mcp::run(store, loaded.config(), loaded.resolved.file.as_deref(), out)?;
        }
        Command::Reindex => admin::reindex(&mut open_store(loaded, ctx)?, out)?,
        Command::Archive { command } => {
            admin::archive(&mut open_store(loaded, ctx)?, command, out)?;
        }
        Command::Check => admin::check(&open_store(loaded, ctx)?, out)?,
        Command::Systemd { command } => systemd_command(command, loaded, ctx, out)?,
        Command::Project { command } => {
            admin::project(&mut open_store(loaded, ctx)?, command, out)?;
        }
        Command::Key { command } => {
            admin::key(&mut open_store(loaded, ctx)?, command, ctx.random, out)?;
        }
        Command::AdminToken { command } => {
            admin::admin_token(&mut open_store(loaded, ctx)?, command, ctx.random, out)?;
        }
        Command::Agents => agents::run(loaded, ctx, out)?,
        Command::Backup {
            file,
            restore: false,
            ..
        } => {
            let store = open_store(loaded, ctx)?;
            let created = backup::create(&store, ctx.clock.as_ref(), file)?;
            admin::report_backup(file, &created, out)?;
        }
        Command::Backup {
            file,
            restore: true,
            state_dir,
            content_dir,
        } => restore(
            file,
            state_dir.as_deref(),
            content_dir.as_deref(),
            loaded,
            ctx,
            out,
        )?,
        Command::Status => {
            let store = open_store(loaded, ctx)?;
            let report = status::collect(&store, loaded, ctx.services)?;
            status::render(&report, out)?;
        }
        Command::Get {
            document_id,
            version,
            content_only,
            front_matter_only,
            file,
        } => {
            let part = if *content_only {
                admin::Part::Body
            } else if *front_matter_only {
                admin::Part::FrontMatter
            } else {
                admin::Part::Whole
            };
            let markdown = admin::fetch(&open_store(loaded, ctx)?, document_id, *version)?;
            admin::write_document(&markdown, part, file.as_deref(), out)?;
        }
    }
    Ok(())
}

/// Runs `joedocs backup --restore`: extracts the archive into empty or
/// absent directories (the configured ones by default), opens the restored
/// store, which migrates an older database, and checks it.
///
/// # Errors
///
/// Returns backup, store and output errors, and [`Error::Problems`] when the
/// restored store does not pass `check`.
pub fn restore(
    file: &std::path::Path,
    state_dir: Option<&std::path::Path>,
    content_dir: Option<&std::path::Path>,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let configured = Paths::from_config(loaded.config(), &ctx.app, ctx.env)?;
    let paths = Paths {
        home: content_dir.map_or_else(|| configured.home.clone(), std::path::Path::to_path_buf),
        state_dir: state_dir.map_or_else(
            || configured.state_dir.clone(),
            std::path::Path::to_path_buf,
        ),
    };
    let manifest = backup::extract(file, &paths.state_dir, &paths.home)?;
    let history = GitCli::new(paths.git_dir(), &paths.home);
    let store = Store::open(
        paths.clone(),
        Settings::from_config(loaded.config()),
        Box::new(history),
        Arc::clone(&ctx.clock),
    )?;
    admin::report_restore(&manifest, &paths, &configured, out)?;
    admin::check(&store, out)
}

/// Rebuilds the index when it is behind the history (an interrupted write,
/// or a database from an older build), logging what was found.
///
/// # Errors
///
/// Returns [`Error::Store`] when the rebuild fails.
pub fn catch_up(store: &mut Store) -> Result<(), Error> {
    if !store.needs_reindex()? {
        return Ok(());
    }
    tracing::info!("the index is behind the history; rebuilding it");
    let report = store.reindex()?;
    tracing::info!(
        documents = report.documents,
        archived = report.archived,
        revisions = report.revisions,
        "rebuilt the index"
    );
    for (name, key) in &report.projects_created {
        tracing::warn!(%name, %key, "recreated a project missing from the database");
    }
    for (id, path) in &report.drifted {
        tracing::warn!(%id, %path, "the vault file differs from the history; writes to it are refused");
    }
    Ok(())
}

/// Runs a `systemd` subcommand. `install` requires an initialised store, so
/// the unit cannot start a server that would fail at once.
///
/// # Errors
///
/// Returns [`Error::Systemd`], and [`Error::Store`] or [`Error::Config`]
/// when the store is not set up.
pub fn systemd_command(
    command: &SystemdCommand,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        SystemdCommand::Install {
            system,
            run_as,
            dry_run,
        } => {
            let scope = systemd::Scope::from_flag(*system);
            let run_as = scope.run_as(run_as.as_deref())?;
            let paths = open_store(loaded, ctx)?.paths().clone();
            let executable = ctx
                .services
                .executable()
                .map_err(|source| systemd::Error::Io {
                    action: "find",
                    path: "the joedocs executable".into(),
                    source,
                    hint: "",
                })?;
            let unit = systemd::Unit {
                scope,
                run_as,
                executable,
                environment: systemd::environment(&ctx.app, &loaded.resolved, &paths),
                paths,
            };
            systemd::install(
                &unit,
                &scope.unit_path(ctx.env)?,
                ctx.services,
                *dry_run,
                out,
            )?;
        }
        SystemdCommand::Remove { system, dry_run } => {
            let scope = systemd::Scope::from_flag(*system);
            systemd::remove(
                scope,
                &scope.unit_path(ctx.env)?,
                ctx.services,
                *dry_run,
                out,
            )?;
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
            services: &NoServices,
        }
    }

    /// A service manager that is never expected to run anything.
    struct NoServices;

    impl ServiceManager for NoServices {
        fn executable(&self) -> io::Result<std::path::PathBuf> {
            Ok("/usr/local/bin/joedocs".into())
        }
        fn systemctl(&self, _args: &[&str]) -> io::Result<systemd::Ran> {
            Err(io::Error::other("systemctl must not run in this test"))
        }
        fn host_name(&self) -> io::Result<String> {
            Ok("testhost".into())
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
    fn serve_first_catches_up_an_index_behind_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let env = storage_env(dir.path());
        let fs = MemFs::new();
        run(&["app", "init"], &env, &fs).unwrap();
        run(&["app", "project", "add", "p", "--key", "P"], &env, &fs).unwrap();
        let cli = Cli::try_parse_from(["app", "serve"]).unwrap();
        let ctx = context(&env, &fs);
        let loaded = load(&cli, &ctx).unwrap();
        let mut store = open_store(&loaded, &ctx).unwrap();
        store
            .create(
                &store::Actor::new("k", "c"),
                store::NewDocument {
                    project: "p".into(),
                    title: "T".into(),
                    content: "x".into(),
                    ..store::NewDocument::default()
                },
            )
            .unwrap();
        drop(store);
        std::fs::write(dir.path().join("home/p/home/T.md"), "edited").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let file = dir.path().join(format!("state/joedocs.db{suffix}"));
            std::fs::remove_file(file).unwrap_or_default();
        }
        run(&["app", "init"], &env, &fs).unwrap();

        let mut store = open_store(&loaded, &ctx).unwrap();
        assert!(store.needs_reindex().unwrap());
        catch_up(&mut store).unwrap();
        assert!(!store.needs_reindex().unwrap());
        assert_eq!(store.projects().unwrap()[0].name, "p");
        catch_up(&mut store).unwrap();
        let out = run(&["app", "reindex"], &env, &fs).unwrap();
        assert!(out.starts_with("Rebuilt the index"), "{out}");
    }

    #[test]
    fn systemd_install_pins_the_effective_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let env = storage_env(dir.path())
            .with("HOME", dir.path().join("user").to_str().unwrap())
            .with("MY_APP__SERVER__BIND", "127.0.0.1:9000");
        let fs = MemFs::new().with("/etc/my-app/config.toml", "[log]\nlevel = \"warn\"\n");
        let args = ["app", "systemd", "install", "--dry-run"];
        let err = run(&args, &env, &fs).unwrap_err();
        assert!(err.to_string().contains("run `joedocs init`"), "{err}");
        run(&["app", "init"], &env, &fs).unwrap();

        let out = run(
            &[
                "app",
                "--log-level",
                "debug",
                "systemd",
                "install",
                "--dry-run",
            ],
            &env,
            &fs,
        )
        .unwrap();
        let unit_path = dir.path().join("user/.config/systemd/user/joedocs.service");
        assert!(
            out.starts_with(&format!("Would write {}:", unit_path.display())),
            "{out}"
        );
        for line in [
            "ExecStart=\"/usr/local/bin/joedocs\" serve\n".to_owned(),
            "Environment=\"MY_APP_CONFIG_FILE=/etc/my-app/config.toml\"\n".to_owned(),
            "Environment=\"MY_APP__LOG__LEVEL=debug\"\n".to_owned(),
            "Environment=\"MY_APP__SERVER__BIND=127.0.0.1:9000\"\n".to_owned(),
            format!(
                "Environment=\"MY_APP__STORAGE__HOME={}\"\n",
                dir.path().join("home").display()
            ),
            format!(
                "Environment=\"MY_APP__STORAGE__STATE_DIR={}\"\n",
                dir.path().join("state").display()
            ),
            "Would run: systemctl --user enable joedocs.service\n".to_owned(),
        ] {
            assert!(out.contains(&line), "{line}: {out}");
        }
        assert!(!unit_path.exists());

        let out = run(
            &[
                "app",
                "systemd",
                "install",
                "--system",
                "--run-as",
                "joedocs",
                "--dry-run",
            ],
            &env,
            &fs,
        )
        .unwrap();
        assert!(
            out.starts_with("Would write /etc/systemd/system/joedocs.service:"),
            "{out}"
        );
        assert!(out.contains("User=joedocs\n"), "{out}");
        let err = run(&["app", "systemd", "install", "--system"], &env, &fs).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_DATA, "{err}");

        let out = run(&["app", "systemd", "remove"], &env, &fs).unwrap();
        assert!(out.contains("nothing to remove"), "{out}");
        let err = run(&["app", "systemd", "install"], &env, &fs).unwrap_err();
        assert_eq!(err.exit_code(), EXIT_UNAVAILABLE, "{err}");
        assert!(err.to_string().contains("systemctl must not run"), "{err}");
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
