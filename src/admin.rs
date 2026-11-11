//! Administrator commands run on the server host: `init`, `project`, `key`,
//! `reindex`, `archive`, `check` and `get`.
//!
//! Each command calls the [`Store`] and writes a plain-text report.

use std::io::Write;
use std::path::Path;

use secrecy::ExposeSecret as _;

use crate::Error;
use crate::auth::{Grant, RandomSource};
use crate::cli::{AdminTokenCommand, ArchiveCommand, DocumentCommand, KeyCommand, ProjectCommand};
use crate::db::{AdminTokenRow, KeyRow};
use crate::store::{Actor, CheckReport, InitOutcome, Paths, Reindexed, Store};

/// The author recorded for changes made with admin commands.
pub const ADMIN_AUTHOR: &str = "admin";

/// The client recorded for changes made with admin commands.
pub const ADMIN_CLIENT: &str = "joedocs-cli";

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

/// Runs an `archive` subcommand.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn archive(
    store: &mut Store,
    command: &ArchiveCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        ArchiveCommand::List { project, doc_type } => {
            let archived = store.archived(project.as_deref(), doc_type.as_deref())?;
            if archived.is_empty() {
                writeln!(out, "No archived documents.")?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = archived
                .into_iter()
                .map(|a| {
                    vec![
                        a.document.id,
                        a.document.project,
                        a.document.doc_type,
                        a.document.title,
                        a.archived_at,
                        a.archived_by,
                        a.reason,
                    ]
                })
                .collect();
            write_table(
                out,
                &["ID", "PROJECT", "TYPE", "TITLE", "ARCHIVED", "BY", "REASON"],
                &rows,
            )?;
        }
        ArchiveCommand::Restore { document_id } => {
            let actor = Actor::new(ADMIN_AUTHOR, ADMIN_CLIENT);
            let restored = store.unarchive(&actor, document_id)?;
            writeln!(
                out,
                "Restored {} at revision {}: {}",
                restored.id, restored.revision, restored.path
            )?;
        }
    }
    Ok(())
}

/// Runs `joedocs admin-token`.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn admin_token(
    store: &mut Store,
    command: &AdminTokenCommand,
    random: &dyn RandomSource,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        AdminTokenCommand::Create { name } => {
            let created = store.create_admin_token(name, random)?;
            writeln!(out, "Created admin token {name}.")?;
            writeln!(
                out,
                "Add it to Google Authenticator (or another TOTP app) with this secret:"
            )?;
            writeln!(out, "  {}", created.totp_secret.expose_secret())?;
            writeln!(
                out,
                "or this URI (pipe it to `qrencode -t ansiutf8` for a QR code):"
            )?;
            writeln!(out, "  {}", created.uri.expose_secret())?;
            writeln!(
                out,
                "The admin token follows, shown once. Keep it in a password manager; on the client, export JOEDOCS_ADMIN_TOKEN with it."
            )?;
            writeln!(out, "{}", created.token.expose_secret())?;
        }
        AdminTokenCommand::List => {
            let tokens = store.admin_tokens()?;
            if tokens.is_empty() {
                writeln!(out, "No admin tokens.")?;
                return Ok(());
            }
            let now = store.now();
            let rows: Vec<Vec<String>> = tokens.iter().map(|t| admin_token_row(t, &now)).collect();
            write_table(out, &["NAME", "CREATED", "LAST USED", "STATE"], &rows)?;
        }
        AdminTokenCommand::Revoke { name } => {
            let row = store.revoke_admin_token(name)?;
            writeln!(
                out,
                "Revoked admin token {name} and ended its sessions ({}).",
                row.revoked_at.unwrap_or_default()
            )?;
        }
    }
    Ok(())
}

fn admin_token_row(token: &AdminTokenRow, now: &str) -> Vec<String> {
    let state = match (&token.revoked_at, &token.locked_until) {
        (Some(at), _) => format!("revoked {at}"),
        (None, Some(until)) if until.as_str() > now => format!("locked until {until}"),
        _ => "active".to_owned(),
    };
    vec![
        token.name.clone(),
        token.created_at.clone(),
        token
            .last_used_at
            .clone()
            .unwrap_or_else(|| "never".to_owned()),
        state,
    ]
}

/// Reports what `joedocs backup` wrote.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn report_backup(
    file: &Path,
    created: &crate::backup::Created,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let count = created.manifest.documents;
    let noun = if count == 1 { "document" } else { "documents" };
    writeln!(
        out,
        "Backed up {count} {noun} to {} ({}).",
        file.display(),
        crate::status::human(created.bytes)
    )?;
    for link in &created.skipped_links {
        writeln!(out, "  skipped the symlink {link} in the vault")?;
    }
    writeln!(out, "The file holds API key hashes: keep it private.")?;
    Ok(())
}

/// Reports what `joedocs backup --restore` restored, and the settings to
/// use when it was restored somewhere other than the configured paths.
///
/// # Errors
///
/// Returns [`Error::Output`] when writing fails.
pub fn report_restore(
    manifest: &crate::backup::Manifest,
    restored: &Paths,
    configured: &Paths,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let count = manifest.documents;
    let noun = if count == 1 { "document" } else { "documents" };
    writeln!(
        out,
        "Restored {count} {noun} from a JoeDocs {} backup taken {}.",
        manifest.joedocs_version, manifest.created_at
    )?;
    writeln!(out, "  vault: {}", restored.home.display())?;
    writeln!(out, "  state: {}", restored.state_dir.display())?;
    if restored.home != configured.home || restored.state_dir != configured.state_dir {
        writeln!(
            out,
            "The configuration points elsewhere. To use the restored store, set:"
        )?;
        writeln!(out, "  [storage]")?;
        writeln!(out, "  home = {:?}", restored.home.display().to_string())?;
        writeln!(
            out,
            "  state_dir = {:?}",
            restored.state_dir.display().to_string()
        )?;
    }
    Ok(())
}

/// Which part of a document `joedocs get` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Front matter and body, exactly as stored.
    Whole,
    /// The body only.
    Body,
    /// The front matter YAML only, without the `---` lines.
    FrontMatter,
}

impl Part {
    /// The part `--content-only` and `--front-matter-only` select.
    #[must_use]
    pub fn new(content_only: bool, front_matter_only: bool) -> Self {
        if content_only {
            Self::Body
        } else if front_matter_only {
            Self::FrontMatter
        } else {
            Self::Whole
        }
    }
}

/// The whole Markdown of a document, or of one of its revisions.
///
/// # Errors
///
/// Returns [`Error::Archived`] for an archived document, and store errors.
pub fn fetch(store: &Store, id: &str, version: Option<u64>) -> Result<String, Error> {
    match store.get(id, version) {
        Ok(fetched) => Ok(fetched.markdown),
        Err(crate::store::Error::Archived(id)) => Err(Error::Archived(id)),
        Err(error) => Err(error.into()),
    }
}

/// One file of a multi-file document, as `joedocs get` lists it after the
/// main file (and as the admin API sends it).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListedFile {
    /// Path inside the document's folder, without `.md`.
    pub file: String,
    /// The document revision at which it last changed.
    pub revision: u64,
    /// When it last changed.
    pub updated: String,
}

/// The files of document `id` besides its main file, at `version`; `None`
/// for a single-file document.
///
/// # Errors
///
/// Returns [`Error::Archived`] for an archived document, and store errors.
pub fn other_files(
    store: &Store,
    id: &str,
    version: Option<u64>,
) -> Result<Option<Vec<ListedFile>>, Error> {
    let info = store.document_info(id).map_err(archived)?;
    if !info.multifile {
        return Ok(None);
    }
    let files = store
        .list_files(id, version)
        .map_err(archived)?
        .into_iter()
        .filter(|f| !f.main)
        .map(|f| ListedFile {
            file: f.file,
            revision: f.revision,
            updated: f.updated,
        })
        .collect();
    Ok(Some(files))
}

fn archived(error: crate::store::Error) -> Error {
    match error {
        crate::store::Error::Archived(id) => Error::Archived(id),
        other => other.into(),
    }
}

/// Writes the list of a multi-file document's other files, after its main
/// file, under a separating line.
///
/// # Errors
///
/// Returns [`Error::Output`].
pub fn write_file_list(id: &str, files: &[ListedFile], out: &mut dyn Write) -> Result<(), Error> {
    writeln!(out, "\n--- Other files of {id} ---")?;
    if files.is_empty() {
        writeln!(out, "No other files.")?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = files
        .iter()
        .map(|f| vec![f.file.clone(), f.revision.to_string(), f.updated.clone()])
        .collect();
    write_table(out, &["FILE", "REVISION", "UPDATED"], &rows)
}

/// The whole Markdown of one file of a multi-file document, or of one of
/// its revisions.
///
/// # Errors
///
/// Returns [`Error::Archived`] for an archived document, and store errors.
pub fn fetch_file(
    store: &Store,
    id: &str,
    file: &str,
    version: Option<u64>,
) -> Result<String, Error> {
    Ok(store
        .get_file(id, file, version)
        .map_err(archived)?
        .markdown)
}

/// A gzip tar of every file of multi-file document `id` at `version` (the
/// latest when `None`), with paths relative to its folder.
///
/// # Errors
///
/// Returns a store [`crate::store::Error::Invalid`] for a single-file
/// document, [`Error::Archived`], and store errors.
pub fn folder_archive(store: &Store, id: &str, version: Option<u64>) -> Result<Vec<u8>, Error> {
    let info = store.document_info(id).map_err(archived)?;
    if !info.multifile {
        return Err(crate::store::Error::invalid(
            "archive",
            format!("{} is a single-file document; use -f to save it", info.id),
        )
        .into());
    }
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for file in store.list_files(id, version).map_err(archived)? {
        let markdown = if file.main {
            store.get(id, version).map_err(archived)?.markdown
        } else {
            fetch_file(store, id, &file.file, version)?
        };
        let mut header = tar::Header::new_gnu();
        header.set_size(markdown.len() as u64);
        header.set_mode(0o644);
        let at = file
            .updated
            .parse::<jiff::Timestamp>()
            .map_or(0, |t| u64::try_from(t.as_second()).unwrap_or(0));
        header.set_mtime(at);
        header.set_cksum();
        tar.append_data(
            &mut header,
            format!("{}.md", file.file),
            markdown.as_bytes(),
        )
        .map_err(Error::Output)?;
    }
    let encoder = tar.into_inner().map_err(Error::Output)?;
    encoder.finish().map_err(Error::Output)
}

/// Checks that `bytes` is a gzip tar, as [`folder_archive`] writes.
///
/// # Errors
///
/// Returns a description of what is wrong.
pub fn check_folder_archive(bytes: &[u8]) -> Result<(), String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive.entries().map_err(|e| e.to_string())?;
    for entry in entries {
        entry.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Writes `part` of `markdown` to `file` (replacing it atomically) or, when
/// there is no file, to `out`.
///
/// # Errors
///
/// Returns [`Error::Write`] when writing `file` fails, [`Error::Output`]
/// when writing to `out` fails, and a store integrity error when the stored
/// text has no front matter block.
pub fn write_document(
    markdown: &str,
    part: Part,
    file: Option<&Path>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let text = match part {
        Part::Whole => markdown,
        Part::Body | Part::FrontMatter => {
            let (yaml, body) = crate::document::front_matter::split(markdown)
                .map_err(|e| crate::store::Error::invalid("document", e.to_string()))?;
            if part == Part::Body { body } else { yaml }
        }
    };
    match file {
        Some(path) => write_file(path, text.as_bytes())?,
        None => out.write_all(text.as_bytes())?,
    }
    Ok(())
}

/// Runs a `joedocs document` command and writes its report.
///
/// # Errors
///
/// Returns store errors and [`Error::Output`].
pub fn document(
    store: &mut Store,
    command: &DocumentCommand,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let actor = Actor::new(ADMIN_AUTHOR, ADMIN_CLIENT);
    match command {
        DocumentCommand::Convert { document_id } => {
            let converted = store.convert(&actor, document_id)?;
            writeln!(
                out,
                "Converted {} to a multi-file document at revision {}: {}",
                converted.id, converted.revision, converted.path
            )?;
        }
        DocumentCommand::MoveFile {
            document_id,
            file,
            to,
            as_file,
        } => {
            let moved = store.move_file(&actor, document_id, file, to, as_file.as_deref())?;
            writeln!(
                out,
                "Moved {file} from {} (now revision {}) to {} (now revision {}) as {}: {}",
                moved.source,
                moved.source_revision,
                moved.target,
                moved.target_revision,
                moved.file,
                moved.path
            )?;
        }
        DocumentCommand::RestoreFile {
            document_id,
            file,
            version,
        } => {
            let restored = store.restore_file(&actor, document_id, file, *version)?;
            writeln!(
                out,
                "Restored {} of {} as it was at revision {version}, now revision {}: {}",
                restored.file.unwrap_or_default(),
                restored.id,
                restored.revision,
                restored.path
            )?;
        }
    }
    Ok(())
}

/// [`write_atomically`] for a file the user named, such as `get -f PATH`.
///
/// # Errors
///
/// Returns [`Error::Write`], which names `path`, when writing fails.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    write_atomically(path, bytes).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes `bytes` to `path` through a temporary file in the same directory,
/// renamed into place, so readers never see a partial file. The temporary
/// file has a random name and is created exclusively (an existing file or
/// symlink there is never opened or followed). `path` keeps its mode when it
/// exists; a new file is `0600`.
///
/// # Errors
///
/// Returns the I/O error, after removing the temporary file.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|e| std::io::Error::other(e.to_string()))?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path.file_name().map_or_else(
        || "joedocs".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp = dir.join(format!(".{name}.{}.tmp", crate::auth::hex(&random)));
    write_via(path, bytes, &temp)
}

/// [`write_atomically`] through the temporary file `temp`.
fn write_via(path: &Path, bytes: &[u8], temp: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    // `create_new` is O_CREAT|O_EXCL: it fails on any existing entry,
    // including a symlink, without following it. Nothing to clean up then.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(temp, path)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    written
}

/// Runs `joedocs check` and writes its report.
///
/// # Errors
///
/// Returns [`Error::Problems`] when it finds any, store errors, and
/// [`Error::Output`].
pub fn check(store: &Store, out: &mut dyn Write) -> Result<(), Error> {
    let report = store.check()?;
    report_check(&report, store.paths(), out)?;
    match report.problems() {
        0 => Ok(()),
        count => Err(Error::Problems(count)),
    }
}

pub(crate) fn report_check(
    report: &CheckReport,
    paths: &Paths,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if report.index_behind {
        writeln!(
            out,
            "The index is behind the history: run `joedocs reindex` (serve also rebuilds it on start)."
        )?;
    }
    if !report.drifted.is_empty() {
        writeln!(
            out,
            "Vault files that are missing or differ from the history (writes to them are refused):"
        )?;
        for (id, path) in &report.drifted {
            writeln!(out, "  {id}  {path}")?;
        }
        writeln!(
            out,
            "  To discard the change, restore the committed file:\n    \
             git --git-dir '{}' --work-tree '{}' restore --source=HEAD -- '<path>'\n  \
             To keep it, copy the file aside, restore it, then send the content with update_document.",
            paths.git_dir().display(),
            paths.home.display()
        )?;
    }
    if !report.unknown_files.is_empty() {
        writeln!(
            out,
            "Vault files that belong to no document (move them out of the vault):"
        )?;
        for path in &report.unknown_files {
            writeln!(out, "  {path}")?;
        }
    }
    if !report.stale_index_files.is_empty() {
        writeln!(
            out,
            "INDEX.md files that are missing or out of date (`joedocs reindex` rewrites them):"
        )?;
        for path in &report.stale_index_files {
            writeln!(out, "  {path}")?;
        }
    }
    if report.problems() == 0 {
        writeln!(out, "No problems found.")?;
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
        KeyCommand::Ungrant { name, projects } => {
            let key = store.ungrant(name, projects)?;
            writeln!(
                out,
                "API key {} grants: {}",
                key.name,
                grant_list(&key.grants)
            )?;
        }
        KeyCommand::Rotate { name } => {
            let rotated = store.rotate_key(name, random)?;
            writeln!(
                out,
                "Rotated API key {} (id {}); the old token no longer works.",
                rotated.key.name, rotated.key.id
            )?;
            writeln!(
                out,
                "The new token is shown only now. Give it to the MCP client as:"
            )?;
            writeln!(out, "  Authorization: Bearer <token>")?;
            writeln!(out, "{}", rotated.token.expose_secret())?;
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

pub(crate) fn grant_list(grants: &[Grant]) -> String {
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
pub(crate) fn write_table(
    out: &mut dyn Write,
    header: &[&str],
    rows: &[Vec<String>],
) -> Result<(), Error> {
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
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).map_or(0, |m| m.permissions().mode() & 0o777)
    }

    #[test]
    fn exports_never_follow_or_truncate_a_planted_temporary_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let dest = dir.path().join("doc.md");
        let temp = dir.path().join(".doc.md.planted.tmp");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, &temp).unwrap();
        assert!(write_via(&dest, b"new", &temp).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(
            std::fs::symlink_metadata(&temp)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, "someone else's").unwrap();
        assert!(write_via(&dest, b"new", &temp).is_err());
        assert_eq!(std::fs::read_to_string(&temp).unwrap(), "someone else's");
        assert!(!dest.exists());
    }

    #[test]
    fn exports_keep_an_existing_mode_and_create_private_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let fresh = dir.path().join("fresh.md");
        write_atomically(&fresh, b"one").unwrap();
        assert_eq!(mode(&fresh), 0o600);
        let shared = dir.path().join("shared.md");
        std::fs::write(&shared, "old").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomically(&shared, b"two").unwrap();
        assert_eq!(
            (
                mode(&shared),
                std::fs::read_to_string(&shared).unwrap().as_str()
            ),
            (0o644, "two")
        );
        let private = dir.path().join("private.md");
        std::fs::write(&private, "old").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomically(&private, b"three").unwrap();
        assert_eq!(mode(&private), 0o600);
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_failed_export_leaves_no_temporary_file() {
        let dir = tempfile::TempDir::new().unwrap();
        // A non-empty directory cannot be replaced by a file.
        let dest = dir.path().join("taken");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("inside"), "x").unwrap();
        assert!(write_atomically(&dest, b"new").is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["taken"]);
    }

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
