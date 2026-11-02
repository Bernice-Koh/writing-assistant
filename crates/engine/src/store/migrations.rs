//! The store's schema as numbered SQL migrations compiled into the engine, and the runner that
//! applies them when the store opens, on the store thread before it serves its first request.
//!
//! Each migration is a file under `migrations/`, named by a four-digit number and a name, embedded
//! with `include_str!` and listed in [`MIGRATIONS`]. The first creates `schema_version`, where the
//! runner records each migration it applies, with its number, the SHA-256 of its SQL and the time,
//! inside the migration's own transaction; the store's schema version is the highest number
//! recorded there. A migration holds no transaction statements of its own.
//!
//! A merged migration is never edited, and a change to the schema is a new migration. At every open
//! the runner checks each recorded migration against the build's: a recorded hash that the build's
//! migration of that number no longer matches fails the open, and so does a store recorded above
//! the build's newest migration, which an older build leaves as it is.
//!
//! Every table a migration creates is insert-only. Its id is `id INTEGER PRIMARY KEY
//! AUTOINCREMENT`, so ids only ever increase and the rows up to an id form a snapshot that later
//! inserts never change. Beside the table, in the same migration, two triggers,
//! `<table>_no_update` and `<table>_no_delete`, raise before any update or delete with a fixed
//! message naming the table. A test walks the schema and fails for a table without them.

use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::error::StoreError;
use crate::hash::sha256_hex;

/// One migration: the number that orders it, the name its file carries after the number, and its
/// SQL.
#[derive(Clone)]
pub(super) struct Migration {
    pub(super) number: u32,
    pub(super) name: &'static str,
    pub(super) sql: &'static str,
}

/// Every migration, in order and numbered from 1 without a gap. A test holds this list and the
/// files under `migrations/` equal.
pub(super) static MIGRATIONS: &[Migration] = &[
    Migration {
        number: 1,
        name: "schema_version",
        sql: include_str!("migrations/0001_schema_version.sql"),
    },
    Migration {
        number: 2,
        name: "passages",
        sql: include_str!("migrations/0002_passages.sql"),
    },
    Migration {
        number: 3,
        name: "card_and_ruleset",
        sql: include_str!("migrations/0003_card_and_ruleset.sql"),
    },
    Migration {
        number: 4,
        name: "candidates_and_screen_results",
        sql: include_str!("migrations/0004_candidates_and_screen_results.sql"),
    },
    Migration {
        number: 5,
        name: "items",
        sql: include_str!("migrations/0005_items.sql"),
    },
    Migration {
        number: 6,
        name: "events_and_habit_rollups",
        sql: include_str!("migrations/0006_events_and_habit_rollups.sql"),
    },
];

/// Brings the schema of the store at `path`, open on `connection`, up to the newest of
/// `migrations`, and returns the schema version it ends at.
///
/// Each pass takes the write lock before it reads what the store records, so when the app and `wa`
/// open one store at once, the second waits inside the busy timeout and then reads the first's rows
/// rather than applying a migration twice. A store already at the newest version takes the lock,
/// reads, and writes nothing.
pub(super) fn migrate(
    connection: &mut Connection,
    migrations: &[Migration],
    path: &Path,
) -> Result<u32, StoreError> {
    loop {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| open_error(path, source))?;
        let version = check_recorded(&transaction, migrations, path)?;
        // Returning drops the transaction, which rolls it back, and it wrote nothing.
        let Some(migration) = migrations
            .iter()
            .find(|migration| migration.number > version)
        else {
            return Ok(version);
        };
        apply(transaction, migration).map_err(|source| StoreError::MigrationFailed {
            path: path.to_path_buf(),
            number: migration.number,
            source,
        })?;
        log::debug!(
            "applied migration {} ({}) to the store",
            migration.number,
            migration.name
        );
    }
}

/// Checks each migration the store records against the one of its number in `migrations`, and
/// returns the store's schema version: the highest number recorded, or 0 before the first
/// migration has created `schema_version`.
fn check_recorded(
    transaction: &Transaction,
    migrations: &[Migration],
    path: &Path,
) -> Result<u32, StoreError> {
    let recorded = recorded(transaction).map_err(|source| open_error(path, source))?;
    let version = recorded.last().map_or(0, |(number, _)| *number);
    let newest = migrations.last().map_or(0, |migration| migration.number);
    if version > newest {
        return Err(StoreError::SchemaNewerThanBuild {
            path: path.to_path_buf(),
            version,
            newest,
        });
    }
    for (number, sha256) in &recorded {
        let embedded = migrations
            .iter()
            .find(|migration| migration.number == *number);
        if embedded.is_none_or(|migration| sha256_hex(migration.sql.as_bytes()) != *sha256) {
            return Err(StoreError::MigrationChanged {
                path: path.to_path_buf(),
                number: *number,
            });
        }
    }
    Ok(version)
}

/// The number and hash of every migration the store records, in order of number, and none before
/// the first migration has created `schema_version`.
fn recorded(connection: &Connection) -> rusqlite::Result<Vec<(u32, String)>> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }
    connection
        .prepare("SELECT number, sha256 FROM schema_version ORDER BY number")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// Runs `migration` and records it inside `transaction`, then commits. On any failure the
/// transaction drops and rolls back, so the migration leaves nothing behind and the recorded
/// version stays where it was.
fn apply(transaction: Transaction, migration: &Migration) -> rusqlite::Result<()> {
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_version (number, sha256, applied_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        (migration.number, sha256_hex(migration.sql.as_bytes())),
    )?;
    transaction.commit()
}

fn open_error(path: &Path, source: rusqlite::Error) -> StoreError {
    StoreError::Open {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::connection::{self, WhenAbsent, BUSY_TIMEOUT, STORE_FILE_NAME};
    use crate::test_support::{capture_logs, TempRoot};
    use log::Level;
    use regex::Regex;
    use rusqlite::OptionalExtension;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// A table a later build's migration might create.
    const LATER_TABLE: &str = "CREATE TABLE later (id INTEGER PRIMARY KEY AUTOINCREMENT);";

    /// Every merged migration's number and the SHA-256 of its file, so an edit to a merged
    /// migration fails CI before a store that recorded the old hash refuses to open. Every pull
    /// request that adds a migration appends its entry.
    const PINNED: &[(u32, &str)] = &[
        (
            1,
            "8eaca0519cc4ad0b27e67139adabafe433cdf1ab4ab4df95d5b2ae5856228a0b",
        ),
        (
            2,
            "b0296cd30eaf421b42b7da602939ac164d36b2cc6539a1cf8aa5ae58d2567fd2",
        ),
        (
            3,
            "8c1f86fe2370a1d5067c184052e91163008e4afbaa96a8e40ea91c6f924a6d6d",
        ),
        (
            4,
            "50da4b19e3379d58824cffed66f4c42f3899e06d9642861a13a7c933cbd5ca30",
        ),
        (
            5,
            "3a97a07efb7719e99c3984cf8ca95b8eb076aaa8a5508b25b5f92b8064bde3a3",
        ),
        (
            6,
            "1b1d1a4a76314a7ac1112a092626699fd7011481531fc10869da7b8de050a816",
        ),
    ];

    fn newest() -> u32 {
        MIGRATIONS
            .last()
            .expect("the first migration creates schema_version")
            .number
    }

    /// This build's migrations followed by one numbered after the newest, running `sql`: the list
    /// a later build might carry.
    fn with_next(sql: &'static str) -> Vec<Migration> {
        let mut migrations = MIGRATIONS.to_vec();
        migrations.push(Migration {
            number: newest() + 1,
            name: "next",
            sql,
        });
        migrations
    }

    fn store_path(root: &TempRoot) -> PathBuf {
        root.path().join(STORE_FILE_NAME)
    }

    #[derive(Debug, PartialEq)]
    struct Recorded {
        number: u32,
        sha256: String,
        applied_at: String,
    }

    fn recorded_rows(connection: &Connection) -> Vec<Recorded> {
        connection
            .prepare("SELECT number, sha256, applied_at FROM schema_version ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok(Recorded {
                    number: row.get(0)?,
                    sha256: row.get(1)?,
                    applied_at: row.get(2)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Every way the schema on `connection` breaks the insert-only guard, one line each, naming the
    /// table. SQLite's own tables are skipped: `AUTOINCREMENT` keeps `sqlite_sequence`, which
    /// SQLite itself updates.
    fn unguarded(connection: &Connection) -> Vec<String> {
        let tables: Vec<(String, String)> = connection
            .prepare(
                "SELECT name, sql FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut faults = Vec::new();
        for (table, sql) in tables {
            let primary_key: Vec<(String, String)> = connection
                .prepare("SELECT name, type FROM pragma_table_info(?1) WHERE pk > 0")
                .unwrap()
                .query_map([&table], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let id_is_the_integer_key = matches!(
                primary_key.as_slice(),
                [(name, kind)] if name == "id" && kind.eq_ignore_ascii_case("INTEGER")
            );
            if !id_is_the_integer_key {
                faults.push(format!(
                    "{table}: its primary key is not one id INTEGER column"
                ));
            }
            // SQLite accepts AUTOINCREMENT only on the INTEGER PRIMARY KEY, so with the check above
            // it can only be the id's.
            if !sql.to_ascii_uppercase().contains("AUTOINCREMENT") {
                faults.push(format!("{table}: its id does not AUTOINCREMENT"));
            }
            for event in ["UPDATE", "DELETE"] {
                let trigger = format!("{table}_no_{}", event.to_ascii_lowercase());
                let trigger_sql: Option<String> = connection
                    .query_row(
                        "SELECT sql FROM sqlite_schema
                         WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                        [&trigger, &table],
                        |row| row.get(0),
                    )
                    .optional()
                    .unwrap();
                if !trigger_sql
                    .is_some_and(|sql| raises_before_every(&sql, &trigger, event, &table))
                {
                    faults.push(format!(
                        "{table}: no trigger {trigger} raising before every {event} with a \
                         message naming the table"
                    ));
                }
            }
        }
        faults
    }

    /// Whether `sql` creates `trigger` to abort every `event` on `table` before it touches a row,
    /// with no condition and a fixed message naming the table, which so holds no row content.
    fn raises_before_every(sql: &str, trigger: &str, event: &str, table: &str) -> bool {
        let normalised = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let form = Regex::new(&format!(
            r"(?i)^CREATE TRIGGER {trigger} BEFORE {event} ON {table} BEGIN SELECT RAISE\(ABORT, '([^']*)'\); END$",
            trigger = regex::escape(trigger),
            table = regex::escape(table),
        ))
        .unwrap();
        form.captures(&normalised)
            .is_some_and(|captures| captures[1].contains(table))
    }

    #[test]
    fn every_migration_file_is_listed_in_order_from_one_without_a_gap() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/store/migrations");
        let file_name =
            |migration: &Migration| format!("{:04}_{}.sql", migration.number, migration.name);

        let files: BTreeSet<String> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        let listed: BTreeSet<String> = MIGRATIONS.iter().map(file_name).collect();
        assert_eq!(files, listed);
        assert_eq!(listed.len(), MIGRATIONS.len(), "two entries name one file");

        for migration in MIGRATIONS {
            let contents = std::fs::read_to_string(directory.join(file_name(migration))).unwrap();
            assert_eq!(
                contents, migration.sql,
                "migration {} embeds a file other than its own",
                migration.number
            );
        }
        let numbers: Vec<u32> = MIGRATIONS
            .iter()
            .map(|migration| migration.number)
            .collect();
        let from_one: Vec<u32> = (1..).take(MIGRATIONS.len()).collect();
        assert_eq!(numbers, from_one);
    }

    #[test]
    fn every_migration_hashes_to_its_pinned_digest() {
        let numbers: Vec<u32> = MIGRATIONS
            .iter()
            .map(|migration| migration.number)
            .collect();
        let pinned: Vec<u32> = PINNED.iter().map(|(number, _)| *number).collect();
        assert_eq!(numbers, pinned, "a migration without a pinned digest");

        for (migration, (_, digest)) in MIGRATIONS.iter().zip(PINNED) {
            assert_eq!(
                sha256_hex(migration.sql.as_bytes()),
                *digest,
                "migration {} no longer hashes to its pinned digest",
                migration.number
            );
        }
    }

    /// `.gitattributes` checks every `.sql` file out with LF endings, so a migration hashes the
    /// same on every machine. A file an editor saved with CRLF would hash differently on that
    /// machine alone.
    #[test]
    fn no_embedded_migration_holds_a_carriage_return() {
        for migration in MIGRATIONS {
            assert!(
                !migration.sql.contains('\r'),
                "migration {} holds a carriage return",
                migration.number
            );
        }
    }

    #[test]
    fn a_new_store_ends_at_the_newest_migration_with_one_row_for_each() {
        let root = TempRoot::new();

        let ((connection, version), lines) = capture_logs(|| {
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap()
        });

        assert_eq!(version, newest());
        let rows = recorded_rows(&connection);
        assert_eq!(rows.len(), MIGRATIONS.len());
        let utc_to_the_millisecond =
            Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
        for (row, migration) in rows.iter().zip(MIGRATIONS) {
            assert_eq!(row.number, migration.number);
            assert_eq!(row.sha256, sha256_hex(migration.sql.as_bytes()));
            assert!(utc_to_the_millisecond.is_match(&row.applied_at), "{row:?}");
        }
        let applied = lines
            .iter()
            .filter(|(level, line)| *level == Level::Debug && line.starts_with("applied migration"))
            .count();
        assert_eq!(applied, MIGRATIONS.len(), "{lines:?}");
        assert!(
            lines.iter().all(|(level, _)| *level > Level::Info),
            "a migration logged above debug: {lines:?}"
        );
    }

    #[test]
    fn reopening_a_store_at_the_newest_migration_applies_nothing() {
        let root = TempRoot::new();
        let (first, _) = connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        let rows = recorded_rows(&first);
        drop(first);
        let bytes = std::fs::read(store_path(&root)).unwrap();

        let ((second, version), lines) = capture_logs(|| {
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap()
        });

        assert_eq!(version, newest());
        assert_eq!(recorded_rows(&second), rows);
        assert!(lines.is_empty(), "{lines:?}");
        drop(second);
        assert_eq!(std::fs::read(store_path(&root)).unwrap(), bytes);
    }

    #[test]
    fn a_recorded_hash_the_migration_no_longer_matches_fails_the_open_naming_it() {
        let root = TempRoot::new();
        drop(connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap());
        let tamper = Connection::open(store_path(&root)).unwrap();
        // The update trigger refuses the edit, so the tamper drops it first.
        tamper
            .execute_batch(
                "DROP TRIGGER schema_version_no_update;
                 UPDATE schema_version SET sha256 = 'tampered' WHERE number = 1;",
            )
            .unwrap();
        drop(tamper);
        let bytes = std::fs::read(store_path(&root)).unwrap();

        let error = connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap_err();

        assert!(
            matches!(error, StoreError::MigrationChanged { number: 1, .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("migration 1"), "{error}");
        assert_eq!(std::fs::read(store_path(&root)).unwrap(), bytes);
    }

    #[test]
    fn an_applied_migration_edited_since_fails_the_open_naming_it() {
        let root = TempRoot::new();
        let path = store_path(&root);
        let (mut connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        migrate(&mut connection, &with_next(LATER_TABLE), &path).unwrap();
        let edited =
            with_next("CREATE TABLE later (id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT);");

        let error = migrate(&mut connection, &edited, &path).unwrap_err();

        assert!(
            matches!(error, StoreError::MigrationChanged { number, .. } if number == newest() + 1),
            "{error:?}"
        );
    }

    #[test]
    fn a_store_recorded_above_the_newest_migration_fails_the_open_and_is_left_as_it_is() {
        let root = TempRoot::new();
        let (mut later_build, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        migrate(
            &mut later_build,
            &with_next(LATER_TABLE),
            &store_path(&root),
        )
        .unwrap();
        drop(later_build);
        let bytes = std::fs::read(store_path(&root)).unwrap();

        let error = connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap_err();

        assert!(
            matches!(
                error,
                StoreError::SchemaNewerThanBuild { version, newest: build_newest, .. }
                    if version == newest() + 1 && build_newest == newest()
            ),
            "{error:?}"
        );
        assert_eq!(std::fs::read(store_path(&root)).unwrap(), bytes);
    }

    #[test]
    fn a_migration_that_fails_part_way_rolls_back_and_leaves_the_recorded_version() {
        let root = TempRoot::new();
        let path = store_path(&root);
        let (mut connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        let rows = recorded_rows(&connection);
        let fails_part_way = with_next(
            "CREATE TABLE half (id INTEGER PRIMARY KEY AUTOINCREMENT);
             INSERT INTO missing (id) VALUES (1);",
        );

        let error = migrate(&mut connection, &fails_part_way, &path).unwrap_err();

        assert!(
            matches!(&error, StoreError::MigrationFailed { number, .. } if *number == newest() + 1),
            "{error:?}"
        );
        assert_eq!(recorded_rows(&connection), rows);
        let half_exists: bool = connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'half')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !half_exists,
            "the migration's first statement outlived the rollback"
        );
        assert_eq!(
            migrate(&mut connection, MIGRATIONS, &path).unwrap(),
            newest()
        );
    }

    /// Two connections opening one store at once stand in for the app and `wa` starting together on
    /// a store a build before the first migration created: in write-ahead mode, with no tables.
    /// The file exists before the race, so the race is the runner's alone.
    #[test]
    fn two_opens_at_once_of_a_store_without_migrations_apply_each_migration_once() {
        let root = TempRoot::new();
        std::fs::create_dir_all(root.path()).unwrap();
        Connection::open(store_path(&root))
            .unwrap()
            .pragma_update(None, "journal_mode", "wal")
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let openers: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let data_root = root.path().to_path_buf();
                thread::spawn(move || {
                    barrier.wait();
                    connection::open(&data_root, BUSY_TIMEOUT, WhenAbsent::Create)
                        .map(|(_, version)| version)
                })
            })
            .collect();
        for opener in openers {
            assert_eq!(opener.join().unwrap().unwrap(), newest());
        }

        let (connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        assert_eq!(recorded_rows(&connection).len(), MIGRATIONS.len());
    }

    #[test]
    fn every_table_the_migrations_create_has_an_autoincrement_id_and_both_triggers() {
        let root = TempRoot::new();
        let (connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();

        assert_eq!(unguarded(&connection), Vec::<String>::new());
    }

    #[test]
    fn the_artefact_tables_exist_once_every_migration_has_run() {
        let root = TempRoot::new();
        let (connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();

        let tables: BTreeSet<String> = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for table in [
            "clauses",
            "clause_texts",
            "card_versions",
            "card_members",
            "card_changes",
            "checkers",
            "ruleset_versions",
            "ruleset_members",
            "candidates",
            "screen_results",
            "items",
            "item_slots",
            "events",
            "habit_rollups",
        ] {
            assert!(tables.contains(table), "no table {table} in {tables:?}");
        }
    }

    #[test]
    fn the_schema_walk_names_each_table_added_without_the_guard() {
        let root = TempRoot::new();
        let (mut connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        let unguarded_tables = with_next(
            "CREATE TABLE bare (id INTEGER PRIMARY KEY, note TEXT);
             CREATE TABLE conditional (id INTEGER PRIMARY KEY AUTOINCREMENT);
             CREATE TRIGGER conditional_no_update BEFORE UPDATE ON conditional
             WHEN OLD.id > 1
             BEGIN
                 SELECT RAISE(ABORT, 'conditional is insert-only');
             END;
             CREATE TRIGGER conditional_no_delete BEFORE DELETE ON conditional
             BEGIN
                 SELECT RAISE(ABORT, 'conditional is insert-only');
             END;",
        );
        migrate(&mut connection, &unguarded_tables, &store_path(&root)).unwrap();

        let faults = unguarded(&connection);

        assert_eq!(faults.len(), 4, "{faults:?}");
        assert_eq!(
            faults
                .iter()
                .filter(|fault| fault.starts_with("bare: "))
                .count(),
            3,
            "{faults:?}"
        );
        assert!(
            faults
                .iter()
                .any(|fault| fault.starts_with("conditional: no trigger conditional_no_update")),
            "{faults:?}"
        );
    }

    #[test]
    fn updating_or_deleting_a_schema_version_row_raises_and_leaves_it_as_it_was() {
        let root = TempRoot::new();
        let (connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        let rows = recorded_rows(&connection);

        for statement in [
            "UPDATE schema_version SET applied_at = '2000-01-01T00:00:00.000Z'",
            "DELETE FROM schema_version",
        ] {
            match connection.execute(statement, []) {
                Err(rusqlite::Error::SqliteFailure(failure, Some(message))) => {
                    assert_eq!(
                        failure.extended_code,
                        rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER,
                        "{statement}"
                    );
                    assert!(
                        message.starts_with("schema_version is insert-only"),
                        "{message}"
                    );
                }
                other => panic!("{statement}: expected the trigger to raise, got {other:?}"),
            }
        }
        assert_eq!(recorded_rows(&connection), rows);
    }
}
