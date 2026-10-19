//! The items tables: each item is the record of what was shown to the owner, a passage with a
//! candidate in each of its two slots, under the Card version and the ruleset version in force. The
//! store derives an item's never-export flag at insert from the item's type and its passage's split,
//! so the caller cannot supply the flag, and the triggers keep it from ever being cleared. The store
//! also takes each slot's generator id from the slot's candidate. Nothing writes an item until
//! Phase 5, whose clause-audit sittings are the first writer; an item's sitting and its position in
//! it arrive with the sittings table there.
//!
//! An item holds ids, names and a flag, and no text: the passage's text and the candidates' stay in
//! their own tables, so nothing here needs a `Debug` rendering that hides text.

use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::candidates::CandidateId;
use super::card::{clause_exists, CardVersionId};
use super::error::StoreError;
use super::passages::{from_column, PassageId};
use super::split::Split;
use super::thread::Store;
use crate::flag::{ClauseId, RulesetVersion};

const INSERT_ITEM: &str = "insert an item";
const READ_ITEM: &str = "read an item";

/// The number of slots an item holds, at positions 0 and 1.
pub(super) const SLOTS: usize = 2;

/// An item's id in the store, assigned at its insert and never given to another item. An `i64`
/// because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemId(pub i64);

/// What an item is for. A fresh item is a pair the owner has not judged before. The other three
/// measure: an anchor, with one candidate corrupted so that the clean side is right by
/// construction, the owner's validity; a repeat, which shows an earlier item again, the owner's
/// reliability; and a clause audit the judge's calibration on one residual clause. The owner is
/// never told which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    FreshCore,
    FreshFringe,
    Anchor,
    Repeat,
    ClauseAudit,
}

/// How an item's two candidates differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Contrast {
    /// No difference was constructed between them.
    None,
    /// One is a clean candidate and the other the same candidate with this residual clause's twin
    /// written into it, so the item tests that one clause.
    Minimal(ClauseId),
}

/// An item to insert. It has no never-export flag, which the store derives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    pub item_type: ItemType,
    pub passage: PassageId,
    /// A register name from the settings' register map.
    pub register: String,
    /// The candidate shown in each slot, in slot order.
    pub slots: Vec<CandidateId>,
    pub card_version: CardVersionId,
    pub ruleset_version: RulesetVersion,
    pub contrast: Contrast,
}

/// One slot of a stored item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSlot {
    pub candidate: CandidateId,
    /// The generator id the candidate's row holds.
    pub generator_id: String,
}

/// An item as its rows hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub item_type: ItemType,
    pub passage: PassageId,
    pub register: String,
    /// In slot order.
    pub slots: Vec<ItemSlot>,
    pub card_version: CardVersionId,
    pub ruleset_version: RulesetVersion,
    pub contrast: Contrast,
    /// Set when the item must never reach a training file. See [`Store::insert_item`] for the rule.
    pub never_export: bool,
}

impl Store {
    /// Inserts `item` and returns its id.
    ///
    /// The store derives the never-export flag, and the caller cannot supply it. The flag is set
    /// for every item on a bench, gate or sealed passage, whatever its type, and for every anchor,
    /// repeat and clause audit, whatever its passage's split. A fresh item on a train or monitor
    /// passage is left without it: monitor passages never train, and the exporter filters by split
    /// as well as by the flag. Each slot's generator id is the one its candidate's row holds.
    ///
    /// Everything happens in one transaction, and every id is checked against the store before
    /// anything is written, so a refused insert writes nothing.
    ///
    /// # Errors
    /// [`StoreError::SlotCount`] when `item` lists other than two slots, and
    /// [`StoreError::NoPassage`], [`StoreError::NoCardVersion`],
    /// [`StoreError::NoRulesetVersion`], [`StoreError::NoClause`] or [`StoreError::NoCandidate`]
    /// for an id the store does not hold, each inserting nothing. [`StoreError::Busy`] when another
    /// connection holds the write lock past the busy timeout, [`StoreError::Request`] for any other
    /// failure of the insert, and [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn insert_item(&self, item: &NewItem) -> Result<ItemId, StoreError> {
        let item = item.clone();
        self.request(INSERT_ITEM, move |connection| insert(connection, &item))
            .await?
    }

    /// [`insert_item`](Self::insert_item) for a synchronous caller, such as `wa`, which blocks on
    /// the reply.
    ///
    /// # Errors
    /// As [`insert_item`](Self::insert_item).
    ///
    /// # Panics
    /// When called from async code, which awaits [`insert_item`](Self::insert_item) instead.
    pub fn insert_item_blocking(&self, item: &NewItem) -> Result<ItemId, StoreError> {
        let item = item.clone();
        self.request_blocking(INSERT_ITEM, move |connection| insert(connection, &item))?
    }

    /// The item stored under `id`, with its slots in order.
    ///
    /// # Errors
    /// [`StoreError::NoItem`] when the store holds no item under `id`, [`StoreError::Busy`] when
    /// another connection holds a lock the read needs, [`StoreError::Request`] for any other
    /// failure of the read, and [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn item(&self, id: ItemId) -> Result<Item, StoreError> {
        self.request(READ_ITEM, move |connection| read(connection, id))
            .await?
            .ok_or(StoreError::NoItem { id })
    }

    /// [`item`](Self::item) for a synchronous caller, such as `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`item`](Self::item).
    ///
    /// # Panics
    /// When called from async code, which awaits [`item`](Self::item) instead.
    pub fn item_blocking(&self, id: ItemId) -> Result<Item, StoreError> {
        self.request_blocking(READ_ITEM, move |connection| read(connection, id))?
            .ok_or(StoreError::NoItem { id })
    }
}

/// Whether an item of `item_type` on a passage in `split` must never reach a training file. Both
/// matches name every variant, so a new split or item type cannot reach an insert unclassified.
fn never_export(item_type: ItemType, split: Split) -> bool {
    let held_out = match split {
        Split::Bench | Split::Gate | Split::Sealed => true,
        Split::Train | Split::Monitor => false,
    };
    let measures = match item_type {
        ItemType::Anchor | ItemType::Repeat | ItemType::ClauseAudit => true,
        ItemType::FreshCore | ItemType::FreshFringe => false,
    };
    held_out || measures
}

/// Begins `IMMEDIATE`, since the lookups come before the writes, and a deferred transaction that
/// reads first fails at its first write, without waiting, while another connection holds the write
/// lock. Its outer result is SQLite's, and its inner one is the store's refusal.
fn insert(
    connection: &mut Connection,
    item: &NewItem,
) -> rusqlite::Result<Result<ItemId, StoreError>> {
    if item.slots.len() != SLOTS {
        return Ok(Err(StoreError::SlotCount {
            count: item.slots.len(),
        }));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let split: Option<Split> = transaction
        .query_row(
            "SELECT split FROM passages WHERE id = ?1",
            [item.passage.0],
            |row| row.get(0),
        )
        .optional()?;
    let Some(split) = split else {
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        return Ok(Err(StoreError::NoPassage { id: item.passage }));
    };
    if !exists(
        &transaction,
        "SELECT EXISTS (SELECT 1 FROM card_versions WHERE id = ?1)",
        item.card_version.0,
    )? {
        return Ok(Err(StoreError::NoCardVersion {
            id: item.card_version,
        }));
    }
    if !exists(
        &transaction,
        "SELECT EXISTS (SELECT 1 FROM ruleset_versions WHERE id = ?1)",
        item.ruleset_version.0,
    )? {
        return Ok(Err(StoreError::NoRulesetVersion {
            version: item.ruleset_version,
        }));
    }
    if let Contrast::Minimal(clause) = item.contrast {
        if !clause_exists(&transaction, clause)? {
            return Ok(Err(StoreError::NoClause { id: clause }));
        }
    }
    let mut generator_ids = Vec::with_capacity(SLOTS);
    for &candidate in &item.slots {
        let generator_id: Option<String> = transaction
            .query_row(
                "SELECT generator_id FROM candidates WHERE id = ?1",
                [candidate.0],
                |row| row.get(0),
            )
            .optional()?;
        match generator_id {
            Some(generator_id) => generator_ids.push(generator_id),
            None => return Ok(Err(StoreError::NoCandidate { id: candidate })),
        }
    }
    transaction.execute(
        "INSERT INTO items (item_type, passage_id, register, card_version_id, ruleset_version_id,
                            contrast, contrast_clause_id, never_export)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        (
            item.item_type,
            item.passage.0,
            &item.register,
            item.card_version.0,
            item.ruleset_version.0,
            item.contrast,
            item.contrast.clause().map(|clause| clause.0),
            never_export(item.item_type, split),
        ),
    )?;
    let id = ItemId(transaction.last_insert_rowid());
    for (position, (candidate, generator_id)) in (0_u8..).zip(item.slots.iter().zip(&generator_ids))
    {
        transaction.execute(
            "INSERT INTO item_slots (item_id, position, candidate_id, generator_id)
             VALUES (?1, ?2, ?3, ?4)",
            (id.0, position, candidate.0, generator_id),
        )?;
    }
    transaction.commit()?;
    Ok(Ok(id))
}

/// Whether `statement`, an `EXISTS` query over one id, finds its row.
pub(super) fn exists(connection: &Connection, statement: &str, id: i64) -> rusqlite::Result<bool> {
    connection.query_row(statement, [id], |row| row.get(0))
}

/// Reads the item's row before its slots, which were committed with it, so a row that is there
/// has its slots there too.
fn read(connection: &Connection, id: ItemId) -> rusqlite::Result<Option<Item>> {
    let Some(mut item) = connection
        .query_row(
            "SELECT item_type, passage_id, register, card_version_id, ruleset_version_id,
                    contrast_clause_id, never_export
             FROM items WHERE id = ?1",
            [id.0],
            |row| {
                let contrast_clause: Option<i64> = row.get(5)?;
                Ok(Item {
                    item_type: row.get(0)?,
                    passage: PassageId(row.get(1)?),
                    register: row.get(2)?,
                    slots: Vec::with_capacity(SLOTS),
                    card_version: CardVersionId(row.get(3)?),
                    ruleset_version: RulesetVersion(row.get(4)?),
                    // The table's CHECK holds the clause present exactly when the contrast is
                    // minimal, so the clause alone gives the contrast.
                    contrast: contrast_clause
                        .map_or(Contrast::None, |clause| Contrast::Minimal(ClauseId(clause))),
                    never_export: row.get(6)?,
                })
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    item.slots = connection
        .prepare(
            "SELECT candidate_id, generator_id FROM item_slots WHERE item_id = ?1
             ORDER BY position",
        )?
        .query_map([id.0], |row| {
            Ok(ItemSlot {
                candidate: CandidateId(row.get(0)?),
                generator_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(item))
}

impl ItemType {
    const ALL: [Self; 5] = [
        Self::FreshCore,
        Self::FreshFringe,
        Self::Anchor,
        Self::Repeat,
        Self::ClauseAudit,
    ];

    /// The type as the `item_type` column holds it, one of the names its CHECK constraint allows.
    fn column_value(self) -> &'static str {
        match self {
            Self::FreshCore => "fresh_core",
            Self::FreshFringe => "fresh_fringe",
            Self::Anchor => "anchor",
            Self::Repeat => "repeat",
            Self::ClauseAudit => "clause_audit",
        }
    }
}

impl Contrast {
    /// The contrast as the `contrast` column holds it, one of the names its CHECK constraint
    /// allows.
    fn column_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal(_) => "minimal",
        }
    }

    /// The clause the `contrast_clause_id` column holds.
    fn clause(self) -> Option<ClauseId> {
        match self {
            Self::None => None,
            Self::Minimal(clause) => Some(clause),
        }
    }
}

impl ToSql for ItemType {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.column_value().into())
    }
}

impl FromSql for ItemType {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        from_column(value, &Self::ALL, Self::column_value)
    }
}

impl ToSql for Contrast {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.column_value().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_hex;
    use crate::store::candidates::tests::{local, teacher, PASSAGE_TEXT, TEXT};
    use crate::store::card::tests::{assert_insert_only, new, open, refusal, rows, HEDGES, NOTE};
    use crate::store::passages::PassageKind;
    use crate::store::split::split_for;
    use crate::test_support::TempRoot;
    use rusqlite::types::Value;
    use ItemType::{Anchor, ClauseAudit, FreshCore, FreshFringe, Repeat};
    use Split::{Bench, Gate, Monitor, Sealed, Train};

    const REGISTER: &str = "default";

    /// An item row written past the store's insert, for the tests of what the table refuses.
    const ITEM_ROW: &str = "INSERT INTO items (item_type, passage_id, register, card_version_id,
                                               ruleset_version_id, contrast, contrast_clause_id,
                                               never_export)
                            VALUES (?1, ?2, 'default', ?3, ?4, ?5, ?6, 0)";

    /// The Card version and the ruleset version items are built under, and the Card's one clause.
    struct Versions {
        card: CardVersionId,
        clause: ClauseId,
        ruleset: RulesetVersion,
    }

    fn versions(store: &Store) -> Versions {
        let card = store
            .insert_card_version_blocking(&[new(HEDGES)], NOTE)
            .unwrap();
        let clause = store.card_version_blocking(card).unwrap().clauses[0].clause_id;
        let ruleset = store.insert_ruleset_version_blocking(&[]).unwrap();
        Versions {
            card,
            clause,
            ruleset,
        }
    }

    /// Inserts a captured passage whose content hash puts it in `split` and returns its id. Every
    /// call for one split inserts one text, and so returns one passage.
    fn passage_in(store: &Store, split: Split) -> PassageId {
        let text = (0..)
            .map(|n| format!("a passage an item shows, number {n}"))
            .find(|text| split_for(&sha256_hex(text.as_bytes()), PassageKind::Captured) == split)
            .expect("some n lands in each of the captured kind's five splits");
        let id = store
            .insert_passage_blocking(PassageKind::Captured, &text, None)
            .unwrap();
        assert_eq!(store.passage_blocking(id).unwrap().split, split);
        id
    }

    /// Two candidates rewriting `passage` with one text, the first from the local generator and the
    /// second from the teacher.
    fn candidates_for(store: &Store, passage: PassageId) -> Vec<CandidateId> {
        [local(), teacher()]
            .iter()
            .map(|generation| {
                store
                    .insert_candidate_blocking(passage, TEXT, generation)
                    .unwrap()
            })
            .collect()
    }

    fn new_item(
        item_type: ItemType,
        passage: PassageId,
        slots: Vec<CandidateId>,
        versions: &Versions,
    ) -> NewItem {
        NewItem {
            item_type,
            passage,
            register: REGISTER.to_owned(),
            slots,
            card_version: versions.card,
            ruleset_version: versions.ruleset,
            contrast: Contrast::None,
        }
    }

    /// Inserts an item of `item_type` on a passage in `split` and returns its stored flag.
    fn flag_for(store: &Store, versions: &Versions, item_type: ItemType, split: Split) -> bool {
        let passage = passage_in(store, split);
        let slots = candidates_for(store, passage);
        let id = store
            .insert_item_blocking(&new_item(item_type, passage, slots, versions))
            .unwrap();
        store.item_blocking(id).unwrap().never_export
    }

    fn assert_nothing_written(store: &Store) {
        assert!(rows(store, "items").is_empty());
        assert!(rows(store, "item_slots").is_empty());
    }

    #[test]
    fn an_item_reads_back_whole() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);
        let local_slot = ItemSlot {
            candidate: slots[0],
            generator_id: local().generator_id,
        };
        let teacher_slot = ItemSlot {
            candidate: slots[1],
            generator_id: teacher().generator_id,
        };
        let minimal = NewItem {
            register: "email".to_owned(),
            contrast: Contrast::Minimal(versions.clause),
            ..new_item(FreshFringe, passage, vec![slots[1], slots[0]], &versions)
        };

        let plain_id = store
            .insert_item_blocking(&new_item(FreshCore, passage, slots.clone(), &versions))
            .unwrap();
        let minimal_id = store.insert_item_blocking(&minimal).unwrap();

        assert_eq!(
            store.item_blocking(plain_id).unwrap(),
            Item {
                item_type: FreshCore,
                passage,
                register: REGISTER.to_owned(),
                slots: vec![local_slot.clone(), teacher_slot.clone()],
                card_version: versions.card,
                ruleset_version: versions.ruleset,
                contrast: Contrast::None,
                never_export: false,
            }
        );
        assert_eq!(
            store.item_blocking(minimal_id).unwrap(),
            Item {
                item_type: FreshFringe,
                passage,
                register: "email".to_owned(),
                slots: vec![teacher_slot, local_slot],
                card_version: versions.card,
                ruleset_version: versions.ruleset,
                contrast: Contrast::Minimal(versions.clause),
                never_export: false,
            }
        );
        let positions: Vec<(Value, Value)> = rows(&store, "item_slots")
            .into_iter()
            .map(|row| (row[1].clone(), row[2].clone()))
            .collect();
        assert_eq!(
            positions,
            [
                (Value::Integer(plain_id.0), Value::Integer(0)),
                (Value::Integer(plain_id.0), Value::Integer(1)),
                (Value::Integer(minimal_id.0), Value::Integer(0)),
                (Value::Integer(minimal_id.0), Value::Integer(1)),
            ]
        );
    }

    #[test]
    fn the_five_item_types_insert_and_the_table_refuses_a_sixth() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);

        for item_type in [FreshCore, FreshFringe, Anchor, Repeat, ClauseAudit] {
            let id = store
                .insert_item_blocking(&new_item(item_type, passage, slots.clone(), &versions))
                .unwrap();
            assert_eq!(store.item_blocking(id).unwrap().item_type, item_type);
        }
        let sixth = refusal(
            &store,
            ITEM_ROW,
            vec![
                Value::Text("transitivity".to_owned()),
                Value::Integer(passage.0),
                Value::Integer(versions.card.0),
                Value::Integer(versions.ruleset.0),
                Value::Text("none".to_owned()),
                Value::Null,
            ],
        );

        assert_eq!(
            ItemType::ALL,
            [FreshCore, FreshFringe, Anchor, Repeat, ClauseAudit]
        );
        assert_eq!(sixth, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_eq!(rows(&store, "items").len(), 5);
    }

    #[test]
    fn an_item_on_a_bench_gate_or_sealed_passage_is_flagged_whatever_its_type() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);

        for split in [Bench, Gate, Sealed] {
            for item_type in ItemType::ALL {
                assert!(
                    flag_for(&store, &versions, item_type, split),
                    "{item_type:?} on a {split:?} passage"
                );
            }
        }
    }

    #[test]
    fn an_anchor_repeat_or_clause_audit_is_flagged_on_a_train_or_monitor_passage() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);

        for split in [Train, Monitor] {
            for item_type in [Anchor, Repeat, ClauseAudit] {
                assert!(
                    flag_for(&store, &versions, item_type, split),
                    "{item_type:?} on a {split:?} passage"
                );
            }
        }
    }

    #[test]
    fn a_fresh_item_on_a_train_or_monitor_passage_is_not_flagged() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);

        for split in [Train, Monitor] {
            for item_type in [FreshCore, FreshFringe] {
                assert!(
                    !flag_for(&store, &versions, item_type, split),
                    "{item_type:?} on a {split:?} passage"
                );
            }
        }
    }

    /// The literal names every field of the input, with no `..`, so an input that gained a flag
    /// field would fail to compile here. Two inputs alike but for their passage then get two
    /// flags, which follow the passages' splits.
    #[test]
    fn the_caller_cannot_supply_the_flag_and_the_store_derives_it_from_the_split() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);

        let flags: Vec<bool> = [Train, Bench]
            .into_iter()
            .map(|split| {
                let passage = passage_in(&store, split);
                let item = NewItem {
                    item_type: FreshCore,
                    passage,
                    register: REGISTER.to_owned(),
                    slots: candidates_for(&store, passage),
                    card_version: versions.card,
                    ruleset_version: versions.ruleset,
                    contrast: Contrast::None,
                };
                let id = store.insert_item_blocking(&item).unwrap();
                store.item_blocking(id).unwrap().never_export
            })
            .collect();

        assert_eq!(flags, [false, true]);
    }

    #[test]
    fn clearing_a_set_flag_raises_and_leaves_it_set() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);
        let id = store
            .insert_item_blocking(&new_item(Anchor, passage, slots, &versions))
            .unwrap();
        assert!(store.item_blocking(id).unwrap().never_export);

        let cleared =
            store.request_blocking("clear an item's never-export flag", move |connection| {
                connection.execute("UPDATE items SET never_export = 0 WHERE id = ?1", [id.0])
            });

        match cleared {
            Err(StoreError::Request {
                source: rusqlite::Error::SqliteFailure(failure, Some(message)),
                ..
            }) => {
                assert_eq!(
                    failure.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
                );
                assert!(message.starts_with("items is insert-only"), "{message}");
            }
            other => panic!("expected the trigger to raise, got {other:?}"),
        }
        assert!(store.item_blocking(id).unwrap().never_export);
    }

    #[test]
    fn the_table_refuses_a_minimal_contrast_without_a_clause_and_none_with_one() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let row = |contrast: &str, clause: Value| {
            vec![
                Value::Text("fresh_core".to_owned()),
                Value::Integer(passage.0),
                Value::Integer(versions.card.0),
                Value::Integer(versions.ruleset.0),
                Value::Text(contrast.to_owned()),
                clause,
            ]
        };

        let minimal_without = refusal(&store, ITEM_ROW, row("minimal", Value::Null));
        let none_with = refusal(
            &store,
            ITEM_ROW,
            row("none", Value::Integer(versions.clause.0)),
        );

        assert_eq!(minimal_without, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_eq!(none_with, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_nothing_written(&store);
    }

    #[test]
    fn an_id_the_store_does_not_hold_is_refused_by_name_and_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);
        let valid = new_item(FreshCore, passage, slots.clone(), &versions);
        let absent_passage = PassageId(passage.0 + 1);
        let absent_card = CardVersionId(versions.card.0 + 1);
        let absent_ruleset = RulesetVersion(versions.ruleset.0 + 1);
        let absent_clause = ClauseId(versions.clause.0 + 1);
        let absent_candidate = CandidateId(slots[1].0 + 1);
        let refused = |item: NewItem| store.insert_item_blocking(&item).unwrap_err();

        let no_passage = refused(NewItem {
            passage: absent_passage,
            ..valid.clone()
        });
        let no_card = refused(NewItem {
            card_version: absent_card,
            ..valid.clone()
        });
        let no_ruleset = refused(NewItem {
            ruleset_version: absent_ruleset,
            ..valid.clone()
        });
        let no_clause = refused(NewItem {
            contrast: Contrast::Minimal(absent_clause),
            ..valid.clone()
        });
        let no_candidate = refused(NewItem {
            slots: vec![slots[0], absent_candidate],
            ..valid
        });

        assert!(
            matches!(no_passage, StoreError::NoPassage { id } if id == absent_passage),
            "{no_passage:?}"
        );
        assert_eq!(
            no_passage.to_string(),
            "the store holds no passage with id 2"
        );
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
        assert!(
            matches!(no_clause, StoreError::NoClause { id } if id == absent_clause),
            "{no_clause:?}"
        );
        assert_eq!(no_clause.to_string(), "the store holds no clause with id 2");
        assert!(
            matches!(no_candidate, StoreError::NoCandidate { id } if id == absent_candidate),
            "{no_candidate:?}"
        );
        assert_eq!(
            no_candidate.to_string(),
            "the store holds no candidate with id 3"
        );
        assert_nothing_written(&store);
    }

    #[test]
    fn an_item_listing_other_than_two_slots_is_refused_and_inserts_nothing() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);

        for listed in [vec![], vec![slots[0]], vec![slots[0], slots[1], slots[0]]] {
            let count = listed.len();
            let error = store
                .insert_item_blocking(&new_item(FreshCore, passage, listed, &versions))
                .unwrap_err();
            assert!(
                matches!(error, StoreError::SlotCount { count: refused } if refused == count),
                "{error:?}"
            );
            assert_eq!(
                error.to_string(),
                format!("an item holds 2 slots, and the insert listed {count}")
            );
        }
        assert_nothing_written(&store);
    }

    #[test]
    fn the_slots_table_refuses_a_third_position_a_repeated_one_and_another_candidates_generator() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);
        let item = store
            .insert_item_blocking(&new_item(FreshCore, passage, slots.clone(), &versions))
            .unwrap();
        store
            .request_blocking("insert an item with no slots", move |connection| {
                connection.execute(
                    ITEM_ROW,
                    (
                        "fresh_core",
                        passage.0,
                        versions.card.0,
                        versions.ruleset.0,
                        "none",
                        None::<i64>,
                    ),
                )
            })
            .unwrap();
        let slotless = item.0 + 1;
        let slot = "INSERT INTO item_slots (item_id, position, candidate_id, generator_id)
                    VALUES (?1, ?2, ?3, ?4)";
        // The first slot's candidate came from the local generator.
        let row = |item_id: i64, position: i64, generator_id: String| {
            vec![
                Value::Integer(item_id),
                Value::Integer(position),
                Value::Integer(slots[0].0),
                Value::Text(generator_id),
            ]
        };

        let third = refusal(&store, slot, row(item.0, 2, local().generator_id));
        let repeated = refusal(&store, slot, row(item.0, 1, local().generator_id));
        let another_generator = refusal(&store, slot, row(slotless, 0, teacher().generator_id));

        assert_eq!(third, rusqlite::ffi::SQLITE_CONSTRAINT_CHECK);
        assert_eq!(repeated, rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE);
        assert_eq!(
            another_generator,
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        );
        assert_eq!(rows(&store, "item_slots").len(), 2);
    }

    #[test]
    fn updating_or_deleting_an_item_or_a_slot_raises_and_leaves_the_item_as_it_was() {
        let root = TempRoot::new();
        let store = open(&root);
        let versions = versions(&store);
        let passage = passage_in(&store, Train);
        let slots = candidates_for(&store, passage);
        let id = store
            .insert_item_blocking(&new_item(Repeat, passage, slots, &versions))
            .unwrap();
        let stored = store.item_blocking(id).unwrap();

        assert_insert_only(&store, "items");
        assert_insert_only(&store, "item_slots");

        assert_eq!(store.item_blocking(id).unwrap(), stored);
    }

    #[test]
    fn reading_an_id_the_store_does_not_hold_returns_no_item() {
        let root = TempRoot::new();
        let store = open(&root);

        let error = store.item_blocking(ItemId(1)).unwrap_err();

        assert!(
            matches!(error, StoreError::NoItem { id: ItemId(1) }),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "the store holds no item with id 1");
    }

    /// Blocking on the store inside the runtime panics, so the fixtures use the awaited forms too.
    /// The passages module's fixed text lands a captured passage in gate.
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
        let item = NewItem {
            item_type: FreshCore,
            passage,
            register: REGISTER.to_owned(),
            slots: vec![first, second],
            card_version: card,
            ruleset_version: ruleset,
            contrast: Contrast::None,
        };

        let id = store.insert_item(&item).await.unwrap();
        let read = store.item(id).await.unwrap();
        let refused = store
            .insert_item(&NewItem {
                slots: vec![first],
                ..item
            })
            .await;
        let absent = store.item(ItemId(id.0 + 1)).await;

        let shown: Vec<CandidateId> = read.slots.iter().map(|slot| slot.candidate).collect();
        assert_eq!(shown, [first, second]);
        assert!(read.never_export, "a fresh item on a gate passage");
        assert!(
            matches!(refused, Err(StoreError::SlotCount { count: 1 })),
            "{refused:?}"
        );
        assert!(
            matches!(absent, Err(StoreError::NoItem { .. })),
            "{absent:?}"
        );
    }
}
