//! Filesystem access behind a trait so config lookup (and the `PATH`
//! lookup of the `PlantUML` command) is unit-testable.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// The filesystem operations config resolution needs.
pub trait FileSystem {
    /// Returns `true` when `path` exists and is a regular file.
    fn is_file(&self, path: &Path) -> bool;

    /// Returns `true` when `path` is a regular file (after following
    /// symlinks) that someone may execute. `false` by default.
    fn is_executable(&self, path: &Path) -> bool {
        let _ = path;
        false
    }

    /// Reads `path` as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns any I/O or encoding error from the underlying store.
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
}

/// The real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl FileSystem for RealFs {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_executable(&self, path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// In-memory filesystem for tests.
#[derive(Debug, Default, Clone)]
pub struct MemFs {
    files: BTreeMap<PathBuf, String>,
    executables: BTreeSet<PathBuf>,
}

impl MemFs {
    /// Creates an empty filesystem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the filesystem with a file at `path`.
    #[must_use]
    pub fn with(mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        self.files.insert(path.into(), contents.into());
        self
    }

    /// Returns the filesystem with an empty executable file at `path`.
    #[must_use]
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        self.files.insert(path.clone(), String::new());
        self.executables.insert(path);
        self
    }
}

impl FileSystem for MemFs {
    fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    fn is_executable(&self, path: &Path) -> bool {
        self.executables.contains(path)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_fs_reports_files_it_holds() {
        let fs = MemFs::new().with("/a.toml", "x = 1");
        assert!(fs.is_file(Path::new("/a.toml")));
        assert!(!fs.is_file(Path::new("/b.toml")));
        assert_eq!(fs.read_to_string(Path::new("/a.toml")).unwrap(), "x = 1");
        assert_eq!(
            fs.read_to_string(Path::new("/b.toml")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn real_fs_reads_files_and_ignores_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "y = 2").unwrap();
        assert!(RealFs.is_file(&path));
        assert!(!RealFs.is_file(dir.path()));
        assert_eq!(RealFs.read_to_string(&path).unwrap(), "y = 2");
    }

    #[test]
    fn real_fs_knows_executable_files() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        assert!(!RealFs.is_executable(&path));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RealFs.is_executable(&path));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!RealFs.is_executable(dir.path()), "a directory is not");
        assert!(!RealFs.is_executable(&dir.path().join("missing")));
    }
}
