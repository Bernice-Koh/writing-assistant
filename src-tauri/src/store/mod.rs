//! Local persistence. Today only [`config`], the JSON settings file. The SQLite store for Card
//! versions, rulesets, passages, candidates and events is not yet implemented; the app is its
//! only writer.

pub mod config;
pub mod error;

pub use config::Config;
pub use error::StoreError;
