//! Filesystem access behind a trait so config lookup is unit-testable.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The filesystem operations config resolution needs.
pub trait FileSystem {
    /// Returns `true` when `path` exists and is a regular file.
    fn is_file(&self, path: &Path) -> bool;

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

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// In-memory filesystem for tests.
#[derive(Debug, Default, Clone)]
pub struct MemFs {
    files: BTreeMap<PathBuf, String>,
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
}

impl FileSystem for MemFs {
    fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
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
}
