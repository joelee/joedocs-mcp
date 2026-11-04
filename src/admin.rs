//! Administrator commands run on the server host: `init`, `project`, `key`
//! and `reindex`. Phase 4 adds `archive` and `check`.
//!
//! Each command calls the [`Store`] and writes a plain-text report.

use std::io::Write;

use secrecy::ExposeSecret as _;

use crate::Error;
use crate::auth::{Grant, RandomSource};
use crate::cli::{KeyCommand, ProjectCommand};
use crate::db::KeyRow;
use crate::store::{InitOutcome, Paths, Reindexed, Store};

/// Reports what `joedocs init` did.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn report_init(paths: &Paths, outcome: InitOutcome, out: &mut dyn Write) -> Result<(), Error> {
    let headline = match outcome {
        InitOutcome::Created => "Initialised JoeDocs.",
        InitOutcome::AlreadyInitialised => {
            "Already initialised; the database schema is up to date."
        }
    };
    writeln!(out, "{headline}")?;
    writeln!(out, "  vault: {}", paths.home.display())?;
    writeln!(out, "  state: {}", paths.state_dir.display())?;
    if outcome == InitOutcome::Created {
        writeln!(out, "Next: joedocs project add <name> --key <KEY>")?;
    }
    Ok(())
}

/// Runs `joedocs reindex`: rebuilds the index from the history and
/// reports what it found.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn reindex(store: &mut Store, out: &mut dyn Write) -> Result<(), Error> {
    let report = store.reindex()?;
    report_reindex(&report, out)
}

/// Writes a [`Reindexed`] report.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn report_reindex(report: &Reindexed, out: &mut dyn Write) -> Result<(), Error> {
    writeln!(
        out,
        "Rebuilt the index from the history: {} documents ({} archived), {} revisions.",
        report.documents + report.archived,
        report.archived,
        report.revisions
    )?;
    for (name, key) in &report.projects_created {
        writeln!(
            out,
            "Recreated project {name} (key {key}), which was missing from the database; \
             its title is its name."
        )?;
    }
    if report.skipped_commits > 0 {
        writeln!(
            out,
            "Ignored {} history commits without JoeDocs trailers.",
            report.skipped_commits
        )?;
    }
    if !report.drifted.is_empty() {
        writeln!(
            out,
            "Warning: these vault files differ from the history, so writes to them are refused:"
        )?;
        for (id, path) in &report.drifted {
            writeln!(out, "  {id}  {path}")?;
        }
    }
    Ok(())
}

/// Runs a `project` subcommand.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn project(
    store: &mut Store,
    command: &ProjectCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        ProjectCommand::Add { name, key, title } => {
            let project = store.add_project(name, key, title.as_deref())?;
            writeln!(
                out,
                "Added project {} (key {}): {}",
                project.name, project.key, project.title
            )?;
        }
        ProjectCommand::List => {
            let projects = store.projects()?;
            if projects.is_empty() {
                writeln!(
                    out,
                    "No projects. Add one with: joedocs project add <name> --key <KEY>"
                )?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = projects
                .into_iter()
                .map(|p| vec![p.name, p.key, p.documents.to_string(), p.title])
                .collect();
            write_table(out, &["NAME", "KEY", "DOCUMENTS", "TITLE"], &rows)?;
        }
    }
    Ok(())
}

/// Runs a `key` subcommand. `key create` prints the token alone on the last
/// line, so scripts can capture it with `tail -n 1`.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn key(
    store: &mut Store,
    command: &KeyCommand,
    random: &dyn RandomSource,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        KeyCommand::Create { name, grants } => {
            let created = store.create_key(name, grants, random)?;
            writeln!(
                out,
                "Created API key {} (id {}) with grants: {}",
                created.key.name,
                created.key.id,
                grant_list(&created.key.grants)
            )?;
            writeln!(
                out,
                "The token is shown only now. Give it to the MCP client as:"
            )?;
            writeln!(out, "  Authorization: Bearer <token>")?;
            writeln!(out, "{}", created.token.expose_secret())?;
        }
        KeyCommand::List => {
            let keys = store.keys()?;
            if keys.is_empty() {
                writeln!(
                    out,
                    "No API keys. Create one with: joedocs key create <name> --grant <project>:write"
                )?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = keys.iter().map(key_row).collect();
            write_table(
                out,
                &["NAME", "ID", "CREATED", "LAST USED", "STATUS", "GRANTS"],
                &rows,
            )?;
        }
        KeyCommand::Grant { name, grants } => {
            let key = store.grant(name, grants)?;
            writeln!(
                out,
                "API key {} grants: {}",
                key.name,
                grant_list(&key.grants)
            )?;
        }
        KeyCommand::Revoke { name } => {
            let key = store.revoke(name)?;
            writeln!(
                out,
                "API key {} is revoked (since {}).",
                key.name,
                key.revoked_at.as_deref().unwrap_or("now")
            )?;
        }
    }
    Ok(())
}

fn key_row(key: &KeyRow) -> Vec<String> {
    vec![
        key.name.clone(),
        key.id.clone(),
        key.created_at.clone(),
        key.last_used_at
            .clone()
            .unwrap_or_else(|| "never".to_owned()),
        if key.revoked_at.is_some() {
            "revoked".to_owned()
        } else {
            "active".to_owned()
        },
        grant_list(&key.grants),
    ]
}

fn grant_list(grants: &[Grant]) -> String {
    if grants.is_empty() {
        "none".to_owned()
    } else {
        grants
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Writes left-aligned columns separated by two spaces.
fn write_table(out: &mut dyn Write, header: &[&str], rows: &[Vec<String>]) -> Result<(), Error> {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let last = cells.len().saturating_sub(1);
        let mut text = String::new();
        for (i, (cell, width)) in cells.iter().zip(&widths).enumerate() {
            text.push_str(cell);
            if i < last {
                let pad = width - cell.chars().count() + 2;
                text.push_str(&" ".repeat(pad));
            }
        }
        text
    };
    writeln!(out, "{}", line(header.to_vec()))?;
    for row in rows {
        writeln!(out, "{}", line(row.iter().map(String::as_str).collect()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_without_grants_say_none() {
        assert_eq!(grant_list(&[]), "none");
    }

    #[test]
    fn tables_align_columns() {
        let mut out = Vec::new();
        let rows = vec![
            vec!["joedocs".to_owned(), "JD".to_owned(), "é".to_owned()],
            vec!["a".to_owned(), "LONGKEY".to_owned(), "x".to_owned()],
        ];
        write_table(&mut out, &["NAME", "KEY", "T"], &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME     KEY      T\njoedocs  JD       é\na        LONGKEY  x\n"
        );
    }
}
