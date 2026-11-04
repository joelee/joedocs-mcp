//! Project keys and document IDs such as `JD-42`.

use std::fmt;
use std::str::FromStr;

/// Longest accepted project key, in bytes.
pub const MAX_KEY_LEN: usize = 10;

/// Whether `value` is a project key: 1 to [`MAX_KEY_LEN`] upper-case ASCII
/// letters and digits, starting with a letter (for example `JD`).
///
/// # Examples
///
/// ```
/// use joedocs::document::is_project_key;
///
/// assert!(is_project_key("JD"));
/// assert!(!is_project_key("jd"));
/// ```
#[must_use]
pub fn is_project_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_uppercase)
        && bytes.len() <= MAX_KEY_LEN
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A document ID: the project key and a per-project sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId {
    key: String,
    seq: u64,
}

impl DocumentId {
    /// Builds an ID from a key already known to be valid and a sequence
    /// number.
    #[must_use]
    pub fn new(key: impl Into<String>, seq: u64) -> Self {
        Self {
            key: key.into(),
            seq,
        }
    }

    /// The project key, e.g. `JD`.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The sequence number within the project, starting at 1.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.key, self.seq)
    }
}

/// A string that is not a document ID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("\"{0}\" is not a document ID like JD-42")]
pub struct InvalidId(pub String);

impl FromStr for DocumentId {
    type Err = InvalidId;

    /// Parses `JD-42`. The key is matched case-insensitively, so `jd-42`
    /// is accepted too.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidId(value.to_owned());
        let (key, seq) = value.trim().split_once('-').ok_or_else(invalid)?;
        let key = key.to_ascii_uppercase();
        if !is_project_key(&key)
            || seq.is_empty()
            || seq.starts_with('0')
            || !seq.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let seq = seq.parse().map_err(|_| invalid())?;
        Ok(Self { key, seq })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_keys_are_short_upper_case_and_start_with_a_letter() {
        for ok in ["J", "JD", "X1", "ABCDEFGHIJ"] {
            assert!(is_project_key(ok), "{ok}");
        }
        for bad in ["", "jd", "1D", "J-D", "ABCDEFGHIJK", "JÉ"] {
            assert!(!is_project_key(bad), "{bad}");
        }
    }

    #[test]
    fn ids_round_trip_through_display() {
        let id: DocumentId = "JD-42".parse().unwrap();
        assert_eq!(id, DocumentId::new("JD", 42));
        assert_eq!((id.key(), id.seq()), ("JD", 42));
        assert_eq!(id.to_string(), "JD-42");
        assert_eq!(" jd-7 ".parse::<DocumentId>().unwrap().to_string(), "JD-7");
    }

    #[test]
    fn malformed_ids_are_rejected() {
        for bad in [
            "",
            "JD",
            "JD-",
            "JD-0",
            "JD-01",
            "JD-4a",
            "-4",
            "J_D-4",
            "JD-99999999999999999999",
        ] {
            assert_eq!(
                bad.parse::<DocumentId>(),
                Err(InvalidId(bad.to_owned())),
                "{bad}"
            );
        }
        assert_eq!(
            InvalidId("x".into()).to_string(),
            "\"x\" is not a document ID like JD-42"
        );
    }
}
