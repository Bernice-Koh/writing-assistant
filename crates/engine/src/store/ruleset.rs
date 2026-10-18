//! The ruleset's tables: every checker the ruleset has held, each stored once with its provenance
//! and its content hash, and each ruleset version with the checkers it holds. Every row is inserted
//! once and never changed, so a version reads back as it was published however far the ruleset has
//! moved on, and a checker that two versions share is one row. A publish takes the whole set the
//! ruleset should hold and inserts a version only when that set differs from the latest version's.
//! Nothing publishes a ruleset until Phase 3.

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
const PUBLISH_RULESET: &str = "publish a ruleset";
const READ_RULESET_VERSION: &str = "read a ruleset version";
const READ_LATEST_RULESET_VERSION: &str = "read the latest ruleset version";

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
    /// matcher but a LanguageTool rule id.
    pub content_hash: String,
}

/// A ruleset version as its rows hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedRuleset {
    pub version: RulesetVersion,
    /// When the version was inserted, in UTC to the millisecond, by SQLite's clock.
    pub published_at: String,
    /// The SHA-256, as lowercase hex, of its checkers' content hashes in ascending order, so its
    /// membership alone determines it. A publish compares it with the latest version's to tell
    /// whether the membership changes.
    pub content_hash: String,
    /// In ascending order of id.
    pub checkers: Vec<StoredChecker>,
}

/// The ruleset version a publish left in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RulesetPublish {
    /// The latest version once the publish has run.
    pub version: RulesetVersion,
    /// Whether the publish inserted `version`. It did not when the latest version already held the
    /// published membership.
    pub is_new: bool,
}

impl Store {
    /// Makes `checkers`, each with its provenance, the ruleset in force, and returns the version in
    /// force afterwards. `checkers` is the whole set the ruleset should hold, in any order, and a
    /// checker listed twice is one member.
    ///
    /// Each checker the store does not hold by content hash is inserted with its provenance, and a
    /// stored one keeps the provenance it was stored with. A version holding exactly these
    /// checkers is then inserted unless its content hash equals the latest version's, so a publish
    /// of the membership in force inserts nothing at all. A changed parameter is a new checker, so
    /// its publish inserts a checker row and a version listing it in place of the old one; a
    /// retired checker's publish inserts a version without it. No earlier row changes. Everything
    /// runs in one transaction, so a refused publish writes nothing.
    ///
    /// # Errors
    /// [`StoreError::NoClause`] when the store holds no clause under the clause id of a checker it
    /// does not hold. [`StoreError::Busy`] when another connection holds the write lock past the
    /// busy timeout, [`StoreError::Request`] for any other failure of the publish, and
    /// [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn publish_ruleset(
        &self,
        checkers: &[(Checker, Provenance)],
    ) -> Result<RulesetPublish, StoreError> {
        self.request(PUBLISH_RULESET, publish_work(checkers))
            .await?
    }

    /// [`publish_ruleset`](Self::publish_ruleset) for a synchronous caller, such as `wa`, which
    /// blocks on the reply.
    ///
    /// # Errors
    /// As [`publish_ruleset`](Self::publish_ruleset).
    ///
    /// # Panics
    /// When called from async code, which awaits [`publish_ruleset`](Self::publish_ruleset)
    /// instead.
    pub fn publish_ruleset_blocking(
        &self,
        checkers: &[(Checker, Provenance)],
    ) -> Result<RulesetPublish, StoreError> {
        self.request_blocking(PUBLISH_RULESET, publish_work(checkers))?
    }

    /// The latest ruleset version, with every checker it holds, or `None` before the first.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, a stored checker that does not
    /// decode among them, and [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn latest_ruleset_version(&self) -> Result<Option<PublishedRuleset>, StoreError> {
        self.request(READ_LATEST_RULESET_VERSION, |connection| {
            read_latest(connection)
        })
        .await
    }

    /// [`latest_ruleset_version`](Self::latest_ruleset_version) for a synchronous caller, such as
    /// `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`latest_ruleset_version`](Self::latest_ruleset_version).
    ///
    /// # Panics
    /// When called from async code, which awaits
    /// [`latest_ruleset_version`](Self::latest_ruleset_version) instead.
    pub fn latest_ruleset_version_blocking(&self) -> Result<Option<PublishedRuleset>, StoreError> {
        self.request_blocking(READ_LATEST_RULESET_VERSION, |connection| {
            read_latest(connection)
        })
    }

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

/// A checker as its row writes it, with the definition and its content hash computed before it
/// reaches the store thread.
struct SerialisedChecker {
    clause_id: ClauseId,
    definition: String,
    content_hash: String,
    provenance: Provenance,
}

impl SerialisedChecker {
    fn new(checker: &Checker, provenance: &Provenance) -> Self {
        // Struct fields serialise in their declared order, and the modes and the registers are
        // ordered sets, so equal checkers serialise alike.
        let definition = serde_json::to_string(checker)
            .expect("a checker holds no map, the one shape whose serialisation can fail");
        Self {
            clause_id: checker.clause_id,
            content_hash: sha256_hex(definition.as_bytes()),
            definition,
            provenance: provenance.clone(),
        }
    }
}

/// The work of one checker insert. Its outer result is SQLite's, and its inner one is the store's
/// refusal.
fn insert_checker_work(
    checker: &Checker,
    provenance: &Provenance,
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<CheckerId, StoreError>> + Send + 'static
{
    let checker = SerialisedChecker::new(checker, provenance);
    move |connection| insert_checker(connection, &checker)
}

/// Begins `IMMEDIATE`, so the lookup and the insert run under the write lock: another writer waits
/// inside the busy timeout rather than inserting the same checker between them.
fn insert_checker(
    connection: &mut Connection,
    checker: &SerialisedChecker,
) -> rusqlite::Result<Result<CheckerId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = match stored_or_inserted(&transaction, checker)? {
        Ok(id) => id,
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        Err(refusal) => return Ok(Err(refusal)),
    };
    transaction.commit()?;
    Ok(Ok(id))
}

/// The id of the stored checker with `checker`'s content hash, or, when the store holds none, of
/// `checker` inserted with its provenance.
fn stored_or_inserted(
    connection: &Connection,
    checker: &SerialisedChecker,
) -> rusqlite::Result<Result<CheckerId, StoreError>> {
    let stored: Option<i64> = connection
        .query_row(
            "SELECT id FROM checkers WHERE content_hash = ?1",
            [&checker.content_hash],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = stored {
        return Ok(Ok(CheckerId(id)));
    }
    if !clause_exists(connection, checker.clause_id)? {
        return Ok(Err(StoreError::NoClause {
            id: checker.clause_id,
        }));
    }
    connection.execute(
        "INSERT INTO checkers (clause_id, definition, content_hash, provenance)
         VALUES (?1, ?2, ?3, ?4)",
        (
            checker.clause_id.0,
            &checker.definition,
            &checker.content_hash,
            Json(&checker.provenance),
        ),
    )?;
    Ok(Ok(CheckerId(connection.last_insert_rowid())))
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
    let version = insert_version(&transaction, checkers, &ruleset_content_hash(hashes))?;
    transaction.commit()?;
    Ok(Ok(version))
}

/// The work of one publish. Its outer result is SQLite's, and its inner one is the store's refusal.
fn publish_work(
    checkers: &[(Checker, Provenance)],
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<RulesetPublish, StoreError>> + Send + 'static
{
    let checkers: Vec<SerialisedChecker> = checkers
        .iter()
        .map(|(checker, provenance)| SerialisedChecker::new(checker, provenance))
        .collect();
    move |connection| publish(connection, &checkers)
}

/// Begins `IMMEDIATE`, so the version the membership is compared with is still the latest when a
/// new one commits, and no other writer inserts one of these checkers in between.
fn publish(
    connection: &mut Connection,
    checkers: &[SerialisedChecker],
) -> rusqlite::Result<Result<RulesetPublish, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut listed = HashSet::new();
    let mut members = Vec::with_capacity(checkers.len());
    let mut hashes = Vec::with_capacity(checkers.len());
    for checker in checkers {
        if !listed.insert(checker.content_hash.as_str()) {
            continue;
        }
        match stored_or_inserted(&transaction, checker)? {
            Ok(id) => members.push(id),
            // Returning drops the transaction, which rolls back every checker it inserted.
            Err(refusal) => return Ok(Err(refusal)),
        }
        hashes.push(checker.content_hash.clone());
    }
    let content_hash = ruleset_content_hash(hashes);
    let latest: Option<(i64, String)> = transaction
        .query_row(
            "SELECT id, content_hash FROM ruleset_versions ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((latest, _)) = latest.filter(|(_, latest_hash)| *latest_hash == content_hash) {
        // Every member is a checker the latest version holds, so the transaction inserted nothing.
        return Ok(Ok(RulesetPublish {
            version: RulesetVersion(latest),
            is_new: false,
        }));
    }
    let version = insert_version(&transaction, &members, &content_hash)?;
    transaction.commit()?;
    Ok(Ok(RulesetPublish {
        version,
        is_new: true,
    }))
}

/// Inserts a ruleset version holding `members`, whose content hash is `content_hash`.
fn insert_version(
    connection: &Connection,
    members: &[CheckerId],
    content_hash: &str,
) -> rusqlite::Result<RulesetVersion> {
    connection.execute(
        "INSERT INTO ruleset_versions (published_at, content_hash)
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?1)",
        [content_hash],
    )?;
    let version = RulesetVersion(connection.last_insert_rowid());
    for id in members {
        connection.execute(
            "INSERT INTO ruleset_members (ruleset_version_id, checker_id) VALUES (?1, ?2)",
            (version.0, id.0),
        )?;
    }
    Ok(version)
}

/// The SHA-256 of `checker_hashes` in ascending order, run together. Each is 64 hex digits, so the
/// run needs no separator to be read back unambiguously.
fn ruleset_content_hash(mut checker_hashes: Vec<String>) -> String {
    checker_hashes.sort_unstable();
    sha256_hex(checker_hashes.concat().as_bytes())
}

fn read_latest(connection: &Connection) -> rusqlite::Result<Option<PublishedRuleset>> {
    let latest: Option<i64> =
        connection.query_row("SELECT max(id) FROM ruleset_versions", [], |row| row.get(0))?;
    latest.map_or(Ok(None), |latest| read(connection, RulesetVersion(latest)))
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
    use crate::style::checker::{Band, CheckableMode, Matcher, PhraseEntry, Rule, Unit};
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
            rule: Rule {
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
            },
        }
    }

    fn sentence_length(clause_id: ClauseId) -> Checker {
        Checker {
            clause_id,
            rule: Rule {
                matcher: Matcher::WordCount,
                unit: Unit::Sentence,
                band: Band::Count {
                    minimum: None,
                    maximum: Some(30),
                },
                form_modes: BTreeSet::from([CheckableMode::Prose]),
                register_scope: Some(BTreeSet::from(["email".to_owned()])),
            },
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

    fn publish(store: &Store, checkers: &[(Checker, Provenance)]) -> RulesetPublish {
        store.publish_ruleset_blocking(checkers).unwrap()
    }

    /// Holds that every row `before` held is still in `after`, unchanged and at its place, and
    /// returns how many rows each ruleset table gained.
    fn rows_added(before: &[Vec<Vec<Value>>], after: &[Vec<Vec<Value>>]) -> Vec<usize> {
        for ((table, before), after) in RULESET_TABLES.iter().zip(before).zip(after) {
            assert_eq!(&after[..before.len()], before.as_slice(), "{table}");
        }
        before
            .iter()
            .zip(after)
            .map(|(before, after)| after.len() - before.len())
            .collect()
    }

    /// The checkers a version holds, each as its definition and its provenance.
    fn held(ruleset: &PublishedRuleset) -> Vec<(&Checker, &Provenance)> {
        ruleset
            .checkers
            .iter()
            .map(|stored| (&stored.checker, &stored.provenance))
            .collect()
    }

    #[test]
    fn publishing_a_set_inserts_its_checkers_and_one_version() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        assert_eq!(store.latest_ruleset_version_blocking().unwrap(), None);
        let before = ruleset_rows(&store);
        let hedging = hedging_limit(hedges, 1);
        let length = sentence_length(short);

        let published = publish(
            &store,
            &[(hedging.clone(), drafted()), (length.clone(), templated())],
        );

        assert!(published.is_new);
        let ruleset = store.ruleset_version_blocking(published.version).unwrap();
        assert_eq!(
            held(&ruleset),
            [(&hedging, &drafted()), (&length, &templated())]
        );
        assert_eq!(
            rows_added(&before, &ruleset_rows(&store)),
            [2, 1, 2],
            "rows added to each of {RULESET_TABLES:?}"
        );
        assert_eq!(
            store.latest_ruleset_version_blocking().unwrap(),
            Some(ruleset)
        );
    }

    #[test]
    fn publishing_the_membership_in_force_inserts_nothing_and_returns_the_latest_version() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let hedging = hedging_limit(hedges, 1);
        let length = sentence_length(short);
        let first = publish(
            &store,
            &[(hedging.clone(), drafted()), (length.clone(), templated())],
        );
        let before = ruleset_rows(&store);

        let republished = [
            vec![(hedging.clone(), drafted()), (length.clone(), templated())],
            vec![(length.clone(), templated()), (hedging.clone(), drafted())],
            vec![(hedging.clone(), templated()), (length.clone(), drafted())],
            vec![
                (hedging.clone(), drafted()),
                (length.clone(), templated()),
                (hedging.clone(), drafted()),
            ],
        ];
        for (case, checkers) in ["as published", "reordered", "reattributed", "repeated"]
            .into_iter()
            .zip(republished)
        {
            assert_eq!(
                publish(&store, &checkers),
                RulesetPublish {
                    version: first.version,
                    is_new: false
                },
                "{case}"
            );
        }

        assert_eq!(ruleset_rows(&store), before);
        let ruleset = store.ruleset_version_blocking(first.version).unwrap();
        assert_eq!(
            held(&ruleset),
            [(&hedging, &drafted()), (&length, &templated())]
        );
    }

    #[test]
    fn a_changed_parameter_publishes_a_new_checker_row_and_a_version_listing_it_in_place_of_the_old(
    ) {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let length = sentence_length(short);
        let first = publish(
            &store,
            &[
                (hedging_limit(hedges, 1), drafted()),
                (length.clone(), templated()),
            ],
        );
        let first_read = store.ruleset_version_blocking(first.version).unwrap();
        let before = ruleset_rows(&store);
        let raised = hedging_limit(hedges, 2);

        let second = publish(
            &store,
            &[(raised.clone(), drafted()), (length.clone(), templated())],
        );

        assert!(second.is_new);
        assert_ne!(second.version, first.version);
        assert_eq!(
            rows_added(&before, &ruleset_rows(&store)),
            [1, 1, 2],
            "rows added to each of {RULESET_TABLES:?}"
        );
        let second_read = store.ruleset_version_blocking(second.version).unwrap();
        assert_eq!(
            held(&second_read),
            [(&length, &templated()), (&raised, &drafted())]
        );
        assert_eq!(second_read.checkers[0], first_read.checkers[1]);
        assert_ne!(
            second_read.checkers[1].content_hash,
            first_read.checkers[0].content_hash
        );
        assert_eq!(
            store.ruleset_version_blocking(first.version).unwrap(),
            first_read
        );
        assert_eq!(
            store.latest_ruleset_version_blocking().unwrap(),
            Some(second_read)
        );
    }

    #[test]
    fn retiring_a_checker_publishes_a_version_without_it_and_keeps_its_row_and_every_earlier_version(
    ) {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let length = sentence_length(short);
        let first = publish(
            &store,
            &[
                (hedging_limit(hedges, 1), drafted()),
                (length.clone(), templated()),
            ],
        );
        let second = publish(
            &store,
            &[
                (hedging_limit(hedges, 2), drafted()),
                (length.clone(), templated()),
            ],
        );
        let earlier = [first, second]
            .map(|published| store.ruleset_version_blocking(published.version).unwrap());
        let before = ruleset_rows(&store);

        let retired = publish(&store, &[(length.clone(), templated())]);

        assert!(retired.is_new);
        assert_eq!(
            rows_added(&before, &ruleset_rows(&store)),
            [0, 1, 1],
            "rows added to each of {RULESET_TABLES:?}"
        );
        let retired_read = store.ruleset_version_blocking(retired.version).unwrap();
        assert_eq!(held(&retired_read), [(&length, &templated())]);
        assert_eq!(rows(&store, "checkers").len(), 3);
        for ruleset in earlier {
            assert_eq!(
                store.ruleset_version_blocking(ruleset.version).unwrap(),
                ruleset
            );
        }
    }

    #[test]
    fn a_published_versions_content_hash_is_the_sha256_of_its_members_hashes_in_ascending_order() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        let length = sentence_length(short);
        let mut member_hashes = [
            sha256_hex(HEDGING_LIMIT_JSON.as_bytes()),
            sha256_hex(serde_json::to_string(&length).unwrap().as_bytes()),
        ];
        member_hashes.sort_unstable();

        let published = publish(
            &store,
            &[(length, templated()), (hedging_limit(hedges, 1), drafted())],
        );

        let ruleset = store.ruleset_version_blocking(published.version).unwrap();
        assert_eq!(
            ruleset.content_hash,
            sha256_hex(member_hashes.concat().as_bytes())
        );
    }

    #[test]
    fn the_first_publish_inserts_a_version_even_of_no_checkers_and_a_second_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);

        let first = publish(&store, &[]);
        let second = publish(&store, &[]);

        assert!(first.is_new);
        assert_eq!(
            second,
            RulesetPublish {
                version: first.version,
                is_new: false
            }
        );
        let ruleset = store.latest_ruleset_version_blocking().unwrap().unwrap();
        assert_eq!(ruleset.version, first.version);
        assert!(ruleset.checkers.is_empty());
        assert_eq!(ruleset.content_hash, sha256_hex(b""));
    }

    #[test]
    fn a_publish_naming_a_clause_the_store_does_not_hold_writes_nothing() {
        let root = TempRoot::new();
        let (store, hedges, short) = store_with_clauses(&root);
        publish(&store, &[(hedging_limit(hedges, 1), drafted())]);
        let before = ruleset_rows(&store);
        let absent = ClauseId(short.0 + 1);

        let error = store
            .publish_ruleset_blocking(&[
                (hedging_limit(hedges, 2), drafted()),
                (sentence_length(absent), templated()),
            ])
            .unwrap_err();

        assert!(
            matches!(error, StoreError::NoClause { id } if id == absent),
            "{error:?}"
        );
        assert_eq!(ruleset_rows(&store), before);
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
                rule: Rule {
                    unit: Unit::Sentence,
                    ..original.rule.clone()
                },
                ..original.clone()
            },
            Checker {
                rule: Rule {
                    form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
                    ..original.rule.clone()
                },
                ..original.clone()
            },
            Checker {
                rule: Rule {
                    register_scope: Some(BTreeSet::from(["email".to_owned()])),
                    ..original.rule.clone()
                },
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

    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_publish_and_latest_read_behave_as_the_blocking_ones() {
        let root = TempRoot::new();
        let store = open(&root);
        assert_eq!(store.latest_ruleset_version().await.unwrap(), None);
        let card = store
            .insert_card_version(&[new(HEDGES)], "the first Card")
            .await
            .unwrap();
        let hedges = store.card_version(card).await.unwrap().clauses[0].clause_id;
        let checkers = [(hedging_limit(hedges, 1), drafted())];

        let first = store.publish_ruleset(&checkers).await.unwrap();
        let again = store.publish_ruleset(&checkers).await.unwrap();
        let latest = store.latest_ruleset_version().await.unwrap().unwrap();

        assert!(first.is_new);
        assert_eq!(
            again,
            RulesetPublish {
                version: first.version,
                is_new: false
            }
        );
        assert_eq!(latest.version, first.version);
        assert_eq!(held(&latest), [(&checkers[0].0, &checkers[0].1)]);
    }
}
