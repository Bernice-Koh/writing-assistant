//! Settings and configuration, persisted as JSON in the OS-appropriate config directory. Profiles,
//! the exemplar corpus, the Style Card, and training pairs stay out of this module until
//! onboarding exists to produce them; SQLite is deferred to that phase too.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use super::error::StoreError;

/// Decomposes the identifier already declared in `tauri.conf.json`
/// (`com.bernicekoh.writing-assistant`), so the config directory this resolves to and the app's
/// own bundle identifier agree, rather than picking a separate, unrelated triple.
const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "bernicekoh";
const APPLICATION: &str = "writing-assistant";

const CONFIG_FILE_NAME: &str = "config.json";

/// Settings the user changes through the tray or settings window. `checking_enabled` backs
/// `src/stores/shell-store.ts`'s existing `isCheckingEnabled` flag, today session-only; wiring
/// that store to load and save through this config is later work, not this module's own.
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

/// The OS-appropriate config directory for this app, via the `directories` crate: for example
/// `%APPDATA%\bernicekoh\writing-assistant\config` on Windows. Does not create it; callers that
/// need it to exist create it themselves, since a mere directory lookup should not have a
/// filesystem side effect.
pub fn config_dir() -> Result<PathBuf, StoreError> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or(StoreError::NoConfigDir)
}

/// Loads the config from the OS config directory, writing and returning the default if no config
/// file exists yet, satisfying "a JSON config file is created on first run" without a separate
/// first-run code path: the first `load` call after install is indistinguishable from any other.
pub fn load() -> Result<Config, StoreError> {
    load_from(&config_dir()?)
}

/// Writes `config` to the OS config directory. Atomic: written to a sibling temporary file first,
/// then renamed into place, so a crash or power loss mid-write cannot leave a truncated or
/// half-written config file where the real one belongs.
pub fn save(config: &Config) -> Result<(), StoreError> {
    save_to(&config_dir()?, config)
}

fn load_from(dir: &Path) -> Result<Config, StoreError> {
    let path = dir.join(CONFIG_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| StoreError::Parse { path, source })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = Config::default();
            save_to(dir, &config)?;
            Ok(config)
        }
        Err(source) => Err(StoreError::Read { path, source }),
    }
}

fn save_to(dir: &Path, config: &Config) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir).map_err(|source| StoreError::Write {
        path: dir.to_path_buf(),
        source,
    })?;

    let json = serde_json::to_string_pretty(config)
        .expect("Config has only primitive fields, none of which can fail to serialize");
    let final_path = dir.join(CONFIG_FILE_NAME);
    let temp_path = dir.join(format!("{CONFIG_FILE_NAME}.tmp"));
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
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory under the OS temp directory unique to this call, so concurrently running
    /// tests never read or write each other's config file. `std::process::id()` separates
    /// concurrent test binaries (unlikely here, but cheap to rule out); the atomic counter
    /// separates concurrent tests within this one binary, the collision that actually matters
    /// since `cargo test` runs tests in parallel by default.
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
        let config = load_from(&dir).expect("loading into a fresh directory should succeed");
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
        save_to(&dir, &changed).expect("saving into a fresh directory should succeed");

        let reloaded = load_from(&dir).expect("loading a config just saved should succeed");
        assert_eq!(reloaded, changed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_twice_leaves_no_leftover_temp_file() {
        let dir = unique_temp_dir();
        save_to(&dir, &Config::default()).unwrap();
        save_to(
            &dir,
            &Config {
                checking_enabled: false,
            },
        )
        .unwrap();
        assert!(!dir.join(format!("{CONFIG_FILE_NAME}.tmp")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_dir_resolves_under_the_app_identifier() {
        // Only asserts this app's own directory name is somewhere in the resolved path, not its
        // exact position: on Windows, `ProjectDirs::config_dir` appends a further `config`
        // segment after it, which Linux and macOS do not, per `directories`' own documented
        // per-platform table. The OS-specific parent itself (AppData, XDG_CONFIG_HOME, and so
        // on) is `directories`' own responsibility to get right, not this module's to re-verify.
        let dir =
            config_dir().expect("directories should resolve a config dir on any supported OS");
        assert!(
            dir.components()
                .any(|component| component.as_os_str() == APPLICATION),
            "expected {APPLICATION:?} somewhere in {dir:?}"
        );
    }
}
