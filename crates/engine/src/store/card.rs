//! The Style Card's tables: every clause the Card has held, each wording of each clause, and each
//! Card version with the wordings it holds and the clauses it changed. Every row is inserted once
//! and never changed, so a version reads back as it was published however far the Card has moved
//! on. The store gives a clause its id when the clause's first wording is inserted and never gives
//! that id to another clause, so a clause keeps its id across rewordings and after it has left the
//! Card. Nothing writes a Card until Phase 3, which builds publishing on these inserts.
//!
//! A clause's text, its twin and its example pair, and a version's change note, are the owner's
//! expressed preferences. No error, log line or `Debug` rendering here carries them: each names an
//! operation or an id instead.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::error::StoreError;
use super::json::Json;
use super::thread::Store;
use crate::flag::ClauseId;

const INSERT_CARD_VERSION: &str = "insert a Card version";
const READ_CARD_VERSION: &str = "read a Card version";

/// The id of one wording of one clause, assigned at the wording's insert and never given to another
/// wording. An `i64` because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClauseTextId(pub i64);

/// A Card version's id, assigned at its insert and never given to another version. An `i64` because
/// SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CardVersionId(pub i64);

/// One wording of a clause.
#[derive(Clone, PartialEq, Eq)]
pub struct ClauseWording {
    /// One behaviour, stated so that it can be inverted.
    pub text: String,
    /// The text's negation.
    pub twin: String,
    /// The registers the clause applies in, by the names the settings' register map gives them.
    /// Unset, it applies in every register.
    pub register_scope: Option<BTreeSet<String>>,
    pub example: Option<ExamplePair>,
}

/// A sentence that follows a clause and one that breaks it.
#[derive(Clone, PartialEq, Eq)]
pub struct ExamplePair {
    pub follows: String,
    pub breaks: String,
}

/// One clause of a Card version to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardMember {
    /// A wording the store holds: kept from the latest version, or restored from an earlier one.
    Stored(ClauseTextId),
    /// A new wording of a clause the store holds.
    Reworded {
        clause_id: ClauseId,
        wording: ClauseWording,
    },
    /// The first wording of a new clause, which the insert gives its id.
    New(ClauseWording),
}

/// A clause as a Card version holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedClause {
    pub clause_id: ClauseId,
    pub text_id: ClauseTextId,
    pub wording: ClauseWording,
}

/// A Card version as its rows hold it.
#[derive(Clone, PartialEq, Eq)]
pub struct PublishedCard {
    pub id: CardVersionId,
    /// When the version was inserted, in UTC to the millisecond, by SQLite's clock.
    pub published_at: String,
    /// What the version changed, in the owner's terms.
    pub change_note: String,
    /// In the order the version's insert listed them.
    pub clauses: Vec<PublishedClause>,
    /// The clauses the version added, reworded or dropped against the version before it, in
    /// ascending order of id. The first version changed every clause it holds.
    pub changed: Vec<ClauseId>,
}

/// Shows each text's length in bytes in place of the text, since a `Debug` rendering ends up in log
/// lines and test failure messages.
impl fmt::Debug for ClauseWording {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClauseWording")
            .field("text_bytes", &self.text.len())
            .field("twin_bytes", &self.twin.len())
            .field("register_scope", &self.register_scope)
            .field("example", &self.example)
            .finish()
    }
}

/// Shows each sentence's length in bytes in place of the sentence, as [`ClauseWording`] does.
impl fmt::Debug for ExamplePair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExamplePair")
            .field("follows_bytes", &self.follows.len())
            .field("breaks_bytes", &self.breaks.len())
            .finish()
    }
}

/// Shows the change note's length in bytes in place of the note, as [`ClauseWording`] does.
impl fmt::Debug for PublishedCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedCard")
            .field("id", &self.id)
            .field("published_at", &self.published_at)
            .field("change_note_bytes", &self.change_note.len())
            .field("clauses", &self.clauses)
            .field("changed", &self.changed)
            .finish()
    }
}

impl Store {
    /// Inserts a Card version holding `members`, in their order, with `change_note`, and returns its
    /// id. A new clause gets its id here.
    ///
    /// Everything happens in one transaction, and every member is checked against the store before
    /// anything is written, so a refused insert writes nothing. The version records as changed each
    /// clause whose wording differs from the one the latest stored version holds for it, which
    /// covers a new clause, a reworded one and one restored to an earlier wording, and each clause
    /// the latest version holds that this one drops.
    ///
    /// # Errors
    /// [`StoreError::NoClauseText`] for a stored wording the store does not hold,
    /// [`StoreError::NoClause`] for a new wording of a clause the store does not hold, and
    /// [`StoreError::ClauseRepeated`] when two members are wordings of one clause.
    /// [`StoreError::Busy`] when another connection holds the write lock past the busy timeout,
    /// [`StoreError::Request`] for any other failure of the insert, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn insert_card_version(
        &self,
        members: &[CardMember],
        change_note: &str,
    ) -> Result<CardVersionId, StoreError> {
        self.request(INSERT_CARD_VERSION, insert_work(members, change_note))
            .await?
    }

    /// [`insert_card_version`](Self::insert_card_version) for a synchronous caller, such as `wa`,
    /// which blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_card_version`](Self::insert_card_version).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`insert_card_version`](Self::insert_card_version) instead.
    pub fn insert_card_version_blocking(
        &self,
        members: &[CardMember],
        change_note: &str,
    ) -> Result<CardVersionId, StoreError> {
        self.request_blocking(INSERT_CARD_VERSION, insert_work(members, change_note))?
    }

    /// The Card version stored under `id`, with every clause as it was published.
    ///
    /// # Errors
    /// [`StoreError::NoCardVersion`] when the store holds no Card version under `id`,
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn card_version(&self, id: CardVersionId) -> Result<PublishedCard, StoreError> {
        self.request(READ_CARD_VERSION, move |connection| read(connection, id))
            .await?
            .ok_or(StoreError::NoCardVersion { id })
    }

    /// [`card_version`](Self::card_version) for a synchronous caller, such as `wa`, which blocks on
    /// the reply.
    ///
    /// # Errors
    /// As [`card_version`](Self::card_version).
    ///
    /// # Panics
    /// When called from async code, which awaits [`card_version`](Self::card_version) instead.
    pub fn card_version_blocking(&self, id: CardVersionId) -> Result<PublishedCard, StoreError> {
        self.request_blocking(READ_CARD_VERSION, move |connection| read(connection, id))?
            .ok_or(StoreError::NoCardVersion { id })
    }
}

/// A member checked against the store, with the clause it belongs to where it names one.
enum Checked<'a> {
    Stored {
        clause_id: ClauseId,
        text_id: ClauseTextId,
    },
    Reworded {
        clause_id: ClauseId,
        wording: &'a ClauseWording,
    },
    New(&'a ClauseWording),
}

/// The work of one insert. Its outer result is SQLite's, and its inner one is the store's refusal
/// of a member.
fn insert_work(
    members: &[CardMember],
    change_note: &str,
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<CardVersionId, StoreError>> + Send + 'static
{
    let members = members.to_vec();
    let change_note = change_note.to_owned();
    move |connection| insert(connection, &members, &change_note)
}

/// Begins `IMMEDIATE`, so the version the changes are taken against is still the latest when this
/// one commits, and every member checked against the store is still there when it is written.
fn insert(
    connection: &mut Connection,
    members: &[CardMember],
    change_note: &str,
) -> rusqlite::Result<Result<CardVersionId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let checked = match check(&transaction, members)? {
        Ok(checked) => checked,
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        Err(refusal) => return Ok(Err(refusal)),
    };
    let latest = latest_wordings(&transaction)?;
    transaction.execute(
        "INSERT INTO card_versions (published_at, change_note)
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?1)",
        [change_note],
    )?;
    let version = CardVersionId(transaction.last_insert_rowid());

    let mut changed = BTreeSet::new();
    let mut held = HashSet::new();
    for member in checked {
        let (clause_id, text_id) = match member {
            Checked::Stored { clause_id, text_id } => (clause_id, text_id),
            Checked::Reworded { clause_id, wording } => {
                (clause_id, insert_wording(&transaction, clause_id, wording)?)
            }
            Checked::New(wording) => {
                transaction.execute("INSERT INTO clauses DEFAULT VALUES", [])?;
                let clause_id = ClauseId(transaction.last_insert_rowid());
                (clause_id, insert_wording(&transaction, clause_id, wording)?)
            }
        };
        transaction.execute(
            "INSERT INTO card_members (card_version_id, clause_text_id, clause_id)
             VALUES (?1, ?2, ?3)",
            (version.0, text_id.0, clause_id.0),
        )?;
        if latest.get(&clause_id) != Some(&text_id) {
            changed.insert(clause_id.0);
        }
        held.insert(clause_id);
    }
    let dropped = latest.keys().filter(|clause_id| !held.contains(clause_id));
    changed.extend(dropped.map(|clause_id| clause_id.0));
    for clause_id in changed {
        transaction.execute(
            "INSERT INTO card_changes (card_version_id, clause_id) VALUES (?1, ?2)",
            (version.0, clause_id),
        )?;
    }
    transaction.commit()?;
    Ok(Ok(version))
}

/// Checks every member against the store, in order, and stops at the first it refuses.
fn check<'a>(
    connection: &Connection,
    members: &'a [CardMember],
) -> rusqlite::Result<Result<Vec<Checked<'a>>, StoreError>> {
    let mut checked = Vec::with_capacity(members.len());
    let mut clauses = HashSet::new();
    for member in members {
        let member = match member {
            CardMember::Stored(text_id) => match clause_of(connection, *text_id)? {
                Some(clause_id) => Checked::Stored {
                    clause_id,
                    text_id: *text_id,
                },
                None => return Ok(Err(StoreError::NoClauseText { id: *text_id })),
            },
            CardMember::Reworded { clause_id, wording } => {
                if !clause_exists(connection, *clause_id)? {
                    return Ok(Err(StoreError::NoClause { id: *clause_id }));
                }
                Checked::Reworded {
                    clause_id: *clause_id,
                    wording,
                }
            }
            CardMember::New(wording) => Checked::New(wording),
        };
        if let Checked::Stored { clause_id, .. } | Checked::Reworded { clause_id, .. } = member {
            if !clauses.insert(clause_id) {
                return Ok(Err(StoreError::ClauseRepeated { id: clause_id }));
            }
        }
        checked.push(member);
    }
    Ok(Ok(checked))
}

/// The clause a stored wording belongs to, or `None` when the store holds no wording under
/// `text_id`.
fn clause_of(connection: &Connection, text_id: ClauseTextId) -> rusqlite::Result<Option<ClauseId>> {
    connection
        .query_row(
            "SELECT clause_id FROM clause_texts WHERE id = ?1",
            [text_id.0],
            |row| row.get(0).map(ClauseId),
        )
        .optional()
}

pub(super) fn clause_exists(
    connection: &Connection,
    clause_id: ClauseId,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM clauses WHERE id = ?1)",
        [clause_id.0],
        |row| row.get(0),
    )
}

/// The wording of each clause the latest stored version holds, by clause, and none before the
/// first version.
fn latest_wordings(connection: &Connection) -> rusqlite::Result<HashMap<ClauseId, ClauseTextId>> {
    connection
        .prepare(
            "SELECT clause_id, clause_text_id FROM card_members
             WHERE card_version_id = (SELECT max(id) FROM card_versions)",
        )?
        .query_map([], |row| {
            Ok((ClauseId(row.get(0)?), ClauseTextId(row.get(1)?)))
        })?
        .collect()
}

fn insert_wording(
    connection: &Connection,
    clause_id: ClauseId,
    wording: &ClauseWording,
) -> rusqlite::Result<ClauseTextId> {
    let example = wording.example.as_ref();
    connection.execute(
        "INSERT INTO clause_texts
             (clause_id, text, twin, register_scope, example_follows, example_breaks)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            clause_id.0,
            &wording.text,
            &wording.twin,
            wording.register_scope.as_ref().map(Json),
            example.map(|pair| &pair.follows),
            example.map(|pair| &pair.breaks),
        ),
    )?;
    Ok(ClauseTextId(connection.last_insert_rowid()))
}

fn read(connection: &Connection, id: CardVersionId) -> rusqlite::Result<Option<PublishedCard>> {
    let Some((published_at, change_note)) = connection
        .query_row(
            "SELECT published_at, change_note FROM card_versions WHERE id = ?1",
            [id.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let clauses = connection
        .prepare(
            "SELECT m.clause_id, m.clause_text_id, t.text, t.twin, t.register_scope,
                    t.example_follows, t.example_breaks
             FROM card_members AS m JOIN clause_texts AS t ON t.id = m.clause_text_id
             WHERE m.card_version_id = ?1
             ORDER BY m.id",
        )?
        .query_map([id.0], |row| {
            let register_scope: Option<Json<BTreeSet<String>>> = row.get(4)?;
            let follows: Option<String> = row.get(5)?;
            let breaks: Option<String> = row.get(6)?;
            Ok(PublishedClause {
                clause_id: ClauseId(row.get(0)?),
                text_id: ClauseTextId(row.get(1)?),
                wording: ClauseWording {
                    text: row.get(2)?,
                    twin: row.get(3)?,
                    register_scope: register_scope.map(|Json(scope)| scope),
                    // The table's CHECK holds the two sentences both present or both absent.
                    example: follows
                        .zip(breaks)
                        .map(|(follows, breaks)| ExamplePair { follows, breaks }),
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let changed = connection
        .prepare(
            "SELECT clause_id FROM card_changes WHERE card_version_id = ?1 ORDER BY clause_id",
        )?
        .query_map([id.0], |row| row.get(0).map(ClauseId))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(PublishedCard {
        id,
        published_at,
        change_note,
        clauses,
        changed,
    }))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::test_support::TempRoot;
    use regex::Regex;
    use rusqlite::types::Value;

    pub(in crate::store) const HEDGES: (&str, &str) = (
        "Hedge at most once a paragraph.",
        "Hedge several times in every paragraph.",
    );
    pub(in crate::store) const SHORT: (&str, &str) = (
        "Keep sentences under thirty words.",
        "Write sentences of forty words or more.",
    );
    const PLAIN: (&str, &str) = (
        "Name the thing directly.",
        "Circle the thing with abstractions.",
    );
    const HEDGES_REWORDED: (&str, &str) = (
        "Hedge at most twice a paragraph.",
        "Hedge in every sentence.",
    );
    const EXAMPLE: (&str, &str) = (
        "The results hold.",
        "The results would seem, perhaps, to hold.",
    );
    pub(in crate::store) const NOTE: &str =
        "from 'too many hedges': the hedging limit is now one per paragraph";

    const CARD_TABLES: [&str; 5] = [
        "clauses",
        "clause_texts",
        "card_versions",
        "card_members",
        "card_changes",
    ];

    pub(in crate::store) fn open(root: &TempRoot) -> Store {
        Store::open(root.path()).unwrap()
    }

    pub(in crate::store) fn wording((text, twin): (&str, &str)) -> ClauseWording {
        ClauseWording {
            text: text.to_owned(),
            twin: twin.to_owned(),
            register_scope: None,
            example: None,
        }
    }

    fn example_pair() -> ExamplePair {
        ExamplePair {
            follows: EXAMPLE.0.to_owned(),
            breaks: EXAMPLE.1.to_owned(),
        }
    }

    pub(in crate::store) fn new(clause: (&str, &str)) -> CardMember {
        CardMember::New(wording(clause))
    }

    fn stored(clause: &PublishedClause) -> CardMember {
        CardMember::Stored(clause.text_id)
    }

    /// Inserts a version of `members` and reads it back.
    pub(in crate::store) fn insert_version(store: &Store, members: &[CardMember]) -> PublishedCard {
        let id = store.insert_card_version_blocking(members, NOTE).unwrap();
        store.card_version_blocking(id).unwrap()
    }

    /// Every row of `table`, in order of id.
    pub(in crate::store) fn rows(store: &Store, table: &'static str) -> Vec<Vec<Value>> {
        store
            .request_blocking("read every row of a table", move |connection| {
                connection
                    .prepare(&format!("SELECT * FROM {table} ORDER BY id"))?
                    .query_map([], |row| {
                        (0..row.as_ref().column_count())
                            .map(|index| row.get::<_, Value>(index))
                            .collect()
                    })?
                    .collect()
            })
            .unwrap()
    }

    fn card_rows(store: &Store) -> Vec<Vec<Vec<Value>>> {
        CARD_TABLES.iter().map(|table| rows(store, table)).collect()
    }

    /// SQLite's clock, in the form a version's publish time takes.
    pub(in crate::store) fn sqlite_now(store: &Store) -> String {
        store
            .request_blocking("read SQLite's clock", |connection| {
                connection.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                    row.get(0)
                })
            })
            .unwrap()
    }

    pub(in crate::store) fn assert_utc_to_the_millisecond(time: &str) {
        let form = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
        assert!(form.is_match(time), "{time}");
    }

    /// Holds that an update and a delete of every row of `table` each raise its trigger, and that
    /// the rows are as they were.
    pub(in crate::store) fn assert_insert_only(store: &Store, table: &'static str) {
        let before = rows(store, table);
        assert!(!before.is_empty(), "{table} holds no row to change");
        for (event, statement) in [
            ("update", format!("UPDATE {table} SET id = id")),
            ("delete", format!("DELETE FROM {table}")),
        ] {
            match store
                .request_blocking("change a row of an insert-only table", move |connection| {
                    connection.execute(&statement, [])
                }) {
                Err(StoreError::Request {
                    source: rusqlite::Error::SqliteFailure(failure, Some(message)),
                    ..
                }) => {
                    assert_eq!(
                        failure.extended_code,
                        rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER,
                        "{table}: {event}"
                    );
                    assert!(
                        message.starts_with(&format!("{table} is insert-only")),
                        "{message}"
                    );
                }
                other => panic!("{table}: expected the {event} to raise, got {other:?}"),
            }
        }
        assert_eq!(rows(store, table), before, "{table}");
    }

    /// The SQLite constraint a statement run directly on the store failed.
    pub(in crate::store) fn refusal(
        store: &Store,
        statement: &'static str,
        params: Vec<Value>,
    ) -> std::ffi::c_int {
        match store.request_blocking("insert a row directly", move |connection| {
            connection.execute(statement, rusqlite::params_from_iter(params))
        }) {
            Err(StoreError::Request {
                source: rusqlite::Error::SqliteFailure(failure, _),
                ..
            }) => failure.extended_code,
            other => panic!("expected a constraint to refuse the row, got {other:?}"),
        }
    }

    #[test]
    fn a_clause_keeps_an_id_no_later_clause_is_given_after_it_has_left_the_card() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT)]);
        let (hedges, short) = (&first.clauses[0], &first.clauses[1]);
        assert_ne!(hedges.clause_id, short.clause_id);

        let without_short = insert_version(&store, &[stored(hedges)]);
        let with_plain = insert_version(&store, &[stored(hedges), new(PLAIN)]);
        let plain = &with_plain.clauses[1];
        let short_back = insert_version(&store, &[stored(hedges), stored(plain), stored(short)]);

        assert_eq!(without_short.changed, [short.clause_id]);
        assert!(
            ![hedges.clause_id, short.clause_id].contains(&plain.clause_id),
            "the new clause took id {:?}",
            plain.clause_id
        );
        assert!(plain.clause_id.0 > short.clause_id.0);
        assert_eq!(short_back.clauses[2], *short);
        assert_eq!(short_back.changed, [short.clause_id]);
        assert_eq!(rows(&store, "clauses").len(), 3);
    }

    #[test]
    fn a_published_clause_reads_back_with_its_text_twin_scope_and_example_pair() {
        let root = TempRoot::new();
        let store = open(&root);
        let unscoped = wording(HEDGES);
        let scoped = ClauseWording {
            register_scope: Some(BTreeSet::from(["email".to_owned()])),
            example: Some(example_pair()),
            ..wording(SHORT)
        };

        let card = insert_version(
            &store,
            &[
                CardMember::New(unscoped.clone()),
                CardMember::New(scoped.clone()),
            ],
        );

        assert_eq!(card.clauses.len(), 2);
        assert_eq!(card.clauses[0].wording, unscoped);
        assert_eq!(card.clauses[0].wording.register_scope, None);
        assert_eq!(card.clauses[0].wording.example, None);
        assert_eq!(card.clauses[1].wording, scoped);
        // A scope given as no register set reads as every register because the column holds null.
        let scopes: Vec<Value> = rows(&store, "clause_texts")
            .into_iter()
            .map(|row| row[4].clone())
            .collect();
        assert_eq!(
            scopes,
            [Value::Null, Value::Text(r#"["email"]"#.to_owned())]
        );
    }

    #[test]
    fn a_version_rewording_one_clause_adds_rows_changes_none_and_leaves_the_first_as_published() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT), new(PLAIN)]);
        let ids: Vec<ClauseId> = first
            .clauses
            .iter()
            .map(|clause| clause.clause_id)
            .collect();
        let before = card_rows(&store);
        let reworded = wording(HEDGES_REWORDED);

        let second = insert_version(
            &store,
            &[
                CardMember::Reworded {
                    clause_id: ids[0],
                    wording: reworded.clone(),
                },
                stored(&first.clauses[1]),
                stored(&first.clauses[2]),
            ],
        );

        let after = card_rows(&store);
        for ((table, before), after) in CARD_TABLES.iter().zip(&before).zip(&after) {
            assert_eq!(&after[..before.len()], before.as_slice(), "{table}");
        }
        let added: Vec<usize> = before
            .iter()
            .zip(&after)
            .map(|(before, after)| after.len() - before.len())
            .collect();
        assert_eq!(
            added,
            [0, 1, 1, 3, 1],
            "rows added to each of {CARD_TABLES:?}"
        );
        assert_eq!(first.changed, ids, "the first version changes every clause");
        assert_eq!(second.clauses[0].clause_id, ids[0]);
        assert_ne!(second.clauses[0].text_id, first.clauses[0].text_id);
        assert_eq!(second.clauses[0].wording, reworded);
        assert_eq!(second.clauses[1..], first.clauses[1..]);
        assert_eq!(second.changed, [ids[0]]);
        assert_eq!(store.card_version_blocking(first.id).unwrap(), first);
    }

    #[test]
    fn a_card_version_holds_its_publish_time_and_change_note() {
        let root = TempRoot::new();
        let store = open(&root);
        let before = sqlite_now(&store);

        let id = store
            .insert_card_version_blocking(&[new(HEDGES)], NOTE)
            .unwrap();

        let after = sqlite_now(&store);
        let card = store.card_version_blocking(id).unwrap();
        assert_eq!(card.id, id);
        assert_eq!(card.change_note, NOTE);
        assert_utc_to_the_millisecond(&card.published_at);
        assert!(
            before <= card.published_at && card.published_at <= after,
            "published at {} outside {before} to {after}",
            card.published_at
        );
    }

    #[test]
    fn restoring_an_earlier_wording_adds_no_wording_and_records_the_clause_as_changed() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT)]);
        let hedges = &first.clauses[0];
        insert_version(
            &store,
            &[
                CardMember::Reworded {
                    clause_id: hedges.clause_id,
                    wording: wording(HEDGES_REWORDED),
                },
                stored(&first.clauses[1]),
            ],
        );
        let wordings = rows(&store, "clause_texts").len();

        let restored = insert_version(
            &store,
            &first.clauses.iter().map(stored).collect::<Vec<_>>(),
        );

        assert_eq!(rows(&store, "clause_texts").len(), wordings);
        assert_eq!(restored.clauses, first.clauses);
        assert_eq!(restored.changed, [hedges.clause_id]);
    }

    #[test]
    fn a_version_keeping_every_wording_records_no_change() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT)]);

        let second = insert_version(
            &store,
            &first.clauses.iter().map(stored).collect::<Vec<_>>(),
        );

        assert_eq!(second.clauses, first.clauses);
        assert!(second.changed.is_empty(), "{:?}", second.changed);
    }

    #[test]
    fn a_member_the_store_does_not_hold_or_a_clause_listed_twice_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES)]);
        let hedges = &first.clauses[0];
        let absent_text = ClauseTextId(hedges.text_id.0 + 1);
        let absent_clause = ClauseId(hedges.clause_id.0 + 1);
        let before = card_rows(&store);

        let no_text = store
            .insert_card_version_blocking(&[new(SHORT), CardMember::Stored(absent_text)], NOTE)
            .unwrap_err();
        let no_clause = store
            .insert_card_version_blocking(
                &[
                    new(SHORT),
                    CardMember::Reworded {
                        clause_id: absent_clause,
                        wording: wording(PLAIN),
                    },
                ],
                NOTE,
            )
            .unwrap_err();
        let repeated = store
            .insert_card_version_blocking(
                &[
                    stored(hedges),
                    CardMember::Reworded {
                        clause_id: hedges.clause_id,
                        wording: wording(HEDGES_REWORDED),
                    },
                ],
                NOTE,
            )
            .unwrap_err();

        assert!(
            matches!(no_text, StoreError::NoClauseText { id } if id == absent_text),
            "{no_text:?}"
        );
        assert_eq!(
            no_text.to_string(),
            "the store holds no clause text with id 2"
        );
        assert!(
            matches!(no_clause, StoreError::NoClause { id } if id == absent_clause),
            "{no_clause:?}"
        );
        assert_eq!(no_clause.to_string(), "the store holds no clause with id 2");
        assert!(
            matches!(repeated, StoreError::ClauseRepeated { id } if id == hedges.clause_id),
            "{repeated:?}"
        );
        assert_eq!(
            repeated.to_string(),
            "a Card version lists clause 1 more than once"
        );
        assert_eq!(card_rows(&store), before);
    }

    #[test]
    fn the_members_table_refuses_another_clauses_wording_and_a_clause_twice() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT)]);
        let (hedges, short) = (&first.clauses[0], &first.clauses[1]);
        store
            .request_blocking("insert a version with no members", |connection| {
                connection.execute(
                    "INSERT INTO card_versions (published_at, change_note) VALUES ('now', 'note')",
                    [],
                )
            })
            .unwrap();
        let empty_version = Value::Integer(first.id.0 + 1);
        let member = "INSERT INTO card_members (card_version_id, clause_text_id, clause_id)
                      VALUES (?1, ?2, ?3)";

        let another_clauses = refusal(
            &store,
            member,
            vec![
                empty_version.clone(),
                Value::Integer(hedges.text_id.0),
                Value::Integer(short.clause_id.0),
            ],
        );
        let twice = refusal(
            &store,
            member,
            vec![
                Value::Integer(first.id.0),
                Value::Integer(hedges.text_id.0),
                Value::Integer(hedges.clause_id.0),
            ],
        );

        assert_eq!(another_clauses, rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
        assert_eq!(twice, rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE);
    }

    #[test]
    fn the_wordings_table_refuses_an_empty_text_or_twin_and_half_an_example_pair() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES)]);
        let clause = Value::Integer(first.clauses[0].clause_id.0);
        let text = |value: &str| Value::Text(value.to_owned());
        let statement = "INSERT INTO clause_texts
                             (clause_id, text, twin, example_follows, example_breaks)
                         VALUES (?1, ?2, ?3, ?4, ?5)";

        for (case, params) in [
            (
                "an empty text",
                vec![
                    clause.clone(),
                    text(""),
                    text(HEDGES.1),
                    Value::Null,
                    Value::Null,
                ],
            ),
            (
                "an empty twin",
                vec![
                    clause.clone(),
                    text(HEDGES.0),
                    text(""),
                    Value::Null,
                    Value::Null,
                ],
            ),
            (
                "an example that follows with none that breaks",
                vec![
                    clause.clone(),
                    text(HEDGES.0),
                    text(HEDGES.1),
                    text(EXAMPLE.0),
                    Value::Null,
                ],
            ),
        ] {
            assert_eq!(
                refusal(&store, statement, params),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                "{case}"
            );
        }
    }

    #[test]
    fn updating_or_deleting_any_card_row_raises_and_leaves_each_version_as_published() {
        let root = TempRoot::new();
        let store = open(&root);
        let first = insert_version(&store, &[new(HEDGES), new(SHORT)]);
        let second = insert_version(&store, &[stored(&first.clauses[0])]);

        for table in CARD_TABLES {
            assert_insert_only(&store, table);
        }

        assert_eq!(store.card_version_blocking(first.id).unwrap(), first);
        assert_eq!(store.card_version_blocking(second.id).unwrap(), second);
    }

    #[test]
    fn reading_an_id_the_store_does_not_hold_returns_no_card_version() {
        let root = TempRoot::new();
        let store = open(&root);

        let error = store.card_version_blocking(CardVersionId(1)).unwrap_err();

        assert!(
            matches!(
                error,
                StoreError::NoCardVersion {
                    id: CardVersionId(1)
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "the store holds no Card version with id 1"
        );
    }

    #[test]
    fn no_debug_rendering_or_error_carries_a_clauses_words_or_a_change_note() {
        let root = TempRoot::new();
        let store = open(&root);
        let member = CardMember::New(ClauseWording {
            example: Some(example_pair()),
            ..wording(HEDGES)
        });
        let card = insert_version(&store, std::slice::from_ref(&member));
        let refused = store
            .insert_card_version_blocking(
                &[stored(&card.clauses[0]), stored(&card.clauses[0])],
                NOTE,
            )
            .unwrap_err();
        store
            .request_blocking("drop the members table", |connection| {
                connection.execute_batch("DROP TABLE card_members")
            })
            .unwrap();

        let insert_error = store
            .insert_card_version_blocking(std::slice::from_ref(&member), NOTE)
            .unwrap_err();
        let read_error = store.card_version_blocking(card.id).unwrap_err();

        let renderings = [
            format!("{member:?}"),
            format!("{card:?}"),
            refused.to_string(),
            format!("{refused:?}"),
            insert_error.to_string(),
            format!("{insert_error:?}"),
            read_error.to_string(),
            format!("{read_error:?}"),
        ];
        // Checked before any rendering is printed in a failure message below.
        for (index, rendering) in renderings.iter().enumerate() {
            for words in [HEDGES.0, HEDGES.1, EXAMPLE.0, EXAMPLE.1, NOTE] {
                assert!(
                    !rendering.contains(words),
                    "rendering {index} carries a clause's words or the change note"
                );
            }
        }
        assert!(
            matches!(
                insert_error,
                StoreError::Request {
                    operation: INSERT_CARD_VERSION,
                    ..
                }
            ),
            "{insert_error:?}"
        );
        assert!(
            matches!(
                read_error,
                StoreError::Request {
                    operation: READ_CARD_VERSION,
                    ..
                }
            ),
            "{read_error:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_insert_and_read_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);

        let first = store
            .insert_card_version(&[new(HEDGES), new(SHORT)], NOTE)
            .await
            .unwrap();
        let first = store.card_version(first).await.unwrap();
        let second = store
            .insert_card_version(&[stored(&first.clauses[1])], NOTE)
            .await
            .unwrap();
        let second = store.card_version(second).await.unwrap();
        let refused = store
            .insert_card_version(&[CardMember::Stored(ClauseTextId(99))], NOTE)
            .await;
        let absent = store.card_version(CardVersionId(second.id.0 + 1)).await;

        assert_eq!(first.clauses[0].wording, wording(HEDGES));
        assert_eq!(second.clauses, first.clauses[1..]);
        assert_eq!(second.changed, [first.clauses[0].clause_id]);
        assert!(
            matches!(refused, Err(StoreError::NoClauseText { .. })),
            "{refused:?}"
        );
        assert!(
            matches!(absent, Err(StoreError::NoCardVersion { .. })),
            "{absent:?}"
        );
    }
}
