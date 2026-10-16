//! The store thread, which owns the connection for the life of the store, and [`Store`], the handle
//! every other thread reaches it through. The shape is the UI Automation thread's in
//! `capture::native`: one thread owns the resource, and requests reach it over a channel.

use std::path::Path;
use std::sync::{mpsc as ready_channel, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::{mpsc as request_channel, oneshot};

use super::connection::{self, BUSY_TIMEOUT};
use super::error::StoreError;

/// Work for the store thread: a closure over the connection that sends its own result on the reply
/// channel it captured, so one queue carries requests of every result type.
type Request = Box<dyn FnOnce(&mut Connection) + Send>;

/// The reply to one request, which an async caller awaits and a synchronous caller blocks on.
type Reply<T> = oneshot::Receiver<Result<T, StoreError>>;

/// A handle on the open store, cheap to clone. Every clone reaches the one store thread, which
/// owns the connection; no other thread ever holds it. Dropping the last handle closes the
/// channel, and the thread runs every request already queued, closes the connection and exits,
/// with the drop waiting for it to finish.
#[derive(Clone)]
pub struct Store {
    thread: Arc<StoreThread>,
}

/// Shared by every clone of one [`Store`], so its drop runs once, when the last handle drops.
struct StoreThread {
    requests: Option<request_channel::UnboundedSender<Request>>,
    join: Option<JoinHandle<()>>,
}

impl Store {
    /// Opens the store under `data_root` and starts the store thread that owns its connection:
    /// creates the root and the store file when either is absent, sets write-ahead mode,
    /// foreign-key enforcement and the busy timeout, and applies every embedded migration the
    /// store does not yet record before the thread serves its first request. Blocks until the
    /// thread reports the outcome.
    ///
    /// # Errors
    /// [`StoreError::Write`] when the root cannot be created, [`StoreError::Open`] when SQLite
    /// cannot open the file, set a pragma or read the recorded migrations,
    /// [`StoreError::JournalMode`] when the file does not take write-ahead mode,
    /// [`StoreError::MigrationChanged`] or [`StoreError::SchemaNewerThanBuild`] when the recorded
    /// migrations do not fit this build's, [`StoreError::MigrationFailed`] when a migration fails
    /// and rolls back, and [`StoreError::ThreadSpawn`] or [`StoreError::ThreadNotReady`] when the
    /// thread does not start.
    pub fn open(data_root: &Path) -> Result<Self, StoreError> {
        Self::open_with_busy_timeout(data_root, BUSY_TIMEOUT)
    }

    fn open_with_busy_timeout(
        data_root: &Path,
        busy_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let (request_tx, request_rx) = request_channel::unbounded_channel();
        let (ready_tx, ready_rx) = ready_channel::channel();
        let thread_root = data_root.to_path_buf();
        let join = thread::Builder::new()
            .name("writing-assistant-store".to_owned())
            .spawn(move || {
                // The opener blocks on the other end of `ready_tx` until this send, so it cannot
                // fail.
                match connection::open(&thread_root, busy_timeout) {
                    Ok((connection, schema_version)) => {
                        let _ = ready_tx.send(Ok(schema_version));
                        serve(connection, request_rx);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|source| StoreError::ThreadSpawn { source })?;
        match ready_rx.recv() {
            Ok(Ok(schema_version)) => {
                log::info!(
                    "opened the store under {} in write-ahead mode at schema version \
                     {schema_version}",
                    data_root.display()
                );
                Ok(Self {
                    thread: Arc::new(StoreThread {
                        requests: Some(request_tx),
                        join: Some(join),
                    }),
                })
            }
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(StoreError::ThreadNotReady)
            }
        }
    }

    /// Runs `work` on the store thread and awaits its result, so an async caller never blocks a
    /// runtime worker on SQLite. `operation` names the request in an error and a log line, in place
    /// of anything it reads or writes. A typed method on this handle builds on it, and nothing
    /// outside the store module can call it, so every statement stays inside the module.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when another connection holds a lock `work` needs,
    /// [`StoreError::Request`] for any other failure of `work`, and [`StoreError::ThreadGone`]
    /// when the store thread has stopped.
    pub(in crate::store) async fn request<T, F>(
        &self,
        operation: &'static str,
        work: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let reply = self.send(operation, work)?;
        reply
            .await
            .map_err(|_| StoreError::ThreadGone { operation })?
    }

    /// [`request`](Self::request) for a synchronous caller, such as `wa` or a test, which blocks
    /// on the reply.
    ///
    /// # Errors
    /// As [`request`](Self::request).
    ///
    /// # Panics
    /// When called from async code, where tokio panics rather than let a blocked worker stall the
    /// runtime. Async code awaits [`request`](Self::request) instead.
    pub(in crate::store) fn request_blocking<T, F>(
        &self,
        operation: &'static str,
        work: F,
    ) -> Result<T, StoreError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let reply = self.send(operation, work)?;
        reply
            .blocking_recv()
            .map_err(|_| StoreError::ThreadGone { operation })?
    }

    /// Queues `work` for the store thread and returns the receiving half of its reply.
    fn send<T, F>(&self, operation: &'static str, work: F) -> Result<Reply<T>, StoreError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let Some(requests) = &self.thread.requests else {
            return Err(StoreError::ThreadGone { operation });
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        let request: Request = Box::new(move |connection| {
            let result =
                work(connection).map_err(|source| StoreError::from_request(operation, source));
            if reply_tx.send(result).is_err() {
                log::debug!(
                    "the caller of the store request {operation:?} stopped waiting, so its result \
                     is dropped"
                );
            }
        });
        requests
            .send(request)
            .map_err(|_| StoreError::ThreadGone { operation })?;
        Ok(reply_rx)
    }
}

impl Drop for StoreThread {
    fn drop(&mut self) {
        // Closing the channel ends the thread's loop once it has run every queued request.
        self.requests.take();
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                log::warn!("the store thread had already stopped, after a request panicked on it");
            }
        }
    }
}

/// The store thread's loop. The channel closes when the last [`Store`] drops, and `blocking_recv`
/// still returns every request queued before then, so the thread runs them all before it closes
/// the connection.
fn serve(mut connection: Connection, mut requests: request_channel::UnboundedReceiver<Request>) {
    while let Some(request) = requests.blocking_recv() {
        request(&mut connection);
    }
    match connection.close() {
        Ok(()) => log::debug!("the store thread closed the connection and stopped"),
        Err((_, error)) => log::warn!("the store connection did not close cleanly: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::connection::STORE_FILE_NAME;
    use crate::store::migrations::MIGRATIONS;
    use crate::test_support::{capture_logs, TempRoot};
    use log::Level;
    use rusqlite::{ErrorCode, OpenFlags, TransactionBehavior};
    use std::sync::mpsc;
    use std::time::Instant;

    fn create_scratch_table(store: &Store) {
        store
            .request_blocking("create the scratch table", |connection| {
                connection.execute_batch(
                    "CREATE TABLE scratch (id INTEGER PRIMARY KEY, writer TEXT NOT NULL)",
                )
            })
            .unwrap();
    }

    fn insert_row(connection: &Connection, writer: &str) -> rusqlite::Result<usize> {
        connection.execute("INSERT INTO scratch (writer) VALUES (?1)", [writer])
    }

    fn writers_in_order(store: &Store) -> Vec<String> {
        store
            .request_blocking("read the scratch rows", |connection| {
                connection
                    .prepare("SELECT writer FROM scratch ORDER BY id")?
                    .query_map([], |row| row.get(0))?
                    .collect()
            })
            .unwrap()
    }

    /// Runs a request on its own thread that takes the write lock, inserts a row as `writer`, and
    /// holds the lock with the row uncommitted until `release` yields or closes. The returned
    /// receiver yields once the lock is held.
    fn hold_the_write_lock(
        store: &Store,
        writer: &'static str,
        release: mpsc::Receiver<()>,
    ) -> (
        mpsc::Receiver<()>,
        thread::JoinHandle<Result<(), StoreError>>,
    ) {
        let (holding_tx, holding_rx) = mpsc::channel();
        let store = store.clone();
        let holder = thread::spawn(move || {
            store.request_blocking("hold the write lock", move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                insert_row(&transaction, writer)?;
                holding_tx.send(()).unwrap();
                release.recv().ok();
                transaction.commit()
            })
        });
        (holding_rx, holder)
    }

    #[test]
    fn a_request_runs_on_the_store_thread_and_returns_its_result() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();

        let (thread_name, sum) = store
            .request_blocking("add in SQL", |connection| {
                let sum: i64 = connection.query_row("SELECT 2 + 3", [], |row| row.get(0))?;
                Ok((thread::current().name().map(str::to_owned), sum))
            })
            .unwrap();

        assert_eq!(thread_name.as_deref(), Some("writing-assistant-store"));
        assert_eq!(sum, 5);
    }

    #[test]
    fn the_open_logs_one_info_line_naming_the_schema_version() {
        let root = TempRoot::new();
        let newest = MIGRATIONS.last().unwrap().number;

        let (store, lines) = capture_logs(|| Store::open(root.path()));

        store.unwrap();
        let info: Vec<_> = lines
            .iter()
            .filter(|(level, _)| *level <= Level::Info)
            .collect();
        assert_eq!(info.len(), 1, "{lines:?}");
        assert!(
            info[0].1.ends_with(&format!("at schema version {newest}")),
            "{lines:?}"
        );
    }

    /// On a runtime with one worker thread, the test's own task makes progress only if the
    /// in-flight request awaits its reply rather than blocking that thread. If the request blocked
    /// it, the release below would never be sent and the work would fail on its own timeout.
    #[tokio::test(flavor = "current_thread")]
    async fn an_awaited_request_leaves_its_runtime_free_while_sqlite_works() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let in_flight = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .request("wait on the store thread", move |connection| {
                        started_tx.send(()).unwrap();
                        let released = release_rx.recv_timeout(Duration::from_secs(5)).is_ok();
                        let one: i64 = connection.query_row("SELECT 1", [], |row| row.get(0))?;
                        Ok(released && one == 1)
                    })
                    .await
            }
        });

        started_rx.await.unwrap();
        release_tx.send(()).unwrap();
        assert!(in_flight.await.unwrap().unwrap());
    }

    #[test]
    fn a_failing_request_returns_its_error_and_the_thread_keeps_serving() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();

        let error = store
            .request_blocking("read a table that does not exist", |connection| {
                connection.query_row("SELECT count(*) FROM missing", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Request {
                    operation: "read a table that does not exist",
                    ..
                }
            ),
            "{error:?}"
        );

        create_scratch_table(&store);
        assert!(writers_in_order(&store).is_empty());
    }

    #[test]
    fn a_request_after_the_store_thread_has_stopped_returns_thread_gone() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();

        let panicked = store
            .request_blocking("panic on the store thread", |_| -> rusqlite::Result<()> {
                panic!("a request panicking on the store thread, as a test")
            })
            .unwrap_err();
        assert!(
            matches!(
                panicked,
                StoreError::ThreadGone {
                    operation: "panic on the store thread"
                }
            ),
            "{panicked:?}"
        );

        let later = store
            .request_blocking("ask after the thread stopped", |_| Ok(()))
            .unwrap_err();
        assert!(
            matches!(
                later,
                StoreError::ThreadGone {
                    operation: "ask after the thread stopped"
                }
            ),
            "{later:?}"
        );
    }

    #[test]
    fn dropping_the_last_handle_runs_the_queued_requests_and_closes_the_connection() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        create_scratch_table(&store);
        let wal = root.path().join(format!("{STORE_FILE_NAME}-wal"));
        assert!(wal.exists(), "a write in write-ahead mode leaves a log");

        let replies: Vec<_> = (0..20)
            .map(|_| {
                store
                    .send("queue a row", |connection| insert_row(connection, "queued"))
                    .unwrap()
            })
            .collect();
        let clone = store.clone();
        drop(store);
        assert!(wal.exists(), "a clone still holds the store open");
        drop(clone);

        for reply in replies {
            assert_eq!(reply.blocking_recv().unwrap().unwrap(), 1);
        }
        // SQLite removes the log when the last connection to the file closes.
        assert!(!wal.exists());
        let reopened = Store::open(root.path()).unwrap();
        assert_eq!(writers_in_order(&reopened).len(), 20);
    }

    /// The read-only connection stands in for the Python project, which opens the file with the
    /// same flag.
    #[test]
    fn a_read_only_connection_reads_while_the_writer_holds_rows_in_flight() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        create_scratch_table(&store);
        for _ in 0..3 {
            store
                .request_blocking("write a committed row", |connection| {
                    insert_row(connection, "committed")
                })
                .unwrap();
        }
        let (release_tx, release_rx) = mpsc::channel();
        let (holding, holder) = hold_the_write_lock(&store, "in flight", release_rx);
        holding.recv().unwrap();

        let reader = Connection::open_with_flags(
            root.path().join(STORE_FILE_NAME),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        // With no busy timeout, a read that had to wait on the writer would fail at once.
        reader.busy_timeout(Duration::ZERO).unwrap();
        let count = |reader: &Connection| -> i64 {
            reader
                .query_row("SELECT count(*) FROM scratch", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count(&reader), 3);
        assert_eq!(count(&reader), 3);

        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert_eq!(count(&reader), 4);
    }

    /// Two handles on one file, each with its own thread and connection, stand in for the app and
    /// `wa`, two processes running this code.
    #[test]
    fn two_stores_writing_at_once_wait_their_turn_and_every_row_lands() {
        const HOLD: Duration = Duration::from_millis(500);
        const ROWS_EACH: usize = 50;
        let root = TempRoot::new();
        let app = Store::open(root.path()).unwrap();
        let wa = Store::open(root.path()).unwrap();
        create_scratch_table(&app);

        let (release_tx, release_rx) = mpsc::channel();
        let (holding, holder) = hold_the_write_lock(&app, "app", release_rx);
        holding.recv().unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(HOLD);
            release_tx.send(()).unwrap();
        });
        let started = Instant::now();
        wa.request_blocking("write while the app holds the lock", |connection| {
            insert_row(connection, "wa")
        })
        .expect("the second write waits for the first inside the busy timeout");
        let waited = started.elapsed();
        releaser.join().unwrap();
        holder.join().unwrap().unwrap();
        assert!(
            waited >= HOLD / 2,
            "the second write returned after {waited:?}, so it never waited for the first"
        );
        assert_eq!(writers_in_order(&app), ["app", "wa"]);

        let writers: Vec<_> = [("app", app.clone()), ("wa", wa.clone())]
            .into_iter()
            .map(|(writer, store)| {
                thread::spawn(move || {
                    for _ in 0..ROWS_EACH {
                        store
                            .request_blocking("write a row", move |connection| {
                                insert_row(connection, writer)
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let rows = writers_in_order(&wa);
        assert_eq!(rows.len(), 2 * (ROWS_EACH + 1));
        for writer in ["app", "wa"] {
            let written = rows.iter().filter(|row| *row == writer).count();
            assert_eq!(written, ROWS_EACH + 1, "rows written by {writer}");
        }
    }

    #[test]
    fn a_write_that_outwaits_the_busy_timeout_returns_busy_and_the_store_serves_on() {
        let root = TempRoot::new();
        let app = Store::open(root.path()).unwrap();
        let wa = Store::open_with_busy_timeout(root.path(), Duration::from_millis(50)).unwrap();
        create_scratch_table(&app);

        let (release_tx, release_rx) = mpsc::channel();
        let (holding, holder) = hold_the_write_lock(&app, "app", release_rx);
        holding.recv().unwrap();
        let error = wa
            .request_blocking("write past the busy timeout", |connection| {
                insert_row(connection, "wa")
            })
            .unwrap_err();
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();

        match &error {
            StoreError::Busy { operation, source } => {
                assert_eq!(*operation, "write past the busy timeout");
                assert_eq!(source.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
            }
            other => panic!("expected a busy error, got {other:?}"),
        }
        wa.request_blocking("write once the lock is free", |connection| {
            insert_row(connection, "wa")
        })
        .unwrap();
        assert_eq!(writers_in_order(&app), ["app", "wa"]);
    }

    /// The measurement behind the statement on [`BUSY_TIMEOUT`] that only an `IMMEDIATE` write
    /// transaction waits.
    #[test]
    fn a_deferred_transaction_that_reads_first_fails_its_write_without_waiting() {
        let root = TempRoot::new();
        let app = Store::open(root.path()).unwrap();
        let wa = Store::open(root.path()).unwrap();
        create_scratch_table(&app);

        let (has_read_tx, has_read_rx) = mpsc::channel();
        let (locked_tx, locked_rx) = mpsc::channel::<()>();
        let deferred = thread::spawn({
            let wa = wa.clone();
            move || {
                wa.request_blocking(
                    "read, then write, in one deferred transaction",
                    move |connection| {
                        let transaction = connection.transaction()?;
                        let _: i64 =
                            transaction
                                .query_row("SELECT count(*) FROM scratch", [], |row| row.get(0))?;
                        has_read_tx.send(()).unwrap();
                        locked_rx.recv().unwrap();
                        let started = Instant::now();
                        let write = insert_row(&transaction, "wa");
                        Ok((
                            write.map_err(|error| error.sqlite_error_code()),
                            started.elapsed(),
                        ))
                    },
                )
            }
        });
        has_read_rx.recv().unwrap();
        let (release_tx, release_rx) = mpsc::channel();
        let (holding, holder) = hold_the_write_lock(&app, "app", release_rx);
        holding.recv().unwrap();
        locked_tx.send(()).unwrap();

        let (write, took) = deferred.join().unwrap().unwrap();
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();

        assert_eq!(write, Err(Some(ErrorCode::DatabaseBusy)));
        assert!(
            took < BUSY_TIMEOUT / 5,
            "the write took {took:?}, so it waited on the busy timeout"
        );
    }
}
