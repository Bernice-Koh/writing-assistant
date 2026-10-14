//! Settings, persisted as one JSON file, `config.json`, directly under the data root that
//! [`data_root`](super::data_root) resolves. This module holds settings only, and nothing in the
//! app loads or saves it yet.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::StoreError;

const CONFIG_FILE_NAME: &str = "config.json";

/// Settings the user changes through the tray or settings window. No control sets
/// `checking_enabled` today: the one for it arrives with the settings shell, wired to this
/// config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_checking_enabled")]
    pub checking_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            checking_enabled: default_checking_enabled(),
        }
    }
}

fn default_checking_enabled() -> bool {
    true
}

/// Loads the config from under `data_root`, writing and returning the default if no config file
/// exists yet, satisfying "a JSON config file is created on first run" without a separate
/// first-run code path: the first `load` call after install is indistinguishable from any other.
/// The root is a parameter so that a test hands in a temporary directory and never touches the
/// real one.
pub fn load(data_root: &Path) -> Result<Config, StoreError> {
    let path = data_root.join(CONFIG_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| StoreError::Parse { path, source })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = Config::default();
            save(data_root, &config)?;
            Ok(config)
        }
        Err(source) => Err(StoreError::Read { path, source }),
    }
}

/// Writes `config` under `data_root`, creating the root if it does not exist yet. Atomic: written
/// to a sibling temporary file first, then renamed into place, so a crash or power loss mid-write
/// cannot leave a truncated or half-written config file where the real one belongs.
pub fn save(data_root: &Path, config: &Config) -> Result<(), StoreError> {
    std::fs::create_dir_all(data_root).map_err(|source| StoreError::Write {
        path: data_root.to_path_buf(),
        source,
    })?;

    let json = serde_json::to_string_pretty(config)
        .expect("Config has only primitive fields, none of which can fail to serialize");
    let final_path = data_root.join(CONFIG_FILE_NAME);
    let temp_path = data_root.join(format!("{CONFIG_FILE_NAME}.tmp"));
    std::fs::write(&temp_path, json).map_err(|source| StoreError::Write {
        path: temp_path.clone(),
        source,
    })?;
    std::fs::rename(&temp_path, &final_path).map_err(|source| StoreError::Write {
        path: final_path,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory under the OS temp directory unique to this call, handed in as the data root so
    /// no test touches the real one, and concurrently running tests never read or write each
    /// other's config file. `std::process::id()` separates concurrent test binaries (unlikely
    /// here, but cheap to rule out); the atomic counter separates concurrent tests within this
    /// one binary, the collision that actually matters since `cargo test` runs tests in parallel
    /// by default.
    fn unique_temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "writing-assistant-store-test-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn loading_from_an_empty_directory_creates_and_returns_the_default() {
        let dir = unique_temp_dir();
        let config = load(&dir).expect("loading into a fresh directory should succeed");
        assert_eq!(config, Config::default());
        assert!(
            dir.join(CONFIG_FILE_NAME).exists(),
            "the default should have been written"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_saved_setting_round_trips_through_a_fresh_load() {
        let dir = unique_temp_dir();
        let changed = Config {
            checking_enabled: false,
        };
        save(&dir, &changed).expect("saving into a fresh directory should succeed");

        let reloaded = load(&dir).expect("loading a config just saved should succeed");
        assert_eq!(reloaded, changed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_twice_leaves_no_leftover_temp_file() {
        let dir = unique_temp_dir();
        save(&dir, &Config::default()).unwrap();
        save(
            &dir,
            &Config {
                checking_enabled: false,
            },
        )
        .unwrap();
        assert!(!dir.join(format!("{CONFIG_FILE_NAME}.tmp")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
