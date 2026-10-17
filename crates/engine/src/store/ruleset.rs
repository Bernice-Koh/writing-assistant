//! The ruleset's tables: every checker the ruleset has held, each stored once with its provenance
//! and its content hash, and each ruleset version with the checkers it holds. Every row is inserted
//! once and never changed, so a version reads back as it was published however far the ruleset has
//! moved on, and a checker that two versions share is one row. Nothing writes a ruleset until #95,
//! which builds publishing on these inserts.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::card::clause_exists;
use super::error::StoreError;
use super::json::Json;
use super::thread::Store;
use crate::flag::{ClauseId, RulesetVersion};
use crate::hash::sha256_hex;
use crate::style::checker::Checker;

const INSERT_CHECKER: &str = "insert a checker";
const INSERT_RULESET_VERSION: &str = "insert a ruleset version";
const READ_RULESET_VERSION: &str = "read a ruleset version";

/// A checker's id in the store, assigned at its first insert and never given to another checker.
/// It names the checker in one store only; the content hash names it in every store. An `i64`
/// because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckerId(pub i64);

/// Where a checker came from. It is stored beside the checker and left out of the content hash, so
/// two inserts of one checker are one row whoever wrote each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub author: CheckerAuthor,
    /// A plain statement of what the checker does, written by a model that saw only its code.
    pub back_translation: Option<String>,
    /// Empty for a checker admitted without tests.
    pub test_results: Vec<TestResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckerAuthor {
    /// The template a questionnaire answer maps to, by lookup.
    Template,
    /// A model, drafting the checker for a clause the router wrote.
    Model,
    /// The owner, editing a checker in the app.
    Owner,
}

/// One test a checker ran before it was admitted: a sentence, whether the checker should flag it,
/// and whether it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub sentence: String,
    pub should_flag: bool,
    pub flagged: bool,
}

/// A checker as its row holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredChecker {
    pub id: CheckerId,
    pub checker: Checker,
    pub provenance: Provenance,
    /// The SHA-256 of the checker's serde JSON, as lowercase hex, so any change to the checker
    /// changes it and no change to its provenance does. It is the checker's rule id for every
    /// matcher but a LanguageTool rule id (#91). #95 keeps or replaces this definition.
    pub content_hash: String,
}

/// A ruleset version as its rows hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedRuleset {
    pub version: RulesetVersion,
    /// When the version was inserted, in UTC to the millisecond, by SQLite's clock.
    pub published_at: String,
    /// The SHA-256, as lowercase hex, of its checkers' content hashes in ascending order, so its
    /// membership alone determines it. #95 keeps or replaces this definition.
    pub content_hash: String,
    /// In ascending order of id.
    pub checkers: Vec<StoredChecker>,
}

impl Store {
    /// Inserts `checker` with `provenance` and returns its id. A stored checker with the same
    /// content hash is this checker: its id returns, nothing is added, and the stored provenance
    /// stands.
    ///
    /// # Errors
    /// [`StoreError::NoClause`] when the store holds no clause under the checker's clause id.
    /// [`StoreError::Busy`] when another connection holds the write lock past the busy timeout,
    /// [`StoreError::Request`] for any other failure of the insert, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn insert_checker(
        &self,
        checker: &Checker,
        provenance: &Provenance,
    ) -> Result<CheckerId, StoreError> {
        self.request(INSERT_CHECKER, insert_checker_work(checker, provenance))
            .await?
    }

    /// [`insert_checker`](Self::insert_checker) for a synchronous caller, such as `wa`, which
    /// blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_checker`](Self::insert_checker).
    ///
    /// # Panics
    /// When called from async code, which awaits [`insert_checker`](Self::insert_checker) instead.
    pub fn insert_checker_blocking(
        &self,
        checker: &Checker,
        provenance: &Provenance,
    ) -> Result<CheckerId, StoreError> {
        self.request_blocking(INSERT_CHECKER, insert_checker_work(checker, provenance))?
    }

    /// Inserts a ruleset version holding `checkers` and returns its version. The order of
    /// `checkers` reaches neither the version's content hash nor its read.
    ///
    /// # Errors
    /// [`StoreError::NoChecker`] for a checker the store does not hold, and
    /// [`StoreError::CheckerRepeated`] for one listed twice, either inserting nothing.
    /// [`StoreError::Busy`] when another connection holds the write lock past the busy timeout,
    /// [`StoreError::Request`] for any other failure of the insert, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn insert_ruleset_version(
        &self,
        checkers: &[CheckerId],
    ) -> Result<RulesetVersion, StoreError> {
        self.request(INSERT_RULESET_VERSION, insert_ruleset_work(checkers))
            .await?
    }

    /// [`insert_ruleset_version`](Self::insert_ruleset_version) for a synchronous caller, such as
    /// `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_ruleset_version`](Self::insert_ruleset_version).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`insert_ruleset_version`](Self::insert_ruleset_version) instead.
    pub fn insert_ruleset_version_blocking(
        &self,
        checkers: &[CheckerId],
    ) -> Result<RulesetVersion, StoreError> {
        self.request_blocking(INSERT_RULESET_VERSION, insert_ruleset_work(checkers))?
    }

    /// The ruleset version stored as `version`, with every checker it holds.
    ///
    /// # Errors
    /// [`StoreError::NoRulesetVersion`] when the store holds no such version,
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, a stored checker that does not
    /// decode among them, and [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn ruleset_version(
        &self,
        version: RulesetVersion,
    ) -> Result<PublishedRuleset, StoreError> {
        self.request(READ_RULESET_VERSION, move |connection| {
            read(connection, version)
        })
        .await?
        .ok_or(StoreError::NoRulesetVersion { version })
    }

    /// [`ruleset_version`](Self::ruleset_version) for a synchronous caller, such as `wa`, which
    /// blocks on the reply.
    ///
    /// # Errors
    /// As [`ruleset_version`](Self::ruleset_version).
    ///
    /// # Panics
    /// When called from async code, which awaits [`ruleset_version`](Self::ruleset_version)
    /// instead.
    pub fn ruleset_version_blocking(
        &self,
        version: RulesetVersion,
    ) -> Result<PublishedRuleset, StoreError> {
        self.request_blocking(READ_RULESET_VERSION, move |connection| {
            read(connection, version)
        })?
        .ok_or(StoreError::NoRulesetVersion { version })
    }
}

/// The work of one checker insert, with the definition and its content hash computed before it
/// reaches the store thread. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert_checker_work(
    checker: &Checker,
    provenance: &Provenance,
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<CheckerId, StoreError>> + Send + 'static
{
    // Struct fields serialise in their declared order, and the modes and the registers are ordered
    // sets, so equal checkers serialise alike.
    let definition = serde_json::to_string(checker)
        .expect("a checker holds no map, the one shape whose serialisation can fail");
    let content_hash = sha256_hex(definition.as_bytes());
    let clause_id = checker.clause_id;
    let provenance = provenance.clone();
    move |connection| {
        insert_checker(
            connection,
            clause_id,
            &definition,
            &content_hash,
            &provenance,
        )
    }
}

/// Begins `IMMEDIATE`, so the lookup and the insert run under the write lock: another writer waits
/// inside the busy timeout rather than inserting the same checker between them.
fn insert_checker(
    connection: &mut Connection,
    clause_id: ClauseId,
    definition: &str,
    content_hash: &str,
    provenance: &Provenance,
) -> rusqlite::Result<Result<CheckerId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored: Option<i64> = transaction
        .query_row(
            "SELECT id FROM checkers WHERE content_hash = ?1",
            [content_hash],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = stored {
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        return Ok(Ok(CheckerId(id)));
    }
    if !clause_exists(&transaction, clause_id)? {
        return Ok(Err(StoreError::NoClause { id: clause_id }));
    }
    transaction.execute(
        "INSERT INTO checkers (clause_id, definition, content_hash, provenance)
         VALUES (?1, ?2, ?3, ?4)",
        (clause_id.0, definition, content_hash, Json(provenance)),
    )?;
    let id = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(Ok(CheckerId(id)))
}

/// The work of one ruleset version's insert. Its outer result is SQLite's, and its inner one is the
/// store's refusal.
fn insert_ruleset_work(
    checkers: &[CheckerId],
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<RulesetVersion, StoreError>> + Send + 'static
{
    let checkers = checkers.to_vec();
    move |connection| insert_ruleset(connection, &checkers)
}

/// Begins `IMMEDIATE`, since the lookups come before the writes, and a deferred transaction that
/// reads first fails at its first write, without waiting, while another connection holds the write
/// lock.
fn insert_ruleset(
    connection: &mut Connection,
    checkers: &[CheckerId],
) -> rusqlite::Result<Result<RulesetVersion, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut listed = HashSet::new();
    let mut hashes = Vec::with_capacity(checkers.len());
    for &id in checkers {
        if !listed.insert(id) {
            return Ok(Err(StoreError::CheckerRepeated { id }));
        }
        let hash: Option<String> = transaction
            .query_row(
                "SELECT content_hash FROM checkers WHERE id = ?1",
                [id.0],
                |row| row.get(0),
            )
            .optional()?;
        match hash {
            Some(hash) => hashes.push(hash),
            None => return Ok(Err(StoreError::NoChecker { id })),
        }
    }
    transaction.execute(
        "INSERT INTO ruleset_versions (published_at, content_hash)
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?1)",
        [ruleset_content_hash(hashes)],
    )?;
    let version = RulesetVersion(transaction.last_insert_rowid());
    for id in checkers {
        transaction.execute(
            "INSERT INTO ruleset_members (ruleset_version_id, checker_id) VALUES (?1, ?2)",
            (version.0, id.0),
        )?;
    }
    transaction.commit()?;
    Ok(Ok(version))
}

/// The SHA-256 of `checker_hashes` in ascending order, run together. Each is 64 hex digits, so the
/// run needs no separator to be read back unambiguously.
fn ruleset_content_hash(mut checker_hashes: Vec<String>) -> String {
    checker_hashes.sort_unstable();
    sha256_hex(checker_hashes.concat().as_bytes())
}

fn read(
    connection: &Connection,
    version: RulesetVersion,
) -> rusqlite::Result<Option<PublishedRuleset>> {
    let Some((published_at, content_hash)) = connection
        .query_row(
            "SELECT published_at, content_hash FROM ruleset_versions WHERE id = ?1",
            [version.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let checkers = connection
        .prepare(
            "SELECT c.id, c.definition, c.provenance, c.content_hash
             FROM ruleset_members AS m JOIN checkers AS c ON c.id = m.checker_id
             WHERE m.ruleset_version_id = ?1
             ORDER BY c.id",
        )?
        .query_map([version.0], |row| {
            let Json(checker): Json<Checker> = row.get(1)?;
            let Json(provenance): Json<Provenance> = row.get(2)?;
            Ok(StoredChecker {
                id: CheckerId(row.get(0)?),
                checker,
                provenance,
                content_hash: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(PublishedRuleset {
        version,
        published_at,
        content_hash,
        checkers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::card::tests::{
        assert_insert_only, assert_utc_to_the_millisecond, insert_version, new, open, refusal,
        rows, sqlite_now, HEDGES, SHORT,
    };
    use crate::style::checker::{Band, CheckableMode, Matcher, PhraseEntry, Unit};
    use crate::test_support::TempRoot;
    use rusqlite::types::Value;
    use std::collections::BTreeSet;

    const RULESET_TABLES: [&str; 3] = ["checkers", "ruleset_versions", "ruleset_members"];

    /// The serde JSON of `hedging_limit(ClauseId(1), 1)`, which its content hash is taken over.
    const HEDGING_LIMIT_JSON: &str = r#"{"clauseId":1,"matcher":{"kind":"phraseList","entries":[{"phrase":"perhaps","prefix":false}]},"unit":"paragraph","band":{"kind":"count","minimum":null,"maximum":1},"formModes":["prose"],"registerScope":null}"#;

    /// A store holding two clauses, the hedging limit's and the sentence length's, with ids 1 and
    /// 2.
    fn store_with_clauses(root: &TempRoot) -> (Store, ClauseId, ClauseId) {
        let store = open(root);
        let card = insert_version(&store, &[new(HEDGES), new(SHORT)]);
        (store, card.clauses[0].clause_id, card.clauses[1].clause_id)
    }

    fn hedging_limit(clause_id: ClauseId, maximum: u32) -> Checker {
        Checker {
            clause_id,
            matcher: Matcher::PhraseList {
                entries: vec![PhraseEntry {
                    phrase: "perhaps".to_owned(),
                    prefix: false,
                }],
            },
            unit: Unit::Paragraph,
            band: Band::Count {
                minimum: None,
                maximum: Some(maximum),
            },
            form_modes: BTreeSet::from([CheckableMode::Prose]),
            register_scope: None,
        }
    }

    fn sentence_length(clause_id: ClauseId) -> Checker {
        Checker {
            clause_id,
            matcher: Matcher::WordCount,
            unit: Unit::Sentence,
            band: Band::Count {
                minimum: None,
                maximum: Some(30),
            },
            form_modes: BTreeSet::from([CheckableMode::Prose]),
            register_scope: Some(BTreeSet::from(["email".to_owned()])),
        }
    }

    fn drafted() -> Provenance {
        Provenance {
            author: CheckerAuthor::Model,
            back_translation: Some(
                "Flags a paragraph that says perhaps more than once.".to_owned(),
            ),
            test_results: vec![
                TestResult {
                    sentence: "Perhaps it rains, and perhaps it snows.".to_owned(),
                    should_flag: true,
                    flagged: true,
                },
                TestResult {
                    sentence: "It rains.".to_owned(),
                    should_flag: false,
                    flagged: false,
                },
            ],
        }
    }

    fn templated() -> Provenance {
        Provenance {
            author: CheckerAuthor::Template,
            back_translation: None,
            test_results: Vec::new(),
        }
    }

    fn insert(store: &Store, checker: &Checker, provenance: &Provenance) -> CheckerId {
        store.insert_checker_blocking(checker, provenance).unwrap()
    }

    /// Inserts a ruleset version of `checkers` and reads it back.
    fn insert_ruleset(store: &Store, checkers: &[CheckerId]) -> PublishedRuleset {
        let version = store.insert_ruleset_version_blocking(checkers).unwrap();
        store.ruleset_version_blocking(version).unwrap()
    }

    fn ruleset_rows(store: &Store) -> Vec<Vec<Vec<Value>>> {
        RULESET_TABLES
            .iter()
            .map(|table| rows(store, table))
            .collect()
    }

    #[test]
    fn a_checker_reads_back_whole_with_its_provenance_and_content_hash() {
        let root = TempRoot::new();
        let (store, hedges, _) = store_with_clauses(&root);
        let checker = hedging_limit(hedges, 1);
        let id = insert(&store, &checker, &drafted());
        let before = sqlite_now(&store);

        let ruleset = insert_ruleset(&store, &[id]);

        let after = sqlite_now(&store);
        assert_eq!(
            ruleset.checkers,
            [StoredChecker {
                id,
                checker,
                provenance: drafted(),
                content_hash: sha256_hex(HEDGING_LIMIT_JSON.as_bytes()),
            }]
        );
        assert_eq!(ruleset.checkers[0].checker.clause_id, hedges);
        assert_utc_to_the_millisecond(&ruleset.published_at);
        assert!(
            before <= ruleset.published_at && ruleset.published_at <= after,
            "published at {} outside {before} to {after}",
            ruleset.published_at
        );
        let row = &rows(&store, "checkers")[0];
        assert_eq!(row[1], Value::Integer(hedges.0));
        assert_eq!(row[2], Value::Text(HEDGING_LIMIT_JSON.to_owned()));
        assert_eq!(
            row[4],
            Value::Text(
                r#"{"author":"model","backTranslation":"Flags a paragraph that says perhaps more than once.","testResults":[{"sentence":"Perhaps it rains, and perhaps it snows.","shouldFlag":true,"flagged":true},{"sentence":"It rains.","shouldFlag":false,"flagged":false}]}"#
                    .to_owned()
            )
        );
    }

    #[test]
    fn inserting_a_stored_checker_again_returns_its_id_adds_no_row_and_keeps_its_provenance() {
        let root = TempRoot::new();
        let (store, hedges, _) = store_with_clauses(&root);
        let checker = hedging_limit(hedges, 1);
        let first = insert(&store, &checker, &drafted());

        let again = insert(&store, &checker, &templated());

        assert_eq!(again, first);
        assert_eq!(rows(&store, "checkers").len(), 1);
        let ruleset = insert_ruleset(&store, &[first]);
        assert_eq!(ruleset.checkers[0].provenance, drafted());
    }

    #[test]
    fn a_change_to_any_part_of_a_checker_is_a_new_row_with_its_own_hash() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let original = hedging_limit(hedges, 1);
        let changed = [
            hedging_limit(hedges, 2),
            hedging_limit(short, 1),
            Checker {
                unit: Unit::Sentence,
                ..original.clone()
            },
            Checker {
                form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
                ..original.clone()
            },
            Checker {
                register_scope: Some(BTreeSet::from(["email".to_owned()])),
                ..original.clone()
            },
        ];

        let mut ids = vec![insert(&store, &original, &templated())];
        ids.extend(
            changed
                .iter()
                .map(|checker| insert(&store, checker, &templated())),
        );

        let ruleset = insert_ruleset(&store, &ids);
        let hashes: HashSet<&str> = ruleset
            .checkers
            .iter()
            .map(|stored| stored.content_hash.as_str())
            .collect();
        assert_eq!(ruleset.checkers.len(), 1 + changed.len());
        assert_eq!(hashes.len(), 1 + changed.len());
    }

    #[test]
    fn two_ruleset_versions_sharing_a_checker_list_the_one_row() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let hedging = insert(&store, &hedging_limit(hedges, 1), &drafted());
        let length = insert(&store, &sentence_length(short), &templated());

        let first = insert_ruleset(&store, &[hedging, length]);
        let second = insert_ruleset(&store, &[hedging]);

        assert_eq!(second.checkers, first.checkers[..1]);
        assert_eq!(rows(&store, "checkers").len(), 2);
        let holding_hedging = rows(&store, "ruleset_members")
            .iter()
            .filter(|row| row[2] == Value::Integer(hedging.0))
            .count();
        assert_eq!(holding_hedging, 2);
    }

    #[test]
    fn a_version_omitting_a_checker_leaves_the_earlier_versions_membership() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let hedging = insert(&store, &hedging_limit(hedges, 1), &drafted());
        let length = insert(&store, &sentence_length(short), &templated());
        let first = insert_ruleset(&store, &[hedging, length]);
        let before = ruleset_rows(&store);
        let raised = insert(&store, &hedging_limit(hedges, 2), &drafted());

        let second = insert_ruleset(&store, &[raised, length]);

        let after = ruleset_rows(&store);
        for ((table, before), after) in RULESET_TABLES.iter().zip(&before).zip(&after) {
            assert_eq!(&after[..before.len()], before.as_slice(), "{table}");
        }
        assert_eq!(
            store.ruleset_version_blocking(first.version).unwrap(),
            first
        );
        let held: Vec<CheckerId> = second.checkers.iter().map(|stored| stored.id).collect();
        assert_eq!(held, [length, raised]);
        assert_ne!(second.content_hash, first.content_hash);
    }

    #[test]
    fn a_ruleset_versions_content_hash_follows_its_membership_alone() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let hedging = insert(&store, &hedging_limit(hedges, 1), &drafted());
        let length = insert(&store, &sentence_length(short), &templated());

        let forward = insert_ruleset(&store, &[hedging, length]);
        let backward = insert_ruleset(&store, &[length, hedging]);
        let alone = insert_ruleset(&store, &[hedging]);
        let empty = insert_ruleset(&store, &[]);

        assert_ne!(forward.version, backward.version);
        assert_eq!(forward.checkers, backward.checkers);
        assert_eq!(forward.content_hash, backward.content_hash);
        let mut member_hashes: Vec<&str> = forward
            .checkers
            .iter()
            .map(|stored| stored.content_hash.as_str())
            .collect();
        member_hashes.sort_unstable();
        assert_eq!(
            forward.content_hash,
            sha256_hex(member_hashes.concat().as_bytes())
        );
        assert_ne!(alone.content_hash, forward.content_hash);
        assert!(empty.checkers.is_empty());
        assert_eq!(empty.content_hash, sha256_hex(b""));
    }

    #[test]
    fn the_checkers_table_refuses_a_definition_serving_another_clause_than_the_row() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let statement = "INSERT INTO checkers (clause_id, definition, content_hash, provenance)
                         VALUES (?1, ?2, ?3, '{}')";

        for (case, definition) in [
            (
                "a definition serving the other clause",
                serde_json::to_string(&hedging_limit(hedges, 1)).unwrap(),
            ),
            ("a definition with no clause", "{}".to_owned()),
        ] {
            assert_eq!(
                refusal(
                    &store,
                    statement,
                    vec![
                        Value::Integer(short.0),
                        Value::Text(definition),
                        Value::Text(case.to_owned()),
                    ],
                ),
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                "{case}"
            );
        }
    }

    #[test]
    fn a_checker_for_a_clause_the_store_does_not_hold_inserts_nothing() {
        let root = TempRoot::new();
        let (store, _, short) = store_with_clauses(&root);
        let absent = ClauseId(short.0 + 1);

        let error = store
            .insert_checker_blocking(&hedging_limit(absent, 1), &templated())
            .unwrap_err();

        assert!(
            matches!(error, StoreError::NoClause { id } if id == absent),
            "{error:?}"
        );
        assert!(rows(&store, "checkers").is_empty());
    }

    #[test]
    fn a_ruleset_version_naming_an_absent_checker_or_one_twice_inserts_nothing() {
        let root = TempRoot::new();
        let (store, hedges, _) = store_with_clauses(&root);
        let hedging = insert(&store, &hedging_limit(hedges, 1), &drafted());
        insert_ruleset(&store, &[hedging]);
        let before = ruleset_rows(&store);
        let absent = CheckerId(hedging.0 + 1);

        let no_checker = store
            .insert_ruleset_version_blocking(&[hedging, absent])
            .unwrap_err();
        let repeated = store
            .insert_ruleset_version_blocking(&[hedging, hedging])
            .unwrap_err();

        assert!(
            matches!(no_checker, StoreError::NoChecker { id } if id == absent),
            "{no_checker:?}"
        );
        assert_eq!(
            no_checker.to_string(),
            "the store holds no checker with id 2"
        );
        assert!(
            matches!(repeated, StoreError::CheckerRepeated { id } if id == hedging),
            "{repeated:?}"
        );
        assert_eq!(
            repeated.to_string(),
            "a ruleset version lists checker 1 more than once"
        );
        assert_eq!(ruleset_rows(&store), before);
    }

    #[test]
    fn reading_a_version_the_store_does_not_hold_returns_no_ruleset_version() {
        let root = TempRoot::new();
        let store = open(&root);

        let error = store
            .ruleset_version_blocking(RulesetVersion(1))
            .unwrap_err();

        assert!(
            matches!(
                error,
                StoreError::NoRulesetVersion {
                    version: RulesetVersion(1)
                }
            ),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no ruleset version 1");
    }

    #[test]
    fn updating_or_deleting_any_ruleset_row_raises_and_leaves_each_version_as_published() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let hedging = insert(&store, &hedging_limit(hedges, 1), &drafted());
        let length = insert(&store, &sentence_length(short), &templated());
        let first = insert_ruleset(&store, &[hedging, length]);
        let second = insert_ruleset(&store, &[length]);

        for table in RULESET_TABLES {
            assert_insert_only(&store, table);
        }

        assert_eq!(
            store.ruleset_version_blocking(first.version).unwrap(),
            first
        );
        assert_eq!(
            store.ruleset_version_blocking(second.version).unwrap(),
            second
        );
    }

    #[test]
    fn a_stored_checker_that_does_not_decode_fails_the_read_without_quoting_it() {
        const PHRASE: &str = "a phrase no error may quote";
        let root = TempRoot::new();
        let (store, hedges, _) = store_with_clauses(&root);
        let definition =
            HEDGING_LIMIT_JSON.replace(r#""prefix":false"#, &format!(r#""prefix":"{PHRASE}""#));
        let serde_message = serde_json::from_str::<Checker>(&definition)
            .unwrap_err()
            .to_string();
        assert!(
            serde_message.contains(PHRASE),
            "serde's own message quotes the value"
        );
        let id = store
            .request_blocking("insert a checker that does not decode", move |connection| {
                connection.execute(
                    "INSERT INTO checkers (clause_id, definition, content_hash, provenance)
                     VALUES (?1, ?2, 'undecodable', '{}')",
                    (hedges.0, definition),
                )?;
                Ok(CheckerId(connection.last_insert_rowid()))
            })
            .unwrap();
        let version = store.insert_ruleset_version_blocking(&[id]).unwrap();

        let error = store.ruleset_version_blocking(version).unwrap_err();

        for rendering in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendering.contains(PHRASE),
                "the error quotes the definition"
            );
        }
        assert!(
            matches!(
                &error,
                StoreError::Request {
                    operation: READ_RULESET_VERSION,
                    source: rusqlite::Error::FromSqlConversionFailure(1, _, _),
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_inserts_and_read_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);
        let card = store
            .insert_card_version(&[new(HEDGES)], "the first Card")
            .await
            .unwrap();
        let hedges = store.card_version(card).await.unwrap().clauses[0].clause_id;
        let checker = hedging_limit(hedges, 1);

        let id = store.insert_checker(&checker, &drafted()).await.unwrap();
        let again = store.insert_checker(&checker, &templated()).await.unwrap();
        let version = store.insert_ruleset_version(&[id]).await.unwrap();
        let ruleset = store.ruleset_version(version).await.unwrap();
        let refused = store.insert_ruleset_version(&[id, id]).await;
        let absent = store.ruleset_version(RulesetVersion(version.0 + 1)).await;

        assert_eq!(again, id);
        assert_eq!(ruleset.checkers[0].checker, checker);
        assert_eq!(ruleset.checkers[0].provenance, drafted());
        assert!(
            matches!(refused, Err(StoreError::CheckerRepeated { .. })),
            "{refused:?}"
        );
        assert!(
            matches!(absent, Err(StoreError::NoRulesetVersion { .. })),
            "{absent:?}"
        );
    }
}
