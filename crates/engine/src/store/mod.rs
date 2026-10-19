//! Local persistence, under the one data root that [`data_root`] resolves for the app and `wa`
//! alike. The root holds [`config`], the JSON settings file, and the store: one SQLite file,
//! `store.sqlite`, in write-ahead mode, whose connection one store thread owns and the rest of the
//! engine reaches through a [`Store`] handle. The schema is a list of numbered SQL migrations
//! compiled into the engine, applied in order at open and each recorded in `schema_version`.
//! Beside it, `passages` holds each [`Passage`] with the [`Split`] it was assigned at insert;
//! nothing writes a passage until Phase 4. The Card tables hold each Card version as a
//! [`PublishedCard`], and the ruleset tables each ruleset version as a [`PublishedRuleset`];
//! nothing writes a Card until Phase 3 or a ruleset until #95. `candidates` holds each
//! [`Candidate`], a model's rewrite of a passage with its generation provenance, and
//! `screen_results` each [`ScreenResult`], one screening of a candidate; nothing writes a
//! candidate until Phase 4 or a screen result until the screen exists. Every table is insert-only:
//! triggers raise on update and on delete. The engine's code is the store's only writer, run by the app and `wa` as two
//! processes, so the connection carries a busy timeout.
//!
//! Tauri points WebView2's user-data folder at the same root, so nothing in this module
//! enumerates, copies, sizes or clears the root: every operation names the files it touches.

mod candidates;
mod card;
pub mod config;
mod connection;
pub mod error;
mod items;
mod json;
mod migrations;
mod passages;
mod ruleset;
mod screen_results;
mod split;
mod thread;

use std::path::PathBuf;

use directories::BaseDirs;

pub use candidates::{Candidate, CandidateId, GenerationProvenance};
pub use card::{
    CardMember, CardVersionId, ClauseTextId, ClauseWording, ExamplePair, PublishedCard,
    PublishedClause,
};
pub use config::Config;
pub use error::StoreError;
pub use items::{Contrast, Item, ItemId, ItemSlot, ItemType, NewItem};
pub use passages::{Passage, PassageId, PassageKind};
pub use ruleset::{
    CheckerAuthor, CheckerId, Provenance, PublishedRuleset, RulesetPublish, StoredChecker,
    TestResult,
};
pub use screen_results::{KillingRule, ScreenResult, ScreenResultId, StoredScreenResult};
pub use split::Split;
pub use thread::Store;

/// The bundle identifier `tauri.conf.json` declares, and the data root's folder name. Tauri names
/// its own per-app folders by the same string; a test holds the two equal.
pub const IDENTIFIER: &str = "com.bernicekoh.writing-assistant";

/// The data root, `%LOCALAPPDATA%\com.bernicekoh.writing-assistant`: the one folder for the
/// settings file, the store, model files, adapters and Card renderings. It sits under the local
/// application-data directory rather than the roaming one, because a store with model files does
/// not belong in a roaming profile, which Windows copies to and from a server at every sign-in and
/// sign-out.
///
/// The lookup creates nothing on disk; a writer creates the folder when it first writes. Tauri
/// points WebView2's user-data folder at the same path and creates it when it builds a webview, so
/// WebView2's `EBWebView` folder sits beside the engine's files.
pub fn data_root() -> Result<PathBuf, StoreError> {
    BaseDirs::new()
        .map(|dirs| dirs.data_local_dir().join(IDENTIFIER))
        .ok_or(StoreError::NoDataRoot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn the_data_root_is_the_identifier_under_local_app_data() {
        // The environment variable is a second route to the same known folder, so the expected
        // path does not restate the lookup under test.
        let local_app_data =
            std::env::var_os("LOCALAPPDATA").expect("Windows sets LOCALAPPDATA for every account");
        let root = data_root().expect("the local application-data directory should resolve");
        assert_eq!(root, PathBuf::from(local_app_data).join(IDENTIFIER));
    }

    #[test]
    fn the_identifier_matches_the_one_tauri_declares() {
        let contents = std::fs::read_to_string(test_support::tauri_config_path())
            .expect("the Tauri configuration is committed beside the app");
        let tauri_config: serde_json::Value =
            serde_json::from_str(&contents).expect("the Tauri configuration is valid JSON");
        assert_eq!(tauri_config["identifier"], IDENTIFIER);
    }
}
