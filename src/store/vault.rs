//! Checks on vault paths before a change writes to them: whether anything
//! is there, and whether two paths are the same file.
//!
//! None of them follows a symlink: a symlink, dangling or not, is an entry
//! in its own right, so it is never written over or followed out of the
//! vault. Two paths are the same file only when the filesystem says so
//! (`(dev, ino)`), which a case-insensitive filesystem does for names that
//! differ only in case and a case-sensitive one does not.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use super::Error;

/// Whether anything (a file, a directory, a symlink) is at `path`.
///
/// # Errors
///
/// Returns I/O errors other than "not found".
pub(super) fn occupied(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::io("read", path)(error)),
    }
}

/// Refuses with `already_exists` when anything is at `vault` (a path where
/// the index has no live file), so it is never written over.
///
/// # Errors
///
/// Returns [`Error::AlreadyExists`], and I/O errors.
pub(super) fn refuse_unknown(home: &Path, vault: &str) -> Result<(), Error> {
    if occupied(&home.join(vault))? {
        return Err(Error::AlreadyExists(format!(
            "a file the index does not know at {vault}"
        )));
    }
    Ok(())
}

/// Whether `a` and `b` are the same filesystem entry (both exist, and have
/// the same device and inode). Symlinks are not followed.
///
/// # Errors
///
/// Returns I/O errors other than "not found".
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the case-only rename checks of JD-16 STEP-02 use it"
    )
)]
pub(super) fn same_entry(a: &Path, b: &Path) -> Result<bool, Error> {
    let stat = |path: &Path| match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), meta.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io("read", path)(error)),
    };
    Ok(match (stat(a)?, stat(b)?) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    })
}

/// Refuses moving the entry at `from` to `to` (both vault-relative) when
/// something else is at `to`. A case-only rename onto the same entry, as on
/// a case-insensitive filesystem, is allowed.
///
/// # Errors
///
/// Returns [`Error::AlreadyExists`], and I/O errors.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the case-only rename checks of JD-16 STEP-02 use it"
    )
)]
pub(super) fn refuse_occupied(home: &Path, from: &str, to: &str) -> Result<(), Error> {
    let target = home.join(to);
    if occupied(&target)? && !same_entry(&home.join(from), &target)? {
        return Err(Error::AlreadyExists(format!(
            "a file the index does not know at {to}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `dir` treats names that differ only in case as one.
    fn folds_case(dir: &Path) -> bool {
        let probe = dir.join("case-probe");
        fs::write(&probe, "").unwrap();
        let folds = dir.join("CASE-PROBE").exists();
        fs::remove_file(probe).unwrap();
        folds
    }

    #[test]
    fn nothing_is_free_except_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::write(home.join("file.md"), "x").unwrap();
        fs::create_dir(home.join("dir")).unwrap();
        std::os::unix::fs::symlink("nowhere", home.join("dangling.md")).unwrap();
        std::os::unix::fs::symlink("file.md", home.join("live.md")).unwrap();
        for taken in ["file.md", "dir", "dangling.md", "live.md"] {
            let err = refuse_unknown(home, taken).unwrap_err();
            assert!(matches!(err, Error::AlreadyExists(_)), "{taken}: {err}");
        }
        refuse_unknown(home, "free.md").unwrap();
        assert!(!occupied(&home.join("free.md")).unwrap());
    }

    #[test]
    fn the_same_entry_means_the_same_file_not_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::write(home.join("Notes.md"), "x").unwrap();
        std::os::unix::fs::symlink("Notes.md", home.join("link.md")).unwrap();
        assert!(same_entry(&home.join("Notes.md"), &home.join("Notes.md")).unwrap());
        assert!(!same_entry(&home.join("Notes.md"), &home.join("link.md")).unwrap());
        assert!(!same_entry(&home.join("Notes.md"), &home.join("missing.md")).unwrap());
        if folds_case(home) {
            // One file under two spellings: a case-only rename is allowed.
            assert!(same_entry(&home.join("Notes.md"), &home.join("notes.md")).unwrap());
            refuse_occupied(home, "Notes.md", "notes.md").unwrap();
        } else {
            refuse_occupied(home, "Notes.md", "notes.md").unwrap();
            fs::write(home.join("notes.md"), "other").unwrap();
            assert!(!same_entry(&home.join("Notes.md"), &home.join("notes.md")).unwrap());
            let err = refuse_occupied(home, "Notes.md", "notes.md").unwrap_err();
            assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
        }
        let err = refuse_occupied(home, "Notes.md", "link.md").unwrap_err();
        assert!(matches!(err, Error::AlreadyExists(_)), "{err}");
    }
}
