-- One row per passage: a piece of source text that a rewrite, a bench item or a judgement item is
-- built on. `content_hash` is the SHA-256 of `text` as lowercase hex. `split` is assigned once, at
-- insert, and the triggers keep it from ever changing. A synthetic passage never sits in monitor
-- or gate, which only the captured kind uses.
CREATE TABLE passages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('synthetic', 'captured')),
    text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    split TEXT NOT NULL CHECK (split IN ('train', 'monitor', 'bench', 'gate', 'sealed')),
    document_key TEXT,
    CHECK (kind = 'captured' OR split IN ('train', 'bench', 'sealed'))
);

-- Content identical to a stored passage of the same kind is the same passage.
CREATE UNIQUE INDEX passages_kind_content_hash ON passages (kind, content_hash);

CREATE INDEX passages_document_key ON passages (document_key);

CREATE TRIGGER passages_no_update BEFORE UPDATE ON passages
BEGIN
    SELECT RAISE(ABORT, 'passages is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER passages_no_delete BEFORE DELETE ON passages
BEGIN
    SELECT RAISE(ABORT, 'passages is insert-only: no row of it is ever deleted');
END;
