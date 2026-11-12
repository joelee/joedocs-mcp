-- 0.5.1 (JD-39): the sign-in throttle keyed by user ID. Version 7 kept a
-- row per submitted name, so text typed into the name field (sometimes a
-- password) was stored as is, and its row cap let a flood of made-up names
-- push out a real user's count. Now only real users have rows, by ID, and
-- they are never pushed out; other names are counted in memory only, under
-- a keyed hash (src/store/throttle.rs). The old rows are transient: at
-- worst a lock is lifted early, once, at the upgrade.

DROP INDEX IF EXISTS sign_in_throttle_updated;
DROP TABLE IF EXISTS sign_in_throttle;

CREATE TABLE sign_in_throttle (
    user_id      TEXT PRIMARY KEY REFERENCES users (id),
    failures     INTEGER NOT NULL,
    locked_until TEXT,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE INDEX sign_in_throttle_updated ON sign_in_throttle (updated_at);
