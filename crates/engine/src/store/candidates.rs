//! The candidates table: each model rewrite of a passage, stored once per generation with what is
//! needed to reproduce it. The store computes a candidate's content hash over its passage's content
//! hash and its text, so the caller supplies only the passage, the text and the generation
//! provenance. Two generators that produce one text for one passage are two rows sharing one hash,
//! and so one set of screen results. Nothing writes a candidate until Phase 4.
//!
//! A candidate's text is model output over a passage that may be the owner's own writing, so it is
//! treated as the passage is: no error, log line or `Debug` rendering here carries it, and each
//! names an operation, an id or a content hash instead.

use std::fmt;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Map, Value};

use super::error::StoreError;
use super::json::Json;
use super::passages::PassageId;
use super::thread::Store;
use crate::hash::sha256_hex;

const INSERT_CANDIDATE: &str = "insert a candidate";
const READ_CANDIDATE: &str = "read a candidate";

/// A candidate's id in the store, assigned at its insert and never given to another candidate. An
/// `i64` because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CandidateId(pub i64);

/// What a candidate's generation records, so that the generation can be reproduced. It holds no
/// field of a screening, which `screen_results` holds, and no distinct-2 value, which is a function
/// of the text and which the distinctiveness filter computes over the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationProvenance {
    pub generator_id: String,
    /// The SHA-256 of the adapter that produced the candidate, as lowercase hex, and `None` for a
    /// candidate no adapter produced.
    pub adapter_hash: Option<String>,
    pub decode_parameters: Map<String, Value>,
    /// An `i64` because SQLite's integers are.
    pub seed: i64,
    /// The candidate's place among those its generator produced for the passage in one request.
    pub generation_index: u32,
    /// 0 for a first attempt, and one more for each repair attempt it descends from.
    pub repair_depth: u32,
}

/// A candidate as its row holds it.
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    pub passage: PassageId,
    pub text: String,
    /// See [`Store::insert_candidate`] for the recipe.
    pub content_hash: String,
    pub generation: GenerationProvenance,
}

/// Shows the text's length in bytes in place of the text, since a `Debug` rendering ends up in log
/// lines and test failure messages. The content hash identifies the candidate's text.
impl fmt::Debug for Candidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Candidate")
            .field("passage", &self.passage)
            .field("text_bytes", &self.text.len())
            .field("content_hash", &self.content_hash)
            .field("generation", &self.generation)
            .finish()
    }
}

impl Store {
    /// Inserts a candidate rewriting the passage stored under `passage`, holding `text` and
    /// produced as `generation` records, and returns its id. Every insert adds a row, so two
    /// generations of one text for one passage are two candidates.
    ///
    /// The store computes the content hash, and the caller cannot supply it: the SHA-256, as
    /// lowercase hex, of the passage's content hash, its 64 hex digits as bytes, followed by the
    /// candidate's text as UTF-8. The passage's hash has a fixed length, so the two need no
    /// separator.
    ///
    /// # Errors
    /// [`StoreError::NoPassage`] when the store holds no passage under `passage`, inserting
    /// nothing. [`StoreError::Busy`] when another connection holds the write lock past the busy
    /// timeout, [`StoreError::Request`] for any other failure of the insert, and
    /// [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn insert_candidate(
        &self,
        passage: PassageId,
        text: &str,
        generation: &GenerationProvenance,
    ) -> Result<CandidateId, StoreError> {
        self.request(INSERT_CANDIDATE, insert_work(passage, text, generation))
            .await?
    }

    /// [`insert_candidate`](Self::insert_candidate) for a synchronous caller, such as `wa`, which
    /// blocks on the reply.
    ///
    /// # Errors
    /// As [`insert_candidate`](Self::insert_candidate).
    ///
    /// # Panics
    /// When called from async code, which awaits [`insert_candidate`](Self::insert_candidate)
    /// instead.
    pub fn insert_candidate_blocking(
        &self,
        passage: PassageId,
        text: &str,
        generation: &GenerationProvenance,
    ) -> Result<CandidateId, StoreError> {
        self.request_blocking(INSERT_CANDIDATE, insert_work(passage, text, generation))?
    }

    /// The candidate stored under `id`.
    ///
    /// # Errors
    /// [`StoreError::NoCandidate`] when the store holds no candidate under `id`,
    /// [`StoreError::Busy`] when another connection holds a lock the read needs,
    /// [`StoreError::Request`] for any other failure of the read, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub async fn candidate(&self, id: CandidateId) -> Result<Candidate, StoreError> {
        self.request(READ_CANDIDATE, move |connection| read(connection, id))
            .await?
            .ok_or(StoreError::NoCandidate { id })
    }

    /// [`candidate`](Self::candidate) for a synchronous caller, such as `wa`, which blocks on the
    /// reply.
    ///
    /// # Errors
    /// As [`candidate`](Self::candidate).
    ///
    /// # Panics
    /// When called from async code, which awaits [`candidate`](Self::candidate) instead.
    pub fn candidate_blocking(&self, id: CandidateId) -> Result<Candidate, StoreError> {
        self.request_blocking(READ_CANDIDATE, move |connection| read(connection, id))?
            .ok_or(StoreError::NoCandidate { id })
    }
}

/// The recipe [`Store::insert_candidate`] states.
fn candidate_content_hash(passage_hash: &str, text: &str) -> String {
    sha256_hex(&[passage_hash.as_bytes(), text.as_bytes()].concat())
}

/// The work of one insert. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert_work(
    passage: PassageId,
    text: &str,
    generation: &GenerationProvenance,
) -> impl FnOnce(&mut Connection) -> rusqlite::Result<Result<CandidateId, StoreError>> + Send + 'static
{
    let text = text.to_owned();
    let generation = generation.clone();
    move |connection| insert(connection, passage, &text, &generation)
}

/// Begins `IMMEDIATE`, since the passage's lookup comes before the write, and a deferred
/// transaction that reads first fails at its first write, without waiting, while another
/// connection holds the write lock.
fn insert(
    connection: &mut Connection,
    passage: PassageId,
    text: &str,
    generation: &GenerationProvenance,
) -> rusqlite::Result<Result<CandidateId, StoreError>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let passage_hash: Option<String> = transaction
        .query_row(
            "SELECT content_hash FROM passages WHERE id = ?1",
            [passage.0],
            |row| row.get(0),
        )
        .optional()?;
    let Some(passage_hash) = passage_hash else {
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        return Ok(Err(StoreError::NoPassage { id: passage }));
    };
    transaction.execute(
        "INSERT INTO candidates (passage_id, text, content_hash, generator_id, adapter_hash,
                                 decode_parameters, seed, generation_index, repair_depth)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        (
            passage.0,
            text,
            candidate_content_hash(&passage_hash, text),
            &generation.generator_id,
            &generation.adapter_hash,
            Json(&generation.decode_parameters),
            generation.seed,
            generation.generation_index,
            generation.repair_depth,
        ),
    )?;
    let id = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(Ok(CandidateId(id)))
}

fn read(connection: &Connection, id: CandidateId) -> rusqlite::Result<Option<Candidate>> {
    connection
        .query_row(
            "SELECT passage_id, text, content_hash, generator_id, adapter_hash, decode_parameters,
                    seed, generation_index, repair_depth
             FROM candidates WHERE id = ?1",
            [id.0],
            |row| {
                let Json(decode_parameters) = row.get(5)?;
                Ok(Candidate {
                    passage: PassageId(row.get(0)?),
                    text: row.get(1)?,
                    content_hash: row.get(2)?,
                    generation: GenerationProvenance {
                        generator_id: row.get(3)?,
                        adapter_hash: row.get(4)?,
                        decode_parameters,
                        seed: row.get(6)?,
                        generation_index: row.get(7)?,
                        repair_depth: row.get(8)?,
                    },
                })
            },
        )
        .optional()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::store::card::tests::{assert_insert_only, open, rows};
    use crate::store::passages::PassageKind;
    use crate::test_support::TempRoot;
    use serde_json::json;

    /// The passages module's fixed text, whose SHA-256 is the published example below.
    pub(in crate::store) const PASSAGE_TEXT: &str = "The quick brown fox jumps over the lazy dog.";
    const PASSAGE_HASH: &str = "ef537f25c895bfa782526529a9b63d97aa631564d5d789c2b765448c8635fb6c";
    pub(in crate::store) const OTHER_PASSAGE_TEXT: &str = "A second passage, rewritten alike.";

    /// A fixed candidate text, and the SHA-256 of `PASSAGE_HASH` followed by it, computed outside
    /// the engine.
    pub(in crate::store) const TEXT: &str = "A quick brown fox jumped over a lazy dog.";
    const CANDIDATE_HASH: &str = "8b6ae185a6abfbc538b4017c083e28a32101aed0123e6a6cf107146306ca9ea3";
    const OTHER_TEXT: &str = "A quick brown fox leapt over a lazy dog.";

    /// The columns a candidate's row holds: its passage, its text and hash, and generation
    /// provenance, and nothing of a screening.
    const COLUMNS: [&str; 10] = [
        "id",
        "passage_id",
        "text",
        "content_hash",
        "generator_id",
        "adapter_hash",
        "decode_parameters",
        "seed",
        "generation_index",
        "repair_depth",
    ];

    pub(in crate::store) fn local() -> GenerationProvenance {
        GenerationProvenance {
            generator_id: "local-student".to_owned(),
            adapter_hash: Some(sha256_hex(b"an adapter")),
            decode_parameters: json!({ "temperature": 0.7, "topP": 0.9, "maxTokens": 256 })
                .as_object()
                .cloned()
                .unwrap(),
            seed: 42,
            generation_index: 1,
            repair_depth: 0,
        }
    }

    pub(in crate::store) fn teacher() -> GenerationProvenance {
        GenerationProvenance {
            generator_id: "frontier-teacher".to_owned(),
            adapter_hash: None,
            decode_parameters: Map::new(),
            seed: -7,
            generation_index: 3,
            repair_depth: 1,
        }
    }

    pub(in crate::store) fn insert_passage(store: &Store, text: &str) -> PassageId {
        store
            .insert_passage_blocking(PassageKind::Captured, text, None)
            .unwrap()
    }

    pub(in crate::store) fn insert(
        store: &Store,
        passage: PassageId,
        text: &str,
        generation: &GenerationProvenance,
    ) -> CandidateId {
        store
            .insert_candidate_blocking(passage, text, generation)
            .unwrap()
    }

    fn columns(store: &Store) -> Vec<String> {
        store
            .request_blocking("read the candidates table's columns", |connection| {
                connection
                    .prepare("SELECT name FROM pragma_table_info('candidates') ORDER BY cid")?
                    .query_map([], |row| row.get(0))?
                    .collect()
            })
            .unwrap()
    }

    #[test]
    fn a_candidate_reads_back_whole_and_its_row_holds_no_field_of_a_screening() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);

        let local_id = insert(&store, passage, TEXT, &local());
        let teacher_id = insert(&store, passage, OTHER_TEXT, &teacher());

        assert_eq!(
            store.candidate_blocking(local_id).unwrap(),
            Candidate {
                passage,
                text: TEXT.to_owned(),
                content_hash: CANDIDATE_HASH.to_owned(),
                generation: local(),
            }
        );
        assert_eq!(
            store.candidate_blocking(teacher_id).unwrap(),
            Candidate {
                passage,
                text: OTHER_TEXT.to_owned(),
                content_hash: candidate_content_hash(PASSAGE_HASH, OTHER_TEXT),
                generation: teacher(),
            }
        );
        assert_eq!(columns(&store), COLUMNS);
    }

    /// The expected digest is written out, so a change to the recipe or to the hash fails here.
    #[test]
    fn a_candidates_hash_is_the_sha256_of_its_passages_hash_followed_by_its_text() {
        assert_eq!(candidate_content_hash(PASSAGE_HASH, TEXT), CANDIDATE_HASH);

        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        assert_eq!(
            store.passage_blocking(passage).unwrap().content_hash,
            PASSAGE_HASH
        );

        let id = insert(&store, passage, TEXT, &local());

        assert_eq!(
            store.candidate_blocking(id).unwrap().content_hash,
            CANDIDATE_HASH
        );
    }

    /// The insert takes no hash, so what the store records follows the passage and the text alone,
    /// however the provenance differs.
    #[test]
    fn the_store_computes_the_hash_from_the_passage_and_text_and_nothing_the_caller_supplies() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);

        let hashes: Vec<String> = [local(), teacher()]
            .iter()
            .map(|generation| {
                let id = insert(&store, passage, TEXT, generation);
                store.candidate_blocking(id).unwrap().content_hash
            })
            .collect();

        assert_eq!(hashes, [CANDIDATE_HASH, CANDIDATE_HASH]);
    }

    #[test]
    fn a_candidate_naming_a_passage_the_store_does_not_hold_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let absent = PassageId(passage.0 + 1);

        let error = store
            .insert_candidate_blocking(absent, TEXT, &local())
            .unwrap_err();

        assert!(
            matches!(error, StoreError::NoPassage { id } if id == absent),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no passage with id 2");
        assert!(rows(&store, "candidates").is_empty());
    }

    #[test]
    fn the_table_refuses_a_candidate_whose_passage_is_absent_or_whose_parameters_are_no_object() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let statement = "INSERT INTO candidates (passage_id, text, content_hash, generator_id,
                                                 decode_parameters, seed, generation_index,
                                                 repair_depth)
                         VALUES (?1, 'text', 'hash', 'generator', ?2, 0, 0, 0)";

        for (case, passage_id, parameters, constraint) in [
            (
                "an absent passage",
                passage.0 + 1,
                "{}",
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            ),
            (
                "an array of parameters",
                passage.0,
                "[]",
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
            ),
            (
                "parameters that are not JSON",
                passage.0,
                "temperature",
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
            ),
        ] {
            let result = store.request_blocking("insert a candidate directly", move |connection| {
                connection.execute(statement, (passage_id, parameters))
            });
            match result {
                Err(StoreError::Request {
                    source: rusqlite::Error::SqliteFailure(failure, _),
                    ..
                }) => assert_eq!(failure.extended_code, constraint, "{case}"),
                other => panic!("{case}: expected a constraint to refuse it, got {other:?}"),
            }
        }
        assert!(rows(&store, "candidates").is_empty());
    }

    #[test]
    fn reading_an_id_the_store_does_not_hold_returns_no_candidate() {
        let root = TempRoot::new();
        let store = open(&root);

        let error = store.candidate_blocking(CandidateId(1)).unwrap_err();

        assert!(
            matches!(error, StoreError::NoCandidate { id: CandidateId(1) }),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no candidate with id 1");
    }

    #[test]
    fn updating_or_deleting_a_candidate_raises_and_leaves_it_as_it_was() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let id = insert(&store, passage, TEXT, &local());
        let stored = store.candidate_blocking(id).unwrap();

        assert_insert_only(&store, "candidates");

        assert_eq!(store.candidate_blocking(id).unwrap(), stored);
    }

    #[test]
    fn a_candidates_debug_rendering_and_a_failed_insert_or_read_never_carry_its_text() {
        let root = TempRoot::new();
        let store = open(&root);
        let passage = insert_passage(&store, PASSAGE_TEXT);
        let id = insert(&store, passage, TEXT, &local());
        let rendering = format!("{:?}", store.candidate_blocking(id).unwrap());
        store
            .request_blocking("drop the candidates table", |connection| {
                connection.execute_batch("DROP TABLE candidates")
            })
            .unwrap();

        let insert_error = store
            .insert_candidate_blocking(passage, TEXT, &local())
            .unwrap_err();
        let read_error = store.candidate_blocking(id).unwrap_err();

        assert!(
            !rendering.contains(TEXT),
            "candidate {CANDIDATE_HASH}'s Debug carries its text"
        );
        assert!(rendering.contains(CANDIDATE_HASH), "candidate {id:?}");
        // Checked before either error is printed in a failure message below.
        for error in [&insert_error, &read_error] {
            for rendering in [error.to_string(), format!("{error:?}")] {
                assert!(
                    !rendering.contains(TEXT),
                    "an error for candidate {CANDIDATE_HASH} carries its text"
                );
            }
        }
        assert!(
            matches!(
                insert_error,
                StoreError::Request {
                    operation: INSERT_CANDIDATE,
                    ..
                }
            ),
            "{insert_error:?}"
        );
        assert!(
            matches!(
                read_error,
                StoreError::Request {
                    operation: READ_CANDIDATE,
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
        let passage = store
            .insert_passage(PassageKind::Captured, PASSAGE_TEXT, None)
            .await
            .unwrap();

        let id = store
            .insert_candidate(passage, TEXT, &local())
            .await
            .unwrap();
        let candidate = store.candidate(id).await.unwrap();
        let refused = store
            .insert_candidate(PassageId(passage.0 + 1), TEXT, &local())
            .await;
        let absent = store.candidate(CandidateId(id.0 + 1)).await;

        assert_eq!(candidate.content_hash, CANDIDATE_HASH);
        assert_eq!(candidate.generation, local());
        assert!(
            matches!(refused, Err(StoreError::NoPassage { .. })),
            "{refused:?}"
        );
        assert!(
            matches!(absent, Err(StoreError::NoCandidate { .. })),
            "{absent:?}"
        );
    }
}
