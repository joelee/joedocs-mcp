//! The files of a multi-file document: paths inside the document folder.
//!
//! A multi-file document lives in `<type>/<stem>/`: the main file is
//! `<stem>/<stem>.md` and every other file (a sub-file) sits at a path
//! inside that folder, such as `backlog/item-1.md`. Clients name a sub-file
//! by that path, with or without `.md`.

use super::title::{FORBIDDEN, MAX_STEM_BYTES};

/// Most directory levels a sub-file may sit below the document folder.
pub const MAX_FILE_DEPTH: usize = 4;

/// The generated file listing a folder, never a document file.
pub const INDEX_STEM: &str = "INDEX";

/// A checked sub-file path, relative to the document folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FilePath {
    /// The directories, then the file stem (without `.md`).
    segments: Vec<String>,
}

impl FilePath {
    /// Checks `raw`, a sub-file path given with or without `.md`, for the
    /// document whose main file stem is `main_stem`.
    ///
    /// # Errors
    ///
    /// Returns why the path cannot be used: an empty, absolute or `..` path,
    /// an empty segment, a control or ``* " \ < > : | ? # ^ [ ]``
    /// character, a segment starting with `.` or ending with a space or `.`,
    /// a segment over [`MAX_STEM_BYTES`] bytes, more than
    /// [`MAX_FILE_DEPTH`] directories, a file named `INDEX` (in any case),
    /// or the main file's own name at the top level.
    ///
    /// # Examples
    ///
    /// ```
    /// use joedocs::document::FilePath;
    ///
    /// let path = FilePath::parse("backlog/item-1.md", "Roadmap").unwrap();
    /// assert_eq!(path.as_str(), "backlog/item-1");
    /// assert_eq!(path.file_name(), "backlog/item-1.md");
    /// assert!(FilePath::parse("../escape", "Roadmap").is_err());
    /// ```
    pub fn parse(raw: &str, main_stem: &str) -> Result<Self, String> {
        let without_md = if raw.len() >= 3
            && raw
                .get(raw.len() - 3..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".md"))
        {
            &raw[..raw.len() - 3]
        } else {
            raw
        };
        if without_md.is_empty() {
            return Err("must not be empty".into());
        }
        if without_md.starts_with('/') {
            return Err("must be a path inside the document folder, not an absolute path".into());
        }
        let segments: Vec<String> = without_md.split('/').map(str::to_owned).collect();
        for segment in &segments {
            check_segment(segment)?;
        }
        if segments.len() - 1 > MAX_FILE_DEPTH {
            return Err(format!("must be at most {MAX_FILE_DEPTH} directories deep"));
        }
        let path = Self { segments };
        if path.stem().eq_ignore_ascii_case(INDEX_STEM) {
            return Err(format!(
                "\"{INDEX_STEM}\" is reserved for the folder's generated file list"
            ));
        }
        if path.segments.len() == 1 && path.stem().to_lowercase() == main_stem.to_lowercase() {
            return Err(format!("\"{}\" is the main file's name", path.stem()));
        }
        Ok(path)
    }

    /// The path without `.md`, e.g. `backlog/item-1`.
    #[must_use]
    pub fn as_str(&self) -> String {
        self.segments.join("/")
    }

    /// The path with `.md`, e.g. `backlog/item-1.md`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.md", self.as_str())
    }

    /// The file's stem, the last segment.
    #[must_use]
    pub fn stem(&self) -> &str {
        self.segments.last().map_or("", String::as_str)
    }

    /// The directories above the file, outermost first.
    #[must_use]
    pub fn directories(&self) -> &[String] {
        self.segments.split_last().map_or(&[], |(_, dirs)| dirs)
    }

    /// A key equal for paths that differ only in case, for collision
    /// checks on case-insensitive file systems.
    #[must_use]
    pub fn key(&self) -> String {
        self.as_str().to_lowercase()
    }
}

/// The first folder of `path` (a sub-file path without `.md`) whose name
/// differs only in case from the same folder of one of `used`, as `(path's
/// spelling, the used spelling)`, outermost first.
///
/// A case-insensitive file system has one folder for both spellings, so a
/// file there would not be where the index says. Top-level files have no
/// folder and never clash.
///
/// # Examples
///
/// ```
/// use joedocs::document::folder_case_clash;
///
/// let clash = folder_case_clash("Backlog/b", ["backlog/a"]);
/// assert_eq!(clash, Some(("Backlog".to_owned(), "backlog".to_owned())));
/// assert_eq!(folder_case_clash("backlog/b", ["backlog/a"]), None);
/// ```
pub fn folder_case_clash<'a>(
    path: &str,
    used: impl IntoIterator<Item = &'a str>,
) -> Option<(String, String)> {
    fn folders(path: &str) -> Vec<&str> {
        path.rsplit_once('/')
            .map_or_else(Vec::new, |(dirs, _)| dirs.split('/').collect())
    }
    let ours = folders(path);
    used.into_iter().find_map(|other| {
        let theirs = folders(other);
        (1..=ours.len().min(theirs.len())).find_map(|depth| {
            let (a, b) = (ours[..depth].join("/"), theirs[..depth].join("/"));
            (a != b && a.to_lowercase() == b.to_lowercase()).then_some((a, b))
        })
    })
}

impl std::fmt::Display for FilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

fn check_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty() {
        return Err("must not have empty segments (`//` or a trailing `/`)".into());
    }
    if segment == ".." || segment == "." {
        return Err("must not contain `.` or `..` segments".into());
    }
    if segment.chars().any(char::is_control) {
        return Err("must not contain control characters".into());
    }
    if let Some(bad) = segment.chars().find(|c| FORBIDDEN.contains(c)) {
        return Err(format!(
            "must not contain `{bad}` (none of * \" \\ < > : | ? # ^ [ ])"
        ));
    }
    if segment.starts_with('.') {
        return Err(format!("\"{segment}\" must not start with `.`"));
    }
    if segment.ends_with('.') || segment.ends_with(' ') {
        return Err(format!("\"{segment}\" must not end with a space or `.`"));
    }
    if segment.len() > MAX_STEM_BYTES {
        return Err(format!("each part must be at most {MAX_STEM_BYTES} bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_paths_are_normalised_without_md() {
        for (raw, want, dirs) in [
            ("notes", "notes", 0),
            ("notes.md", "notes", 0),
            ("notes.MD", "notes", 0),
            ("backlog/item-1", "backlog/item-1", 1),
            ("a/b/c/d/e.md", "a/b/c/d/e", 4),
            ("Ünïcödé file", "Ünïcödé file", 0),
            ("v1.2 notes", "v1.2 notes", 0),
            ("index-notes", "index-notes", 0),
            ("INDEX/notes", "INDEX/notes", 1),
            ("Roadmap/Roadmap", "Roadmap/Roadmap", 1),
        ] {
            let path = FilePath::parse(raw, "Roadmap").unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(path.as_str(), want, "{raw}");
            assert_eq!(path.file_name(), format!("{want}.md"));
            assert_eq!(path.directories().len(), dirs, "{raw}");
        }
    }

    #[test]
    fn unsafe_or_reserved_paths_are_refused() {
        let long = "a".repeat(MAX_STEM_BYTES + 1);
        let long_dir = format!("{long}/x");
        for (raw, why) in [
            ("", "empty"),
            (".md", "empty"),
            ("/etc/passwd", "absolute"),
            ("../escape", "`..`"),
            ("a/../b", "`..`"),
            ("./a", "`.`"),
            ("a//b", "empty segments"),
            ("a/", "empty segments"),
            (".hidden", "start with `.`"),
            ("a/.git/x", "start with `.`"),
            ("trailing.", "end with a space"),
            ("trailing ", "end with a space"),
            ("tab\tname", "control"),
            ("a:b", "`:`"),
            ("a\\b", "`\\`"),
            ("[[link]]", "`[`"),
            ("what?", "`?`"),
            ("a#b", "`#`"),
            ("a/b/c/d/e/f", "at most 4"),
            (long.as_str(), "at most 100 bytes"),
            (long_dir.as_str(), "at most 100 bytes"),
            ("INDEX", "reserved"),
            ("index.md", "reserved"),
            ("backlog/Index", "reserved"),
            ("Roadmap", "main file"),
            ("roadmap.md", "main file"),
        ] {
            let err = FilePath::parse(raw, "Roadmap").unwrap_err();
            assert!(err.contains(why), "{raw:?}: {err}");
        }
        assert!(FilePath::parse(&"a".repeat(MAX_STEM_BYTES), "Roadmap").is_ok());
    }

    #[test]
    fn folders_differing_only_in_case_clash() {
        let clash = |a: &str, b: &str| Some((a.to_owned(), b.to_owned()));
        assert_eq!(
            folder_case_clash("Backlog/b", ["backlog/a"]),
            clash("Backlog", "backlog")
        );
        assert_eq!(folder_case_clash("a/B/x", ["a/b/c"]), clash("a/B", "a/b"));
        assert_eq!(folder_case_clash("A/b/x", ["a/b/c"]), clash("A", "a"));
        for fine in ["backlog/b", "Backlog2/b", "backlog/A", "top"] {
            assert_eq!(
                folder_case_clash(fine, ["backlog/a", "other"]),
                None,
                "{fine}"
            );
        }
        // A top-level file is not a folder.
        assert_eq!(folder_case_clash("Notes/x", ["notes"]), None);
    }

    #[test]
    fn keys_ignore_case() {
        let a = FilePath::parse("Backlog/Item", "Main").unwrap();
        let b = FilePath::parse("backlog/ITEM.md", "Main").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.key(), b.key());
        assert_eq!(a.stem(), "Item");
        assert_eq!(a.to_string(), "Backlog/Item");
    }
}
