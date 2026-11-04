-- Phase 1: projects, API keys, documents and revisions.
-- Timestamps are UTC RFC 3339 text with whole seconds, so they sort.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    key        TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL,
    next_seq   INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE api_keys (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    secret_sha256 BLOB NOT NULL,
    created_at    TEXT NOT NULL,
    last_used_at  TEXT,
    revoked_at    TEXT
) STRICT;

CREATE TABLE grants (
    api_key_id TEXT NOT NULL REFERENCES api_keys (id),
    project_id INTEGER NOT NULL REFERENCES projects (id),
    permission TEXT NOT NULL CHECK (permission IN ('read', 'write')),
    PRIMARY KEY (api_key_id, project_id)
) STRICT;

CREATE TABLE documents (
    id             TEXT PRIMARY KEY,
    project_id     INTEGER NOT NULL REFERENCES projects (id),
    seq            INTEGER NOT NULL,
    type           TEXT NOT NULL,
    title          TEXT NOT NULL,
    path           TEXT NOT NULL,
    status         TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    created_by     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    updated_by     TEXT NOT NULL,
    client         TEXT NOT NULL,
    content_sha256 BLOB NOT NULL,
    archived_at    TEXT,
    archived_by    TEXT,
    archive_reason TEXT,
    UNIQUE (project_id, seq)
) STRICT;

-- Live documents own their path; archived ones give it up.
CREATE UNIQUE INDEX documents_live_path
    ON documents (path COLLATE NOCASE) WHERE archived_at IS NULL;
CREATE INDEX documents_listing ON documents (project_id, archived_at, type, status);

CREATE TABLE revisions (
    document_id TEXT NOT NULL REFERENCES documents (id),
    revision    INTEGER NOT NULL,
    commit_id   TEXT NOT NULL,
    path        TEXT NOT NULL,
    action      TEXT NOT NULL,
    author      TEXT NOT NULL,
    client      TEXT NOT NULL,
    summary     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (document_id, revision)
) STRICT;
