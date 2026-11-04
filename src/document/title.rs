//! Document titles and the file names derived from them.

/// Longest accepted title, in characters.
pub const MAX_TITLE_CHARS: usize = 200;

/// Longest file stem (the file name without `.md`), in bytes.
pub const MAX_STEM_BYTES: usize = 100;

/// Characters Obsidian rejects in file names, or that break `[[links]]`.
const FORBIDDEN: &[char] = &[
    '*', '"', '\\', '/', '<', '>', ':', '|', '?', '#', '^', '[', ']',
];

/// Checks a client-supplied title and returns it trimmed.
///
/// # Errors
///
/// Returns a message when the title is empty, too long, or contains line
/// breaks or other control characters.
pub fn check_title(raw: &str) -> Result<String, String> {
    let title = raw.trim();
    if title.is_empty() {
        return Err("must not be empty".to_owned());
    }
    if title.chars().any(char::is_control) {
        return Err("must be a single line without control characters".to_owned());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("must be at most {MAX_TITLE_CHARS} characters"));
    }
    Ok(title.to_owned())
}

/// The file stem for a title: ``* " \ / < > : | ? # ^ [ ]`` are dropped,
/// runs of whitespace become one space, leading and trailing spaces and dots
/// are trimmed, and the result is cut to [`MAX_STEM_BYTES`]. A title with
/// nothing left becomes `Untitled`.
///
/// # Examples
///
/// ```
/// use joedocs::document::file_stem;
///
/// assert_eq!(file_stem("Search: query #1"), "Search query 1");
/// ```
#[must_use]
pub fn file_stem(title: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|c| !FORBIDDEN.contains(c) && !c.is_control())
        .collect();
    let mut stem = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if stem.len() > MAX_STEM_BYTES {
        let mut end = MAX_STEM_BYTES;
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        stem.truncate(end);
    }
    let stem = stem.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if stem.is_empty() {
        "Untitled".to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_trimmed_and_must_be_one_short_line() {
        assert_eq!(check_title("  Plan  ").unwrap(), "Plan");
        assert_eq!(check_title(" ").unwrap_err(), "must not be empty");
        assert!(check_title("a\nb").unwrap_err().contains("single line"));
        assert!(check_title("a\tb").is_err());
        assert!(check_title(&"é".repeat(MAX_TITLE_CHARS)).is_ok());
        assert!(
            check_title(&"é".repeat(MAX_TITLE_CHARS + 1))
                .unwrap_err()
                .contains("at most 200")
        );
    }

    #[test]
    fn stems_drop_forbidden_characters_and_tidy_spaces() {
        assert_eq!(file_stem("A/B: c?"), "AB c");
        assert_eq!(
            file_stem("[[Link]] *bold* <x> | y ^ \"q\" \\"),
            "Link bold x y q"
        );
        assert_eq!(file_stem("  many    spaces "), "many spaces");
        assert_eq!(file_stem("..hidden."), "hidden");
        assert_eq!(file_stem("Ünïcödé ok"), "Ünïcödé ok");
        assert_eq!(file_stem("???"), "Untitled");
    }

    #[test]
    fn stems_are_cut_on_a_character_boundary() {
        let stem = file_stem(&"é".repeat(80));
        assert_eq!(stem.len(), MAX_STEM_BYTES);
        let stem = file_stem(&format!("a{}", "é".repeat(80)));
        assert_eq!(stem.len(), MAX_STEM_BYTES - 1);
        let stem = file_stem(&format!("{} .x", "a".repeat(MAX_STEM_BYTES - 2)));
        assert_eq!(stem, "a".repeat(MAX_STEM_BYTES - 2));
    }
}
