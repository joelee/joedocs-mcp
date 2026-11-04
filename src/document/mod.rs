//! Document domain types: project names, document types, document IDs,
//! statuses and revisions; front matter parsing and serialisation with the
//! reserved-key rules; title to file name sanitising.
//!
//! Pure code with no I/O. Only the slug rule exists so far; see
//! `docs/plan.md` sections 4 and 5 for the rest.

/// Longest accepted slug, in bytes.
pub const MAX_SLUG_LEN: usize = 64;

/// Whether `value` is a slug: 1 to [`MAX_SLUG_LEN`] lower-case ASCII
/// letters, digits and `-`, starting with a letter or digit. Project names,
/// document types and statuses are slugs because they become directory names
/// and front matter values.
///
/// # Examples
///
/// ```
/// use joedocs::document::is_slug;
///
/// assert!(is_slug("code-reviews"));
/// assert!(!is_slug("Code Reviews"));
/// ```
#[must_use]
pub fn is_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= MAX_SLUG_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lower_case_letters_digits_and_hyphens() {
        for ok in ["a", "plans", "code-reviews", "2026-notes", "x-"] {
            assert!(is_slug(ok), "{ok}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        let too_long = "a".repeat(MAX_SLUG_LEN + 1);
        for bad in [
            "", "-a", "Plans", "a b", "a_b", "a.b", "é", "../x", &too_long,
        ] {
            assert!(!is_slug(bad), "{bad:?}");
        }
        assert!(is_slug(&"a".repeat(MAX_SLUG_LEN)));
    }
}
