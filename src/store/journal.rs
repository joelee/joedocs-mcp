//! Vault file changes that undo themselves unless the change completes.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::Error;

enum Undo {
    /// Put these bytes back.
    Restore(PathBuf, Vec<u8>),
    /// Delete the file (it did not exist before).
    Remove(PathBuf),
    /// Rename `.0` back to `.1`.
    Rename(PathBuf, PathBuf),
}

/// Records each file change so it can be undone. Dropping a journal that
/// was not [`Journal::complete`]d undoes every change, newest first.
#[derive(Default)]
pub(super) struct Journal {
    undo: Vec<Undo>,
}

impl Journal {
    /// Writes `bytes` to `path` atomically (temporary file and rename),
    /// creating parent directories.
    pub(super) fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        let previous = fs::read(path).ok();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(Error::io("create", parent))?;
        }
        write_atomic(path, bytes)?;
        self.undo.push(match previous {
            Some(bytes) => Undo::Restore(path.to_owned(), bytes),
            None => Undo::Remove(path.to_owned()),
        });
        Ok(())
    }

    /// Deletes `path`.
    pub(super) fn remove(&mut self, path: &Path) -> Result<(), Error> {
        let previous = fs::read(path).map_err(Error::io("read", path))?;
        fs::remove_file(path).map_err(Error::io("remove", path))?;
        self.undo.push(Undo::Restore(path.to_owned(), previous));
        Ok(())
    }

    /// Renames `from` to `to`.
    pub(super) fn rename(&mut self, from: &Path, to: &Path) -> Result<(), Error> {
        fs::rename(from, to).map_err(Error::io("rename", from))?;
        self.undo.push(Undo::Rename(to.to_owned(), from.to_owned()));
        Ok(())
    }

    /// Keeps every change.
    pub(super) fn complete(mut self) {
        self.undo.clear();
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        while let Some(undo) = self.undo.pop() {
            let result = match &undo {
                Undo::Restore(path, bytes) => {
                    if let Some(parent) = path.parent() {
                        drop(fs::create_dir_all(parent));
                    }
                    write_atomic(path, bytes)
                }
                Undo::Remove(path) => fs::remove_file(path).map_err(Error::io("remove", path)),
                Undo::Rename(from, to) => fs::rename(from, to).map_err(Error::io("rename", from)),
            };
            if let Err(error) = result {
                tracing::error!(%error, "cannot undo a vault change");
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.joedocs-tmp"));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    result.map_err(|source| {
        drop(fs::remove_file(&temp));
        Error::Io {
            action: "write",
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_an_incomplete_journal_undoes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.md");
        let moved = dir.path().join("moved.md");
        let new = dir.path().join("sub/new.md");
        fs::write(&kept, "old").unwrap();
        fs::write(&moved, "m").unwrap();

        let mut journal = Journal::default();
        journal.write(&kept, b"new").unwrap();
        journal.write(&new, b"n").unwrap();
        journal
            .rename(&moved, &dir.path().join("renamed.md"))
            .unwrap();
        journal.remove(&kept).unwrap();
        assert!(!kept.exists());
        drop(journal);

        assert_eq!(fs::read_to_string(&kept).unwrap(), "old");
        assert_eq!(fs::read_to_string(&moved).unwrap(), "m");
        assert!(!new.exists());
        assert!(!dir.path().join("renamed.md").exists());
    }

    #[test]
    fn completed_journals_keep_changes_and_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        let mut journal = Journal::default();
        journal.write(&path, b"x").unwrap();
        journal.complete();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failures_are_reported_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::default();
        let missing = dir.path().join("missing.md");
        let err = journal.remove(&missing).unwrap_err();
        assert!(err.to_string().starts_with("cannot read "), "{err}");
        let err = journal
            .rename(&missing, &dir.path().join("b.md"))
            .unwrap_err();
        assert!(err.to_string().starts_with("cannot rename "), "{err}");

        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        let err = journal.write(&blocker.join("a.md"), b"x").unwrap_err();
        assert!(err.to_string().starts_with("cannot create "), "{err}");
        let err = journal.write(dir.path(), b"x").unwrap_err();
        assert!(err.to_string().starts_with("cannot write "), "{err}");
    }
}
