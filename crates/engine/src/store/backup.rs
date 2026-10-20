//! A copy of the store for `wa backup`: one file holding a consistent snapshot of every committed
//! row, taken while the app runs and writes. It copies the store file alone; the settings file,
//! model files and adapters under the data root are not part of it.
//!
//! The copy is SQLite's `VACUUM INTO`, run on the store thread's connection. It reads inside one
//! read transaction, so it sees one snapshot: every row committed before it began, whether in the
//! main file or still in the write-ahead log, and none committed after. In write-ahead mode a
//! reader blocks no writer, so the app's writes go on while it runs. It needs no checkpoint and no
//! clean close of the store, and the copy is a database of its own with no `-wal` or `-shm` beside
//! it. A byte copy of `store.sqlite` would miss every row still in the log, and a checkpoint ahead
//! of one waits on readers and races the app's next write.
//!
//! The copy is in rollback-journal mode, `delete`, whatever the store's mode: `VACUUM INTO` writes
//! a new database whose header marks no write-ahead log, measured under SQLite 3.53.2 and held by a
//! test. A read-only connection opens it and creates nothing beside it. The engine's open switches
//! it to write-ahead mode, as it does a new file, so a copy put back as `store.sqlite` opens like
//! the store it came from.
//!
//! The copy is written to a temporary file beside the destination, so the final rename is one
//! operation on one file system, checked with SQLite's integrity check on a connection of its own,
//! and renamed into place without replacing anything. The destination either holds a checked copy
//! or does not exist, and a failure at any step removes the temporary file.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
use windows_core::HSTRING;

use super::error::StoreError;
use super::thread::Store;

const BACK_UP: &str = "back up the store";

impl Store {
    /// Copies the store to `destination`, which must not exist, as one consistent snapshot of every
    /// committed row, on the store thread and while other connections go on writing. Refuses an
    /// existing destination before anything else, and leaves nothing at the destination when any
    /// later step fails.
    ///
    /// # Errors
    /// [`StoreError::BackupDestinationExists`] when the destination exists, before the backup
    /// writes anything or when a file appears there while it runs;
    /// [`StoreError::BackupDestinationNotAFile`] when the destination ends in no file name;
    /// [`StoreError::Write`] naming the temporary file when it cannot be created, as when the
    /// destination's directory does not exist, or naming the destination when the rename fails;
    /// [`StoreError::BackupPathNotUnicode`], [`StoreError::BackupFailed`] or
    /// [`StoreError::BackupCorrupt`] when SQLite cannot copy the store or the copy fails its
    /// check; [`StoreError::Read`] when the destination's existence cannot be determined; and
    /// [`StoreError::ThreadGone`] when the store thread has stopped.
    pub async fn backup(&self, destination: &Path) -> Result<(), StoreError> {
        let destination = destination.to_path_buf();
        self.typed_request(BACK_UP, move |connection| back_up(connection, &destination))
            .await
    }

    /// [`backup`](Self::backup) for a synchronous caller, such as `wa`, which blocks on the reply.
    ///
    /// # Errors
    /// As [`backup`](Self::backup).
    ///
    /// # Panics
    /// When called from async code, which awaits [`backup`](Self::backup) instead.
    pub fn backup_blocking(&self, destination: &Path) -> Result<(), StoreError> {
        let destination = destination.to_path_buf();
        self.typed_request_blocking(BACK_UP, move |connection| back_up(connection, &destination))
    }
}

fn back_up(connection: &Connection, destination: &Path) -> Result<(), StoreError> {
    refuse_existing(destination)?;
    let partial = copy_to_partial(connection, destination)?;
    partial.move_to(destination)?;
    log::info!("backed up the store to {}", destination.display());
    Ok(())
}

fn refuse_existing(destination: &Path) -> Result<(), StoreError> {
    match destination.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(StoreError::BackupDestinationExists {
            path: destination.to_path_buf(),
        }),
        Err(source) => Err(StoreError::Read {
            path: destination.to_path_buf(),
            source,
        }),
    }
}

/// Copies the store into a [`Partial`] beside `destination` and checks the copy.
fn copy_to_partial(connection: &Connection, destination: &Path) -> Result<Partial, StoreError> {
    let partial = Partial::claim(destination)?;
    let path = partial
        .path
        .to_str()
        .ok_or_else(|| StoreError::BackupPathNotUnicode {
            path: partial.path.clone(),
        })?;
    connection
        .execute("VACUUM INTO ?1", [path])
        .map_err(|source| partial.failed(source))?;
    partial.check_integrity()?;
    Ok(partial)
}

/// The temporary file a backup writes and checks before renaming it into place. Its drop removes
/// the file unless the rename succeeded.
struct Partial {
    path: PathBuf,
    moved: bool,
}

impl Partial {
    /// Creates `<destination>.<process id>.partial`, empty, in the destination's directory. The
    /// create fails when the file exists, so a drop only ever removes a file this backup created.
    /// `VACUUM INTO` writes into an empty file as into an absent one.
    fn claim(destination: &Path) -> Result<Self, StoreError> {
        let mut name = destination
            .file_name()
            .ok_or_else(|| StoreError::BackupDestinationNotAFile {
                path: destination.to_path_buf(),
            })?
            .to_os_string();
        name.push(format!(".{}.partial", std::process::id()));
        let path = destination.with_file_name(name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| StoreError::Write {
                path: path.clone(),
                source,
            })?;
        Ok(Self { path, moved: false })
    }

    fn failed(&self, source: rusqlite::Error) -> StoreError {
        StoreError::BackupFailed {
            path: self.path.clone(),
            source,
        }
    }

    /// Opens the copy read-only on a connection of its own and runs SQLite's integrity check,
    /// which reads every page, expecting the single row `ok`.
    fn check_integrity(&self) -> Result<(), StoreError> {
        let copy = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| self.failed(source))?;
        let finding: String = copy
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|source| self.failed(source))?;
        // Closed before the rename, which Windows refuses while another handle holds the file.
        copy.close().map_err(|(_, source)| self.failed(source))?;
        if finding == "ok" {
            Ok(())
        } else {
            Err(StoreError::BackupCorrupt {
                path: self.path.clone(),
                finding,
            })
        }
    }

    /// Renames the copy to `destination` without replacing a file there. `std::fs::rename`
    /// replaces on Windows, so a file created at the destination after the check would be lost.
    fn move_to(mut self, destination: &Path) -> Result<(), StoreError> {
        let from = HSTRING::from(self.path.as_os_str());
        let to = HSTRING::from(destination.as_os_str());
        // SAFETY: both arguments are null-terminated wide strings that outlive the call.
        unsafe { MoveFileExW(&from, &to, MOVEFILE_WRITE_THROUGH) }.map_err(|error| {
            // From the Win32 code, since the `io::Error` the windows crate builds from its own
            // error carries the HRESULT, which no `ErrorKind` maps.
            let source = match WIN32_ERROR::from_error(&error) {
                Some(code) => std::io::Error::from_raw_os_error(code.0 as i32),
                None => std::io::Error::from(error),
            };
            if source.kind() == ErrorKind::AlreadyExists {
                StoreError::BackupDestinationExists {
                    path: destination.to_path_buf(),
                }
            } else {
                StoreError::Write {
                    path: destination.to_path_buf(),
                    source,
                }
            }
        })?;
        self.moved = true;
        Ok(())
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if self.moved {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path) {
            log::warn!(
                "could not remove the partial backup {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::connection::{self, WhenAbsent, BUSY_TIMEOUT, STORE_FILE_NAME};
    use crate::store::migrations::MIGRATIONS;
    use crate::store::PassageKind;
    use crate::test_support::{capture_logs, TempRoot};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// A [`TempRoot`] created on disk, to hold a backup's destination.
    fn destination_dir() -> TempRoot {
        let dir = TempRoot::new();
        std::fs::create_dir_all(dir.path()).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn insert_passages(store: &Store, range: std::ops::Range<usize>) {
        for index in range {
            store
                .insert_passage_blocking(PassageKind::Synthetic, &format!("passage {index}"), None)
                .unwrap();
        }
    }

    /// Opens a copy as a plain read-only connection, the way a restore would first look at it.
    fn open_copy(path: &Path) -> Connection {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn passage_rows(connection: &Connection) -> Vec<(i64, String)> {
        connection
            .prepare("SELECT id, content_hash FROM passages ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn integrity(connection: &Connection) -> String {
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn rows_still_in_the_write_ahead_log_appear_in_a_copy_opened_with_no_sidecars() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        insert_passages(&store, 0..5);
        let wal = root.path().join(format!("{STORE_FILE_NAME}-wal"));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        // The main file alone, as a byte copy would take it, holds none of the rows: they are
        // still in the log, since the store is open and nothing has checkpointed it.
        let byte_copy_dir = destination_dir();
        let byte_copy = byte_copy_dir.path().join("byte-copy.sqlite");
        std::fs::copy(root.path().join(STORE_FILE_NAME), &byte_copy).unwrap();
        let byte_copy_tables: i64 = open_copy(&byte_copy)
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'passages'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(byte_copy_tables, 0);

        let dir = destination_dir();
        let destination = dir.path().join("copy.sqlite");
        store.backup_blocking(&destination).unwrap();

        assert_eq!(file_names(dir.path()), ["copy.sqlite"]);
        let copy = open_copy(&destination);
        assert_eq!(passage_rows(&copy).len(), 5);
        drop(copy);
        assert_eq!(file_names(dir.path()), ["copy.sqlite"]);
    }

    /// Two handles on one file stand in for the app writing and `wa` backing up, two processes
    /// with a connection each.
    #[test]
    fn a_backup_taken_while_another_connection_writes_holds_every_row_up_to_one_id() {
        const BEFORE: usize = 50;
        let root = TempRoot::new();
        let app = Store::open(root.path()).unwrap();
        let wa = Store::open_existing(root.path()).unwrap();
        let inserted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = thread::spawn({
            let (app, inserted, stop) = (app.clone(), inserted.clone(), stop.clone());
            move || {
                while !stop.load(Ordering::SeqCst) {
                    let index = inserted.load(Ordering::SeqCst);
                    app.insert_passage_blocking(
                        PassageKind::Synthetic,
                        &format!("passage {index}"),
                        None,
                    )?;
                    inserted.fetch_add(1, Ordering::SeqCst);
                }
                Ok::<_, StoreError>(())
            }
        });
        let wait_past = |count: usize| {
            while inserted.load(Ordering::SeqCst) <= count {
                assert!(!writer.is_finished(), "the writer stopped early");
                thread::sleep(Duration::from_millis(1));
            }
        };
        wait_past(BEFORE);

        let dir = destination_dir();
        let destination = dir.path().join("copy.sqlite");
        wa.backup_blocking(&destination).unwrap();
        wait_past(inserted.load(Ordering::SeqCst));
        stop.store(true, Ordering::SeqCst);
        writer
            .join()
            .unwrap()
            .expect("every write succeeds while the backup runs");

        let source = app
            .request_blocking("read the passages", |connection| {
                Ok(passage_rows(connection))
            })
            .unwrap();
        let copy = open_copy(&destination);
        assert_eq!(integrity(&copy), "ok");
        let copied = passage_rows(&copy);
        assert!(copied.len() > BEFORE, "{} rows copied", copied.len());
        assert!(
            source.len() > copied.len(),
            "the writer went on past the copy"
        );
        assert_eq!(copied, source[..copied.len()]);
    }

    #[test]
    fn an_existing_destination_is_refused_and_left_byte_identical() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let dir = destination_dir();
        let destination = dir.path().join("existing.sqlite");
        let contents = b"the owner's earlier backup";
        std::fs::write(&destination, contents).unwrap();

        match store.backup_blocking(&destination) {
            Err(StoreError::BackupDestinationExists { path }) => assert_eq!(path, destination),
            other => panic!("expected the destination refused, got {other:?}"),
        }

        assert_eq!(std::fs::read(&destination).unwrap(), contents);
        assert_eq!(file_names(dir.path()), ["existing.sqlite"]);
    }

    #[test]
    fn a_destination_in_a_missing_directory_fails_and_leaves_no_file() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let dir = TempRoot::new();
        let destination = dir.path().join("copy.sqlite");

        let error = store.backup_blocking(&destination).unwrap_err();

        assert!(matches!(error, StoreError::Write { .. }), "{error:?}");
        assert!(
            error
                .to_string()
                .contains(&dir.path().display().to_string()),
            "{error}"
        );
        assert!(!dir.path().exists());
    }

    /// The rename is the last step, so a file that appears at the destination after the check is
    /// the one failure that comes after a complete, checked copy.
    #[test]
    fn a_file_appearing_at_the_destination_mid_backup_is_kept_and_the_copy_removed() {
        let root = TempRoot::new();
        let (connection, _) =
            connection::open(root.path(), BUSY_TIMEOUT, WhenAbsent::Create).unwrap();
        let dir = destination_dir();
        let destination = dir.path().join("copy.sqlite");

        refuse_existing(&destination).unwrap();
        let partial = copy_to_partial(&connection, &destination).unwrap();
        assert_eq!(file_names(dir.path()).len(), 1);
        std::fs::write(&destination, "written by someone else").unwrap();
        match partial.move_to(&destination) {
            Err(StoreError::BackupDestinationExists { path }) => assert_eq!(path, destination),
            other => panic!("expected the destination refused, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "written by someone else"
        );
        assert_eq!(file_names(dir.path()), ["copy.sqlite"]);
    }

    #[test]
    fn the_copy_opens_as_a_store_at_the_same_schema_version_and_applies_no_migration() {
        let newest = MIGRATIONS.last().unwrap().number;
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let copy_root = destination_dir();
        store
            .backup_blocking(&copy_root.path().join(STORE_FILE_NAME))
            .unwrap();

        let (copy, lines) = capture_logs(|| Store::open(copy_root.path()));
        let copy = copy.unwrap();

        assert!(
            lines
                .iter()
                .all(|(_, line)| !line.contains("applied migration")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|(_, line)| line.ends_with(&format!("at schema version {newest}"))),
            "{lines:?}"
        );
        let schema_rows = |store: &Store| {
            store
                .request_blocking("read the schema versions", |connection| {
                    connection
                        .prepare(
                            "SELECT id, number, sha256, applied_at FROM schema_version ORDER BY id",
                        )?
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, u32>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
        };
        let source_rows = schema_rows(&store);
        assert_eq!(source_rows.len(), newest as usize);
        assert_eq!(schema_rows(&copy), source_rows);
    }

    #[test]
    fn the_copy_is_in_rollback_mode_until_the_engine_opens_it_in_write_ahead_mode() {
        let journal_mode = |connection: &Connection| -> String {
            connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap()
        };
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let copy_root = destination_dir();
        let destination = copy_root.path().join(STORE_FILE_NAME);
        store.backup_blocking(&destination).unwrap();

        assert_eq!(journal_mode(&open_copy(&destination)), "delete");

        let (connection, _) =
            connection::open(copy_root.path(), BUSY_TIMEOUT, WhenAbsent::Refuse).unwrap();
        assert_eq!(journal_mode(&connection), "wal");
    }

    #[tokio::test]
    async fn an_awaited_backup_writes_the_copy() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let dir = destination_dir();
        let destination = dir.path().join("copy.sqlite");

        store.backup(&destination).await.unwrap();

        assert_eq!(integrity(&open_copy(&destination)), "ok");
    }
}
