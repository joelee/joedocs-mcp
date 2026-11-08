//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use toml::Value;

use crate::auth::Grant;
use crate::config::{CliValue, LogLevel};

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(name = env!("CARGO_PKG_NAME"), version, about, arg_required_else_help = true)]
pub struct Cli {
    /// Config file to use instead of the automatic lookup.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Log level (overrides `log.level`).
    #[arg(long, global = true, value_enum)]
    pub log_level: Option<LogLevel>,

    /// Command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Inspect configuration.
    Config {
        /// What to show.
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Create the vault, state directory, database and history (safe to
    /// rerun).
    Init,
    /// Run the MCP server on `server.bind` until Ctrl-C or SIGTERM. The
    /// index is rebuilt first if it is behind the history.
    Serve,
    /// Rebuild the document index and search from the history (safe to
    /// rerun; projects and API keys are kept).
    Reindex,
    /// Install or remove a systemd unit that runs `joedocs serve`.
    Systemd {
        /// What to do.
        #[command(subcommand)]
        command: SystemdCommand,
    },
    /// List and restore archived documents.
    Archive {
        /// What to do.
        #[command(subcommand)]
        command: ArchiveCommand,
    },
    /// Report drift between the vault, the history and the index (changes
    /// nothing; exits 1 if it finds problems).
    Check,
    /// Register and list projects.
    Project {
        /// What to do.
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Create, list, grant and revoke API keys.
    Key {
        /// What to do.
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Write a backup of the vault, the database and the history to a
    /// `.tgz` file (safe while `serve` runs; the file holds key hashes, so
    /// keep it private), or restore one with `--restore`.
    Backup {
        /// The archive to create (it must not exist yet), or to restore.
        file: PathBuf,
        /// Restore `FILE` into empty or absent directories instead.
        #[arg(long)]
        restore: bool,
        /// Where to restore the state directory (default: `storage.state_dir`).
        #[arg(long, value_name = "PATH", requires = "restore")]
        state_dir: Option<PathBuf>,
        /// Where to restore the vault (default: `storage.home`).
        #[arg(long, value_name = "PATH", requires = "restore")]
        content_dir: Option<PathBuf>,
    },
    /// Print an AGENTS.md-style guide for AI agents that maintain this
    /// server, filled in with this installation's paths (no secrets).
    Agents,
    /// Show where the service runs, the storage sizes, and the state of the
    /// store (changes nothing).
    Status,
    /// Print a document: its front matter and body, or one of them.
    Get {
        /// Document ID, e.g. JD-42.
        document_id: String,
        /// Print this revision instead of the latest.
        #[arg(long, value_name = "N")]
        version: Option<u64>,
        /// Print only the body.
        #[arg(long, conflicts_with = "front_matter_only")]
        content_only: bool,
        /// Print only the front matter, as YAML without the `---` lines.
        #[arg(long)]
        front_matter_only: bool,
        /// Write to this file (replacing it) instead of standard output.
        #[arg(short = 'f', long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
}

/// `project` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ProjectCommand {
    /// Register a project.
    Add {
        /// Project name: lower-case letters, digits and '-' (the vault folder).
        name: String,
        /// Document ID prefix, e.g. JD for JD-42. Cannot be changed later.
        #[arg(long)]
        key: String,
        /// Human-readable title (defaults to the name).
        #[arg(long)]
        title: Option<String>,
    },
    /// List projects.
    List,
}

/// `systemd` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum SystemdCommand {
    /// Write the unit with the current configuration, then enable and
    /// (re)start it. Safe to rerun after changing the configuration.
    Install {
        /// Install a system unit (run as root) instead of a user unit.
        #[arg(long)]
        system: bool,
        /// The account a system unit runs as.
        #[arg(long, value_name = "USER")]
        run_as: Option<String>,
        /// Print the unit and the commands instead of running them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop, disable and delete the unit.
    Remove {
        /// Remove the system unit instead of the user unit.
        #[arg(long)]
        system: bool,
        /// Print the commands instead of running them.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `archive` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ArchiveCommand {
    /// List archived documents, most recently archived first.
    List {
        /// Only this project.
        #[arg(long)]
        project: Option<String>,
    },
    /// Bring an archived document back as it was before it was archived.
    Restore {
        /// Document ID, e.g. JD-42.
        document_id: String,
    },
}

/// `key` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum KeyCommand {
    /// Create a key and print its token once.
    Create {
        /// Key name, recorded as the author of every change made with it.
        name: String,
        /// Grant, e.g. `joedocs:write` or `other:read` (repeatable).
        #[arg(long = "grant", value_name = "PROJECT:PERMISSION")]
        grants: Vec<Grant>,
    },
    /// List keys and their grants (never the tokens).
    List,
    /// Add or change grants on a key.
    Grant {
        /// Key name.
        name: String,
        /// Grants, e.g. `joedocs:write`.
        #[arg(required = true, value_name = "PROJECT:PERMISSION")]
        grants: Vec<Grant>,
    },
    /// Print a new token for a key; the old token stops working at once.
    /// The key keeps its name and grants.
    Rotate {
        /// Key name.
        name: String,
    },
    /// Revoke a key immediately.
    Revoke {
        /// Key name.
        name: String,
    },
}

/// `config` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ConfigCommand {
    /// Show the effective configuration and where each value came from.
    Show,
    /// List the config file search order and which file was selected.
    Paths,
}

impl Cli {
    /// Config values supplied by flags, for the highest-precedence layer.
    #[must_use]
    pub fn cli_values(&self) -> Vec<CliValue> {
        let mut values = Vec::new();
        if let Some(level) = self.log_level {
            values.push(CliValue {
                key: "log.level".to_owned(),
                flag: "--log-level".to_owned(),
                value: Value::String(level.as_str().to_owned()),
            });
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_command_is_required() {
        let err = Cli::try_parse_from(["app"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn no_flags_means_no_cli_values() {
        let cli = Cli::try_parse_from(["app", "config", "show"]).unwrap();
        assert_eq!(cli.cli_values(), Vec::new());
    }

    #[test]
    fn flags_become_cli_values() {
        let cli = Cli::try_parse_from(["app", "--log-level", "debug", "config", "paths"]).unwrap();
        let values = cli.cli_values();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].key, "log.level");
        assert_eq!(values[0].flag, "--log-level");
        assert_eq!(values[0].value, Value::String("debug".into()));
    }

    #[test]
    fn admin_commands_parse_grants() {
        let cli = Cli::try_parse_from([
            "app", "key", "create", "laptop", "--grant", "a:read", "--grant", "b:write",
        ])
        .unwrap();
        assert!(matches!(
            &cli.command,
            Command::Key { command: KeyCommand::Create { name, grants } }
                if name == "laptop" && grants.len() == 2
        ));
        let err = Cli::try_parse_from(["app", "key", "grant", "laptop", "a:admin"]).unwrap_err();
        assert!(err.to_string().contains("read or write"), "{err}");
        assert!(Cli::try_parse_from(["app", "key", "grant", "laptop"]).is_err());
        let cli = Cli::try_parse_from(["app", "project", "add", "p", "--key", "P"]).unwrap();
        assert_eq!(
            cli.command,
            Command::Project {
                command: ProjectCommand::Add {
                    name: "p".into(),
                    key: "P".into(),
                    title: None
                }
            }
        );
    }

    #[test]
    fn get_takes_a_version_a_part_and_a_file() {
        let cli =
            Cli::try_parse_from(["app", "get", "JD-4", "--version", "2", "-f", "x.md"]).unwrap();
        assert_eq!(
            cli.command,
            Command::Get {
                document_id: "JD-4".into(),
                version: Some(2),
                content_only: false,
                front_matter_only: false,
                file: Some(PathBuf::from("x.md")),
            }
        );
        let cli = Cli::try_parse_from(["app", "get", "JD-4", "--front-matter-only"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Get {
                front_matter_only: true,
                content_only: false,
                ..
            }
        ));
        let err = Cli::try_parse_from([
            "app",
            "get",
            "JD-4",
            "--content-only",
            "--front-matter-only",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn global_flags_work_after_subcommands() {
        let cli = Cli::try_parse_from(["app", "config", "show", "--config", "/x.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/x.toml")));
        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Show
            }
        );
    }
}
