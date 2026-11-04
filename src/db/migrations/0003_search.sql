-- Phase 3: search. Both tables hold the latest revision of live documents
-- only. Every write keeps them current, and `joedocs reindex` rebuilds
-- them from the history.

-- Every front matter key, one row per value (a list has a row per item).
-- `num` is set for numbers, so they compare as numbers.
CREATE TABLE doc_fields (
    document_id TEXT NOT NULL REFERENCES documents (id),
    key         TEXT NOT NULL,
    value       TEXT NOT NULL COLLATE NOCASE,
    num         REAL
) STRICT;

CREATE INDEX doc_fields_key_value ON doc_fields (key, value);
CREATE INDEX doc_fields_key_num ON doc_fields (key, num);
CREATE INDEX doc_fields_document ON doc_fields (document_id);

-- Full text. The rowid is project_id * 2^32 + seq (see db::fts_rowid), so
-- a document's row is found without scanning. `title` holds the title and
-- the aliases.
CREATE VIRTUAL TABLE doc_fts USING fts5 (
    title,
    body,
    tokenize = 'porter unicode61 remove_diacritics 2'
);
