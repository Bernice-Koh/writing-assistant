//! The store file under the data root, the pragmas every connection to it carries, and the
//! migration step that brings its schema up to date at open.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use super::error::StoreError;
use super::migrations::{self, MIGRATIONS};

/// The store's file name, directly under the data root. SQLite keeps the write-ahead log and its
/// shared-memory index beside the file as `store.sqlite-wal` and `store.sqlite-shm`, and
/// `.gitignore`'s `*.sqlite*` pattern covers all three.
pub(super) const STORE_FILE_NAME: &str = "store.sqlite";

/// Write-ahead mode, so that a reader on another connection, the Python project's among them,
/// reads a committed snapshot while a writer writes, and neither waits on the other.
const JOURNAL_MODE: &str = "wal";

/// SQLite leaves foreign keys unenforced on every new connection, for compatibility with
/// databases written before it enforced them, so a connection that does not ask for enforcement
/// accepts a row naming a parent that does not exist.
const ENFORCE_FOREIGN_KEYS: bool = true;

/// How long a write waits for another connection's write lock before it fails with
/// [`StoreError::Busy`]. The app and `wa` are two processes running the engine's code, so two
/// writers can contend for the file. Both write rarely and briefly, so a few seconds outlasts any
/// real contention, and a write still waiting after that is held up by a stuck writer, which a
/// longer wait would not cure. `rusqlite` gives a new connection five seconds already, but states
/// that its default may change, so the store sets its own.
///
/// A write transaction waits only if it begins `IMMEDIATE`. A deferred transaction that reads
/// before it writes fails at its first write without waiting when another connection holds the
/// write lock, since its snapshot may already be stale.
pub(super) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens the store file under `data_root` with the pragmas every store connection carries,
/// creating the root and the file when either is absent, and applies every embedded migration the
/// store does not yet record. Returns the connection and the schema version it ends at. Touches no
/// other file under the root: Tauri points WebView2's user-data folder at the same root.
pub(super) fn open(
    data_root: &Path,
    busy_timeout: Duration,
) -> Result<(Connection, u32), StoreError> {
    std::fs::create_dir_all(data_root).map_err(|source| StoreError::Write {
        path: data_root.to_path_buf(),
        source,
    })?;
    let path = data_root.join(STORE_FILE_NAME);
    let open_error = |source| StoreError::Open {
        path: path.clone(),
        source,
    };

    let mut connection = Connection::open(&path).map_err(open_error)?;
    // Set first, so that switching a new file to write-ahead mode, which takes the write lock,
    // waits for another process's write rather than failing. Only the app creates the store file,
    // since `wa` opens an existing store and refuses when it is absent, so two `journal_mode`
    // switches on a new file never race.
    connection.busy_timeout(busy_timeout).map_err(open_error)?;
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", JOURNAL_MODE, |row| row.get(0))
        .map_err(open_error)?;
    if !journal_mode.eq_ignore_ascii_case(JOURNAL_MODE) {
        return Err(StoreError::JournalMode {
            path,
            mode: journal_mode,
        });
    }
    connection
        .pragma_update(None, "foreign_keys", ENFORCE_FOREIGN_KEYS)
        .map_err(open_error)?;

    // Inside the open, so the store thread never serves a request against an older schema.
    let schema_version = migrations::migrate(&mut connection, MIGRATIONS, &path)?;
    Ok((connection, schema_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempRoot;

    #[test]
    fn opening_a_root_that_does_not_exist_creates_it_and_the_store_file() {
        let root = TempRoot::new();
        assert!(!root.path().exists());

        let _connection = open(root.path(), BUSY_TIMEOUT).unwrap();

        assert!(root.path().join(STORE_FILE_NAME).is_file());
    }

    #[test]
    fn a_new_store_is_in_write_ahead_mode_with_foreign_keys_enforced() {
        let root = TempRoot::new();
        let (connection, _) = open(root.path(), BUSY_TIMEOUT).unwrap();

        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");

        connection
            .execute_batch(
                "CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE child (parent_id INTEGER NOT NULL REFERENCES parent (id));",
            )
            .unwrap();
        let orphan = connection.execute("INSERT INTO child (parent_id) VALUES (1)", []);
        assert_eq!(
            orphan.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
    }

    #[test]
    fn reopening_the_store_keeps_its_rows() {
        let root = TempRoot::new();
        let (first, _) = open(root.path(), BUSY_TIMEOUT).unwrap();
        first
            .execute_batch("CREATE TABLE scratch (id INTEGER PRIMARY KEY); INSERT INTO scratch DEFAULT VALUES;")
            .unwrap();
        drop(first);

        let (second, _) = open(root.path(), BUSY_TIMEOUT).unwrap();
        let rows: i64 = second
            .query_row("SELECT count(*) FROM scratch", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn the_open_adds_nothing_under_the_root_but_the_store_file_and_its_sidecars() {
        let root = TempRoot::new();
        let webview_profile = root.path().join("EBWebView");
        std::fs::create_dir_all(&webview_profile).unwrap();
        std::fs::write(webview_profile.join("Local State"), "{}").unwrap();
        std::fs::write(root.path().join("config.json"), "{}").unwrap();

        // The migration step reads at open, and a read opens the write-ahead log, which brings the
        // two sidecars into being before the open returns.
        let _connection = open(root.path(), BUSY_TIMEOUT).unwrap();

        let mut added: Vec<String> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "EBWebView" && name != "config.json")
            .collect();
        added.sort();
        assert_eq!(
            added,
            [
                STORE_FILE_NAME.to_owned(),
                format!("{STORE_FILE_NAME}-shm"),
                format!("{STORE_FILE_NAME}-wal"),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("config.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(webview_profile.join("Local State")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn a_store_file_that_is_not_a_database_fails_the_open_and_is_left_as_it_is() {
        let root = TempRoot::new();
        std::fs::create_dir_all(root.path()).unwrap();
        let path = root.path().join(STORE_FILE_NAME);
        let contents = "not a SQLite database, and long enough to hold a header's worth of bytes";
        std::fs::write(&path, contents).unwrap();

        match open(root.path(), BUSY_TIMEOUT) {
            Err(StoreError::Open {
                path: fault_path,
                source,
            }) => {
                assert_eq!(fault_path, path);
                assert_eq!(
                    source.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::NotADatabase)
                );
            }
            other => panic!("expected an open error, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }
}
