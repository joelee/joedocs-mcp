//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use toml::Value;

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
