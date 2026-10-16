-- One row per migration applied to the store, inserted by the runner inside that migration's own
-- transaction: its number, the SHA-256 of its SQL as lowercase hex, and the time it was applied,
-- in UTC.
CREATE TABLE schema_version (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    number INTEGER NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

CREATE TRIGGER schema_version_no_update BEFORE UPDATE ON schema_version
BEGIN
    SELECT RAISE(ABORT, 'schema_version is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER schema_version_no_delete BEFORE DELETE ON schema_version
BEGIN
    SELECT RAISE(ABORT, 'schema_version is insert-only: no row of it is ever deleted');
END;
