-- The Style Card's tables and the ruleset's. A Card version and a ruleset version are each a set of
-- rows inserted together and never changed, so a published version reads back as it was published
-- however far the Card or the ruleset has moved on.

-- One row per clause the Card has ever held. Its id is the clause's id, which AUTOINCREMENT never
-- gives another clause, including after the clause has left the Card.
CREATE TABLE clauses (
    id INTEGER PRIMARY KEY AUTOINCREMENT
);

CREATE TRIGGER clauses_no_update BEFORE UPDATE ON clauses
BEGIN
    SELECT RAISE(ABORT, 'clauses is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER clauses_no_delete BEFORE DELETE ON clauses
BEGIN
    SELECT RAISE(ABORT, 'clauses is insert-only: no row of it is ever deleted');
END;

-- One row per wording of a clause. `register_scope` is a JSON array of register names, or null for
-- every register. The example pair, a sentence that follows the clause and one that breaks it, is
-- absent or whole. The key on the id and the clause id is the one a Card version's member names,
-- so a member's clause is always its wording's clause.
CREATE TABLE clause_texts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    clause_id INTEGER NOT NULL REFERENCES clauses (id),
    text TEXT NOT NULL CHECK (text <> ''),
    twin TEXT NOT NULL CHECK (twin <> ''),
    register_scope TEXT CHECK (register_scope IS NULL OR json_type(register_scope) = 'array'),
    example_follows TEXT,
    example_breaks TEXT,
    CHECK ((example_follows IS NULL) = (example_breaks IS NULL)),
    UNIQUE (id, clause_id)
);

CREATE TRIGGER clause_texts_no_update BEFORE UPDATE ON clause_texts
BEGIN
    SELECT RAISE(ABORT, 'clause_texts is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER clause_texts_no_delete BEFORE DELETE ON clause_texts
BEGIN
    SELECT RAISE(ABORT, 'clause_texts is insert-only: no row of it is ever deleted');
END;

-- One row per Card version, with the time it was published, in UTC, and its change note in the
-- owner's terms.
CREATE TABLE card_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    published_at TEXT NOT NULL,
    change_note TEXT NOT NULL CHECK (change_note <> '')
);

CREATE TRIGGER card_versions_no_update BEFORE UPDATE ON card_versions
BEGIN
    SELECT RAISE(ABORT, 'card_versions is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER card_versions_no_delete BEFORE DELETE ON card_versions
BEGIN
    SELECT RAISE(ABORT, 'card_versions is insert-only: no row of it is ever deleted');
END;

-- One row per clause a Card version holds, naming the wording it holds, in the order the version
-- lists its clauses. A version holds a clause once.
CREATE TABLE card_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    card_version_id INTEGER NOT NULL REFERENCES card_versions (id),
    clause_text_id INTEGER NOT NULL,
    clause_id INTEGER NOT NULL,
    FOREIGN KEY (clause_text_id, clause_id) REFERENCES clause_texts (id, clause_id),
    UNIQUE (card_version_id, clause_id)
);

CREATE TRIGGER card_members_no_update BEFORE UPDATE ON card_members
BEGIN
    SELECT RAISE(ABORT, 'card_members is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER card_members_no_delete BEFORE DELETE ON card_members
BEGIN
    SELECT RAISE(ABORT, 'card_members is insert-only: no row of it is ever deleted');
END;

-- One row per clause a Card version changed against the version before it: one it added, reworded
-- or dropped. A dropped clause has no member row in the version, so the changes are rows of their
-- own rather than a mark on the members.
CREATE TABLE card_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    card_version_id INTEGER NOT NULL REFERENCES card_versions (id),
    clause_id INTEGER NOT NULL REFERENCES clauses (id),
    UNIQUE (card_version_id, clause_id)
);

CREATE TRIGGER card_changes_no_update BEFORE UPDATE ON card_changes
BEGIN
    SELECT RAISE(ABORT, 'card_changes is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER card_changes_no_delete BEFORE DELETE ON card_changes
BEGIN
    SELECT RAISE(ABORT, 'card_changes is insert-only: no row of it is ever deleted');
END;

-- One row per distinct checker. `definition` is the checker's serde JSON and `content_hash` the
-- SHA-256 of it as lowercase hex, so one checker is one row. `provenance` is JSON recording who wrote
-- the checker and how it was tested, and the hash leaves it out. The last CHECK keeps the row's
-- clause id and the definition's equal.
CREATE TABLE checkers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    clause_id INTEGER NOT NULL REFERENCES clauses (id),
    definition TEXT NOT NULL CHECK (json_valid(definition)),
    content_hash TEXT NOT NULL UNIQUE,
    provenance TEXT NOT NULL CHECK (json_valid(provenance)),
    CHECK (json_extract(definition, '$.clauseId') IS clause_id)
);

CREATE TRIGGER checkers_no_update BEFORE UPDATE ON checkers
BEGIN
    SELECT RAISE(ABORT, 'checkers is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER checkers_no_delete BEFORE DELETE ON checkers
BEGIN
    SELECT RAISE(ABORT, 'checkers is insert-only: no row of it is ever deleted');
END;

-- One row per ruleset version, with the time it was published, in UTC, and its content hash, taken
-- over its members' content hashes so that its membership alone determines it.
CREATE TABLE ruleset_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    published_at TEXT NOT NULL,
    content_hash TEXT NOT NULL
);

CREATE TRIGGER ruleset_versions_no_update BEFORE UPDATE ON ruleset_versions
BEGIN
    SELECT RAISE(ABORT, 'ruleset_versions is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER ruleset_versions_no_delete BEFORE DELETE ON ruleset_versions
BEGIN
    SELECT RAISE(ABORT, 'ruleset_versions is insert-only: no row of it is ever deleted');
END;

-- One row per checker a ruleset version holds.
CREATE TABLE ruleset_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ruleset_version_id INTEGER NOT NULL REFERENCES ruleset_versions (id),
    checker_id INTEGER NOT NULL REFERENCES checkers (id),
    UNIQUE (ruleset_version_id, checker_id)
);

CREATE TRIGGER ruleset_members_no_update BEFORE UPDATE ON ruleset_members
BEGIN
    SELECT RAISE(ABORT, 'ruleset_members is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER ruleset_members_no_delete BEFORE DELETE ON ruleset_members
BEGIN
    SELECT RAISE(ABORT, 'ruleset_members is insert-only: no row of it is ever deleted');
END;
