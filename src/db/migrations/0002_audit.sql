-- Phase 2: one row per MCP tool call that changes (or tries to change) data.

CREATE TABLE audit_log (
    at          TEXT NOT NULL,
    api_key_id  TEXT NOT NULL,
    key_name    TEXT NOT NULL,
    client      TEXT NOT NULL,
    tool        TEXT NOT NULL,
    document_id TEXT,
    outcome     TEXT NOT NULL
) STRICT;

CREATE INDEX audit_log_at ON audit_log (at);
