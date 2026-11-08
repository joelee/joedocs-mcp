//! `joedocs backup`: a gzip-compressed tar archive of the vault, the
//! database and the history, taken consistently while `serve` may run.
//!
//! Layout (format 1):
//!
//! ```text
//! manifest.json            format, versions, time, history head, source paths
//! state/joedocs.db         a VACUUM INTO copy of the database
//! state/history.git/...    the history repository
//! content/...              every regular file and directory of the vault
//! ```
//!
//! The archive holds API key hashes and admin TOTP secrets, so it is
//! written with mode 0600 and must be kept private.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read as _, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{self, Store};

/// The archive format this build writes and reads.
pub const FORMAT: u32 = 1;

/// The manifest file at the root of the archive.
pub const MANIFEST: &str = "manifest.json";

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The archive format, [`FORMAT`].
    pub format: u32,
    /// The program version that wrote it.
    pub joedocs_version: String,
    /// The database schema version (`PRAGMA user_version`).
    pub schema_version: usize,
    /// When the backup was taken (UTC, RFC 3339).
    pub created_at: String,
    /// The history's latest commit in the backup.
    pub history_head: Option<String>,
    /// Live documents in the backup.
    pub documents: usize,
    /// Where the vault was (for information only).
    pub vault: PathBuf,
    /// Where the state directory was (for information only).
    pub state_dir: PathBuf,
}

/// Backup and restore failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The output file exists.
    #[error("{0} already exists; choose another file name")]
    Exists(PathBuf),
    /// Reading or writing a file failed.
    #[error("cannot {action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// A restore target holds files.
    #[error("{0} is not empty; restore needs empty or absent directories")]
    NotEmpty(PathBuf),
    /// The archive is not one this build can restore.
    #[error("cannot restore {file}: {reason}")]
    Unsupported {
        /// The archive.
        file: PathBuf,
        /// Why.
        reason: String,
    },
    /// The store failed.
    #[error(transparent)]
    Store(#[from] store::Error),
}

impl Error {
    /// Whether the user can fix this by changing the request.
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        match self {
            Self::Exists(_) | Self::NotEmpty(_) | Self::Unsupported { .. } => true,
            Self::Io { .. } => false,
            Self::Store(error) => error.is_user_error(),
        }
    }

    fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

/// What [`create`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The manifest stored in the archive.
    pub manifest: Manifest,
    /// The archive's size in bytes.
    pub bytes: u64,
    /// Vault symlinks that were left out.
    pub skipped_links: Vec<String>,
}

/// Writes a backup of `store` to `file`, which must not exist yet.
///
/// # Errors
///
/// Returns [`Error::Exists`] when `file` exists, and I/O or store errors.
pub fn create(store: &Store, file: &Path) -> Result<Created, Error> {
    if fs::symlink_metadata(file).is_ok() {
        return Err(Error::Exists(file.to_path_buf()));
    }
    let staging = Staging::new(&store.paths().state_dir)?;
    let snapshot = store.snapshot(&staging.0)?;
    let manifest = Manifest {
        format: FORMAT,
        joedocs_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: crate::db::SCHEMA_VERSION,
        created_at: store.now(),
        history_head: snapshot.history_head,
        documents: snapshot.documents,
        vault: store.paths().home.clone(),
        state_dir: store.paths().state_dir.clone(),
    };
    let temp = temp_beside(file);
    let written = write_private(&temp, |out| write_archive(&staging.0, &manifest, out));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if fs::symlink_metadata(file).is_ok() {
        let _ = fs::remove_file(&temp);
        return Err(Error::Exists(file.to_path_buf()));
    }
    fs::rename(&temp, file).map_err(Error::io("write", file))?;
    let bytes = fs::metadata(file).map_err(Error::io("read", file))?.len();
    Ok(Created {
        manifest,
        bytes,
        skipped_links: snapshot.skipped_links,
    })
}

/// Reads the manifest at the start of the archive `file`.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when `file` is not a backup.
pub fn read_manifest(file: &Path) -> Result<Manifest, Error> {
    let unsupported = |reason: String| Error::Unsupported {
        file: file.to_path_buf(),
        reason,
    };
    let archive_file = File::open(file).map_err(Error::io("read", file))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive_file));
    let mut entries = archive
        .entries()
        .map_err(|e| unsupported(format!("not a gzip tar archive ({e})")))?;
    match entries.next() {
        Some(Ok(mut entry)) if entry.path().is_ok_and(|p| p == Path::new(MANIFEST)) => {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| unsupported(format!("unreadable {MANIFEST} ({e})")))?;
            serde_json::from_str(&text)
                .map_err(|e| unsupported(format!("invalid {MANIFEST} ({e})")))
        }
        _ => Err(unsupported(format!("it does not start with {MANIFEST}"))),
    }
}

/// Writes the archive for a staged snapshot to `out`.
///
/// # Errors
///
/// Returns I/O errors.
pub fn write_archive(staging: &Path, manifest: &Manifest, out: impl Write) -> Result<(), Error> {
    let gzip = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    tar.follow_symlinks(false);
    let json = serde_json::to_vec_pretty(manifest).map_err(|e| Error::Io {
        action: "encode",
        path: PathBuf::from(MANIFEST),
        source: io::Error::other(e),
    })?;
    let mut header = tar::Header::new_gnu();
    header.set_size(json.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    let failed = |source| Error::Io {
        action: "write",
        path: PathBuf::from("the archive"),
        source,
    };
    tar.append_data(&mut header, MANIFEST, json.as_slice())
        .map_err(failed)?;
    for part in ["state", "content"] {
        tar.append_dir_all(part, staging.join(part))
            .map_err(failed)?;
    }
    let gzip = tar.into_inner().map_err(failed)?;
    gzip.finish().map_err(failed)?.flush().map_err(failed)?;
    Ok(())
}

/// Creates `path` with mode 0600 (failing if it exists) and runs `write`
/// on a buffered writer for it.
fn write_private(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), Error>,
) -> Result<(), Error> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(Error::io("create", path))?;
    let mut out = BufWriter::new(file);
    write(&mut out)?;
    out.flush().map_err(Error::io("write", path))?;
    out.get_ref().sync_all().map_err(Error::io("write", path))
}

/// `dir/name` → `dir/.name.<pid>.tmp`.
fn temp_beside(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map_or_else(|| "backup".into(), |n| n.to_string_lossy().into_owned());
    let dir = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    dir.join(format!(".{name}.{}.tmp", std::process::id()))
}

/// A staging directory in the state directory, removed when dropped.
struct Staging(PathBuf);

impl Staging {
    fn new(state_dir: &Path) -> Result<Self, Error> {
        let dir = state_dir.join(format!("backup-staging-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(Error::io("remove", &dir))?;
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(Error::io("create", &dir))?;
        Ok(Self(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            tracing::warn!(path = %self.0.display(), %error, "cannot remove the backup staging directory");
        }
    }
}

/// Extracts the backup `file` into `state_dir` and `content_dir`, which must
/// be empty or absent. On any failure the targets are emptied again (and
/// removed when this created them). Returns the archive's manifest.
///
/// Only regular files and directories under `state/` and `content/` are
/// accepted; absolute paths, `..`, links and devices are refused before
/// anything is written for them.
///
/// # Errors
///
/// Returns [`Error::NotEmpty`], [`Error::Unsupported`] and I/O errors.
pub fn extract(file: &Path, state_dir: &Path, content_dir: &Path) -> Result<Manifest, Error> {
    let targets = [
        Target::prepare(state_dir, true)?,
        Target::prepare(content_dir, false)?,
    ];
    let result = extract_into(file, state_dir, content_dir);
    if result.is_err() {
        for target in &targets {
            target.undo();
        }
    }
    result
}

fn extract_into(file: &Path, state_dir: &Path, content_dir: &Path) -> Result<Manifest, Error> {
    let unsupported = |reason: String| Error::Unsupported {
        file: file.to_path_buf(),
        reason,
    };
    let archive_file = File::open(file).map_err(Error::io("read", file))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive_file));
    let mut entries = archive
        .entries()
        .map_err(|e| unsupported(format!("not a gzip tar archive ({e})")))?;
    let manifest = match entries.next() {
        Some(Ok(mut entry)) if entry.path().is_ok_and(|p| p == Path::new(MANIFEST)) => {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| unsupported(format!("unreadable {MANIFEST} ({e})")))?;
            serde_json::from_str::<Manifest>(&text)
                .map_err(|e| unsupported(format!("invalid {MANIFEST} ({e})")))?
        }
        _ => return Err(unsupported(format!("it does not start with {MANIFEST}"))),
    };
    if manifest.format != FORMAT {
        return Err(unsupported(format!(
            "it is backup format {}, and this build reads format {FORMAT}",
            manifest.format
        )));
    }
    if manifest.schema_version > crate::db::SCHEMA_VERSION {
        return Err(unsupported(format!(
            "it holds database schema version {}, newer than this build supports ({}); install a newer JoeDocs",
            manifest.schema_version,
            crate::db::SCHEMA_VERSION
        )));
    }
    for entry in entries {
        let mut entry = entry.map_err(|e| unsupported(format!("unreadable entry ({e})")))?;
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let refuse = |why: &str| unsupported(format!("refusing the entry {raw}: {why}"));
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(refuse("only regular files and directories are restored"));
        }
        let relative = Path::new(&raw);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(refuse("absolute paths and `..` are not allowed"));
        }
        let mut parts = relative.components();
        let root = match parts.next() {
            Some(Component::Normal(first)) if first == "state" => state_dir,
            Some(Component::Normal(first)) if first == "content" => content_dir,
            _ => return Err(refuse("entries must be under state/ or content/")),
        };
        let target = root.join(parts.as_path());
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(Error::io("create", &target))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .map_err(Error::io("create", &target))?;
        io::copy(&mut entry, &mut out).map_err(Error::io("write", &target))?;
    }
    Ok(manifest)
}

/// A restore target, and whether this run created it.
struct Target {
    path: PathBuf,
    created: bool,
}

impl Target {
    fn prepare(path: &Path, private: bool) -> Result<Self, Error> {
        match fs::read_dir(path) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(Error::NotEmpty(path.to_path_buf()));
                }
                Ok(Self {
                    path: path.to_path_buf(),
                    created: false,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mode = if private { 0o700 } else { 0o755 };
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(mode)
                    .create(path)
                    .map_err(Error::io("create", path))?;
                Ok(Self {
                    path: path.to_path_buf(),
                    created: true,
                })
            }
            Err(error) => Err(Error::io("read", path)(error)),
        }
    }

    /// Removes what was extracted.
    fn undo(&self) {
        if self.created {
            let _ = fs::remove_dir_all(&self.path);
        } else if let Ok(entries) = fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).map_or(0, |m| m.permissions().mode() & 0o777)
    }

    #[test]
    fn staging_and_restored_state_are_private_from_the_start() {
        let dir = tempfile::TempDir::new().unwrap();
        let staging = Staging::new(dir.path()).unwrap();
        assert_eq!(mode(&staging.0), 0o700);
        let state = dir.path().join("restored/state");
        let target = Target::prepare(&state, true).unwrap();
        assert!(target.created);
        assert_eq!(mode(&state), 0o700);
    }
}
