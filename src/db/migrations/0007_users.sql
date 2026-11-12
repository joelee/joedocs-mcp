-- 0.5.1: named users who sign in with a password and a TOTP code, the
-- device credentials `joedocs auth login` issues, their sessions, and the
-- per-name sign-in throttle. Only hashes of passwords (Argon2id, as PHC
-- strings) and of credential secrets are stored; TOTP secrets are kept as
-- is to check codes (RFC 6238), as admin tokens' are, so this database stays
-- private. Nothing existing changes except two nullable audit columns.

CREATE TABLE users (
    id                     TEXT PRIMARY KEY,
    name                   TEXT NOT NULL UNIQUE,
    password_hash          TEXT NOT NULL,
    must_change_password   INTEGER NOT NULL DEFAULT 1,
    initial_password_until TEXT,
    totp_secret            BLOB,
    totp_enrolled_at       TEXT,
    totp_pending_secret    BLOB,
    totp_pending_until     TEXT,
    setup_ticket_sha256    BLOB,
    last_totp_step         INTEGER,
    created_at             TEXT NOT NULL,
    updated_at             TEXT NOT NULL,
    last_login_at          TEXT,
    disabled_at            TEXT,
    removed_at             TEXT
) STRICT;

CREATE TABLE devices (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users (id),
    name          TEXT NOT NULL,
    secret_sha256 BLOB NOT NULL,
    created_at    TEXT NOT NULL,
    last_used_at  TEXT,
    revoked_at    TEXT,
    failed_codes  INTEGER NOT NULL DEFAULT 0,
    locked_until  TEXT
) STRICT;

CREATE INDEX devices_user ON devices (user_id);

CREATE TABLE user_sessions (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users (id),
    device_id     TEXT REFERENCES devices (id),
    kind          TEXT NOT NULL CHECK (kind IN ('api', 'web')),
    secret_sha256 BLOB NOT NULL,
    created_at    TEXT NOT NULL,
    expires_at    TEXT NOT NULL,
    last_used_at  TEXT,
    revoked_at    TEXT
) STRICT;

CREATE INDEX user_sessions_user ON user_sessions (user_id);
CREATE INDEX user_sessions_device ON user_sessions (device_id);

-- Failed sign-ins per submitted name, real or not, so a refusal never says
-- whether a name exists. Rows untouched for the lock period are pruned, and
-- the table is capped.
CREATE TABLE sign_in_throttle (
    name         TEXT PRIMARY KEY,
    failures     INTEGER NOT NULL,
    locked_until TEXT,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE INDEX sign_in_throttle_updated ON sign_in_throttle (updated_at);

ALTER TABLE audit_log ADD COLUMN user_id TEXT;
ALTER TABLE audit_log ADD COLUMN user_name TEXT;
