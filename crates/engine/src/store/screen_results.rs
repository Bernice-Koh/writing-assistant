//! The screen results table: one row per screening of a candidate, named by the candidate's content
//! hash and recorded under the ruleset version and the telltale catalogue's content hash it ran
//! under, with the app version, the hard and soft results and the killing rule. The three form a
//! lookup key and not a uniqueness rule: a screening under a key already screened adds a row, and
//! a lookup returns the latest. Recording the catalogue hash beside the ruleset version tells a
//! catalogue change from a ruleset change. Nothing writes a screen result until the screen (#103)
//! exists.
//!
//! The store validates the results as JSON and does not type them: the screen defines their shapes
//! and the verdict and reads them back through its own types. They can quote words of a
//! candidate's text, so no error, log line or `Debug` rendering here carries them.

use std::fmt;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use super::error::StoreError;
use super::json::Json;
use super::passages::from_column;
use super::thread::Store;
use crate::flag::{CatalogueHash, RulesetVersion};

const INSERT_SCREEN_RESULT: &str = "insert a screen result";
const READ_SCREEN_RESULT: &str = "read the latest screen result";

/// A screen result's id in the store, assigned at its insert and never given to another result. A
/// later screening under one key has a higher id. An `i64` because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenResultId(pub i64);

/// The first check a candidate failed, in the screen's order: content, spelling, grammar, style,
/// tells. The screen (#103) reuses this type or moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillingRule {
    Content,
    Spelling,
    Grammar,
    Style,
    Tells,
}

/// One screening of a candidate.
#[derive(Clone, PartialEq, Eq)]
pub struct ScreenResult {
    /// The screened candidate's content hash, which every candidate sharing the passage and the
    /// text shares.
    pub candidate_hash: String,
    pub ruleset_version: RulesetVersion,
    pub catalogue_hash: CatalogueHash,
    pub app_version: String,
    pub hard_results: Value,
    pub soft_results: Value,
    /// `None` when no check failed.
    pub killing_rule: Option<KillingRule>,
}

/// A screen result as its row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredScreenResult {
    pub id: ScreenResultId,
    pub result: ScreenResult,
}

/// Shows the length in bytes of each result's JSON in place of the JSON, since a `Debug` rendering
/// ends up in log lines and test failure messages, and a result can quote a candidate's words.
impl fmt::Debug for ScreenResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScreenResult")
            .field("candidate_hash", &self.candidate_hash)
            .field("ruleset_version", &self.ruleset_version)
            .field("catalogue_hash", &self.catalogue_hash)
            .field("app_version", &self.app_version)
            .field("hard_results_bytes", &self.hard_results.to_string().len())
            .field("soft_results_bytes", &self.soft_results.to_string().len())
            .field("killing_rule", &self.killing_rule)
            .finish()
    }
}

impl Store {
    /// Inserts `result` and returns its id. Every insert adds a row, including one under a key
    /// already screened.
    ///
    /// # Errors
    /// [`StoreError::NoCandidateHash`] when no stored candidate has the result's candidate hash,
    /// and [`StoreError::NoRulesetVersion`] when the store holds no such ruleset version, either
    /// inserting nothing. [`StoreError::Busy`] when another connection holds the write lock past
    /// the busy timeout, [`StoreError::Request`] for any other failure of the insert, and
    /// [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn insert_screen_result(
        &self,
        result: &ScreenResult,
    ) -> Result<ScreenResultId, StoreError> {
        let result = result.clone();
        self.request(INSERT_SCREEN_RESULT, move |connection| {
            insert(connection, &result)
        })
        .await?
    }

    /// [`insert_screen_result`](Self::insert_screen_result) for a synchronous caller, such as
    /// `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_screen_result`](Self::insert_screen_result).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`insert_screen_result`](Self::insert_screen_result) instead.
    pub fn insert_screen_result_blocking(
        &self,
        result: &ScreenResult,
    ) -> Result<ScreenResultId, StoreError> {
        let result = result.clone();
        self.request_blocking(INSERT_SCREEN_RESULT, move |connection| {
            insert(connection, &result)
        })?
    }

    /// The latest screen result recorded for the candidate hash `candidate_hash` under
    /// `ruleset_version` and `catalogue_hash`, the one with the highest id, or `None` when the
    /// store holds none under that key.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn latest_screen_result(
        &self,
        candidate_hash: &str,
        ruleset_version: RulesetVersion,
        catalogue_hash: &CatalogueHash,
    ) -> Result<Option<StoredScreenResult>, StoreError> {
        self.request(
            READ_SCREEN_RESULT,
            latest_work(candidate_hash, ruleset_version, catalogue_hash),
        )
        .await
    }

    /// [`latest_screen_result`](Self::latest_screen_result) for a synchronous caller, such as
    /// `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`latest_screen_result`](Self::latest_screen_result).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`latest_screen_result`](Self::latest_screen_result) instead.
    pub fn latest_screen_result_blocking(
        &self,
        candidate_hash: &str,
        ruleset_version: RulesetVersion,
        catalogue_hash: &CatalogueHash,
    ) -> Result<Option<StoredScreenResult>, StoreError> {
        self.request_blocking(
            READ_SCREEN_RESULT,
            latest_work(candidate_hash, ruleset_version, catalogue_hash),
        )
    }
}

/// Begins `IMMEDIATE`, since the lookups come before the write, and a deferred transaction that
/// reads first fails at its first write, without waiting, while another connection holds the write
/// lock. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert(
    connection: &mut Connection,
    result: &ScreenResult,
) -> rusqlite::Result<Result<ScreenResultId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let candidate_exists: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM candidates WHERE content_hash = ?1)",
        [&result.candidate_hash],
        |row| row.get(0),
    )?;
    if !candidate_exists {
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        return Ok(Err(StoreError::NoCandidateHash {
            content_hash: result.candidate_hash.clone(),
        }));
    }
    let version_exists: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM ruleset_versions WHERE id = ?1)",
        [result.ruleset_version.0],
        |row| row.get(0),
    )?;
    if !version_exists {
        return Ok(Err(StoreError::NoRulesetVersion {
            version: result.ruleset_version,
        }));
    }
    transaction.execute(
        "INSERT INTO screen_results (candidate_hash, ruleset_version_id, catalogue_hash,
                                     app_version, hard_results, soft_results, killing_rule)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            &result.candidate_hash,
            result.ruleset_version.0,
            &result.catalogue_hash,
            &result.app_version,
            Json(&result.hard_results),
            Json(&result.soft_results),
            result.killing_rule,
        ),
    )?;
    let id = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(Ok(ScreenResultId(id)))
}

fn latest_work(
    candidate_hash: &str,
    ruleset_version: RulesetVersion,
    catalogue_hash: &CatalogueHash,
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Option<StoredScreenResult>> + Send + 'static {
    let candidate_hash = candidate_hash.to_owned();
    let catalogue_hash = catalogue_hash.clone();
    move |connection| {
        latest(
            connection,
            &candidate_hash,
            ruleset_version,
            &catalogue_hash,
        )
    }
}

/// One step down the key's index, which ends in the id.
fn latest(
    connection: &Connection,
    candidate_hash: &str,
    ruleset_version: RulesetVersion,
    catalogue_hash: &CatalogueHash,
) -> rusqlite::Result<Option<StoredScreenResult>> {
    connection
        .query_row(
            "SELECT id, candidate_hash, ruleset_version_id, catalogue_hash, app_version,
                    hard_results, soft_results, killing_rule
             FROM screen_results
             WHERE candidate_hash = ?1 AND ruleset_version_id = ?2 AND catalogue_hash = ?3
             ORDER BY id DESC LIMIT 1",
            (candidate_hash, ruleset_version.0, catalogue_hash),
            |row| {
                let Json(hard_results) = row.get(5)?;
                let Json(soft_results) = row.get(6)?;
                Ok(StoredScreenResult {
                    id: ScreenResultId(row.get(0)?),
                    result: ScreenResult {
                        candidate_hash: row.get(1)?,
                        ruleset_version: RulesetVersion(row.get(2)?),
                        catalogue_hash: row.get(3)?,
                        app_version: row.get(4)?,
                        hard_results,
                        soft_results,
                        killing_rule: row.get(7)?,
                    },
                })
            },
        )
        .optional()
}

impl KillingRule {
    const ALL: [Self; 5] = [
        Self::Content,
        Self::Spelling,
        Self::Grammar,
        Self::Style,
        Self::Tells,
    ];

    /// The rule as the `killing_rule` column holds it, one of the names its CHECK constraint
    /// allows.
    fn column_value(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Spelling => "spelling",
            Self::Grammar => "grammar",
            Self::Style => "style",
            Self::Tells => "tells",
        }
    }
}

impl ToSql for KillingRule {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.column_value().into())
    }
}

impl FromSql for KillingRule {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        from_column(value, &Self::ALL, Self::column_value)
    }
}

/// Binds the digest as text. `CatalogueHash` gives its digest no accessor, so the digest comes
/// through its `Serialize`, which writes it as a JSON string.
impl ToSql for CatalogueHash {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match serde_json::to_value(self) {
            Ok(Value::String(digest)) => Ok(digest.into()),
            _ => Err(rusqlite::Error::ToSqlConversionFailure(
                "a catalogue hash that does not serialise as a string".into(),
            )),
        }
    }
}

impl FromSql for CatalogueHash {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Self::from_hex(value.as_str()?).ok_or_else(|| {
            FromSqlError::Other("a catalogue hash that is not a lowercase hex digest".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_hex;
    use crate::store::candidates::tests::{
        insert as insert_candidate, insert_passage, local, teacher, OTHER_PASSAGE_TEXT,
        PASSAGE_TEXT, TEXT,
    };
    use crate::store::card::tests::{assert_insert_only, open, rows};
    use crate::store::passages::PassageKind;
    use crate::test_support::TempRoot;
    use serde_json::json;

    const APP_VERSION: &str = "0.1.0";
    /// A word the hard results quote, standing in for a word of a candidate's text.
    const QUOTED: &str = "mispeled";

    fn catalogue(bytes: &[u8]) -> CatalogueHash {
        CatalogueHash::from_hex(&sha256_hex(bytes)).unwrap()
    }

    /// A store holding a passage, one candidate of `TEXT` on it, and an empty ruleset version.
    fn store_with_candidate(root: &TempRoot) -> (Store, String, RulesetVersion) {
        let store = open(root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let id = insert_candidate(&store, passage, TEXT, &local());
        let hash = store.candidate_blocking(id).unwrap().content_hash;
        let version = store.insert_ruleset_version_blocking(&[]).unwrap();
        (store, hash, version)
    }

    /// A failed screening of `candidate_hash`, killed by spelling.
    fn failed(candidate_hash: &str, ruleset_version: RulesetVersion) -> ScreenResult {
        ScreenResult {
            candidate_hash: candidate_hash.to_owned(),
            ruleset_version,
            catalogue_hash: catalogue(b"the catalogue"),
            app_version: APP_VERSION.to_owned(),
            hard_results: json!([
                { "source": "content", "passed": true },
                { "source": "spelling", "passed": false, "words": [QUOTED] },
            ]),
            soft_results: json!([{ "source": "tells", "passed": false, "patterns": ["testament to"] }]),
            killing_rule: Some(KillingRule::Spelling),
        }
    }

    /// A screening of `candidate_hash` that nothing failed.
    fn passed(candidate_hash: &str, ruleset_version: RulesetVersion) -> ScreenResult {
        ScreenResult {
            hard_results: json!([]),
            soft_results: json!({}),
            killing_rule: None,
            ..failed(candidate_hash, ruleset_version)
        }
    }

    fn insert(store: &Store, result: &ScreenResult) -> ScreenResultId {
        store.insert_screen_result_blocking(result).unwrap()
    }

    fn latest(store: &Store, result: &ScreenResult) -> Option<StoredScreenResult> {
        store
            .latest_screen_result_blocking(
                &result.candidate_hash,
                result.ruleset_version,
                &result.catalogue_hash,
            )
            .unwrap()
    }

    #[test]
    fn a_screen_result_reads_back_whole() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let mut results = vec![failed(&hash, version), passed(&hash, version)];
        results[1].catalogue_hash = catalogue(b"another catalogue");
        for (rule, bytes) in KillingRule::ALL.iter().zip([b"c", b"s", b"g", b"y", b"t"]) {
            results.push(ScreenResult {
                killing_rule: Some(*rule),
                catalogue_hash: catalogue(bytes),
                ..failed(&hash, version)
            });
        }

        for result in &results {
            let id = insert(&store, result);
            assert_eq!(
                latest(&store, result),
                Some(StoredScreenResult {
                    id,
                    result: result.clone()
                })
            );
        }
        let row = &rows(&store, "screen_results")[0];
        assert_eq!(row[7], rusqlite::types::Value::Text("spelling".to_owned()));
    }

    #[test]
    fn a_lookup_under_a_key_with_no_screening_returns_none() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let result = failed(&hash, version);
        insert(&store, &result);

        let other_catalogue = ScreenResult {
            catalogue_hash: catalogue(b"another catalogue"),
            ..result.clone()
        };
        let other_version = ScreenResult {
            ruleset_version: RulesetVersion(version.0 + 1),
            ..result.clone()
        };

        assert_eq!(latest(&store, &other_catalogue), None);
        assert_eq!(latest(&store, &other_version), None);
    }

    #[test]
    fn one_text_on_two_passages_is_two_hashes_and_a_result_for_one_is_not_returned_for_the_other() {
        let root = TempRoot::new();
        let store = open(&root);
        let first_passage = insert_passage(&store, PASSAGE_TEXT);
        let second_passage = insert_passage(&store, OTHER_PASSAGE_TEXT);
        let first = insert_candidate(&store, first_passage, TEXT, &local());
        let second = insert_candidate(&store, second_passage, TEXT, &local());
        let first_hash = store.candidate_blocking(first).unwrap().content_hash;
        let second_hash = store.candidate_blocking(second).unwrap().content_hash;
        let version = store.insert_ruleset_version_blocking(&[]).unwrap();

        insert(&store, &failed(&first_hash, version));

        assert_ne!(first_hash, second_hash);
        assert!(latest(&store, &failed(&first_hash, version)).is_some());
        assert_eq!(
            latest(&store, &failed(&second_hash, version)),
            None,
            "candidate {second_hash}"
        );
    }

    #[test]
    fn one_text_on_one_passage_from_two_generators_is_two_rows_served_by_one_result() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let local_id = insert_candidate(&store, passage, TEXT, &local());
        let teacher_id = insert_candidate(&store, passage, TEXT, &teacher());
        let local_candidate = store.candidate_blocking(local_id).unwrap();
        let teacher_candidate = store.candidate_blocking(teacher_id).unwrap();
        let version = store.insert_ruleset_version_blocking(&[]).unwrap();

        let id = insert(&store, &failed(&local_candidate.content_hash, version));

        assert_ne!(local_id, teacher_id);
        assert_eq!(rows(&store, "candidates").len(), 2);
        assert_eq!(local_candidate.generation, local());
        assert_eq!(teacher_candidate.generation, teacher());
        assert_eq!(local_candidate.content_hash, teacher_candidate.content_hash);
        let served = latest(&store, &failed(&teacher_candidate.content_hash, version));
        assert_eq!(served.map(|stored| stored.id), Some(id));
    }

    #[test]
    fn a_screen_result_naming_an_absent_candidate_hash_or_ruleset_version_inserts_nothing() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let absent_hash = sha256_hex(b"no candidate");
        let absent_version = RulesetVersion(version.0 + 1);

        let no_candidate = store
            .insert_screen_result_blocking(&failed(&absent_hash, version))
            .unwrap_err();
        let no_version = store
            .insert_screen_result_blocking(&failed(&hash, absent_version))
            .unwrap_err();

        assert!(
            matches!(&no_candidate, StoreError::NoCandidateHash { content_hash } if *content_hash == absent_hash),
            "{no_candidate:?}"
        );
        assert_eq!(
            no_candidate.to_string(),
            format!("the store holds no candidate with content hash {absent_hash}")
        );
        assert!(
            matches!(no_version, StoreError::NoRulesetVersion { version } if version == absent_version),
            "{no_version:?}"
        );
        assert!(rows(&store, "screen_results").is_empty());
    }

    #[test]
    fn the_table_refuses_an_absent_ruleset_version_a_killing_rule_outside_five_and_invalid_json() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let statement = "INSERT INTO screen_results (candidate_hash, ruleset_version_id,
                                                     catalogue_hash, app_version, hard_results,
                                                     soft_results, killing_rule)
                         VALUES (?1, ?2, 'ab', '0.1.0', ?3, '[]', ?4)";

        for (case, ruleset_version, hard_results, killing_rule, constraint) in [
            (
                "an absent ruleset version",
                version.0 + 1,
                "[]",
                None,
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            ),
            (
                "a sixth killing rule",
                version.0,
                "[]",
                Some("length"),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
            ),
            (
                "hard results that are not JSON",
                version.0,
                "passed",
                None,
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
            ),
        ] {
            let hash = hash.clone();
            let result =
                store.request_blocking("insert a screen result directly", move |connection| {
                    connection.execute(
                        statement,
                        (hash, ruleset_version, hard_results, killing_rule),
                    )
                });
            match result {
                Err(StoreError::Request {
                    source: rusqlite::Error::SqliteFailure(failure, _),
                    ..
                }) => assert_eq!(failure.extended_code, constraint, "{case}"),
                other => panic!("{case}: expected a constraint to refuse it, got {other:?}"),
            }
        }
        assert!(rows(&store, "screen_results").is_empty());
    }

    #[test]
    fn one_candidate_under_two_ruleset_versions_is_two_rows_and_the_first_reads_as_inserted() {
        let root = TempRoot::new();
        let (store, hash, first_version) = store_with_candidate(&root);
        let first = failed(&hash, first_version);
        let first_id = insert(&store, &first);
        let before = rows(&store, "screen_results");
        let second_version = store.insert_ruleset_version_blocking(&[]).unwrap();
        let second = passed(&hash, second_version);

        let second_id = insert(&store, &second);

        assert_ne!(second_id, first_id);
        let after = rows(&store, "screen_results");
        assert_eq!(after.len(), 2);
        assert_eq!(after[..1], before[..]);
        assert_eq!(
            latest(&store, &first),
            Some(StoredScreenResult {
                id: first_id,
                result: first
            })
        );
        assert_eq!(
            latest(&store, &second),
            Some(StoredScreenResult {
                id: second_id,
                result: second
            })
        );
    }

    /// The re-screen differs only in its app version, standing in for a screening on a new build.
    #[test]
    fn one_key_screened_twice_returns_the_latest_and_keeps_both_rows() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let first = failed(&hash, version);
        let first_id = insert(&store, &first);
        let rescreen = ScreenResult {
            app_version: "0.2.0".to_owned(),
            ..passed(&hash, version)
        };

        let rescreen_id = insert(&store, &rescreen);

        assert!(rescreen_id.0 > first_id.0);
        assert_eq!(
            latest(&store, &first),
            Some(StoredScreenResult {
                id: rescreen_id,
                result: rescreen
            })
        );
        assert_eq!(rows(&store, "screen_results").len(), 2);
    }

    #[test]
    fn updating_or_deleting_a_screen_result_raises_and_leaves_it_as_it_was() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let result = failed(&hash, version);
        let id = insert(&store, &result);

        assert_insert_only(&store, "screen_results");

        assert_eq!(
            latest(&store, &result),
            Some(StoredScreenResult { id, result })
        );
    }

    #[test]
    fn a_screen_results_debug_rendering_and_a_failed_insert_or_read_never_carry_its_results() {
        let root = TempRoot::new();
        let (store, hash, version) = store_with_candidate(&root);
        let result = failed(&hash, version);
        let rendering = format!("{result:?}");
        store
            .request_blocking("drop the screen results table", |connection| {
                connection.execute_batch("DROP TABLE screen_results")
            })
            .unwrap();

        let insert_error = store.insert_screen_result_blocking(&result).unwrap_err();
        let read_error = store
            .latest_screen_result_blocking(&hash, version, &result.catalogue_hash)
            .unwrap_err();

        assert!(
            !rendering.contains(QUOTED),
            "a Debug rendering for candidate {hash} carries its results"
        );
        assert!(rendering.contains(&hash), "candidate {hash}");
        // Checked before either error is printed in a failure message below.
        for error in [&insert_error, &read_error] {
            for rendering in [error.to_string(), format!("{error:?}")] {
                assert!(
                    !rendering.contains(QUOTED),
                    "an error for candidate {hash} carries its results"
                );
            }
        }
        assert!(
            matches!(
                insert_error,
                StoreError::Request {
                    operation: INSERT_SCREEN_RESULT,
                    ..
                }
            ),
            "{insert_error:?}"
        );
        assert!(
            matches!(
                read_error,
                StoreError::Request {
                    operation: READ_SCREEN_RESULT,
                    ..
                }
            ),
            "{read_error:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_insert_and_lookup_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = store
            .insert_passage(PassageKind::Captured, PASSAGE_TEXT, None)
            .await
            .unwrap();
        let candidate = store
            .insert_candidate(passage, TEXT, &local())
            .await
            .unwrap();
        let hash = store.candidate(candidate).await.unwrap().content_hash;
        let version = store.insert_ruleset_version(&[]).await.unwrap();
        let result = failed(&hash, version);

        let id = store.insert_screen_result(&result).await.unwrap();
        let found = store
            .latest_screen_result(&hash, version, &result.catalogue_hash)
            .await
            .unwrap();
        let refused = store
            .insert_screen_result(&failed(&sha256_hex(b"no candidate"), version))
            .await;
        let absent = store
            .latest_screen_result(&hash, RulesetVersion(version.0 + 1), &result.catalogue_hash)
            .await
            .unwrap();

        assert_eq!(found, Some(StoredScreenResult { id, result }));
        assert!(
            matches!(refused, Err(StoreError::NoCandidateHash { .. })),
            "{refused:?}"
        );
        assert_eq!(absent, None);
    }
}
