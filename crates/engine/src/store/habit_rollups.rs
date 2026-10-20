//! The habit rollups table: each row is one count of the style flags raised on the owner's typing
//! for a day and a clause, beside the number of words checked. The habit counters keep their counts
//! in memory and write a row, never one per flag, so no row holds a flag's text or its span, and a
//! day's total for a clause is the sum of its rows. Nothing writes a rollup until the habit ledger,
//! in Phase 9.
//!
//! A row holds a day, a clause id and two counts, and no text, so nothing here needs a `Debug`
//! rendering that hides text.

use rusqlite::{Connection, TransactionBehavior};

use super::card::clause_exists;
use super::error::StoreError;
use super::thread::Store;
use crate::flag::ClauseId;

const INSERT_HABIT_ROLLUP: &str = "insert a habit rollup";
const READ_HABIT_TOTAL: &str = "read a day's habit total";

/// A habit rollup's id in the store, assigned at its insert and never given to another rollup. An
/// `i64` because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HabitRollupId(pub i64);

/// One count of the style flags raised on the owner's typing for a day and a clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HabitRollup {
    /// A calendar day, as `YYYY-MM-DD`.
    pub day: String,
    /// The clause the flags cite.
    pub clause: ClauseId,
    pub style_flags: u32,
    pub words_checked: u32,
}

/// The sum of the rollups for one day and one clause. Each sum is an `i64` because SQLite's integers
/// are, and the table's CHECKs keep both from going below zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HabitTotal {
    pub style_flags: i64,
    pub words_checked: i64,
}

impl Store {
    /// Inserts `rollup` and returns its id. Every insert adds a row, including one for a day and a
    /// clause already counted, since a day's total is the sum of its rows.
    ///
    /// # Errors
    /// [`StoreError::NoClause`] when the store holds no clause under the rollup's clause id,
    /// inserting nothing. [`StoreError::Busy`] when another connection holds the write lock past
    /// the busy timeout, [`StoreError::Request`] for any other failure of the insert, a day that is
    /// not a calendar date as `YYYY-MM-DD` among them, and [`StoreError::ThreadGone`] when the
    /// store thread has stopped.
    pub async fn insert_habit_rollup(
        &self,
        rollup: &HabitRollup,
    ) -> Result<HabitRollupId, StoreError> {
        let rollup = rollup.clone();
        self.request(INSERT_HABIT_ROLLUP, move |connection| {
            insert(connection, &rollup)
        })
        .await?
    }

    /// [`insert_habit_rollup`](Self::insert_habit_rollup) for a synchronous caller, such as `wa`,
    /// which blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_habit_rollup`](Self::insert_habit_rollup).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`insert_habit_rollup`](Self::insert_habit_rollup) instead.
    pub fn insert_habit_rollup_blocking(
        &self,
        rollup: &HabitRollup,
    ) -> Result<HabitRollupId, StoreError> {
        let rollup = rollup.clone();
        self.request_blocking(INSERT_HABIT_ROLLUP, move |connection| {
            insert(connection, &rollup)
        })?
    }

    /// The sum of the rollups for `day`, as `YYYY-MM-DD`, and `clause`: zero when the store holds
    /// none.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn habit_total(&self, day: &str, clause: ClauseId) -> Result<HabitTotal, StoreError> {
        let day = day.to_owned();
        self.request(READ_HABIT_TOTAL, move |connection| {
            total(connection, &day, clause)
        })
        .await
    }

    /// [`habit_total`](Self::habit_total) for a synchronous caller, such as `wa`, which blocks on
    /// the reply.
    ///
    /// # Errors
    /// As [`habit_total`](Self::habit_total).
    ///
    /// # Panics
    /// When called from async code, which awaits [`habit_total`](Self::habit_total) instead.
    pub fn habit_total_blocking(
        &self,
        day: &str,
        clause: ClauseId,
    ) -> Result<HabitTotal, StoreError> {
        let day = day.to_owned();
        self.request_blocking(READ_HABIT_TOTAL, move |connection| {
            total(connection, &day, clause)
        })
    }
}

/// Begins `IMMEDIATE`, since the clause's lookup comes before the write, and a deferred transaction
/// that reads first fails at its first write, without waiting, while another connection holds the
/// write lock. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert(
    connection: &mut Connection,
    rollup: &HabitRollup,
) -> rusqlite::Result<Result<HabitRollupId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !clause_exists(&transaction, rollup.clause)? {
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        return Ok(Err(StoreError::NoClause { id: rollup.clause }));
    }
    transaction.execute(
        "INSERT INTO habit_rollups (day, clause_id, style_flags, words_checked)
         VALUES (?1, ?2, ?3, ?4)",
        (
            &rollup.day,
            rollup.clause.0,
            rollup.style_flags,
            rollup.words_checked,
        ),
    )?;
    let id = HabitRollupId(transaction.last_insert_rowid());
    transaction.commit()?;
    Ok(Ok(id))
}

fn total(connection: &Connection, day: &str, clause: ClauseId) -> rusqlite::Result<HabitTotal> {
    connection.query_row(
        "SELECT coalesce(sum(style_flags), 0), coalesce(sum(words_checked), 0)
         FROM habit_rollups WHERE day = ?1 AND clause_id = ?2",
        (day, clause.0),
        |row| {
            Ok(HabitTotal {
                style_flags: row.get(0)?,
                words_checked: row.get(1)?,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::card::tests::{
        assert_insert_only, new, open, refusal, rows, HEDGES, NOTE, SHORT,
    };
    use crate::test_support::TempRoot;
    use rusqlite::types::Value;

    const DAY: &str = "2026-10-20";

    /// The columns a rollup's row holds: a day, a clause and two counts, and no flag's text or span.
    const COLUMNS: [&str; 5] = ["id", "day", "clause_id", "style_flags", "words_checked"];

    /// A rollup row written past the store's insert, for the tests of what the table refuses.
    const ROLLUP_ROW: &str =
        "INSERT INTO habit_rollups (day, clause_id, style_flags, words_checked)
                              VALUES (?1, ?2, ?3, ?4)";

    /// The ids of the two clauses of one Card version.
    fn clauses(store: &Store) -> [ClauseId; 2] {
        let card = store
            .insert_card_version_blocking(&[new(HEDGES), new(SHORT)], NOTE)
            .unwrap();
        let clauses = store.card_version_blocking(card).unwrap().clauses;
        [clauses[0].clause_id, clauses[1].clause_id]
    }

    fn rollup(day: &str, clause: ClauseId, style_flags: u32, words_checked: u32) -> HabitRollup {
        HabitRollup {
            day: day.to_owned(),
            clause,
            style_flags,
            words_checked,
        }
    }

    fn insert(store: &Store, rollup: &HabitRollup) -> HabitRollupId {
        store.insert_habit_rollup_blocking(rollup).unwrap()
    }

    fn total(store: &Store, day: &str, clause: ClauseId) -> HabitTotal {
        store.habit_total_blocking(day, clause).unwrap()
    }

    fn columns(store: &Store) -> Vec<String> {
        store
            .request_blocking("read the habit rollups table's columns", |connection| {
                connection
                    .prepare("SELECT name FROM pragma_table_info('habit_rollups') ORDER BY cid")?
                    .query_map([], |row| row.get(0))?
                    .collect()
            })
            .unwrap()
    }

    #[test]
    fn a_rollup_reads_back_as_its_days_total_and_its_row_holds_no_text_or_span() {
        let root = TempRoot::new();
        let store = open(&root);
        let [hedges, _] = clauses(&store);

        let id = insert(&store, &rollup(DAY, hedges, 3, 412));

        assert_eq!(
            total(&store, DAY, hedges),
            HabitTotal {
                style_flags: 3,
                words_checked: 412,
            }
        );
        assert_eq!(
            rows(&store, "habit_rollups"),
            [[
                Value::Integer(id.0),
                Value::Text(DAY.to_owned()),
                Value::Integer(hedges.0),
                Value::Integer(3),
                Value::Integer(412),
            ]]
        );
        assert_eq!(columns(&store), COLUMNS);
    }

    #[test]
    fn two_rollups_for_one_day_and_clause_sum_and_no_other_day_or_clause_counts() {
        let root = TempRoot::new();
        let store = open(&root);
        let [hedges, short] = clauses(&store);

        for row in [
            rollup(DAY, hedges, 3, 412),
            rollup(DAY, hedges, 2, 150),
            rollup(DAY, short, 7, 900),
            rollup("2026-10-21", hedges, 5, 300),
        ] {
            insert(&store, &row);
        }

        assert_eq!(
            total(&store, DAY, hedges),
            HabitTotal {
                style_flags: 5,
                words_checked: 562,
            }
        );
        assert_eq!(
            total(&store, DAY, short),
            HabitTotal {
                style_flags: 7,
                words_checked: 900,
            }
        );
        assert_eq!(total(&store, "2026-10-19", hedges), HabitTotal::default());
        assert_eq!(rows(&store, "habit_rollups").len(), 4);
    }

    #[test]
    fn a_rollup_naming_a_clause_the_store_does_not_hold_is_refused_by_name_and_by_the_tables_key() {
        let root = TempRoot::new();
        let store = open(&root);
        let [_, short] = clauses(&store);
        let absent = ClauseId(short.0 + 1);

        let error = store
            .insert_habit_rollup_blocking(&rollup(DAY, absent, 1, 10))
            .unwrap_err();
        let by_key = refusal(
            &store,
            ROLLUP_ROW,
            vec![
                Value::Text(DAY.to_owned()),
                Value::Integer(absent.0),
                Value::Integer(1),
                Value::Integer(10),
            ],
        );

        assert!(
            matches!(error, StoreError::NoClause { id } if id == absent),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no clause with id 3");
        assert_eq!(by_key, rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
        assert!(rows(&store, "habit_rollups").is_empty());
    }

    #[test]
    fn the_table_refuses_a_day_that_is_not_a_calendar_date_and_a_negative_count() {
        let root = TempRoot::new();
        let store = open(&root);
        let [hedges, _] = clauses(&store);

        for day in [
            "2026-02-30",
            "2026-10-20T09:00:00Z",
            "2026-1-5",
            "20 October 2026",
            "",
        ] {
            match store.insert_habit_rollup_blocking(&rollup(day, hedges, 1, 10)) {
                Err(StoreError::Request {
                    source: rusqlite::Error::SqliteFailure(failure, _),
                    ..
                }) => assert_eq!(
                    failure.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                    "{day:?}"
                ),
                other => panic!("{day:?}: expected the CHECK to refuse it, got {other:?}"),
            }
        }
        for (style_flags, words_checked) in [(-1, 10), (1, -10)] {
            assert_eq!(
                refusal(
                    &store,
                    ROLLUP_ROW,
                    vec![
                        Value::Text(DAY.to_owned()),
                        Value::Integer(hedges.0),
                        Value::Integer(style_flags),
                        Value::Integer(words_checked),
                    ],
                ),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                "{style_flags} flags in {words_checked} words"
            );
        }
        assert!(rows(&store, "habit_rollups").is_empty());
    }

    #[test]
    fn updating_or_deleting_a_rollup_raises_and_leaves_the_total_as_it_was() {
        let root = TempRoot::new();
        let store = open(&root);
        let [hedges, _] = clauses(&store);
        insert(&store, &rollup(DAY, hedges, 3, 412));

        assert_insert_only(&store, "habit_rollups");

        assert_eq!(
            total(&store, DAY, hedges),
            HabitTotal {
                style_flags: 3,
                words_checked: 412,
            }
        );
    }

    /// Blocking on the store inside the runtime panics, so the fixture uses the awaited forms too.
    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_insert_and_total_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);
        let card = store
            .insert_card_version(&[new(HEDGES)], NOTE)
            .await
            .unwrap();
        let hedges = store.card_version(card).await.unwrap().clauses[0].clause_id;

        store
            .insert_habit_rollup(&rollup(DAY, hedges, 3, 412))
            .await
            .unwrap();
        store
            .insert_habit_rollup(&rollup(DAY, hedges, 4, 88))
            .await
            .unwrap();
        let summed = store.habit_total(DAY, hedges).await.unwrap();
        let refused = store
            .insert_habit_rollup(&rollup(DAY, ClauseId(hedges.0 + 1), 1, 10))
            .await;

        assert_eq!(
            summed,
            HabitTotal {
                style_flags: 7,
                words_checked: 500,
            }
        );
        assert!(
            matches!(refused, Err(StoreError::NoClause { .. })),
            "{refused:?}"
        );
    }
}
