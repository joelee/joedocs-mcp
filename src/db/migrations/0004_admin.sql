-- 0.3.0: admin tokens for the remote admin API, each with a TOTP secret,
-- and the sessions a token plus a code opens. Only SHA-256 hashes of the
-- token and session secrets are stored; the TOTP secret must be kept as is
-- to check codes (RFC 6238), so this database stays private.

CREATE TABLE admin_tokens (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    secret_sha256   BLOB NOT NULL,
    totp_secret     BLOB NOT NULL,
    created_at      TEXT NOT NULL,
    last_used_at    TEXT,
    revoked_at      TEXT,
    failed_logins   INTEGER NOT NULL DEFAULT 0,
    locked_until    TEXT,
    last_totp_step  INTEGER
) STRICT;

CREATE TABLE admin_sessions (
    id              TEXT PRIMARY KEY,
    admin_token_id  TEXT NOT NULL REFERENCES admin_tokens (id),
    secret_sha256   BLOB NOT NULL,
    created_at      TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    last_used_at    TEXT,
    revoked_at      TEXT
) STRICT;

CREATE INDEX admin_sessions_token ON admin_sessions (admin_token_id);
