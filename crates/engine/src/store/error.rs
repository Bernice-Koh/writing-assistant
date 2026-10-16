//! Errors from resolving the data root, reading or writing the config file under it, opening the
//! store file, and the requests the store thread serves.

use std::path::PathBuf;

use thiserror::Error;

/// The store module's one error type.
///
/// No variant carries the text of a row, and neither does any log line the module writes. A
/// variant names a path, an operation or a journal mode beside the underlying error. Every
/// statement binds its values as parameters and never formats one into its SQL, so the statement
/// text and SQLite's message in a [`rusqlite::Error`] name a table, a column or a constraint, and
/// never a value.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("could not determine the local application-data directory that holds the data root")]
    NoDataRoot,

    #[error("could not read {path}: {source}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}: {source}", path = path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse config at {path}: {source}", path = path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// SQLite could not open the store file or set one of the pragmas every connection carries.
    #[error("could not open the store {path}: {source}", path = path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// SQLite answered the request for write-ahead mode with another journal mode.
    #[error("the store {path} is in journal mode {mode:?}, not write-ahead", path = path.display())]
    JournalMode { path: PathBuf, mode: String },

    #[error("could not start the store thread: {source}")]
    ThreadSpawn {
        #[source]
        source: std::io::Error,
    },

    #[error("the store thread ended before it reported whether the store opened")]
    ThreadNotReady,

    /// Another connection held the lock the request needed.
    #[error(
        "the store request {operation:?} found the store locked by another connection: {source}"
    )]
    Busy {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("the store request {operation:?} failed: {source}")]
    Request {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    /// The store thread stopped before it replied, which happens only when a request panics on it.
    #[error("the store thread has stopped, so the request {operation:?} got no reply")]
    ThreadGone { operation: &'static str },
}

impl StoreError {
    /// The error for a request whose work failed: [`StoreError::Busy`] when SQLite reports another
    /// connection's lock, and [`StoreError::Request`] for any other failure.
    pub(super) fn from_request(operation: &'static str, source: rusqlite::Error) -> Self {
        if source.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) {
            Self::Busy { operation, source }
        } else {
            Self::Request { operation, source }
        }
    }
}
