//! The sign-in throttle for names that are not users (JD-39 D-01).
//!
//! Every failed sign-in counts against the submitted name, real or not, so
//! a reply never says whether a name exists (see `sign_in`). A real user's
//! count lives in the database, by user ID (`sign_in_throttle`), and is
//! never pushed out by other names. Any other name, one that is no user's
//! or a removed user's, is counted here, in memory only, under an
//! HMAC-SHA256 of the name keyed by 32 random bytes made when the store
//! opens. The typed text is never stored, and the key never leaves the
//! process, so the entries cannot be checked against guesses.
//!
//! At most [`UNKNOWN_CAP`] names are held. When full, expired entries go
//! first, then the least recently failed tenth. A restart forgets them all.
//! Either can lift an unknown name's lock early, which is how a sustained
//! flood of failed sign-ins, or a restart, can tell an unknown name from a
//! real one: an accepted residual, documented in `docs/deployment.md`.

use std::collections::HashMap;
use std::fmt;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;

use crate::auth::{RandomError, RandomSource};
use crate::db::ThrottleRow;

/// Most unknown names held in memory at once.
pub const UNKNOWN_CAP: usize = 100_000;
/// Bytes in the key that names are hashed with.
const KEY_BYTES: usize = 32;

/// What the throttle knows about one unknown name.
struct Entry {
    failures: u32,
    locked_until: Option<String>,
    updated_at: String,
}

/// Failed sign-ins of names that are not users, by keyed hash of the name.
pub(super) struct UnknownNames {
    key: [u8; KEY_BYTES],
    cap: usize,
    entries: HashMap<[u8; 32], Entry>,
}

impl fmt::Debug for UnknownNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Neither the key nor the hashed names are shown.
        f.debug_struct("UnknownNames")
            .field("entries", &self.entries.len())
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl UnknownNames {
    /// An empty throttle with a new random key, holding at most
    /// [`UNKNOWN_CAP`] names.
    ///
    /// # Errors
    ///
    /// Returns [`RandomError`] when no random bytes can be read.
    pub(super) fn new(random: &dyn RandomSource) -> Result<Self, RandomError> {
        Self::with_cap(random, UNKNOWN_CAP)
    }

    /// As [`UnknownNames::new`], holding at most `cap` names (at least one).
    ///
    /// # Errors
    ///
    /// Returns [`RandomError`] when no random bytes can be read.
    pub(super) fn with_cap(random: &dyn RandomSource, cap: usize) -> Result<Self, RandomError> {
        let mut key = [0_u8; KEY_BYTES];
        random.fill(&mut key)?;
        Ok(Self {
            key,
            cap: cap.max(1),
            entries: HashMap::new(),
        })
    }

    /// How many names are held.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The record for `name`, if any.
    pub(super) fn get(&self, name: &str) -> Option<ThrottleRow> {
        self.entries.get(&self.slot(name)).map(|e| ThrottleRow {
            failures: e.failures,
            locked_until: e.locked_until.clone(),
            updated_at: e.updated_at.clone(),
        })
    }

    /// Records a failed sign-in for `name` at `at`: stores the count
    /// `failures` the caller decided, and locks the name until `lock_until`
    /// when given. When the throttle is full, entries untouched since
    /// `prune_before` whose lock has ended go first, then the least recently
    /// failed tenth.
    pub(super) fn failure(
        &mut self,
        name: &str,
        failures: u32,
        lock_until: Option<&str>,
        at: &str,
        prune_before: &str,
    ) {
        let slot = self.slot(name);
        if !self.entries.contains_key(&slot) && self.entries.len() >= self.cap {
            self.make_room(prune_before);
        }
        let entry = self.entries.entry(slot).or_insert_with(|| Entry {
            failures: 0,
            locked_until: None,
            updated_at: String::new(),
        });
        entry.failures = failures;
        entry.locked_until = lock_until.map(str::to_owned);
        at.clone_into(&mut entry.updated_at);
    }

    /// Frees at least one place: expired entries, then the oldest tenth.
    fn make_room(&mut self, prune_before: &str) {
        self.entries.retain(|_, e| {
            e.updated_at.as_str() >= prune_before
                || e.locked_until.as_deref().is_some_and(|t| t >= prune_before)
        });
        if self.entries.len() < self.cap {
            return;
        }
        let mut ages: Vec<([u8; 32], String)> = self
            .entries
            .iter()
            .map(|(slot, e)| (*slot, e.updated_at.clone()))
            .collect();
        ages.sort_by(|a, b| a.1.cmp(&b.1));
        let evict = (self.cap / 10).max(1);
        for (slot, _) in ages.into_iter().take(evict) {
            self.entries.remove(&slot);
        }
    }

    /// The keyed hash `name` is held under.
    fn slot(&self, name: &str) -> [u8; 32] {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.key) else {
            // HMAC takes a key of any length; this cannot happen.
            return [0; 32];
        };
        mac.update(name.as_bytes());
        <[u8; 32]>::try_from(mac.finalize().into_bytes().as_slice()).unwrap_or([0; 32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CountingRandom, OsRandom};

    #[test]
    fn names_count_lock_and_are_held_by_keyed_hash_only() {
        let mut names = UnknownNames::new(&OsRandom).unwrap();
        names.failure("nobody", 1, None, "t1", "t0");
        names.failure("nobody", 2, None, "t2", "t0");
        assert_eq!(names.get("nobody").unwrap().failures, 2);
        names.failure("nobody", 0, Some("t9"), "t3", "t0");
        assert_eq!(
            names.get("nobody").unwrap(),
            ThrottleRow {
                failures: 0,
                locked_until: Some("t9".to_owned()),
                updated_at: "t3".to_owned(),
            }
        );
        assert_eq!(names.get("somebody"), None);
        let shown = format!("{names:?}");
        assert!(!shown.contains("nobody"), "{shown}");
        assert!(!shown.contains("key:"), "{shown}");
        // Another key, another slot: nothing about a name carries over.
        let other = UnknownNames::new(&CountingRandom(1)).unwrap();
        assert_ne!(other.slot("nobody"), names.slot("nobody"));
    }

    #[test]
    fn a_full_throttle_prunes_expired_names_then_the_oldest_tenth() {
        let mut names = UnknownNames::with_cap(&OsRandom, 20).unwrap();
        for i in 0..20 {
            names.failure(&format!("n{i:02}"), 1, None, &format!("t{i:02}"), "t00");
        }
        assert_eq!(names.len(), 20);
        // Entries untouched since t05 go first.
        names.failure("new1", 1, None, "t20", "t05");
        assert_eq!(names.len(), 16);
        assert!(names.get("n04").is_none() && names.get("n05").is_some());
        for i in 0..4 {
            names.failure(&format!("more{i}"), 1, None, "t21", "t00");
        }
        assert_eq!(names.len(), 20);
        // Nothing expired: the two least recently failed go.
        names.failure("new2", 1, None, "t22", "t00");
        assert_eq!(names.len(), 19);
        assert!(names.get("n05").is_none() && names.get("n06").is_none());
        assert!(names.get("n07").is_some());
        // A lock that has not ended keeps an old entry.
        let mut locked = UnknownNames::with_cap(&OsRandom, 2).unwrap();
        locked.failure("a", 0, Some("t99"), "t01", "t00");
        locked.failure("b", 1, None, "t02", "t00");
        locked.failure("c", 1, None, "t03", "t50");
        assert!(locked.get("a").is_some());
        assert!(locked.get("b").is_none());
        assert_eq!(locked.len(), 2);
    }
}
