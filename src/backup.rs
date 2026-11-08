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
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clock::Clock;
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
    /// The store failed.
    #[error(transparent)]
    Store(#[from] store::Error),
}

impl Error {
    /// Whether the user can fix this by changing the request.
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        match self {
            Self::Exists(_) => true,
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
pub fn create(store: &Store, clock: &dyn Clock, file: &Path) -> Result<Created, Error> {
    if fs::symlink_metadata(file).is_ok() {
        return Err(Error::Exists(file.to_path_buf()));
    }
    let staging = Staging::new(&store.paths().state_dir)?;
    let snapshot = store.snapshot(&staging.0)?;
    let manifest = Manifest {
        format: FORMAT,
        joedocs_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: crate::db::SCHEMA_VERSION,
        created_at: crate::clock::format_timestamp(clock.now()),
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
        fs::create_dir(&dir).map_err(Error::io("create", &dir))?;
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
