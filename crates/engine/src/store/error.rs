//! Errors from resolving the data root, reading or writing the config file under it, opening the
//! store file and migrating its schema, and the requests the store thread serves.

use std::path::PathBuf;

use thiserror::Error;

use super::candidates::CandidateId;
use super::card::{CardVersionId, ClauseTextId};
use super::passages::PassageId;
use super::ruleset::CheckerId;
use crate::flag::{ClauseId, RulesetVersion};

/// The store module's one error type.
///
/// No variant carries the text of a row, and neither does any log line the module writes. A
/// variant names a path, an operation, a journal mode, a migration number or a row's id beside the
/// underlying error. Every statement binds its values as parameters and never formats one into
/// its SQL, so the statement text and SQLite's message in a [`rusqlite::Error`] name a table, a
/// column or a constraint, and never a value.
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

    /// SQLite could not open the store file, set one of the pragmas every connection carries, or
    /// read the migrations the store records.
    #[error("could not open the store {path}: {source}", path = path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// SQLite answered the request for write-ahead mode with another journal mode.
    #[error("the store {path} is in journal mode {mode:?}, not write-ahead", path = path.display())]
    JournalMode { path: PathBuf, mode: String },

    /// The store records a migration whose hash this build's migration of that number does not
    /// match, so the migration was edited after it was applied, and the open leaves the file as it
    /// is.
    #[error(
        "the store {path} records migration {number}, and this build's migration {number} no \
         longer matches it",
        path = path.display()
    )]
    MigrationChanged { path: PathBuf, number: u32 },

    /// The store records a migration newer than any this build carries, so a later build wrote it,
    /// and the open leaves the file as it is.
    #[error(
        "the store {path} is at schema version {version}, above this build's newest migration, \
         {newest}",
        path = path.display()
    )]
    SchemaNewerThanBuild {
        path: PathBuf,
        version: u32,
        newest: u32,
    },

    /// A migration's SQL, or the row recording it, failed, and its transaction rolled back, so the
    /// store stays at the version it had.
    #[error(
        "migration {number} failed on the store {path} and rolled back: {source}",
        path = path.display()
    )]
    MigrationFailed {
        path: PathBuf,
        number: u32,
        #[source]
        source: rusqlite::Error,
    },

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

    /// A read, or a candidate, named a passage id the store does not hold.
    #[error("the store holds no passage with id {id}", id = id.0)]
    NoPassage { id: PassageId },

    /// A Card version's member, or a checker, named a clause id the store does not hold.
    #[error("the store holds no clause with id {id}", id = id.0)]
    NoClause { id: ClauseId },

    /// A Card version's member named a clause text id the store does not hold.
    #[error("the store holds no clause text with id {id}", id = id.0)]
    NoClauseText { id: ClauseTextId },

    /// A Card version's members held two wordings of one clause.
    #[error("a Card version lists clause {id} more than once", id = id.0)]
    ClauseRepeated { id: ClauseId },

    /// A read named a Card version id the store does not hold.
    #[error("the store holds no Card version with id {id}", id = id.0)]
    NoCardVersion { id: CardVersionId },

    /// A ruleset version's members named a checker id the store does not hold.
    #[error("the store holds no checker with id {id}", id = id.0)]
    NoChecker { id: CheckerId },

    /// A ruleset version's members named one checker twice.
    #[error("a ruleset version lists checker {id} more than once", id = id.0)]
    CheckerRepeated { id: CheckerId },

    /// A read, or a screen result, named a ruleset version the store does not hold.
    #[error("the store holds no ruleset version {version}", version = version.0)]
    NoRulesetVersion { version: RulesetVersion },

    /// A read named a candidate id the store does not hold.
    #[error("the store holds no candidate with id {id}", id = id.0)]
    NoCandidate { id: CandidateId },

    /// A screen result named a candidate content hash no stored candidate has.
    #[error("the store holds no candidate with content hash {content_hash}")]
    NoCandidateHash { content_hash: String },
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
