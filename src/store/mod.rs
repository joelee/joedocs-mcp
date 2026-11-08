//! Document use cases behind every MCP tool and admin command (create, get,
//! update, edit, append, rename, move, change front matter, restore a
//! revision, archive and unarchive, list, search, history and diff;
//! projects and API keys; reindex and check), the single-writer lock, and
//! the ordering of the vault file, history and database steps.
//!
//! Every document change runs in this order (see `docs/plan.md` section 11):
//!
//! 1. take the write lock and check the index matches the history;
//! 2. open a database transaction and validate the change;
//! 3. check the file on disk is the one the index recorded (the integrity
//!    guard);
//! 4. write the vault files, remembering how to undo each write;
//! 5. commit to history;
//! 6. record the revision and the new history head, and commit the
//!    transaction.
//!
//! A failure before step 5 undoes the file writes and rolls the
//! transaction back, so nothing changes.

mod admin_tokens;
mod archive;
mod backup;
mod check;
mod documents;
mod editing;
mod index_file;
mod journal;
mod registry;
mod reindex;
mod revisions;
mod search;
mod summary;

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use admin_tokens::{
    AdminRefusal, CreatedAdminToken, LOCK_SECONDS, MAX_FAILED_LOGINS, OpenedSession,
};
pub use archive::ArchivedInfo;
pub use backup::Snapshot;
pub use check::CheckReport;
pub use documents::{
    Archived, Changed, DocumentInfo, Fetched, ListRequest, Listing, MAX_LIST_LIMIT, NewDocument,
    RevisionInfo, Update,
};
pub use editing::{MAX_EDITS, Replacement};
pub use registry::CreatedKey;
pub use reindex::Reindexed;
pub use revisions::{Diff, HistoryPage, MAX_DIFF_CONTEXT, MAX_HISTORY_LIMIT};
pub use search::{MAX_SEARCH_LIMIT, SearchHit, SearchRequest, SearchResults};
pub use summary::{ProjectCount, Summary};

use crate::AppId;
use crate::clock::Clock;
use crate::config::{Config, ConfigError};
use crate::db::{self, Connection, DbError};
pub use crate::db::{AdminTokenRow, KeyRow, ProjectRow, Sort};
use crate::env::Env;
use crate::history::{History, HistoryError};

/// A store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An argument broke a rule.
    #[error("invalid {field}: {message}")]
    Invalid {
        /// The argument at fault.
        field: &'static str,
        /// What is wrong with it.
        message: String,
    },
    /// No document has this ID.
    #[error("document {0} does not exist")]
    NotFound(String),
    /// The document is archived.
    #[error("document {0} is archived")]
    Archived(String),
    /// No project has this name.
    #[error("project {0} does not exist")]
    ProjectNotFound(String),
    /// No API key has this name.
    #[error("API key {0} does not exist")]
    KeyNotFound(String),
    /// Something with this name already exists.
    #[error("{0} already exists")]
    AlreadyExists(String),
    /// The document changed since the client read it.
    #[error(
        "document {id} is at revision {current}, not {expected}; read it again and reapply the change"
    )]
    Conflict {
        /// Document ID.
        id: String,
        /// Revision the client expected.
        expected: u64,
        /// Current revision.
        current: u64,
    },
    /// The file on disk is not the one the store last wrote.
    #[error(
        "document {id}: {path} was changed or removed outside JoeDocs, so it will not be overwritten"
    )]
    Integrity {
        /// Document ID.
        id: String,
        /// Vault-relative path.
        path: String,
    },
    /// The document would exceed `server.max_document_bytes`.
    #[error(
        "the document would be {size} bytes, over the limit of {limit} (server.max_document_bytes)"
    )]
    TooLarge {
        /// Size of the serialised document.
        size: u64,
        /// The configured limit.
        limit: u64,
    },
    /// `joedocs init` has not been run.
    #[error("JoeDocs is not initialised (state directory {0}); run `joedocs init`")]
    NotInitialised(PathBuf),
    /// `storage.home` has files but no history.
    #[error(
        "{0} is not empty and has no JoeDocs history; choose an empty directory for storage.home"
    )]
    HomeNotEmpty(PathBuf),
    /// The database index does not match the history.
    #[error(
        "the index records history commit {index} but the history is at {history}; \
         a previous write was interrupted, so writes are refused until the index is rebuilt \
         (run `joedocs reindex`, or restart `joedocs serve`, which rebuilds it)"
    )]
    OutOfSync {
        /// Commit the index records.
        index: String,
        /// Commit the history is at.
        history: String,
    },
    /// The history cannot be replayed into the index.
    #[error("cannot rebuild the index from history commit {commit}: {message}")]
    Rebuild {
        /// The commit at fault.
        commit: String,
        /// What is wrong with it.
        message: String,
    },
    /// The write lock could not be taken.
    #[error("cannot lock {path}: {source}")]
    Lock {
        /// Lock file.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// A vault or state file could not be read or written.
    #[error("cannot {action} {path}: {source}")]
    Io {
        /// What was being done, e.g. `write`.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Cause.
        source: io::Error,
    },
    /// An admin login or session was refused.
    #[error(transparent)]
    AdminRefused(AdminRefusal),
    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The history failed.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// Random bytes were unavailable.
    #[error(transparent)]
    Random(#[from] crate::auth::RandomError),
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(error))
    }
}

impl Error {
    /// Whether the caller can fix this by changing the request (as opposed
    /// to a fault in the server, its files or its tools).
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::Invalid { .. }
                | Self::NotFound(_)
                | Self::Archived(_)
                | Self::ProjectNotFound(_)
                | Self::KeyNotFound(_)
                | Self::AlreadyExists(_)
                | Self::Conflict { .. }
                | Self::TooLarge { .. }
                | Self::NotInitialised(_)
                | Self::HomeNotEmpty(_)
        )
    }

    pub(crate) fn invalid(field: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            message: message.into(),
        }
    }

    pub(crate) fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_owned();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

/// Where the vault and the state live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The vault (`storage.home`).
    pub home: PathBuf,
    /// Database, history and lock (`storage.state_dir`).
    pub state_dir: PathBuf,
}

impl Paths {
    /// Resolves the paths from configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Missing`] when `storage.home` is unset, or when
    /// no state directory can be derived.
    pub fn from_config(config: &Config, app: &AppId, env: &dyn Env) -> Result<Self, ConfigError> {
        Ok(Self {
            home: config.storage.require_home(app)?.to_owned(),
            state_dir: config.storage.resolve_state_dir(app, env)?,
        })
    }

    /// The SQLite database.
    #[must_use]
    pub fn db(&self) -> PathBuf {
        self.state_dir.join("joedocs.db")
    }

    /// The git directory.
    #[must_use]
    pub fn git_dir(&self) -> PathBuf {
        self.state_dir.join("history.git")
    }

    /// The lock file that serialises writers.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("joedocs.lock")
    }
}

/// The document rules taken from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Allowed document types.
    pub types: Vec<String>,
    /// Allowed statuses.
    pub statuses: Vec<String>,
    /// Type used when none is given.
    pub default_type: String,
    /// Status used when none is given.
    pub default_status: String,
    /// Largest serialised document, in bytes.
    pub max_document_bytes: u64,
    /// Whether each project folder gets a generated `INDEX.md`.
    pub index_file: bool,
}

impl Settings {
    /// Takes the settings from validated configuration.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            types: config.documents.types.clone(),
            statuses: config.documents.statuses.clone(),
            default_type: config.documents.default_type.clone(),
            default_status: config.documents.default_status.clone(),
            max_document_bytes: config.server.max_document_bytes,
            index_file: config.documents.index_file,
        }
    }
}

/// Longest client name kept, in characters.
const MAX_CLIENT_CHARS: usize = 64;

/// Who is making a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    name: String,
    client: String,
}

impl Actor {
    /// `name` is the API key name (trusted); `client` is what the MCP client
    /// calls itself, tidied to one short line.
    #[must_use]
    pub fn new(name: impl Into<String>, client: &str) -> Self {
        let client: String = client
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_CLIENT_CHARS)
            .collect();
        Self {
            name: name.into(),
            client: if client.is_empty() {
                "unknown".to_owned()
            } else {
                client
            },
        }
    }

    /// The API key name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The client name.
    #[must_use]
    pub fn client(&self) -> &str {
        &self.client
    }
}

/// What [`Store::init`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitOutcome {
    /// Created the vault, database and history.
    Created,
    /// Everything already existed; the database schema was brought up to
    /// date.
    AlreadyInitialised,
}

/// The document store.
pub struct Store {
    paths: Paths,
    settings: Settings,
    conn: Connection,
    history: Box<dyn History>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("paths", &self.paths)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Holds the write lock until dropped.
#[derive(Debug)]
struct WriteLock(File);

impl Drop for WriteLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "cannot release the write lock");
        }
    }
}

/// Creates `dir` (and missing parents) with mode `0700`, so no other
/// account can read the database or the admin TOTP secrets in it.
fn create_private_dir(dir: &Path) -> Result<(), Error> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(Error::io("create", dir))
}

/// Makes the state directory `0700` and the database files `0600` when
/// group or other can access them. Existing installs predate the rule, so
/// each change is logged (`quiet` only for files this run just created). A
/// change that fails is logged with the fix, and the store still opens.
fn protect_state(paths: &Paths, quiet: bool) {
    let files = ["joedocs.db", "joedocs.db-wal", "joedocs.db-shm"]
        .map(|name| (paths.state_dir.join(name), 0o600));
    for (path, private) in std::iter::once((paths.state_dir.clone(), 0o700)).chain(files) {
        make_private(&path, private, quiet);
    }
}

fn make_private(path: &Path, private: u32, quiet: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode.trailing_zeros() >= 6 {
        return;
    }
    match fs::set_permissions(path, fs::Permissions::from_mode(private)) {
        Ok(()) if quiet => {
            tracing::debug!(path = %path.display(), "made private");
        }
        Ok(()) => tracing::warn!(
            path = %path.display(),
            old_mode = format!("{mode:o}"),
            "made {} private: it holds API key hashes and admin TOTP secrets",
            path.display()
        ),
        Err(error) => tracing::warn!(
            path = %path.display(),
            %error,
            "{} is readable by other accounts; fix it with: chmod {private:o} '{}'",
            path.display(),
            path.display()
        ),
    }
}

fn lock(paths: &Paths) -> Result<WriteLock, Error> {
    let path = paths.lock_file();
    let lock_error = |source| Error::Lock {
        path: path.clone(),
        source,
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(lock_error)?;
    file.lock().map_err(lock_error)?;
    Ok(WriteLock(file))
}

impl Store {
    /// Creates the vault and state directories, the database and the
    /// history. Running it again on an initialised store only migrates the
    /// database.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HomeNotEmpty`] when the vault directory already has
    /// files but no history, and I/O, database or history errors.
    pub fn init(paths: &Paths, history: &dyn History) -> Result<InitOutcome, Error> {
        create_private_dir(&paths.state_dir)?;
        let _lock = lock(paths)?;
        let existed = history.exists() && paths.db().is_file();
        if !history.exists() && has_entries(&paths.home)? {
            return Err(Error::HomeNotEmpty(paths.home.clone()));
        }
        fs::create_dir_all(&paths.home).map_err(Error::io("create", &paths.home))?;
        history.init()?;
        index_file::exclude_from_history(&paths.git_dir())?;
        let conn = db::open(&paths.db())?;
        protect_state(paths, !existed);
        if history.head()?.is_none() {
            // An empty history needs no rebuild.
            db::set_index_format(&conn)?;
        }
        Ok(if existed {
            InitOutcome::AlreadyInitialised
        } else {
            InitOutcome::Created
        })
    }

    /// Opens an initialised store.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotInitialised`] before `init`, and database or
    /// history errors.
    pub fn open(
        paths: Paths,
        settings: Settings,
        history: Box<dyn History>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        if !paths.db().is_file() || !history.exists() || !paths.home.is_dir() {
            return Err(Error::NotInitialised(paths.state_dir));
        }
        let conn = db::open(&paths.db())?;
        protect_state(&paths, false);
        // Older installs predate the rule; it only tidies `git status`.
        if let Err(error) = index_file::exclude_from_history(&paths.git_dir()) {
            tracing::warn!(%error, "could not add INDEX.md to the history's exclude file");
        }
        Ok(Self {
            paths,
            settings,
            conn,
            history,
            clock,
        })
    }

    /// The paths this store uses.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The rules this store enforces.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The store clock's current time, formatted as in the database.
    #[must_use]
    pub fn now(&self) -> String {
        crate::clock::format_timestamp(self.clock.now())
    }
}

/// Takes the write lock and checks the index matches the history.
fn begin_write(
    paths: &Paths,
    conn: &Connection,
    history: &dyn History,
) -> Result<WriteLock, Error> {
    let guard = lock(paths)?;
    let index = db::history_head(conn)?;
    let head = history.head()?;
    if index != head {
        return Err(Error::OutOfSync {
            index: index.unwrap_or_else(|| "(none)".to_owned()),
            history: head.unwrap_or_else(|| "(none)".to_owned()),
        });
    }
    Ok(guard)
}

fn has_entries(dir: &Path) -> Result<bool, Error> {
    match fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::io("read", dir)(error)),
    }
}

#[cfg(test)]
mod tests;
