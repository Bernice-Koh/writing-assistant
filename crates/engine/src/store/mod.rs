//! Local persistence, under the one data root that [`data_root`] resolves for the app and `wa`
//! alike. Today the root holds only [`config`], the JSON settings file. The SQLite store for Card
//! versions, rulesets, passages, candidates and events is not yet implemented; the engine's code is
//! its only writer.

pub mod config;
pub mod error;

use std::path::PathBuf;

use directories::BaseDirs;

pub use config::Config;
pub use error::StoreError;

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
