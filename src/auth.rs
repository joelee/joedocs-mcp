//! API keys: generation, hashing, verification, and per-project grants
//! (`read` or `write`).
//!
//! A key is `jd_<id>_<secret>`: a 12-hex-digit public ID that names the
//! database row and a 64-hex-digit secret (32 random bytes). Only
//! `SHA-256(secret)` is stored. See `docs/plan.md` section 8.

use std::fmt;
use std::str::FromStr;

use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};

use crate::document::is_slug;

/// Bytes of randomness in a key's public ID.
const ID_BYTES: usize = 6;
/// Bytes of randomness in a key's secret.
const SECRET_BYTES: usize = 32;
/// Prefix of every API key.
pub const KEY_PREFIX: &str = "jd_";
/// Prefix of every admin token (`jda_<id>_<secret>`).
pub const ADMIN_PREFIX: &str = "jda_";
/// Prefix of every admin session token (`jds_<id>_<secret>`).
pub const SESSION_PREFIX: &str = "jds_";
/// Prefix of every web interface session token (`jdw_<id>_<secret>`).
pub const WEB_SESSION_PREFIX: &str = "jdw_";
/// Prefix of every device credential (`jdd_<id>_<secret>`), issued by
/// `joedocs auth login` to one machine.
pub const DEVICE_PREFIX: &str = "jdd_";
/// Prefix of every user session on the admin API (`jdu_<id>_<secret>`).
pub const USER_SESSION_PREFIX: &str = "jdu_";
/// Prefix of every user session in the web interface (`jdv_<id>_<secret>`).
pub const USER_WEB_SESSION_PREFIX: &str = "jdv_";
/// Prefix of every TOTP setup ticket (`jdt_<user id>_<secret>`).
pub const SETUP_PREFIX: &str = "jdt_";

/// A source of cryptographically secure random bytes.
pub trait RandomSource {
    /// Fills `buf` with random bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot supply randomness.
    fn fill(&self, buf: &mut [u8]) -> Result<(), RandomError>;
}

/// The operating system could not supply random bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot read random bytes: {0}")]
pub struct RandomError(pub String);

/// The operating system's random number generator.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(buf).map_err(|e| RandomError(e.to_string()))
    }
}

/// What a grant allows on a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// List, get and search documents.
    Read,
    /// Everything `Read` allows, plus every document change.
    Write,
}

impl Permission {
    /// The name stored in the database and shown to users.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// Whether this permission includes `needed`.
    #[must_use]
    pub fn allows(self, needed: Self) -> bool {
        self >= needed
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            _ => Err(format!("\"{value}\" is not a permission (read or write)")),
        }
    }
}

/// A permission on one project, written `project:permission`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    /// Project name.
    pub project: String,
    /// What the key may do there.
    pub permission: Permission,
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.project, self.permission)
    }
}

impl FromStr for Grant {
    type Err = String;

    /// Parses `joedocs:write`.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (project, permission) = value.split_once(':').ok_or_else(|| {
            format!("\"{value}\" is not a grant like project:read or project:write")
        })?;
        if !is_slug(project) {
            return Err(format!("\"{project}\" is not a project name"));
        }
        Ok(Self {
            project: project.to_owned(),
            permission: permission.parse()?,
        })
    }
}

/// A freshly generated key. The token is shown to the user once; only the
/// ID and the hash are stored.
#[derive(Debug)]
pub struct NewKey {
    /// Public ID (the database row key).
    pub id: String,
    /// SHA-256 of the secret.
    pub secret_sha256: [u8; 32],
    /// The full `jd_<id>_<secret>` token.
    pub token: SecretString,
}

/// Generates a new key.
///
/// # Errors
///
/// Returns an error when random bytes are unavailable.
pub fn generate(random: &dyn RandomSource) -> Result<NewKey, RandomError> {
    generate_prefixed(KEY_PREFIX, random)
}

/// Generates a new token with `prefix` ([`KEY_PREFIX`], [`ADMIN_PREFIX`],
/// [`SESSION_PREFIX`] or [`WEB_SESSION_PREFIX`]).
///
/// # Errors
///
/// Returns an error when random bytes are unavailable.
pub fn generate_prefixed(prefix: &str, random: &dyn RandomSource) -> Result<NewKey, RandomError> {
    let mut id = [0u8; ID_BYTES];
    random.fill(&mut id)?;
    secret_for(prefix, &hex(&id), random)
}

/// Generates a new secret, and so a new token, for the key with this ID.
///
/// # Errors
///
/// Returns an error when random bytes are unavailable.
pub fn new_secret(id: &str, random: &dyn RandomSource) -> Result<NewKey, RandomError> {
    secret_for(KEY_PREFIX, id, random)
}

/// A new token `<prefix><id>_<secret>` for an existing `id`.
///
/// # Errors
///
/// Returns an error when random bytes are unavailable.
pub fn secret_for(
    prefix: &str,
    id: &str,
    random: &dyn RandomSource,
) -> Result<NewKey, RandomError> {
    let mut secret = [0u8; SECRET_BYTES];
    random.fill(&mut secret)?;
    let id = id.to_owned();
    let secret = hex(&secret);
    let secret_sha256 = hash_secret(&secret);
    let token = SecretString::from(format!("{prefix}{id}_{secret}"));
    Ok(NewKey {
        id,
        secret_sha256,
        token,
    })
}

/// Splits a token into its public ID and secret, or `None` if it is not in
/// the `jd_<id>_<secret>` format.
#[must_use]
pub fn parse_token(token: &SecretString) -> Option<(String, SecretString)> {
    parse_prefixed(KEY_PREFIX, token)
}

/// Splits a `<prefix><id>_<secret>` token into its public ID and secret.
#[must_use]
pub fn parse_prefixed(prefix: &str, token: &SecretString) -> Option<(String, SecretString)> {
    let rest = token.expose_secret().strip_prefix(prefix)?;
    let (id, secret) = rest.split_once('_')?;
    let is_hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    (is_hex(id, ID_BYTES * 2) && is_hex(secret, SECRET_BYTES * 2))
        .then(|| (id.to_owned(), SecretString::from(secret.to_owned())))
}

/// SHA-256 of a key secret.
#[must_use]
pub fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Whether `secret` hashes to `expected`, compared in constant time.
#[must_use]
pub fn verify_secret(secret: &SecretString, expected: &[u8]) -> bool {
    let actual = hash_secret(secret.expose_secret());
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Lower-case hexadecimal encoding.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// A predictable [`RandomSource`] for tests: byte `n` of every request is
/// `seed + n`.
#[derive(Debug, Clone, Copy)]
pub struct CountingRandom(pub u8);

impl RandomSource for CountingRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), RandomError> {
        let mut value = self.0;
        for byte in buf {
            *byte = value;
            value = value.wrapping_add(1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken;
    impl RandomSource for Broken {
        fn fill(&self, _: &mut [u8]) -> Result<(), RandomError> {
            Err(RandomError("no entropy".into()))
        }
    }

    #[test]
    fn generated_keys_have_the_documented_shape_and_verify() {
        let key = generate(&CountingRandom(0)).unwrap();
        assert_eq!(key.id, "000102030405");
        let token = key.token.expose_secret();
        assert!(token.starts_with("jd_000102030405_000102"), "{token}");
        assert_eq!(token.len(), 3 + 12 + 1 + 64);

        let (id, secret) = parse_token(&key.token).unwrap();
        assert_eq!(id, key.id);
        assert!(verify_secret(&secret, &key.secret_sha256));
        assert!(!verify_secret(&SecretString::from("x"), &key.secret_sha256));
        assert!(!verify_secret(&secret, &key.secret_sha256[..31]));
    }

    #[test]
    fn os_random_produces_distinct_keys() {
        let a = generate(&OsRandom).unwrap();
        let b = generate(&OsRandom).unwrap();
        assert_ne!(a.id, b.id);
        assert_ne!(a.secret_sha256, b.secret_sha256);
    }

    #[test]
    fn random_failures_are_reported() {
        let err = generate(&Broken).unwrap_err();
        assert_eq!(err.to_string(), "cannot read random bytes: no entropy");
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let valid = format!("jd_{}_{}", "a".repeat(12), "b".repeat(64));
        assert!(parse_token(&SecretString::from(valid)).is_some());
        for bad in [
            String::new(),
            "jd_".to_owned(),
            format!("xx_{}_{}", "a".repeat(12), "b".repeat(64)),
            format!("jd_{}_{}", "a".repeat(11), "b".repeat(64)),
            format!("jd_{}_{}", "a".repeat(12), "g".repeat(64)),
            format!("jd_{}{}", "a".repeat(12), "b".repeat(64)),
        ] {
            assert!(
                parse_token(&SecretString::from(bad.clone())).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn permissions_and_grants_parse_and_display() {
        assert!(Permission::Write.allows(Permission::Read));
        assert!(!Permission::Read.allows(Permission::Write));
        let grant: Grant = "joedocs:write".parse().unwrap();
        assert_eq!(grant.to_string(), "joedocs:write");
        assert_eq!(grant.permission, Permission::Write);
        assert!(
            "joedocs"
                .parse::<Grant>()
                .unwrap_err()
                .contains("not a grant")
        );
        assert!(
            "Joe:read"
                .parse::<Grant>()
                .unwrap_err()
                .contains("not a project")
        );
        assert!(
            "joe:admin"
                .parse::<Grant>()
                .unwrap_err()
                .contains("read or write")
        );
    }

    #[test]
    fn hex_encodes_lower_case() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
