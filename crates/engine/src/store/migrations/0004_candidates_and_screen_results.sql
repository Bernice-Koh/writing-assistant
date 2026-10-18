-- Candidates and their screenings. A candidate is a model's rewrite of a passage, stored with what
-- is needed to reproduce it, and a screen result is one screening of a candidate. Both are inserted
-- once and never changed, so a candidate screened again gains a row and loses none.

-- One row per generated candidate. `content_hash` is the SHA-256, as lowercase hex, of the
-- passage's content hash followed by `text`, since a screening reads both. The rest is generation
-- provenance: `adapter_hash` is null for a candidate no adapter produced, and `decode_parameters`
-- is a JSON object. Two generators that produce one text for one passage are two rows sharing one
-- content hash, so the hash's index is not unique.
CREATE TABLE candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    passage_id INTEGER NOT NULL REFERENCES passages (id),
    text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    generator_id TEXT NOT NULL CHECK (generator_id <> ''),
    adapter_hash TEXT,
    decode_parameters TEXT NOT NULL CHECK (
        json_valid(decode_parameters) AND json_type(decode_parameters) = 'object'
    ),
    seed INTEGER NOT NULL,
    generation_index INTEGER NOT NULL CHECK (generation_index >= 0),
    repair_depth INTEGER NOT NULL CHECK (repair_depth >= 0)
);

CREATE INDEX candidates_content_hash ON candidates (content_hash);

CREATE TRIGGER candidates_no_update BEFORE UPDATE ON candidates
BEGIN
    SELECT RAISE(ABORT, 'candidates is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER candidates_no_delete BEFORE DELETE ON candidates
BEGIN
    SELECT RAISE(ABORT, 'candidates is insert-only: no row of it is ever deleted');
END;

-- One row per screening of a candidate, named by its content hash, under a ruleset version and the
-- telltale catalogue's content hash. `candidate_hash` has no foreign key, since no unique index
-- holds a candidate's content hash; the insert checks that a candidate holds it. The three form a
-- lookup key and not a uniqueness rule: a screening under a key already screened adds a row, and a
-- lookup returns the latest, the one with the highest id. The hard and soft results are JSON whose
-- shape the screen defines. `killing_rule` is the first failure in the order content, spelling,
-- grammar, style, tells, and null when nothing failed.
CREATE TABLE screen_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_hash TEXT NOT NULL,
    ruleset_version_id INTEGER NOT NULL REFERENCES ruleset_versions (id),
    catalogue_hash TEXT NOT NULL,
    app_version TEXT NOT NULL CHECK (app_version <> ''),
    hard_results TEXT NOT NULL CHECK (json_valid(hard_results)),
    soft_results TEXT NOT NULL CHECK (json_valid(soft_results)),
    killing_rule TEXT CHECK (
        killing_rule IS NULL
        OR killing_rule IN ('content', 'spelling', 'grammar', 'style', 'tells')
    )
);

CREATE INDEX screen_results_key ON screen_results
    (candidate_hash, ruleset_version_id, catalogue_hash, id);

CREATE TRIGGER screen_results_no_update BEFORE UPDATE ON screen_results
BEGIN
    SELECT RAISE(ABORT, 'screen_results is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER screen_results_no_delete BEFORE DELETE ON screen_results
BEGIN
    SELECT RAISE(ABORT, 'screen_results is insert-only: no row of it is ever deleted');
END;
