//! Passwords: the rules a new one must meet, Argon2id hashing as PHC
//! strings, and the gate that keeps hashing off the store lock and to two at
//! a time.
//!
//! The rules follow NIST SP 800-63B-4 § 3.1.1.2: a password used with a
//! second factor is at least 12 characters here (NIST asks for 8), counted
//! as Unicode code points after NFC normalisation, at most 256 (a bound on
//! the hashing work), with no composition rules, and refused when it is the
//! user's name, the service's name or a commonly used password. Hashes use
//! Argon2id at OWASP's minimum (19 MiB, 2 iterations, 1 lane), which are the
//! `argon2` crate's defaults; a hash with weaker parameters is reported so
//! the caller can rehash it at the next sign-in.
//!
//! Hashing takes about 12 ms and 19 MiB. It must never run while the store
//! lock is held, or MCP requests wait behind sign-in attempts; [`Gate`] runs
//! it on a blocking thread, at most two at once.

use std::sync::LazyLock;
use std::time::Duration;

use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, PasswordHash};
use secrecy::{ExposeSecret as _, SecretString};

use crate::auth::{RandomError, RandomSource};

/// Fewest characters (Unicode code points, after NFC) in a new password.
pub const MIN_CHARS: usize = 12;
/// Most characters in a password.
pub const MAX_CHARS: usize = 256;
/// Bytes of salt in each hash.
const SALT_BYTES: usize = 16;
/// Hashes that may run at once, server-wide.
pub const HASHING_PERMITS: usize = 2;
/// How long a request waits for a hashing place before it is refused.
pub const HASHING_WAIT: Duration = Duration::from_secs(2);

/// Commonly used passwords of 12 or more characters, lower-case. A new
/// password equal to one of these (ignoring case) is refused. Short common
/// passwords are already refused by the length rule.
const COMMON: &[&str] = &[
    "000000000000",
    "111111111111",
    "123123123123",
    "123456123456",
    "123456654321",
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123456789abc",
    "1234567890ab",
    "1234567890qwerty",
    "12345678910111213",
    "1234qwerasdf",
    "1234qwerasdfzxcv",
    "147258369147",
    "1q2w3e4r5t6y",
    "1q2w3e4r5t6y7u8i",
    "1qaz2wsx3edc",
    "1qaz2wsx3edc4rfv",
    "987654321098",
    "aaaaaaaaaaaa",
    "abc123456789",
    "abcd1234abcd",
    "abcdefghijkl",
    "admin1234567",
    "adminadmin123",
    "administrator",
    "administrator1",
    "asdfasdfasdf",
    "asdfghjkl123",
    "asdfghjklzxcv",
    "baseball1234",
    "changeme1234",
    "changemenow1",
    "correcthorsebatterystaple",
    "default12345",
    "dragon123456",
    "football1234",
    "iloveyou1234",
    "iloveyouiloveyou",
    "joedocs12345",
    "joedocsjoedocs",
    "joedocspassword",
    "letmein12345",
    "letmeinletmein",
    "monkey123456",
    "mypassword123",
    "p@ssw0rd1234",
    "p@ssword1234",
    "passw0rd1234",
    "password!234",
    "password1234",
    "password12345",
    "password123456",
    "passwordpassword",
    "princess1234",
    "q1w2e3r4t5y6",
    "qazwsxedcrfv",
    "qwerty123456",
    "qwertyqwerty",
    "qwertyuiop123",
    "qwertyuiopasdf",
    "qwertyuiopasdfghjkl",
    "rootpassword",
    "secretpassword",
    "starwars1234",
    "sunshine1234",
    "superman1234",
    "temporary123",
    "trustno1trustno1",
    "welcome12345",
    "zaq12wsxcde3",
];

/// Why a new password was refused. Each message says what to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Under [`MIN_CHARS`].
    #[error("the password is too short: use at least {MIN_CHARS} characters")]
    TooShort,
    /// Over [`MAX_CHARS`].
    #[error("the password is too long: use at most {MAX_CHARS} characters")]
    TooLong,
    /// The user's own name.
    #[error("the password must not be the user name")]
    UserName,
    /// The service's name.
    #[error("the password must not be the name of this service")]
    ServiceName,
    /// On the list of common passwords.
    #[error("the password is too common: choose one that is not on lists of known passwords")]
    Common,
}

/// Hashing could not be done.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// No salt could be drawn.
    #[error(transparent)]
    Random(#[from] RandomError),
    /// The hasher failed.
    #[error("cannot hash the password: {0}")]
    Hash(String),
}

/// `text` in Unicode Normalization Form C, so a password typed on two
/// systems that encode accents differently is the same password.
#[must_use]
pub fn normalize(text: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .into_owned()
}

/// Checks a new password for `user_name` against the rules.
///
/// # Errors
///
/// Returns the first rule it breaks.
pub fn check_new(user_name: &str, candidate: &SecretString) -> Result<(), Refusal> {
    let text = normalize(candidate.expose_secret());
    let chars = text.chars().count();
    if chars < MIN_CHARS {
        return Err(Refusal::TooShort);
    }
    if chars > MAX_CHARS {
        return Err(Refusal::TooLong);
    }
    let lower = text.to_lowercase();
    if lower == user_name.to_lowercase() {
        return Err(Refusal::UserName);
    }
    if lower == "joedocs" {
        return Err(Refusal::ServiceName);
    }
    if COMMON.contains(&lower.as_str()) {
        return Err(Refusal::Common);
    }
    Ok(())
}

/// The Argon2id PHC string of `password` with a new random salt.
///
/// # Errors
///
/// Returns [`HashError`] when no salt can be drawn or hashing fails.
pub fn hash(password: &SecretString, random: &dyn RandomSource) -> Result<String, HashError> {
    let mut salt = [0_u8; SALT_BYTES];
    random.fill(&mut salt)?;
    hash_with_salt(password, &salt)
}

fn hash_with_salt(password: &SecretString, salt: &[u8]) -> Result<String, HashError> {
    let text = normalize(password.expose_secret());
    Argon2::default()
        .hash_password_with_salt(text.as_bytes(), salt)
        .map(|phc| phc.to_string())
        .map_err(|e| HashError::Hash(e.to_string()))
}

/// The result of checking a password against a stored hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    /// Whether the password matches.
    pub matches: bool,
    /// Whether the stored hash uses weaker parameters than today's, so it
    /// should be replaced (only meaningful when it matches).
    pub needs_rehash: bool,
}

/// Checks `password` against the PHC string `stored`. A malformed hash
/// never matches.
#[must_use]
pub fn verify(password: &SecretString, stored: &str) -> Verified {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return Verified {
            matches: false,
            needs_rehash: false,
        };
    };
    let text = normalize(password.expose_secret());
    let matches = Argon2::default()
        .verify_password(text.as_bytes(), &parsed)
        .is_ok();
    Verified {
        matches,
        needs_rehash: weaker_than_today(&parsed),
    }
}

fn weaker_than_today(parsed: &PasswordHash) -> bool {
    let today = Params::default();
    if parsed.algorithm != Algorithm::Argon2id.ident() {
        return true;
    }
    Params::try_from(parsed).map_or(true, |p| {
        p.m_cost() < today.m_cost() || p.t_cost() < today.t_cost() || p.p_cost() < today.p_cost()
    })
}

/// A fixed hash with today's parameters, checked when a sign-in names no
/// user, so the reply takes as long as for a wrong password.
#[must_use]
pub fn dummy_hash() -> &'static str {
    static DUMMY: LazyLock<String> = LazyLock::new(|| {
        let password = SecretString::from("no user has this password".to_owned());
        hash_with_salt(&password, b"joedocs-no-user!").unwrap_or_default()
    });
    &DUMMY
}

/// Characters of a one-time initial password: upper-case letters and
/// digits without `0`, `1`, `I` and `O`, which are easily confused. 32 of
/// them, so each random byte maps to one evenly.
const INITIAL_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Characters in a one-time initial password (5 bits each: 100 bits).
const INITIAL_CHARS: usize = 20;

/// A new one-time initial password, `XXXXX-XXXXX-XXXXX-XXXXX`, for an
/// admin to hand to a new or reset user. It skips the rules of
/// [`check_new`]: it is random, and the user replaces it at the first
/// sign-in.
///
/// # Errors
///
/// Returns [`RandomError`] when random bytes are unavailable.
pub fn initial_password(random: &dyn RandomSource) -> Result<SecretString, RandomError> {
    let mut bytes = [0_u8; INITIAL_CHARS];
    random.fill(&mut bytes)?;
    let mut text = String::with_capacity(INITIAL_CHARS + 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 && i % 5 == 0 {
            text.push('-');
        }
        text.push(char::from(INITIAL_ALPHABET[usize::from(byte % 32)]));
    }
    Ok(SecretString::from(text))
}

/// Both hashing places stayed taken for [`HASHING_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the server is busy checking other sign-ins; try again in a moment")]
pub struct Busy;

/// Runs password hashing on blocking threads, never under the store lock,
/// with at most [`HASHING_PERMITS`] at once server-wide.
#[derive(Debug)]
pub struct Gate {
    permits: tokio::sync::Semaphore,
    wait: Duration,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new(HASHING_PERMITS, HASHING_WAIT)
    }
}

impl Gate {
    /// A gate of `permits` places, refusing after `wait`.
    #[must_use]
    pub fn new(permits: usize, wait: Duration) -> Self {
        Self {
            permits: tokio::sync::Semaphore::new(permits),
            wait,
        }
    }

    /// Runs `work` on a blocking thread once a place is free.
    ///
    /// # Errors
    ///
    /// Returns [`Busy`] when no place frees up within the wait, or the
    /// blocking task cannot run.
    pub async fn run<T, F>(&self, work: F) -> Result<T, Busy>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = tokio::time::timeout(self.wait, self.permits.acquire())
            .await
            .map_err(|_| Busy)?
            .map_err(|_| Busy)?;
        let result = tokio::task::spawn_blocking(work).await.map_err(|_| Busy);
        drop(permit);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CountingRandom;

    fn secret(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    #[test]
    fn lengths_count_code_points_after_nfc() {
        assert_eq!(
            check_new("alice", &secret("a-b-c-d-e-f")),
            Err(Refusal::TooShort)
        );
        assert_eq!(check_new("alice", &secret("a-b-c-d-e-fg")), Ok(()));
        // Twelve characters of two or three bytes each.
        assert_eq!(check_new("alice", &secret("éàüöçñßøåæœ€")), Ok(()));
        // "é" decomposed is two code points but one after NFC.
        let decomposed = "e\u{301}".repeat(11);
        assert_eq!(
            check_new("alice", &secret(&decomposed)),
            Err(Refusal::TooShort)
        );
        assert_eq!(check_new("alice", &secret(&"x".repeat(256))), Ok(()));
        assert_eq!(
            check_new("alice", &secret(&"x".repeat(257))),
            Err(Refusal::TooLong)
        );
    }

    #[test]
    fn names_and_common_passwords_are_refused_with_the_reason() {
        assert_eq!(
            check_new("alice-at-home", &secret("Alice-At-Home")),
            Err(Refusal::UserName)
        );
        assert_eq!(
            check_new("alice", &secret("Password1234")),
            Err(Refusal::Common)
        );
        assert_eq!(
            check_new("alice", &secret("JOEDOCSJOEDOCS")),
            Err(Refusal::Common)
        );
        assert!(Refusal::TooShort.to_string().contains("12 characters"));
        assert!(Refusal::Common.to_string().contains("too common"));
        assert!(Refusal::UserName.to_string().contains("user name"));
        // The service's name alone is short, but the rule stands for a
        // longer minimum's sake.
        assert_eq!(
            check_new("alice", &secret("joedocs")),
            Err(Refusal::TooShort)
        );
        for entry in COMMON {
            assert!(entry.chars().count() >= MIN_CHARS, "{entry}");
            assert_eq!(*entry, entry.to_lowercase(), "{entry}");
        }
    }

    #[test]
    fn hashes_are_argon2id_phc_strings_with_owasp_minimum_parameters() {
        let phc = hash(&secret("a long enough password"), &CountingRandom(1)).unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        let ok = verify(&secret("a long enough password"), &phc);
        assert_eq!(
            ok,
            Verified {
                matches: true,
                needs_rehash: false
            }
        );
        assert!(!verify(&secret("another password!"), &phc).matches);
        assert!(!verify(&secret("x"), "not a hash").matches);
    }

    #[test]
    fn nfc_and_nfd_spellings_are_the_same_password() {
        let composed = "caf\u{e9} au lait, please";
        let decomposed = "cafe\u{301} au lait, please";
        let phc = hash(&secret(composed), &CountingRandom(2)).unwrap();
        assert!(verify(&secret(decomposed), &phc).matches);
    }

    #[test]
    fn weaker_hashes_need_a_rehash() {
        let weak = Argon2::new(
            Algorithm::Argon2id,
            argon2::Version::V0x13,
            Params::new(8192, 1, 1, None).unwrap(),
        )
        .hash_password_with_salt(b"a long enough password", b"0123456789abcdef")
        .unwrap()
        .to_string();
        let checked = verify(&secret("a long enough password"), &weak);
        assert_eq!(
            checked,
            Verified {
                matches: true,
                needs_rehash: true
            }
        );
        let argon2i = Argon2::new(
            Algorithm::Argon2i,
            argon2::Version::V0x13,
            Params::default(),
        )
        .hash_password_with_salt(b"a long enough password", b"0123456789abcdef")
        .unwrap()
        .to_string();
        assert!(verify(&secret("a long enough password"), &argon2i).needs_rehash);
    }

    #[test]
    fn the_dummy_hash_has_todays_parameters_and_matches_nothing_typed() {
        assert!(dummy_hash().starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(!verify(&secret("password1234"), dummy_hash()).matches);
    }

    #[test]
    fn initial_passwords_are_grouped_unambiguous_and_random() {
        let a = initial_password(&CountingRandom(0)).unwrap();
        let text = a.expose_secret();
        assert_eq!(text.len(), 23);
        assert_eq!(text.matches('-').count(), 3);
        assert!(
            text.chars()
                .all(|c| c == '-' || INITIAL_ALPHABET.contains(&(c as u8)))
        );
        assert_eq!(&text[..5], "23456");
        let b = initial_password(&CountingRandom(7)).unwrap();
        assert_ne!(a.expose_secret(), b.expose_secret());
    }

    #[tokio::test]
    async fn the_gate_runs_work_and_refuses_when_both_places_stay_taken() {
        let gate = std::sync::Arc::new(Gate::new(2, Duration::from_millis(50)));
        assert_eq!(gate.run(|| 7).await, Ok(7));
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Arc::new(std::sync::Mutex::new(wait));
        let mut held = Vec::new();
        for _ in 0..2 {
            let (gate, wait) = (gate.clone(), wait.clone());
            held.push(tokio::spawn(async move {
                gate.run(move || {
                    let _ = wait.lock().unwrap().recv();
                })
                .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(gate.run(|| 1).await, Err(Busy));
        release.send(()).unwrap();
        release.send(()).unwrap();
        for task in held {
            assert_eq!(task.await.unwrap(), Ok(()));
        }
        assert_eq!(gate.run(|| 2).await, Ok(2));
    }
}
