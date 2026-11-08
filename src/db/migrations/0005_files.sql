-- 0.4.0: multi-file documents. A multi-file document is a folder: its main
-- file keeps the `documents` row, and every other file (a sub-file) has a
-- row in `document_files`. Every change to any file raises the document's
-- revision; each file records the revision at which it last changed.
-- Like the rest of the index, all of this is rebuilt from the history by
-- `joedocs reindex`.

ALTER TABLE documents ADD COLUMN multifile INTEGER NOT NULL DEFAULT 0
    CHECK (multifile IN (0, 1));
-- The document revision at which the main file last changed.
ALTER TABLE documents ADD COLUMN main_revision INTEGER NOT NULL DEFAULT 0;
UPDATE documents SET main_revision = revision;

-- The sub-files of multi-file documents, live and archived. `path` is
-- relative to the document folder, without `.md`, e.g. `backlog/item-1`.
-- `revision` is the document revision at which the file last changed.
CREATE TABLE document_files (
    id             INTEGER PRIMARY KEY,
    document_id    TEXT NOT NULL REFERENCES documents (id),
    path           TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    created_by     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    updated_by     TEXT NOT NULL,
    client         TEXT NOT NULL,
    content_sha256 BLOB NOT NULL,
    archived_at    TEXT,
    archived_by    TEXT
) STRICT;

-- Live files own their path within the document; archived ones give it up.
CREATE UNIQUE INDEX document_files_live_path
    ON document_files (document_id, path COLLATE NOCASE) WHERE archived_at IS NULL;
CREATE INDEX document_files_document ON document_files (document_id, archived_at);

-- The files each revision of a multi-file document changed. `main` marks
-- the main file, whose `path` is its stem at that revision. A rename
-- records the old path in `from_path`.
CREATE TABLE revision_files (
    document_id TEXT NOT NULL REFERENCES documents (id),
    revision    INTEGER NOT NULL,
    path        TEXT NOT NULL,
    main        INTEGER NOT NULL DEFAULT 0 CHECK (main IN (0, 1)),
    change      TEXT NOT NULL CHECK (change IN ('added', 'modified', 'renamed', 'removed')),
    from_path   TEXT,
    PRIMARY KEY (document_id, revision, main, path)
) STRICT;

-- Search entries of sub-files: their front matter rows carry the file, and
-- their full-text row in `doc_fts` has the rowid `-document_files.id`
-- (main files keep `project_id * 2^32 + seq`), so main files and sub-files
-- rank in one query.
ALTER TABLE doc_fields ADD COLUMN file_id INTEGER REFERENCES document_files (id);
CREATE INDEX doc_fields_file ON doc_fields (file_id);
