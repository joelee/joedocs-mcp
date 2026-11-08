//! `joedocs agents`: an AGENTS.md-style guide for AI agents that maintain
//! this server, filled in with this installation's facts. It never contains
//! a secret: only paths, the listen address, the version and the unit.

use std::io::Write;

use crate::status::{UnitState, unit_state};
use crate::store::Paths;
use crate::systemd::{Scope, ServiceManager};
use crate::{Context, Error, Loaded};

/// The guide, with `{{name}}` placeholders.
const TEMPLATE: &str = include_str!("agents.md");

/// The installation facts the guide is filled in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The program version.
    pub version: String,
    /// The config file in use, or a note that there is none.
    pub config_file: String,
    /// `storage.home`.
    pub home: String,
    /// `storage.state_dir`.
    pub state_dir: String,
    /// `server.bind`.
    pub bind: String,
    /// How the service is run.
    pub service: String,
}

impl Facts {
    /// Gathers the facts from the effective configuration and the host.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the storage paths are not configured.
    pub fn gather(loaded: &Loaded, ctx: &Context<'_>) -> Result<Self, Error> {
        let paths = Paths::from_config(loaded.config(), &ctx.app, ctx.env)?;
        Ok(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            config_file: loaded.resolved.file.as_ref().map_or_else(
                || "none (built-in defaults and environment overrides)".to_owned(),
                |f| format!("`{}`", f.display()),
            ),
            home: format!("`{}`", paths.home.display()),
            state_dir: format!("`{}`", paths.state_dir.display()),
            bind: format!("`{}`", loaded.config().server.bind),
            service: service(ctx.services, ctx),
        })
    }
}

/// Describes the systemd units, if any.
fn service(services: &dyn ServiceManager, ctx: &Context<'_>) -> String {
    let mut found = Vec::new();
    for (scope, user, label, logs) in [
        (
            Scope::User,
            true,
            "systemd user unit",
            "journalctl --user -u joedocs",
        ),
        (
            Scope::System,
            false,
            "systemd system unit",
            "journalctl -u joedocs",
        ),
    ] {
        if let UnitState::Installed { active, .. } = unit_state(services, user) {
            let path = scope
                .unit_path(ctx.env)
                .map(|p| format!(" at `{}`", p.display()))
                .unwrap_or_default();
            found.push(format!("{label}{path}, {active}; logs: `{logs}`"));
        }
    }
    if found.is_empty() {
        "no systemd unit found (it may run under `brew services`, Docker, or by hand)".to_owned()
    } else {
        found.join("; ")
    }
}

/// The guide filled in with `facts`.
#[must_use]
pub fn render(facts: &Facts) -> String {
    TEMPLATE
        .replace("{{version}}", &facts.version)
        .replace("{{config_file}}", &facts.config_file)
        .replace("{{home}}", &facts.home)
        .replace("{{state_dir}}", &facts.state_dir)
        .replace("{{bind}}", &facts.bind)
        .replace("{{service}}", &facts.service)
}

/// Runs `joedocs agents`.
///
/// # Errors
///
/// Returns [`Error::Config`] for unset storage paths and [`Error::Output`].
pub fn run(loaded: &Loaded, ctx: &Context<'_>, out: &mut dyn Write) -> Result<(), Error> {
    out.write_all(render(&Facts::gather(loaded, ctx)?).as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            version: "9.9.9".into(),
            config_file: "`/etc/joedocs/config.toml`".into(),
            home: "`/srv/vault`".into(),
            state_dir: "`/srv/state`".into(),
            bind: "`127.0.0.1:8750`".into(),
            service: "systemd user unit, active".into(),
        }
    }

    #[test]
    fn every_placeholder_is_filled() {
        let text = render(&facts());
        assert!(!text.contains("{{"), "{text}");
        for expected in [
            "9.9.9",
            "/etc/joedocs/config.toml",
            "/srv/vault",
            "/srv/state",
            "127.0.0.1:8750",
            "systemd user unit, active",
        ] {
            assert!(text.contains(expected), "{expected}");
        }
    }

    #[test]
    fn the_guide_covers_safe_commands_approvals_and_prohibitions() {
        let text = render(&facts());
        for section in [
            "## Safe to run at any time",
            "## Ask the operator first",
            "## Never",
            "## Upgrading",
            "## Troubleshooting",
        ] {
            assert!(text.contains(section), "{section}");
        }
        for command in [
            "joedocs status",
            "joedocs check",
            "joedocs backup",
            "joedocs reindex",
            "backup --restore",
            "docs/upgrading.md",
        ] {
            assert!(text.contains(command), "{command}");
        }
    }
}
