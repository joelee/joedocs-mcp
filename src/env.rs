//! Environment access behind a trait.
//!
//! Nothing in this crate reads or mutates the real process environment
//! directly; callers inject an [`Env`]. Production code uses [`ProcessEnv`],
//! tests use [`MapEnv`]. This keeps tests deterministic and avoids the
//! `unsafe` `std::env::set_var` that Rust 2024 requires.

use std::collections::BTreeMap;
use std::path::Path;

/// Read-only view of environment variables.
pub trait Env {
    /// Returns the value of `key`, or `None` when unset or not valid UTF-8.
    fn var(&self, key: &str) -> Option<String>;

    /// Returns every variable as `(name, value)` pairs, sorted by name.
    fn vars(&self) -> Vec<(String, String)>;

    /// Returns the value of `key` when it is set and non-empty.
    fn non_empty(&self, key: &str) -> Option<String> {
        self.var(key).filter(|value| !value.is_empty())
    }
}

/// In-memory environment for tests and embedding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MapEnv {
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    /// Creates an empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the environment with `key` set to `value`.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for MapEnv {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            vars: iter
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn vars(&self) -> Vec<(String, String)> {
        self.vars
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// The real process environment, optionally overlaid on a `.env` file.
///
/// Values from the process always win over values from `.env`. The `.env`
/// file is read into memory; the process environment is never modified.
#[derive(Debug, Default, Clone)]
pub struct ProcessEnv {
    dotenv: MapEnv,
}

impl ProcessEnv {
    /// Uses the process environment only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses the process environment overlaid on `path`. A missing file is
    /// not an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn with_dotenv_file(path: &Path) -> Result<Self, dotenvy::Error> {
        let iter = match dotenvy::from_path_iter(path) {
            Ok(iter) => iter,
            Err(error) if error.not_found() => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let dotenv = iter.collect::<Result<MapEnv, _>>()?;
        Ok(Self { dotenv })
    }
}

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().or_else(|| self.dotenv.var(key))
    }

    fn vars(&self) -> Vec<(String, String)> {
        let mut merged = self.dotenv.vars.clone();
        for (key, value) in std::env::vars_os() {
            if let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) {
                merged.insert(key, value);
            }
        }
        merged.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn map_env_returns_values_and_sorted_pairs() {
        let env = MapEnv::new().with("B", "2").with("A", "1");
        assert_eq!(env.var("A").as_deref(), Some("1"));
        assert_eq!(env.var("C"), None);
        assert_eq!(
            env.vars(),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
    }

    #[test]
    fn non_empty_treats_empty_as_unset() {
        let env: MapEnv = [("EMPTY", ""), ("SET", "x")].into_iter().collect();
        assert_eq!(env.non_empty("EMPTY"), None);
        assert_eq!(env.non_empty("SET").as_deref(), Some("x"));
    }

    #[test]
    fn missing_dotenv_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let env = ProcessEnv::with_dotenv_file(&dir.path().join(".env")).unwrap();
        assert_eq!(env.dotenv, MapEnv::new());
    }

    #[test]
    fn dotenv_values_are_visible_when_process_does_not_set_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "TEMPLATE_TEST_ONLY_IN_DOTENV=from-dotenv").unwrap();

        let env = ProcessEnv::with_dotenv_file(&path).unwrap();
        assert_eq!(
            env.var("TEMPLATE_TEST_ONLY_IN_DOTENV").as_deref(),
            Some("from-dotenv")
        );
        assert!(
            env.vars()
                .iter()
                .any(|(key, _)| key == "TEMPLATE_TEST_ONLY_IN_DOTENV")
        );
    }

    #[test]
    fn malformed_dotenv_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "NOT VALID LINE\n").unwrap();
        assert!(ProcessEnv::with_dotenv_file(&path).is_err());
    }
}
