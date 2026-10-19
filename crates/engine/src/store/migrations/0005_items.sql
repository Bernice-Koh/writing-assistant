-- Items: the record of what was shown to the owner. An item is one passage with a candidate in each
-- of its two slots, under the Card version and the ruleset version in force. Every row of both
-- tables is inserted once and never changed.

-- One row per item. `register` is a name from the settings' register map. `contrast` is `minimal`
-- for an item whose slots differ in one residual clause, written into a clean candidate as the
-- clause's twin, and `contrast_clause_id` names that clause; it is `none` otherwise, with no
-- clause. `never_export` is 1 for an item that must never reach a training file: every item on a
-- bench, gate or sealed passage, and every anchor, repeat and clause audit. The store derives it at
-- insert from `item_type` and the passage's split, and the triggers keep it from ever changing.
CREATE TABLE items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item_type TEXT NOT NULL CHECK (
        item_type IN ('fresh_core', 'fresh_fringe', 'anchor', 'repeat', 'clause_audit')
    ),
    passage_id INTEGER NOT NULL REFERENCES passages (id),
    register TEXT NOT NULL CHECK (register <> ''),
    card_version_id INTEGER NOT NULL REFERENCES card_versions (id),
    ruleset_version_id INTEGER NOT NULL REFERENCES ruleset_versions (id),
    contrast TEXT NOT NULL CHECK (contrast IN ('none', 'minimal')),
    contrast_clause_id INTEGER REFERENCES clauses (id),
    never_export INTEGER NOT NULL CHECK (never_export IN (0, 1)),
    CHECK ((contrast = 'minimal') = (contrast_clause_id IS NOT NULL))
);

CREATE TRIGGER items_no_update BEFORE UPDATE ON items
BEGIN
    SELECT RAISE(ABORT, 'items is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER items_no_delete BEFORE DELETE ON items
BEGIN
    SELECT RAISE(ABORT, 'items is insert-only: no row of it is ever deleted');
END;

-- The key a slot names, so that a slot's generator id is always its candidate's. The id alone is
-- unique, so the index holds for every row `candidates` has or will have.
CREATE UNIQUE INDEX candidates_id_generator_id ON candidates (id, generator_id);

-- One row per slot of an item: the candidate shown at the slot's position and the generator that
-- produced it. An item holds a position once.
CREATE TABLE item_slots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id INTEGER NOT NULL REFERENCES items (id),
    position INTEGER NOT NULL CHECK (position IN (0, 1)),
    candidate_id INTEGER NOT NULL,
    generator_id TEXT NOT NULL,
    FOREIGN KEY (candidate_id, generator_id) REFERENCES candidates (id, generator_id),
    UNIQUE (item_id, position)
);

CREATE TRIGGER item_slots_no_update BEFORE UPDATE ON item_slots
BEGIN
    SELECT RAISE(ABORT, 'item_slots is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER item_slots_no_delete BEFORE DELETE ON item_slots
BEGIN
    SELECT RAISE(ABORT, 'item_slots is insert-only: no row of it is ever deleted');
END;
