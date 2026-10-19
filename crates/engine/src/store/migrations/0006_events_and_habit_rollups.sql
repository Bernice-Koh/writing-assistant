-- Events and habit rollups. An event is one action of the owner's, and events are the one record of
-- those actions from which everything downstream derives. A habit rollup is one count of the style
-- flags raised on the owner's typing for a day and a clause. Every row of both tables is inserted
-- once and never changed.

-- One row per action. `recorded_at` is the time of the insert, in UTC, by SQLite's clock. `stream`
-- is the mode the action was collected in, `working` for ordinary use and `judgement` for a
-- deliberate sitting; it is set at insert, and the triggers keep it from ever changing. `item_id`
-- and `candidate_id` name the item and the candidate the action concerns, where it concerns one.
-- `slot_order` is a JSON array of the candidate ids in the order the slots showed them, and
-- `decision_ms` the time the owner took to act, in milliseconds. `register` is a name from the
-- settings' register map, and `surface` names where the owner was writing, null until the capture
-- layer supplies it. `card_version_id` and `ruleset_version_id` are the versions in force, null
-- only before the first of each was published, as for the first questionnaire's answers, from
-- which the first Card version is built. `payload` is JSON holding what a kind carries beyond these
-- columns: an edit's diff, an answer's value, a typed replacement's text. The last two CHECKs hold
-- the kinds to their shapes: a pick, a label, an edit, a dismissal and an accept name an item, and
-- an edit carries its diff.
CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (
        kind IN (
            'pick', 'label', 'edit', 'dismissal', 'accept', 'typed_replacement', 'card_publish',
            'checker_admission', 'questionnaire_answer', 'observation_answer'
        )
    ),
    recorded_at TEXT NOT NULL,
    stream TEXT NOT NULL CHECK (stream IN ('working', 'judgement')),
    item_id INTEGER REFERENCES items (id),
    candidate_id INTEGER REFERENCES candidates (id),
    slot_order TEXT CHECK (
        slot_order IS NULL OR (json_valid(slot_order) AND json_type(slot_order) = 'array')
    ),
    decision_ms INTEGER CHECK (decision_ms IS NULL OR decision_ms >= 0),
    register TEXT NOT NULL CHECK (register <> ''),
    surface TEXT CHECK (surface IS NULL OR surface <> ''),
    card_version_id INTEGER REFERENCES card_versions (id),
    ruleset_version_id INTEGER REFERENCES ruleset_versions (id),
    payload TEXT CHECK (payload IS NULL OR json_valid(payload)),
    CHECK (kind NOT IN ('pick', 'label', 'edit', 'dismissal', 'accept') OR item_id IS NOT NULL),
    CHECK (kind <> 'edit' OR payload IS NOT NULL)
);

CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events is insert-only: no row of it is ever deleted');
END;

-- One row per count of the style flags raised on the owner's typing: a calendar day as
-- `YYYY-MM-DD`, the clause the flags cite, how many there were, and how many words were checked.
-- The habit counters write a row, never one per flag, so no row holds a flag's text or its span. A
-- day's total for a clause is the sum of its rows, and the table has no unique key: a later count
-- for the same day and clause is a row of its own.
CREATE TABLE habit_rollups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL CHECK (date(day) IS day),
    clause_id INTEGER NOT NULL REFERENCES clauses (id),
    style_flags INTEGER NOT NULL CHECK (style_flags >= 0),
    words_checked INTEGER NOT NULL CHECK (words_checked >= 0)
);

CREATE INDEX habit_rollups_day_clause ON habit_rollups (day, clause_id);

CREATE TRIGGER habit_rollups_no_update BEFORE UPDATE ON habit_rollups
BEGIN
    SELECT RAISE(ABORT, 'habit_rollups is insert-only: no row of it is ever updated');
END;

CREATE TRIGGER habit_rollups_no_delete BEFORE DELETE ON habit_rollups
BEGIN
    SELECT RAISE(ABORT, 'habit_rollups is insert-only: no row of it is ever deleted');
END;
