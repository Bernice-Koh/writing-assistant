//! The events table: each row is one action of the owner's, and events are the one record of those
//! actions from which everything downstream derives. An event holds its kind, the stream it was
//! collected in, the item and the candidate it concerns where it concerns one, the slot order shown,
//! the decision time, the register, the surface, and the Card version and the ruleset version in
//! force, and the store stamps the time it records it. Nothing writes an event until Phase 3, whose
//! questionnaire answers are the first; the working card's actions follow in Phase 4.
//!
//! An edit's diff and a typed replacement's text are the owner's words, and an answer's value can
//! hold a clause in the owner's words, so no error, log line or `Debug` rendering here carries a
//! payload: each names an operation, an id, a kind or a length instead.

use std::fmt;

use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, Type, ValueRef};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use super::candidates::CandidateId;
use super::card::CardVersionId;
use super::error::StoreError;
use super::items::{exists, ItemId};
use super::json::Json;
use super::passages::from_column;
use super::thread::Store;
use crate::flag::RulesetVersion;

const INSERT_EVENT: &str = "insert an event";
const READ_EVENT: &str = "read an event";

/// An event's id in the store, assigned at its insert and never given to another event. An `i64`
/// because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventId(pub i64);

/// The mode an action was collected in. An event's stream is set at its insert and never
/// reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    /// Ordinary use: accepting, editing or dismissing a rewrite, or picking between its two slots.
    Working,
    /// A deliberate sitting.
    Judgement,
}

/// What an action was, with what its kind carries beyond the columns every event has. The store
/// holds a diff and an answer's value as JSON without typing them, since their writers define their
/// shapes.
#[derive(Clone, PartialEq, Eq)]
pub enum EventKind {
    /// The owner chose one of an item's candidates.
    Pick,
    /// The owner labelled an item's candidate against a clause.
    Label,
    /// The owner edited an item's candidate.
    Edit { diff: Value },
    /// The owner dismissed an item.
    Dismissal,
    /// The owner accepted an item's candidate.
    Accept,
    /// The owner typed a replacement instead of either candidate. It is a habit event and never a
    /// training target.
    TypedReplacement { text: String },
    /// A Card version was published.
    CardPublish,
    /// A checker was admitted to the ruleset.
    CheckerAdmission,
    /// The owner answered a questionnaire item.
    QuestionnaireAnswer { value: Value },
    /// The owner answered an observation.
    ObservationAnswer { value: Value },
}

/// Shows the length in bytes of a payload's JSON in place of the payload, since a `Debug` rendering
/// ends up in log lines and test failure messages, and a payload holds the owner's words.
impl fmt::Debug for EventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.payload() {
            Some(payload) => formatter
                .debug_struct(&format!("{:?}", self.kind()))
                .field("payload_bytes", &payload.to_string().len())
                .finish(),
            None => fmt::Debug::fmt(&self.kind(), formatter),
        }
    }
}

/// An event to insert. The store stamps the time it records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub stream: Stream,
    /// The item the action concerns, which a pick, a label, an edit, a dismissal and an accept
    /// must name.
    pub item: Option<ItemId>,
    pub candidate: Option<CandidateId>,
    /// The candidates in the order the item's slots showed them.
    pub slot_order: Option<Vec<CandidateId>>,
    /// The time the owner took to act, in milliseconds.
    pub decision_ms: Option<u32>,
    /// A register name from the settings' register map.
    pub register: String,
    /// Where the owner was writing, `None` until the capture layer supplies it in Phase 2.
    pub surface: Option<String>,
    /// The Card version in force, `None` only while the store holds none.
    pub card_version: Option<CardVersionId>,
    /// The ruleset version in force, `None` only while the store holds none.
    pub ruleset_version: Option<RulesetVersion>,
}

/// An event as its row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// When the event was inserted, in UTC to the millisecond, by SQLite's clock.
    pub recorded_at: String,
    pub event: Event,
}

impl Store {
    /// Inserts `event`, stamped with SQLite's clock, and returns its id. Its stream is set here and
    /// never changes.
    ///
    /// The insert refuses what the table's CHECKs would refuse, before it writes: a pick, a label,
    /// an edit, a dismissal or an accept that names no item. An edit always carries its diff, since
    /// its kind holds it. Every id is checked against the store, the slot order's among them, and a
    /// version left unnamed is refused once the store holds a version of its kind, since one is in
    /// force from then on. Everything happens in one transaction, so a refused insert writes
    /// nothing.
    ///
    /// # Errors
    /// [`StoreError::EventWithoutItem`] for a kind that names an item and an event that names none.
    /// [`StoreError::NoItem`], [`StoreError::NoCandidate`], [`StoreError::NoCardVersion`] or
    /// [`StoreError::NoRulesetVersion`] for an id the store does not hold, and
    /// [`StoreError::EventWithoutCardVersion`] or [`StoreError::EventWithoutRulesetVersion`] for a
    /// version left unnamed while the store holds one, each inserting nothing.
    /// [`StoreError::Busy`] when another connection holds the write lock past the busy timeout,
    /// [`StoreError::Request`] for any other failure of the insert, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn insert_event(&self, event: &Event) -> Result<EventId, StoreError> {
        let event = event.clone();
        self.request(INSERT_EVENT, move |connection| insert(connection, &event))
            .await?
    }

    /// [`insert_event`](Self::insert_event) for a synchronous caller, such as `wa`, which blocks on
    /// the reply.
    ///
    /// # Errors
    /// As [`insert_event`](Self::insert_event).
    ///
    /// # Panics
    /// When called from async code, which awaits [`insert_event`](Self::insert_event) instead.
    pub fn insert_event_blocking(&self, event: &Event) -> Result<EventId, StoreError> {
        let event = event.clone();
        self.request_blocking(INSERT_EVENT, move |connection| insert(connection, &event))?
    }

    /// The event stored under `id`.
    ///
    /// # Errors
    /// [`StoreError::NoEvent`] when the store holds no event under `id`, [`StoreError::Busy`] when
    /// another connection holds a lock the read needs, [`StoreError::Request`] for any other
    /// failure of the read, and [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn event(&self, id: EventId) -> Result<StoredEvent, StoreError> {
        self.request(READ_EVENT, move |connection| read(connection, id))
            .await?
            .ok_or(StoreError::NoEvent { id })
    }

    /// [`event`](Self::event) for a synchronous caller, such as `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`event`](Self::event).
    ///
    /// # Panics
    /// When called from async code, which awaits [`event`](Self::event) instead.
    pub fn event_blocking(&self, id: EventId) -> Result<StoredEvent, StoreError> {
        self.request_blocking(READ_EVENT, move |connection| read(connection, id))?
            .ok_or(StoreError::NoEvent { id })
    }
}

/// Begins `IMMEDIATE`, since the lookups come before the write, and a deferred transaction that
/// reads first fails at its first write, without waiting, while another connection holds the write
/// lock. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert(
    connection: &mut Connection,
    event: &Event,
) -> rusqlite::Result<Result<EventId, StoreError>> {
    let kind = event.kind.kind();
    if kind.names_an_item() && event.item.is_none() {
        return Ok(Err(StoreError::EventWithoutItem {
            kind: kind.column_value(),
        }));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(item) = event.item {
        if !exists(
            &transaction,
            "SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1)",
            item.0,
        )? {
            // Returning drops the transaction, which rolls it back, and it wrote nothing.
            return Ok(Err(StoreError::NoItem { id: item }));
        }
    }
    for &candidate in event
        .candidate
        .iter()
        .chain(event.slot_order.iter().flatten())
    {
        if !exists(
            &transaction,
            "SELECT EXISTS (SELECT 1 FROM candidates WHERE id = ?1)",
            candidate.0,
        )? {
            return Ok(Err(StoreError::NoCandidate { id: candidate }));
        }
    }
    match event.card_version {
        Some(id) => {
            if !exists(
                &transaction,
                "SELECT EXISTS (SELECT 1 FROM card_versions WHERE id = ?1)",
                id.0,
            )? {
                return Ok(Err(StoreError::NoCardVersion { id }));
            }
        }
        None => {
            if holds_any(&transaction, "card_versions")? {
                return Ok(Err(StoreError::EventWithoutCardVersion {
                    kind: kind.column_value(),
                }));
            }
        }
    }
    match event.ruleset_version {
        Some(version) => {
            if !exists(
                &transaction,
                "SELECT EXISTS (SELECT 1 FROM ruleset_versions WHERE id = ?1)",
                version.0,
            )? {
                return Ok(Err(StoreError::NoRulesetVersion { version }));
            }
        }
        None => {
            if holds_any(&transaction, "ruleset_versions")? {
                return Ok(Err(StoreError::EventWithoutRulesetVersion {
                    kind: kind.column_value(),
                }));
            }
        }
    }
    transaction.execute(
        "INSERT INTO events (kind, recorded_at, stream, item_id, candidate_id, slot_order,
                             decision_ms, register, surface, card_version_id, ruleset_version_id,
                             payload)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11)",
        (
            kind,
            event.stream,
            event.item.map(|item| item.0),
            event.candidate.map(|candidate| candidate.0),
            event.slot_order.as_deref().map(candidate_ids).map(Json),
            event.decision_ms,
            &event.register,
            &event.surface,
            event.card_version.map(|id| id.0),
            event.ruleset_version.map(|version| version.0),
            event.kind.payload().map(Json),
        ),
    )?;
    let id = EventId(transaction.last_insert_rowid());
    transaction.commit()?;
    Ok(Ok(id))
}

/// Whether `table`, one of the version tables, holds a row. The name is the caller's literal and
/// never a value.
fn holds_any(connection: &Connection, table: &'static str) -> rusqlite::Result<bool> {
    connection.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM {table})"),
        [],
        |row| row.get(0),
    )
}

fn candidate_ids(candidates: &[CandidateId]) -> Vec<i64> {
    candidates.iter().map(|candidate| candidate.0).collect()
}

/// The payload's index among the columns the read selects, which the error for a payload that does
/// not fit its kind names.
const PAYLOAD_INDEX: usize = 11;

fn read(connection: &Connection, id: EventId) -> rusqlite::Result<Option<StoredEvent>> {
    connection
        .query_row(
            "SELECT kind, recorded_at, stream, item_id, candidate_id, slot_order, decision_ms,
                    register, surface, card_version_id, ruleset_version_id, payload
             FROM events WHERE id = ?1",
            [id.0],
            |row| {
                let kind: Kind = row.get(0)?;
                let slot_order: Option<Json<Vec<i64>>> = row.get(5)?;
                let payload: Option<Json<Value>> = row.get(PAYLOAD_INDEX)?;
                let kind = EventKind::from_columns(kind, payload.map(|Json(payload)| payload))
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            PAYLOAD_INDEX,
                            Type::Text,
                            "a payload that does not fit the event's kind".into(),
                        )
                    })?;
                Ok(StoredEvent {
                    recorded_at: row.get(1)?,
                    event: Event {
                        kind,
                        stream: row.get(2)?,
                        item: row.get::<_, Option<i64>>(3)?.map(ItemId),
                        candidate: row.get::<_, Option<i64>>(4)?.map(CandidateId),
                        slot_order: slot_order
                            .map(|Json(ids)| ids.into_iter().map(CandidateId).collect()),
                        decision_ms: row.get(6)?,
                        register: row.get(7)?,
                        surface: row.get(8)?,
                        card_version: row.get::<_, Option<i64>>(9)?.map(CardVersionId),
                        ruleset_version: row.get::<_, Option<i64>>(10)?.map(RulesetVersion),
                    },
                })
            },
        )
        .optional()
}

/// An event's kind without its data, as the `kind` column names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pick,
    Label,
    Edit,
    Dismissal,
    Accept,
    TypedReplacement,
    CardPublish,
    CheckerAdmission,
    QuestionnaireAnswer,
    ObservationAnswer,
}

impl Kind {
    const ALL: [Self; 10] = [
        Self::Pick,
        Self::Label,
        Self::Edit,
        Self::Dismissal,
        Self::Accept,
        Self::TypedReplacement,
        Self::CardPublish,
        Self::CheckerAdmission,
        Self::QuestionnaireAnswer,
        Self::ObservationAnswer,
    ];

    /// The kind as the `kind` column holds it, one of the names its CHECK constraint allows.
    fn column_value(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Label => "label",
            Self::Edit => "edit",
            Self::Dismissal => "dismissal",
            Self::Accept => "accept",
            Self::TypedReplacement => "typed_replacement",
            Self::CardPublish => "card_publish",
            Self::CheckerAdmission => "checker_admission",
            Self::QuestionnaireAnswer => "questionnaire_answer",
            Self::ObservationAnswer => "observation_answer",
        }
    }

    /// Whether an event of this kind must name an item, as the table's CHECK requires. The match
    /// names every variant, so a new kind cannot reach an insert unclassified.
    fn names_an_item(self) -> bool {
        match self {
            Self::Pick | Self::Label | Self::Edit | Self::Dismissal | Self::Accept => true,
            Self::TypedReplacement
            | Self::CardPublish
            | Self::CheckerAdmission
            | Self::QuestionnaireAnswer
            | Self::ObservationAnswer => false,
        }
    }
}

impl EventKind {
    fn kind(&self) -> Kind {
        match self {
            Self::Pick => Kind::Pick,
            Self::Label => Kind::Label,
            Self::Edit { .. } => Kind::Edit,
            Self::Dismissal => Kind::Dismissal,
            Self::Accept => Kind::Accept,
            Self::TypedReplacement { .. } => Kind::TypedReplacement,
            Self::CardPublish => Kind::CardPublish,
            Self::CheckerAdmission => Kind::CheckerAdmission,
            Self::QuestionnaireAnswer { .. } => Kind::QuestionnaireAnswer,
            Self::ObservationAnswer { .. } => Kind::ObservationAnswer,
        }
    }

    /// The `payload` column's value: the diff, the text as a JSON string, the answer's value, or
    /// nothing for a kind that carries none.
    fn payload(&self) -> Option<Value> {
        match self {
            Self::Edit { diff } => Some(diff.clone()),
            Self::TypedReplacement { text } => Some(Value::String(text.clone())),
            Self::QuestionnaireAnswer { value } | Self::ObservationAnswer { value } => {
                Some(value.clone())
            }
            Self::Pick
            | Self::Label
            | Self::Dismissal
            | Self::Accept
            | Self::CardPublish
            | Self::CheckerAdmission => None,
        }
    }

    /// The kind `payload` belongs with, or `None` when it does not fit `kind`, as with no diff for
    /// an edit or a payload on a kind that carries none.
    fn from_columns(kind: Kind, payload: Option<Value>) -> Option<Self> {
        match (kind, payload) {
            (Kind::Pick, None) => Some(Self::Pick),
            (Kind::Label, None) => Some(Self::Label),
            (Kind::Edit, Some(diff)) => Some(Self::Edit { diff }),
            (Kind::Dismissal, None) => Some(Self::Dismissal),
            (Kind::Accept, None) => Some(Self::Accept),
            (Kind::TypedReplacement, Some(Value::String(text))) => {
                Some(Self::TypedReplacement { text })
            }
            (Kind::CardPublish, None) => Some(Self::CardPublish),
            (Kind::CheckerAdmission, None) => Some(Self::CheckerAdmission),
            (Kind::QuestionnaireAnswer, Some(value)) => Some(Self::QuestionnaireAnswer { value }),
            (Kind::ObservationAnswer, Some(value)) => Some(Self::ObservationAnswer { value }),
            _ => None,
        }
    }
}

impl Stream {
    const ALL: [Self; 2] = [Self::Working, Self::Judgement];

    /// The stream as the `stream` column holds it, one of the names its CHECK constraint allows.
    fn column_value(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Judgement => "judgement",
        }
    }
}

impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.column_value().into())
    }
}

impl FromSql for Kind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        from_column(value, &Self::ALL, Self::column_value)
    }
}

impl ToSql for Stream {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.column_value().into())
    }
}

impl FromSql for Stream {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        from_column(value, &Self::ALL, Self::column_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::candidates::tests::{
        insert as insert_candidate, insert_passage, local, teacher, PASSAGE_TEXT, TEXT,
    };
    use crate::store::card::tests::{
        assert_insert_only, assert_utc_to_the_millisecond, new, open, refusal, rows, sqlite_now,
        HEDGES, NOTE,
    };
    use crate::store::items::{Contrast, ItemType, NewItem};
    use crate::store::passages::PassageKind;
    use crate::test_support::TempRoot;
    use rusqlite::types::Value as SqlValue;
    use serde_json::json;

    const REGISTER: &str = "default";
    const SURFACE: &str = "WINWORD.EXE";

    /// Words the owner wrote, which no rendering or error may carry.
    const EDITED_WORDS: &str = "the owner's own wording of the edited sentence";
    const REPLACEMENT: &str = "A sentence the owner typed instead of either candidate.";
    const FREE_CLAUSE: &str = "Open with the finding, never with the method.";

    /// An event row written past the store's insert, for the tests of what the table refuses. The
    /// parameters, in order, are those [`row`] builds.
    const EVENT_ROW: &str = "INSERT INTO events (kind, recorded_at, stream, item_id, candidate_id,
                                                register, card_version_id, ruleset_version_id,
                                                payload)
                             VALUES (?1, '2026-10-20T09:00:00.000Z', 'working', ?2, ?3, 'default',
                                     ?4, ?5, ?6)";
    const ITEM: usize = 1;
    const CANDIDATE: usize = 2;
    const CARD_VERSION: usize = 3;
    const RULESET_VERSION: usize = 4;
    const PAYLOAD: usize = 5;

    /// The rows an event names: an item, its two candidates on one passage, and the Card version and
    /// ruleset version it was built under.
    struct Fixture {
        item: ItemId,
        slots: [CandidateId; 2],
        card: CardVersionId,
        ruleset: RulesetVersion,
    }

    fn fixture(store: &Store) -> Fixture {
        let card = store
            .insert_card_version_blocking(&[new(HEDGES)], NOTE)
            .unwrap();
        let ruleset = store.insert_ruleset_version_blocking(&[]).unwrap();
        let passage = insert_passage(store, PASSAGE_TEXT);
        let slots = [local(), teacher()]
            .map(|generation| insert_candidate(store, passage, TEXT, &generation));
        let item = store
            .insert_item_blocking(&NewItem {
                item_type: ItemType::FreshCore,
                passage,
                register: REGISTER.to_owned(),
                slots: slots.to_vec(),
                card_version: card,
                ruleset_version: ruleset,
                contrast: Contrast::None,
            })
            .unwrap();
        Fixture {
            item,
            slots,
            card,
            ruleset,
        }
    }

    fn diff() -> Value {
        json!({ "replaced": [4, 9], "with": EDITED_WORDS })
    }

    /// One event kind of each of the ten, in the order of [`Kind::ALL`].
    fn every_kind() -> [EventKind; 10] {
        [
            EventKind::Pick,
            EventKind::Label,
            EventKind::Edit { diff: diff() },
            EventKind::Dismissal,
            EventKind::Accept,
            EventKind::TypedReplacement {
                text: REPLACEMENT.to_owned(),
            },
            EventKind::CardPublish,
            EventKind::CheckerAdmission,
            EventKind::QuestionnaireAnswer {
                value: json!({ "section": "free clauses", "clause": FREE_CLAUSE }),
            },
            EventKind::ObservationAnswer {
                value: json!("not me"),
            },
        ]
    }

    /// A working event of `kind` on the fixture's item, with every field set.
    fn on_item(kind: EventKind, fixture: &Fixture) -> Event {
        Event {
            kind,
            stream: Stream::Working,
            item: Some(fixture.item),
            candidate: Some(fixture.slots[0]),
            slot_order: Some(vec![fixture.slots[1], fixture.slots[0]]),
            decision_ms: Some(1840),
            register: REGISTER.to_owned(),
            surface: Some(SURFACE.to_owned()),
            card_version: Some(fixture.card),
            ruleset_version: Some(fixture.ruleset),
        }
    }

    /// A judgement event of `kind` naming no item, candidate, slot order, decision time or surface,
    /// under the fixture's versions.
    fn off_item(kind: EventKind, fixture: &Fixture) -> Event {
        Event {
            stream: Stream::Judgement,
            item: None,
            candidate: None,
            slot_order: None,
            decision_ms: None,
            surface: None,
            ..on_item(kind, fixture)
        }
    }

    /// The parameters of [`EVENT_ROW`] for a row of `kind` that names the fixture's item, its first
    /// candidate and its versions, with an empty JSON object as its payload.
    fn row(kind: &str, fixture: &Fixture) -> Vec<SqlValue> {
        vec![
            SqlValue::Text(kind.to_owned()),
            SqlValue::Integer(fixture.item.0),
            SqlValue::Integer(fixture.slots[0].0),
            SqlValue::Integer(fixture.card.0),
            SqlValue::Integer(fixture.ruleset.0),
            SqlValue::Text("{}".to_owned()),
        ]
    }

    fn insert(store: &Store, event: &Event) -> EventId {
        store.insert_event_blocking(event).unwrap()
    }

    fn assert_nothing_written(store: &Store) {
        assert!(rows(store, "events").is_empty());
    }

    #[test]
    fn the_ten_kinds_insert_and_read_back_and_the_table_refuses_an_eleventh() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);

        for kind in every_kind() {
            let id = insert(&store, &on_item(kind.clone(), &fixture));
            assert_eq!(store.event_blocking(id).unwrap().event.kind, kind);
        }
        let eleventh = refusal(&store, EVENT_ROW, row("reaction", &fixture));

        let names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.column_value()).collect();
        assert_eq!(
            names,
            [
                "pick",
                "label",
                "edit",
                "dismissal",
                "accept",
                "typed_replacement",
                "card_publish",
                "checker_admission",
                "questionnaire_answer",
                "observation_answer",
            ]
        );
        assert_eq!(every_kind().map(|kind| kind.kind()), Kind::ALL);
        assert_eq!(eleventh, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_eq!(rows(&store, "events").len(), 10);
    }

    #[test]
    fn an_event_reads_back_whole_stamped_with_sqlites_clock() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let edit = on_item(EventKind::Edit { diff: diff() }, &fixture);
        let replacement = Event {
            item: Some(fixture.item),
            decision_ms: Some(0),
            ..off_item(
                EventKind::TypedReplacement {
                    text: REPLACEMENT.to_owned(),
                },
                &fixture,
            )
        };
        let answer = off_item(
            EventKind::QuestionnaireAnswer {
                value: json!({ "feature": "hedging", "answer": "limit" }),
            },
            &fixture,
        );
        let before = sqlite_now(&store);

        let ids = [&edit, &replacement, &answer].map(|event| insert(&store, event));

        let after = sqlite_now(&store);
        let stored = ids.map(|id| store.event_blocking(id).unwrap());
        assert_eq!(stored[0].event, edit);
        assert_eq!(stored[1].event, replacement);
        assert_eq!(stored[2].event, answer);
        for stored in &stored {
            assert_utc_to_the_millisecond(&stored.recorded_at);
            assert!(
                before <= stored.recorded_at && stored.recorded_at <= after,
                "{} outside {before} to {after}",
                stored.recorded_at
            );
        }
        // The columns, in the table's order from the id: the slot order as a JSON array of the
        // candidates' ids in the order shown, the decision time in milliseconds, and the payload
        // as JSON, the replacement's text as a string.
        let table = rows(&store, "events");
        assert_eq!(
            table[0][6],
            SqlValue::Text(format!("[{},{}]", fixture.slots[1].0, fixture.slots[0].0))
        );
        assert_eq!(table[0][7], SqlValue::Integer(1840));
        let SqlValue::Text(edit_payload) = &table[0][12] else {
            panic!("an edit's payload is not text");
        };
        assert_eq!(serde_json::from_str::<Value>(edit_payload).unwrap(), diff());
        assert_eq!(
            table[1][12],
            SqlValue::Text(serde_json::to_string(REPLACEMENT).unwrap())
        );
    }

    #[test]
    fn an_events_stream_is_set_at_insert_and_an_update_raises_and_leaves_it() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let working = insert(&store, &on_item(EventKind::Pick, &fixture));
        let judgement = insert(
            &store,
            &Event {
                stream: Stream::Judgement,
                ..on_item(EventKind::Pick, &fixture)
            },
        );

        let changed = store.request_blocking("change an event's stream", move |connection| {
            connection.execute(
                "UPDATE events SET stream = 'judgement' WHERE id = ?1",
                [working.0],
            )
        });

        match changed {
            Err(StoreError::Request {
                source: rusqlite::Error::SqliteFailure(failure, Some(message)),
                ..
            }) => {
                assert_eq!(
                    failure.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
                );
                assert!(message.starts_with("events is insert-only"), "{message}");
            }
            other => panic!("expected the trigger to raise, got {other:?}"),
        }
        assert_eq!(
            store.event_blocking(working).unwrap().event.stream,
            Stream::Working
        );
        assert_eq!(
            store.event_blocking(judgement).unwrap().event.stream,
            Stream::Judgement
        );
    }

    #[test]
    fn an_id_the_store_does_not_hold_is_refused_by_name_and_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let valid = on_item(EventKind::Accept, &fixture);
        let absent_item = ItemId(fixture.item.0 + 1);
        let absent_candidate = CandidateId(fixture.slots[1].0 + 1);
        let absent_card = CardVersionId(fixture.card.0 + 1);
        let absent_ruleset = RulesetVersion(fixture.ruleset.0 + 1);
        let refused = |event: Event| store.insert_event_blocking(&event).unwrap_err();

        let no_item = refused(Event {
            item: Some(absent_item),
            ..valid.clone()
        });
        let no_candidate = refused(Event {
            candidate: Some(absent_candidate),
            ..valid.clone()
        });
        let no_shown_candidate = refused(Event {
            slot_order: Some(vec![fixture.slots[0], absent_candidate]),
            ..valid.clone()
        });
        let no_card = refused(Event {
            card_version: Some(absent_card),
            ..valid.clone()
        });
        let no_ruleset = refused(Event {
            ruleset_version: Some(absent_ruleset),
            ..valid
        });

        assert!(
            matches!(no_item, StoreError::NoItem { id } if id == absent_item),
            "{no_item:?}"
        );
        assert_eq!(no_item.to_string(), "the store holds no item with id 2");
        for no_candidate in [&no_candidate, &no_shown_candidate] {
            assert!(
                matches!(no_candidate, StoreError::NoCandidate { id } if *id == absent_candidate),
                "{no_candidate:?}"
            );
            assert_eq!(
                no_candidate.to_string(),
                "the store holds no candidate with id 3"
            );
        }
        assert!(
            matches!(no_card, StoreError::NoCardVersion { id } if id == absent_card),
            "{no_card:?}"
        );
        assert_eq!(
            no_card.to_string(),
            "the store holds no Card version with id 2"
        );
        assert!(
            matches!(no_ruleset, StoreError::NoRulesetVersion { version } if version == absent_ruleset),
            "{no_ruleset:?}"
        );
        assert_eq!(
            no_ruleset.to_string(),
            "the store holds no ruleset version 2"
        );
        assert_nothing_written(&store);
    }

    #[test]
    fn the_tables_keys_refuse_an_absent_item_candidate_card_version_or_ruleset_version() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let absent = |column: usize| {
            let mut params = row("accept", &fixture);
            params[column] = SqlValue::Integer(1000);
            params
        };

        for (column, name) in [
            (ITEM, "item"),
            (CANDIDATE, "candidate"),
            (CARD_VERSION, "Card version"),
            (RULESET_VERSION, "ruleset version"),
        ] {
            assert_eq!(
                refusal(&store, EVENT_ROW, absent(column)),
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
                "an absent {name}"
            );
        }
        assert_nothing_written(&store);
    }

    #[test]
    fn a_kind_that_names_an_item_is_refused_without_one_and_the_others_insert_without_one() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let (needs_item, others): (Vec<EventKind>, Vec<EventKind>) = every_kind()
            .into_iter()
            .partition(|kind| kind.kind().names_an_item());

        for kind in needs_item {
            let name = kind.kind().column_value();
            let error = store
                .insert_event_blocking(&off_item(kind, &fixture))
                .unwrap_err();
            assert!(
                matches!(error, StoreError::EventWithoutItem { kind } if kind == name),
                "{error:?}"
            );
            assert_eq!(
                error.to_string(),
                format!("an event of kind {name} names an item, and the insert named none")
            );
        }
        assert_nothing_written(&store);
        let inserted = others.len();
        for kind in others {
            insert(&store, &off_item(kind, &fixture));
        }

        assert_eq!(
            Kind::ALL
                .iter()
                .filter(|kind| kind.names_an_item())
                .map(|kind| kind.column_value())
                .collect::<Vec<_>>(),
            ["pick", "label", "edit", "dismissal", "accept"]
        );
        assert_eq!(rows(&store, "events").len(), inserted);
    }

    #[test]
    fn the_table_refuses_an_item_kind_without_an_item_and_an_edit_without_its_diff() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);

        for kind in ["pick", "label", "edit", "dismissal", "accept"] {
            let mut params = row(kind, &fixture);
            params[ITEM] = SqlValue::Null;
            assert_eq!(
                refusal(&store, EVENT_ROW, params),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                "a {kind} without an item"
            );
        }
        let mut edit_without_diff = row("edit", &fixture);
        edit_without_diff[PAYLOAD] = SqlValue::Null;
        let edit = refusal(&store, EVENT_ROW, edit_without_diff);

        assert_eq!(edit, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_nothing_written(&store);
    }

    /// The first questionnaire's answers come before the first Card version, which is built from
    /// them, and before the first ruleset version.
    #[test]
    fn a_version_left_unnamed_is_refused_once_the_store_holds_one_of_its_kind() {
        let root = TempRoot::new();
        let store = open(&root);
        let answer = |card_version, ruleset_version| Event {
            kind: EventKind::QuestionnaireAnswer {
                value: json!("british"),
            },
            stream: Stream::Working,
            item: None,
            candidate: None,
            slot_order: None,
            decision_ms: Some(5200),
            register: REGISTER.to_owned(),
            surface: None,
            card_version,
            ruleset_version,
        };

        let before_either = insert(&store, &answer(None, None));
        let card = store
            .insert_card_version_blocking(&[new(HEDGES)], NOTE)
            .unwrap();
        let no_card = store
            .insert_event_blocking(&answer(None, None))
            .unwrap_err();
        let card_only = insert(&store, &answer(Some(card), None));
        let ruleset = store.insert_ruleset_version_blocking(&[]).unwrap();
        let no_ruleset = store
            .insert_event_blocking(&answer(Some(card), None))
            .unwrap_err();
        let both = insert(&store, &answer(Some(card), Some(ruleset)));

        assert_eq!(
            store.event_blocking(before_either).unwrap().event,
            answer(None, None)
        );
        assert!(
            matches!(
                no_card,
                StoreError::EventWithoutCardVersion {
                    kind: "questionnaire_answer"
                }
            ),
            "{no_card:?}"
        );
        assert_eq!(
            no_card.to_string(),
            "an event of kind questionnaire_answer names no Card version, and the store holds one"
        );
        assert_eq!(
            store.event_blocking(card_only).unwrap().event,
            answer(Some(card), None)
        );
        assert!(
            matches!(
                no_ruleset,
                StoreError::EventWithoutRulesetVersion {
                    kind: "questionnaire_answer"
                }
            ),
            "{no_ruleset:?}"
        );
        assert_eq!(
            no_ruleset.to_string(),
            "an event of kind questionnaire_answer names no ruleset version, and the store holds \
             one"
        );
        assert_eq!(
            store.event_blocking(both).unwrap().event,
            answer(Some(card), Some(ruleset))
        );
        assert_eq!(rows(&store, "events").len(), 3);
    }

    #[test]
    fn updating_or_deleting_an_event_raises_and_leaves_it_as_it_was() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let id = insert(&store, &on_item(EventKind::Edit { diff: diff() }, &fixture));
        let stored = store.event_blocking(id).unwrap();

        assert_insert_only(&store, "events");

        assert_eq!(store.event_blocking(id).unwrap(), stored);
    }

    #[test]
    fn no_debug_rendering_or_error_carries_a_diff_a_replacement_or_an_answer() {
        let root = TempRoot::new();
        let store = open(&root);
        let fixture = fixture(&store);
        let owner_words = [EDITED_WORDS, REPLACEMENT, FREE_CLAUSE];
        let events: Vec<Event> = every_kind()
            .into_iter()
            .map(|kind| on_item(kind, &fixture))
            .collect();
        let ids: Vec<EventId> = events.iter().map(|event| insert(&store, event)).collect();
        let mut renderings: Vec<String> = events.iter().map(|event| format!("{event:?}")).collect();
        renderings.extend(
            ids.iter()
                .map(|&id| format!("{:?}", store.event_blocking(id).unwrap())),
        );
        // A replacement whose payload is no JSON string, written past the store's insert, fails the
        // read.
        let mut misfit_row = row("typed_replacement", &fixture);
        misfit_row[PAYLOAD] = SqlValue::Text(json!({ "text": REPLACEMENT }).to_string());
        let misfit = store
            .request_blocking(
                "insert a replacement with an object payload",
                move |connection| {
                    connection.execute(EVENT_ROW, rusqlite::params_from_iter(misfit_row))?;
                    Ok(EventId(connection.last_insert_rowid()))
                },
            )
            .unwrap();
        let misfit_error = store.event_blocking(misfit).unwrap_err();
        store
            .request_blocking("drop the events table", |connection| {
                connection.execute_batch("DROP TABLE events")
            })
            .unwrap();
        let insert_error = store.insert_event_blocking(&events[2]).unwrap_err();
        let read_error = store.event_blocking(ids[2]).unwrap_err();

        // Checked before anything is printed in a failure message below.
        let error_renderings = [&misfit_error, &insert_error, &read_error]
            .into_iter()
            .flat_map(|error| [error.to_string(), format!("{error:?}")]);
        for (index, rendering) in renderings
            .iter()
            .cloned()
            .chain(error_renderings)
            .enumerate()
        {
            for (word, words) in owner_words.iter().enumerate() {
                assert!(
                    !rendering.contains(words),
                    "rendering {index} carries the owner's words {word}"
                );
            }
        }
        assert!(
            renderings[2].contains("kind: Edit { payload_bytes: "),
            "{}",
            renderings[2]
        );
        assert!(renderings[0].contains("kind: Pick,"), "{}", renderings[0]);
        assert!(
            matches!(
                misfit_error,
                StoreError::Request {
                    operation: READ_EVENT,
                    source: rusqlite::Error::FromSqlConversionFailure(PAYLOAD_INDEX, ..),
                }
            ),
            "{misfit_error:?}"
        );
        assert!(
            matches!(
                insert_error,
                StoreError::Request {
                    operation: INSERT_EVENT,
                    ..
                }
            ),
            "{insert_error:?}"
        );
        assert!(
            matches!(
                read_error,
                StoreError::Request {
                    operation: READ_EVENT,
                    ..
                }
            ),
            "{read_error:?}"
        );
    }

    #[test]
    fn reading_an_id_the_store_does_not_hold_returns_no_event() {
        let root = TempRoot::new();
        let store = open(&root);

        let error = store.event_blocking(EventId(1)).unwrap_err();

        assert!(
            matches!(error, StoreError::NoEvent { id: EventId(1) }),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no event with id 1");
    }

    /// Blocking on the store inside the runtime panics, so the fixtures use the awaited forms too.
    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_insert_and_read_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);
        let card = store
            .insert_card_version(&[new(HEDGES)], NOTE)
            .await
            .unwrap();
        let ruleset = store.insert_ruleset_version(&[]).await.unwrap();
        let passage = store
            .insert_passage(PassageKind::Captured, PASSAGE_TEXT, None)
            .await
            .unwrap();
        let first = store
            .insert_candidate(passage, TEXT, &local())
            .await
            .unwrap();
        let second = store
            .insert_candidate(passage, TEXT, &teacher())
            .await
            .unwrap();
        let item = store
            .insert_item(&NewItem {
                item_type: ItemType::FreshCore,
                passage,
                register: REGISTER.to_owned(),
                slots: vec![first, second],
                card_version: card,
                ruleset_version: ruleset,
                contrast: Contrast::None,
            })
            .await
            .unwrap();
        let fixture = Fixture {
            item,
            slots: [first, second],
            card,
            ruleset,
        };
        let edit = on_item(EventKind::Edit { diff: diff() }, &fixture);

        let id = store.insert_event(&edit).await.unwrap();
        let read = store.event(id).await.unwrap();
        let refused = store
            .insert_event(&Event {
                item: None,
                ..edit.clone()
            })
            .await;
        let absent = store.event(EventId(id.0 + 1)).await;

        assert_eq!(read.event, edit);
        assert!(
            matches!(refused, Err(StoreError::EventWithoutItem { kind: "edit" })),
            "{refused:?}"
        );
        assert!(
            matches!(absent, Err(StoreError::NoEvent { .. })),
            "{absent:?}"
        );
    }
}
