//! The local store: profiles, exemplar corpus and its index, Style Card, training pairs,
//! adapters, and config. Everything here stays on the machine as files the user can open,
//! edit, or delete.
//!
//! Only [`config`] is implemented so far, scoped to settings only. Profiles, the corpus, the
//! Style Card, and training pairs all need onboarding to produce them, and SQLite is deferred to
//! that phase too.

pub mod config;
pub mod error;

pub use config::Config;
pub use error::StoreError;
