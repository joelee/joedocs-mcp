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
pub mod browse;
pub mod cli;
pub mod client;
pub mod clock;
pub mod config;
pub mod db;
pub mod document;
pub mod env;
pub mod fs;
pub mod history;
pub mod mcp;
pub mod password;
pub mod search;
pub mod secrets;
pub mod status;
pub mod store;
pub mod systemd;
pub mod totp;
pub mod web;

use std::io::{self, Write};
use std::sync::Arc;

pub use app_id::AppId;
pub use cli::{
    ArchiveCommand, AuthCommand, Cli, Command, ConfigCommand, DevicesCommand, DocumentCommand,
    KeyCommand, ProjectCommand, SystemdCommand, UserCommand,
};
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
    /// Writing the file the user named (`get -f`, `get --archive`) failed.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The path as given.
        path: std::path::PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] store::Error),
    /// A systemd unit could not be installed or removed.
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
    /// A call to a remote server's admin API failed.
    #[error(transparent)]
    Client(#[from] client::Error),
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
            Self::Client(error) => error.exit_code(),
            Self::Store(error) if error.is_user_error() => EXIT_DATA,
            Self::Backup(error) if error.is_user_error() => EXIT_DATA,
            Self::Systemd(error) if error.is_user_error() => EXIT_DATA,
            Self::Archived(_) => EXIT_DATA,
            Self::Output(_)
            | Self::Write { .. }
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
    /// Asks for a TOTP code when a remote session has expired.
    pub prompt: &'a dyn client::Prompt,
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

/// Whether logs written to stderr may use ANSI colour: only when stderr is
/// a terminal and `NO_COLOR` is unset or empty (<https://no-color.org>), so
/// the journal, `brew services` logs and redirected output stay plain.
#[must_use]
pub fn log_colour(stderr_is_terminal: bool, env: &dyn Env) -> bool {
    stderr_is_terminal && env.non_empty("NO_COLOR").is_none()
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
    // The name only: arguments can hold a TOTP code (`login --code`).
    tracing::debug!(command = cli.command.name(), file = ?loaded.resolved.file, "running command");
    if let Some(done) = remote(cli, loaded, ctx, out) {
        return done;
    }
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
        Command::Document { command } => {
            admin::document(&mut open_store(loaded, ctx)?, command, out)?;
        }
        Command::Check => admin::check(&open_store(loaded, ctx)?, out)?,
        Command::Systemd { command } => systemd_command(command, loaded, ctx, out)?,
        Command::Project { command } => {
            admin::project(&mut open_store(loaded, ctx)?, command, out)?;
        }
        Command::Key { command } => {
            admin::key(&mut open_store(loaded, ctx)?, command, ctx.random, out)?;
        }
        Command::User { command } => {
            admin::user(&mut open_store(loaded, ctx)?, command, ctx.random, out)?;
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
            let created = backup::create(&store, file)?;
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
        Command::Login { .. } | Command::Logout | Command::Auth { .. } => {}
        command @ Command::Get { .. } => {
            if let Some((request, file)) = GetRequest::of(command) {
                local_get(&open_store(loaded, ctx)?, &request, file, out)?;
            }
        }
        command @ (Command::List(_)
        | Command::Search(_)
        | Command::History(_)
        | Command::Diff(_)
        | Command::Audit(_)) => local_browse(command, loaded, ctx, out)?,
    }
    Ok(())
}

/// The commands that view and find documents, on the local store.
fn local_browse(
    command: &Command,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let store = open_store(loaded, ctx)?;
    match command {
        Command::List(args) => {
            let reply = browse::list(&store, args)?;
            browse::print(&reply, args.json, browse::render_list, out)
        }
        Command::Search(args) => {
            let spec = browse::SearchSpec::from_args(args)?;
            let reply = browse::search(&store, &spec)?;
            browse::print(&reply, args.json, browse::render_search, out)
        }
        Command::History(args) => {
            let reply = browse::history(&store, args)?;
            browse::print(&reply, args.json, browse::render_history, out)
        }
        Command::Diff(args) => {
            let reply = browse::diff(&store, args)?;
            browse::print(&reply, args.json, browse::render_diff, out)
        }
        Command::Audit(args) => {
            let reply = browse::audit(&store, args)?;
            browse::print(&reply, args.json, browse::render_audit, out)
        }
        _ => Ok(()),
    }
}

/// Runs `cli` against the admin API of `client.server`, or returns `None`
/// when the command runs locally.
fn remote(
    cli: &Cli,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Option<Result<(), Error>> {
    let server = loaded.config().client.server.as_str();
    if server.is_empty() {
        let signs_in = matches!(
            cli.command,
            Command::Login { .. }
                | Command::Auth {
                    command: AuthCommand::Login { .. }
                }
        );
        if signs_in && ctx.prompt.interactive() {
            return Some(
                set_up_server(loaded, ctx, out)
                    .and_then(|server| run_remote(cli, &server, loaded, ctx, out)),
            );
        }
        return matches!(
            cli.command,
            Command::Login { .. } | Command::Logout | Command::Auth { .. }
        )
        .then(|| {
            Err(client::Error::Usage(
                "set client.server, or pass --server URL, to log in to a server's admin API".into(),
            )
            .into())
        });
    }
    let local_only = match &cli.command {
        Command::Config { .. } => return None,
        Command::Init => Some("init"),
        Command::Serve => Some("serve"),
        Command::Systemd { .. } => Some("systemd"),
        Command::AdminToken { .. } => Some("admin-token"),
        Command::User {
            command: UserCommand::Remove { .. },
        } => Some("user remove"),
        Command::Agents => Some("agents"),
        Command::Backup { restore: true, .. } => Some("backup --restore"),
        _ => None,
    };
    if let Some(name) = local_only {
        return Some(Err(client::Error::Usage(format!(
            "`joedocs {name}` runs only on the server; drop --server (or clear client.server) to run it here"
        ))
        .into()));
    }
    Some(run_remote(cli, server, loaded, ctx, out))
}

/// `joedocs login` at a terminal with no server: asks for one, checks it,
/// and saves it as `client.server` in the config file in use (or the
/// user's own config file).
fn set_up_server(loaded: &Loaded, ctx: &Context<'_>, out: &mut dyn Write) -> Result<String, Error> {
    let target = config::client_file(loaded.resolved.file.as_deref(), &ctx.app, ctx.env)
        .ok_or_else(|| {
            client::Error::Usage(
                "set HOME or XDG_CONFIG_HOME so `joedocs login` can save client.server, or pass --server URL".into(),
            )
        })?;
    let server = client::ask_server(ctx.prompt)?;
    config::set_client_server(&target, &server).map_err(|source| client::Error::Io {
        action: "save client.server in",
        path: target.clone(),
        source,
    })?;
    writeln!(out, "Saved client.server in {}.", target.display())?;
    Ok(server)
}

fn run_remote(
    cli: &Cli,
    server: &str,
    loaded: &Loaded,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let remote = remote_for(cli, server, loaded, ctx)?;
    match &cli.command {
        Command::Auth { command } => remote_auth(&remote, command, ctx, out)?,
        Command::User { command } => remote_user(&remote, command, out)?,
        Command::Login { code } => {
            let (name, saved) = remote.login(code.as_deref())?;
            writeln!(
                out,
                "Logged in to {} as {name}; the session expires at {}.",
                remote.server(),
                saved.expires_at
            )?;
        }
        Command::Logout => {
            if remote.logout()? {
                writeln!(out, "Logged out of {}.", remote.server())?;
            } else {
                writeln!(out, "Not logged in to {}.", remote.server())?;
            }
        }
        Command::Status => {
            let reply = remote.call(&reqwest::Method::GET, "status", None)?;
            let report: status::StatusReport = remote.parse(&reply, "status report")?;
            status::render(&report, out)?;
            let expires = remote
                .saved()?
                .map_or_else(|| "unknown".to_owned(), |s| s.expires_at);
            writeln!(
                out,
                "\nClient\n  server: {}\n  session expires: {expires}",
                remote.server()
            )?;
        }
        command => remote_admin(&remote, command, out)?,
    }
    Ok(())
}

/// The connection for `cli`'s remote command, proving who it is as P-05 of
/// JD-37 says: `login` and `logout` use the admin token; `auth` and every
/// other command use the user `client.user` (or `--user`) when set, else
/// `JOEDOCS_ADMIN_TOKEN` when set, else the only user with a device stored
/// for this server.
fn remote_for<'a>(
    cli: &Cli,
    server: &str,
    loaded: &Loaded,
    ctx: &Context<'a>,
) -> Result<client::Remote<'a>, Error> {
    let token = loaded.secrets.admin_token.as_ref();
    // One spelling of the server for the device file and the calls alike.
    let server = server.trim_end_matches('/');
    if matches!(cli.command, Command::Login { .. } | Command::Logout) {
        return Ok(client::Remote::new(server, token, ctx.env, ctx.prompt)?);
    }
    let configured = loaded.config().client.user.as_str();
    let is_auth = matches!(cli.command, Command::Auth { .. });
    let user = if !configured.is_empty() {
        Some(configured.to_owned())
    } else if token.is_some() && !is_auth {
        // The admin token wins without a user, so the device file is not
        // even read.
        None
    } else {
        let stored = client::Devices::locate(ctx.env)?.users(server)?;
        match (stored.as_slice(), is_auth) {
            ([only], _) => Some(only.clone()),
            (_, true) => {
                return Err(client::Error::Usage(
                    "pass --user NAME (or set client.user) to sign in as that user".into(),
                )
                .into());
            }
            _ => {
                return Err(client::Error::Auth(format!(
                    "sign in to {server} first with `joedocs auth login --user NAME` (or export JOEDOCS_ADMIN_TOKEN, deprecated)"
                ))
                .into());
            }
        }
    };
    Ok(match user {
        Some(user) => client::Remote::for_user(server, &user, ctx.env, ctx.prompt)?,
        None => client::Remote::new(server, token, ctx.env, ctx.prompt)?,
    })
}

/// `joedocs auth ...` on the server of `remote`.
fn remote_auth(
    remote: &client::Remote<'_>,
    command: &AuthCommand,
    ctx: &Context<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    use reqwest::Method;
    match command {
        AuthCommand::Login { code, device } => {
            let device = device
                .clone()
                .or_else(|| ctx.services.host_name().ok())
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "device".to_owned());
            let signed = remote.auth_login(code.as_deref(), &device)?;
            writeln!(
                out,
                "Signed in to {} as {} on the device {}; the session expires at {}.",
                remote.server(),
                signed.user,
                signed.device,
                signed.expires_at
            )?;
        }
        AuthCommand::Logout { forget_device } => {
            let ended = remote.auth_logout(*forget_device)?;
            let what = match (ended, forget_device) {
                (false, _) => "Not signed in",
                (true, true) => "Signed out and forgot this device on",
                (true, false) => "Signed out of",
            };
            writeln!(out, "{what} {}.", remote.server())?;
        }
        AuthCommand::Status => {
            let reply = remote.call(&Method::GET, "me", None)?;
            let me: client::MeReply = remote.parse(&reply, "account reply")?;
            let text = |value: Option<String>| value.unwrap_or_else(|| "-".to_owned());
            writeln!(
                out,
                "Signed in to {} as {}\n  device: {}\n  session expires: {}\n  authenticator set up: {}\n  last sign-in: {}",
                remote.server(),
                me.user,
                text(me.device),
                me.expires_at,
                text(me.totp_enrolled_at),
                text(me.last_login_at),
            )?;
        }
        AuthCommand::Password => {
            let output = remote.change_password()?;
            write!(out, "{output}")?;
        }
        AuthCommand::Devices { command: None } => {
            remote_text(remote, &Method::GET, "me/devices", None, out)?;
        }
        AuthCommand::Devices {
            command: Some(DevicesCommand::Revoke { name }),
        } => {
            let output = remote.revoke_device(name)?;
            write!(out, "{output}")?;
        }
    }
    Ok(())
}

/// `joedocs user ...` on the server of `remote` (all but `remove`).
fn remote_user(
    remote: &client::Remote<'_>,
    command: &UserCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    use reqwest::Method;
    use serde_json::json;
    let action = |name: &str, action: &str| format!("users/{}/{action}", percent_encode(name));
    match command {
        UserCommand::List => remote_text(remote, &Method::GET, "users", None, out),
        UserCommand::Create { name } => {
            let body = json!({"name": name});
            remote_text(remote, &Method::POST, "users", Some(&body), out)
        }
        UserCommand::Disable { name } => {
            remote_text(remote, &Method::POST, &action(name, "disable"), None, out)
        }
        UserCommand::Enable { name } => {
            remote_text(remote, &Method::POST, &action(name, "enable"), None, out)
        }
        UserCommand::Unlock { name } => {
            remote_text(remote, &Method::POST, &action(name, "unlock"), None, out)
        }
        UserCommand::ResetPassword { name } => remote_text(
            remote,
            &Method::POST,
            &action(name, "reset-password"),
            None,
            out,
        ),
        UserCommand::ResetTotp { name } => remote_text(
            remote,
            &Method::POST,
            &action(name, "reset-totp"),
            None,
            out,
        ),
        // Refused by `remote` before it gets here: it runs on the server only.
        UserCommand::Remove { .. } => Ok(()),
    }
}

/// The warning `command` prints on standard error, when it is deprecated:
/// admin tokens give way to users (0.5.1). Standard output is unchanged, so
/// scripts that read it keep working.
#[must_use]
pub fn deprecation_warning(command: &Command) -> Option<&'static str> {
    match command {
        Command::Login { .. } => Some(
            "warning: `joedocs login` with an admin token is deprecated and will be removed; sign in as a user with `joedocs auth login --user NAME`",
        ),
        Command::AdminToken { .. } => Some(
            "warning: admin tokens are deprecated and will be removed; create users with `joedocs user create NAME`, who sign in with `joedocs auth login --user NAME`",
        ),
        _ => None,
    }
}

/// Prints the `output` of an admin API call, as the local command would,
/// and turns `check` problems into exit `1`.
/// `joedocs document` through the admin API.
fn remote_document(
    remote: &client::Remote<'_>,
    command: &DocumentCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    use reqwest::Method;
    match command {
        DocumentCommand::Convert { document_id } => remote_text(
            remote,
            &Method::POST,
            &format!("documents/{document_id}/convert"),
            None,
            out,
        ),
        DocumentCommand::MoveFile {
            document_id,
            file,
            to,
            as_file,
        } => {
            let body = serde_json::json!({"file": file, "to": to, "as": as_file});
            remote_text(
                remote,
                &Method::POST,
                &format!("documents/{document_id}/files/move"),
                Some(&body),
                out,
            )
        }
        DocumentCommand::RestoreFile {
            document_id,
            file,
            version,
        } => {
            let body = serde_json::json!({"file": file, "version": version});
            remote_text(
                remote,
                &Method::POST,
                &format!("documents/{document_id}/files/restore"),
                Some(&body),
                out,
            )
        }
    }
}

fn remote_text(
    remote: &client::Remote<'_>,
    method: &reqwest::Method,
    path: &str,
    body: Option<&serde_json::Value>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let reply = remote.call(method, path, body)?;
    let text: client::TextReply = remote.parse(&reply, "command reply")?;
    out.write_all(text.output.as_bytes())?;
    match text.problems {
        0 => Ok(()),
        count => Err(Error::Problems(
            usize::try_from(count).unwrap_or(usize::MAX),
        )),
    }
}

/// `joedocs key` through the admin API.
fn remote_key(
    remote: &client::Remote<'_>,
    command: &KeyCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    use reqwest::Method;
    use serde_json::json;
    let grants =
        |grants: &[auth::Grant]| grants.iter().map(ToString::to_string).collect::<Vec<_>>();
    match command {
        KeyCommand::List => remote_text(remote, &Method::GET, "keys", None, out),
        KeyCommand::Create { name, grants: g } => {
            let body = json!({"name": name, "grants": grants(g)});
            remote_text(remote, &Method::POST, "keys", Some(&body), out)
        }
        KeyCommand::Grant { name, grants: g } => {
            let body = json!({"grants": grants(g)});
            remote_text(
                remote,
                &Method::POST,
                &format!("keys/{name}/grants"),
                Some(&body),
                out,
            )
        }
        KeyCommand::Ungrant { name, projects } => {
            let body = json!({"projects": projects});
            let path = format!("keys/{}/ungrant", percent_encode(name));
            remote_text(remote, &Method::POST, &path, Some(&body), out).map_err(|e| match e {
                Error::Client(error) => newer_route(error, remote.server(), "key ungrant").into(),
                other => other,
            })
        }
        KeyCommand::Rotate { name } => remote_text(
            remote,
            &Method::POST,
            &format!("keys/{name}/rotate"),
            None,
            out,
        ),
        KeyCommand::Revoke { name } => remote_text(
            remote,
            &Method::POST,
            &format!("keys/{name}/revoke"),
            None,
            out,
        ),
    }
}

/// The admin commands that run remotely and print what they print locally.
fn remote_admin(
    remote: &client::Remote<'_>,
    command: &Command,
    out: &mut dyn Write,
) -> Result<(), Error> {
    use reqwest::Method;
    use serde_json::json;
    match command {
        Command::Project {
            command: ProjectCommand::List,
        } => remote_text(remote, &Method::GET, "projects", None, out),
        Command::Project {
            command: ProjectCommand::Add { name, key, title },
        } => {
            let body = json!({"name": name, "key": key, "title": title});
            remote_text(remote, &Method::POST, "projects", Some(&body), out)
        }
        Command::Key { command } => remote_key(remote, command, out),
        Command::Archive {
            command: ArchiveCommand::List { project, doc_type },
        } => {
            let path = with_query(
                "archive",
                &[
                    ("project", project.as_deref()),
                    ("type", doc_type.as_deref()),
                ],
            );
            remote_text(remote, &Method::GET, &path, None, out)
        }
        Command::Archive {
            command: ArchiveCommand::Restore { document_id },
        } => remote_text(
            remote,
            &Method::POST,
            &format!("archive/{document_id}/restore"),
            None,
            out,
        ),
        Command::Document { command } => remote_document(remote, command, out),
        Command::Check => remote_text(remote, &Method::GET, "check", None, out),
        Command::Reindex => remote_text(remote, &Method::POST, "reindex", None, out),
        command @ Command::Get { .. } => match GetRequest::of(command) {
            Some((request, file)) => remote_get(remote, &request, file, out),
            None => Ok(()),
        },
        Command::Backup { file, .. } => remote_backup(remote, file, out),
        Command::List(args) => remote_list(remote, args, out),
        Command::Search(args) => {
            let spec = browse::SearchSpec::from_args(args)?;
            let body =
                serde_json::to_value(&spec).map_err(|e| Error::Output(io::Error::other(e)))?;
            let reply = remote_page(remote, "search", Some(&body), "search", browse::is_page)?;
            browse::print(&reply, args.json, browse::render_search, out)
        }
        Command::History(args) => remote_history(remote, args, out),
        Command::Diff(args) => remote_diff(remote, args, out),
        Command::Audit(args) => remote_audit(remote, args, out),
        // Handled by `run_remote`, or refused by `remote` before it.
        _ => Ok(()),
    }
}

/// `path?name=value&...` with each value percent-encoded; `None` values are
/// left out.
fn with_query(path: &str, pairs: &[(&str, Option<&str>)]) -> String {
    let query: Vec<String> = pairs
        .iter()
        .filter_map(|(name, value)| value.map(|v| format!("{name}={}", percent_encode(v))))
        .collect();
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", query.join("&"))
    }
}

/// `joedocs list` through the admin API.
fn remote_list(
    remote: &client::Remote<'_>,
    args: &cli::ListArgs,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let (start, limit) = (args.start.to_string(), args.limit.to_string());
    let path = with_query(
        "documents",
        &[
            ("project", Some(&args.project)),
            ("type", args.doc_type.as_deref()),
            ("status", args.status.as_deref()),
            ("sort", Some(args.sort.api_name())),
            ("start", Some(&start)),
            ("limit", Some(&limit)),
        ],
    );
    let reply = remote_page(remote, &path, None, "list", browse::is_page)?;
    browse::print(&reply, args.json, browse::render_list, out)
}

/// `joedocs history` through the admin API.
fn remote_history(
    remote: &client::Remote<'_>,
    args: &cli::HistoryArgs,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let (start, limit) = (args.start.to_string(), args.limit.to_string());
    let path = with_query(
        &format!("documents/{}/history", percent_encode(&args.document_id)),
        &[
            ("file", args.part.as_deref()),
            ("start", Some(&start)),
            ("limit", Some(&limit)),
        ],
    );
    let reply = remote_page(remote, &path, None, "history", browse::is_history)?;
    browse::print(&reply, args.json, browse::render_history, out)
}

/// `joedocs diff` through the admin API.
fn remote_diff(
    remote: &client::Remote<'_>,
    args: &cli::DiffArgs,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let from = args.from.to_string();
    let to = args.to.map(|to| to.to_string());
    let context = args.context.to_string();
    let path = with_query(
        &format!("documents/{}/diff", percent_encode(&args.document_id)),
        &[
            ("from", Some(&from)),
            ("to", to.as_deref()),
            ("context", Some(&context)),
            ("file", args.part.as_deref()),
        ],
    );
    let reply = remote_page(remote, &path, None, "diff", browse::is_diff)?;
    browse::print(&reply, args.json, browse::render_diff, out)
}

/// `joedocs audit` through the admin API.
fn remote_audit(
    remote: &client::Remote<'_>,
    args: &cli::AuditArgs,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let (start, limit) = (args.start.to_string(), args.limit.to_string());
    let path = with_query(
        "audit",
        &[
            ("since", args.since.as_deref()),
            ("until", args.until.as_deref()),
            ("key", args.key.as_deref()),
            ("tool", args.tool.as_deref()),
            ("document", args.document.as_deref()),
            ("outcome", args.outcome.as_deref()),
            ("start", Some(&start)),
            ("limit", Some(&limit)),
        ],
    );
    let reply = remote_page(remote, &path, None, "audit", browse::is_page)?;
    browse::print(&reply, args.json, browse::render_audit, out)
}

/// Calls one of the routes added in 0.4.2 and checks the reply with
/// `expected`. A server without the route (an older `JoeDocs`) gets a
/// message that says to upgrade it.
fn remote_page(
    remote: &client::Remote<'_>,
    path: &str,
    body: Option<&serde_json::Value>,
    command: &str,
    expected: fn(&serde_json::Value) -> bool,
) -> Result<serde_json::Value, Error> {
    let what = format!("`{command}` reply");
    let method = if body.is_some() {
        reqwest::Method::POST
    } else {
        reqwest::Method::GET
    };
    let reply = remote
        .call(&method, path, body)
        .map_err(|e| newer_route(e, remote.server(), command))?;
    let page: serde_json::Value = remote.parse(&reply, &what)?;
    if !expected(&page) {
        return Err(client::Error::Protocol(format!(
            "{} sent something that is not a JoeDocs {what}; is client.server the JoeDocs server?",
            remote.server()
        ))
        .into());
    }
    Ok(page)
}

/// The message for a route the server does not have.
fn newer_route(error: client::Error, server: &str, command: &str) -> client::Error {
    match error {
        client::Error::Remote { code, message }
            if code == "not_found" && message == "no such admin API route" =>
        {
            client::Error::Remote {
                code,
                message: format!(
                    "{server} has no admin API route for `joedocs {command}`; it may run an older JoeDocs, so upgrade it to {} or later",
                    env!("CARGO_PKG_VERSION")
                ),
            }
        }
        other => other,
    }
}

/// `joedocs get` on the local store.
fn local_get(
    store: &store::Store,
    request: &GetRequest<'_>,
    file: Option<&std::path::Path>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let id = request.id;
    if let Some(path) = request.archive {
        let bytes = admin::folder_archive(store, id, request.version)?;
        admin::write_file(path, &bytes)?;
    } else if let Some(name) = request.file_part {
        let markdown = admin::fetch_file(store, id, name, request.version)?;
        admin::write_document(&markdown, request.part, file, out)?;
    } else {
        let markdown = admin::fetch(store, id, request.version)?;
        admin::write_document(&markdown, request.part, file, out)?;
        if request.part == admin::Part::Whole
            && file.is_none()
            && let Some(files) = admin::other_files(store, id, request.version)?
        {
            admin::write_file_list(id.trim(), &files, out)?;
        }
    }
    Ok(())
}

/// What `joedocs get` asks for.
struct GetRequest<'a> {
    id: &'a str,
    version: Option<u64>,
    part: admin::Part,
    file_part: Option<&'a str>,
    archive: Option<&'a std::path::Path>,
}

impl<'a> GetRequest<'a> {
    /// The request of a `get` command and its `-f` file; `None` for any
    /// other command.
    fn of(command: &'a Command) -> Option<(Self, Option<&'a std::path::Path>)> {
        let Command::Get {
            document_id,
            version,
            content_only,
            front_matter_only,
            file,
            part: file_part,
            archive,
        } = command
        else {
            return None;
        };
        let request = Self {
            id: document_id,
            version: *version,
            part: admin::Part::new(*content_only, *front_matter_only),
            file_part: file_part.as_deref(),
            archive: archive.as_deref(),
        };
        Some((request, file.as_deref()))
    }
}

fn remote_get(
    remote: &client::Remote<'_>,
    request: &GetRequest<'_>,
    file: Option<&std::path::Path>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let mut query = Vec::new();
    if let Some(version) = request.version {
        query.push(format!("version={version}"));
    }
    if let Some(name) = request.file_part {
        query.push(format!("part={}", percent_encode(name)));
    }
    if request.archive.is_some() {
        query.push("archive=true".to_owned());
    }
    let path = if query.is_empty() {
        format!("documents/{}", request.id)
    } else {
        format!("documents/{}?{}", request.id, query.join("&"))
    };
    let reply = remote.call(&reqwest::Method::GET, &path, None)?;
    if let Some(dest) = request.archive {
        admin::check_folder_archive(&reply.body).map_err(|e| {
            client::Error::Protocol(format!(
                "{} did not send a document archive ({e}); is client.server the JoeDocs server?",
                remote.server()
            ))
        })?;
        admin::write_file(dest, &reply.body)?;
        return Ok(());
    }
    let document: client::DocumentReply = remote.parse(&reply, "document reply")?;
    admin::write_document(&document.markdown, request.part, file, out)?;
    if let Some(files) = document.files
        && request.part == admin::Part::Whole
        && file.is_none()
        && request.file_part.is_none()
    {
        admin::write_file_list(request.id.trim(), &files, out)?;
    }
    Ok(())
}

/// `text` with every byte but unreserved characters percent-encoded, for a
/// query value.
pub(crate) fn percent_encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn remote_backup(
    remote: &client::Remote<'_>,
    file: &std::path::Path,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if std::fs::symlink_metadata(file).is_ok() {
        return Err(backup::Error::Exists(file.to_path_buf()).into());
    }
    let downloaded = remote.download("backup", file)?;
    let manifest = backup::read_manifest(file).map_err(|e| {
        let _ = std::fs::remove_file(file);
        client::Error::Protocol(format!(
            "{} sent something that is not a JoeDocs backup ({e})",
            remote.server()
        ))
    })?;
    let created = backup::Created {
        manifest,
        bytes: downloaded.bytes,
        skipped_links: downloaded.skipped_links,
    };
    admin::report_backup(file, &created, out)
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
    let extracted = backup::extract(file, &paths.state_dir, &paths.home)?;
    let history = GitCli::new(paths.git_dir(), &paths.home);
    // If the restored store does not open (or migrate), `extracted` is
    // dropped here and the targets are left as they were.
    let store = Store::open(
        paths.clone(),
        Settings::from_config(loaded.config()),
        Box::new(history),
        Arc::clone(&ctx.clock),
    )?;
    let manifest = extracted.commit();
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
            prompt: &client::TerminalPrompt,
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
        assert!(out.contains("# MY_APP_ADMIN_TOKEN: unset"), "{out}");
        assert!(!out.contains("API_TOKEN"), "{out}");

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
    fn logs_are_coloured_only_on_a_terminal_without_no_colour() {
        let unset = MapEnv::new();
        let set = MapEnv::new().with("NO_COLOR", "1");
        let empty = MapEnv::new().with("NO_COLOR", "");
        assert!(log_colour(true, &unset));
        assert!(!log_colour(true, &set));
        assert!(log_colour(true, &empty));
        assert!(!log_colour(false, &unset));
        assert!(!log_colour(false, &set));
    }

    #[test]
    fn a_failed_file_write_names_the_path_and_maps_to_ex_ioerr() {
        let err = Error::Write {
            path: "missing/out.md".into(),
            source: io::Error::new(io::ErrorKind::NotFound, "gone"),
        };
        assert_eq!(err.exit_code(), EXIT_IO);
        assert_eq!(err.to_string(), "cannot write missing/out.md: gone");
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
