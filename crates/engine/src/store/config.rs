//! Settings, persisted as one JSON file, `config.json`, directly under the data root that
//! [`data_root`](super::data_root) resolves. This module holds settings only. The app loads the
//! file once at startup, so an edit made while it runs takes effect at the next start. Until the
//! settings shell arrives in Phase 3, the file is edited by hand. Its shape:
//!
//! ```json
//! {
//!   "checking_enabled": true,
//!   "default_register": "default",
//!   "register_map": { "<surface key>": "<register>" },
//!   "telltale_toggles": { "<pattern id>": true, "curly-quotes": false }
//! }
//! ```
//!
//! - `checking_enabled`: whether checking runs. Its control and its reader arrive with the
//!   settings shell.
//! - `default_register`: the register of a surface the map has no entry for.
//! - `register_map`: surface key to register, empty by default. A key is an opaque string until
//!   the capture layer's surface identity gives it a meaning in Phase 2.
//! - `telltale_toggles`: telltale pattern id to on or off. The file written on first start lists
//!   every catalogue pattern. A pattern the map omits takes its default, off for `curly-quotes`
//!   and on for every other pattern, and an id the catalogue does not hold is dropped on load.
//!
//! Every field has a default, so a file that lacks any of them still parses. No field holds a
//! credential or any text of a draft.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::StoreError;
use crate::style::ai_tell;

const CONFIG_FILE_NAME: &str = "config.json";

const DEFAULT_REGISTER: &str = "default";

/// Off by default because Word autoformats straight quotation marks into curly ones, and the owner
/// writes in Word.
const TELLTALES_OFF_BY_DEFAULT: &[&str] = &["curly-quotes"];

/// The settings `config.json` holds; the module comment gives the file's shape. The maps are
/// `BTreeMap`s so a saved file lists its entries in a stable order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_checking_enabled")]
    pub checking_enabled: bool,
    #[serde(default = "default_register_name")]
    pub default_register: String,
    #[serde(default)]
    pub register_map: BTreeMap<String, String>,
    #[serde(default = "default_telltale_toggles")]
    pub telltale_toggles: BTreeMap<String, bool>,
}

impl Config {
    /// The register of the surface `surface_key` names: its entry in the register map, or the
    /// default register when the map has none.
    pub fn register_for(&self, surface_key: &str) -> &str {
        self.register_map
            .get(surface_key)
            .unwrap_or(&self.default_register)
    }

    /// Whether the telltale pattern `pattern_id` is on: its toggle, or the pattern's default when
    /// the toggles omit it.
    pub fn is_telltale_enabled(&self, pattern_id: &str) -> bool {
        self.telltale_toggles
            .get(pattern_id)
            .copied()
            .unwrap_or_else(|| telltale_enabled_by_default(pattern_id))
    }

    /// Drops every toggle whose id the catalogue does not hold, with one `warn` line per id, so a
    /// mistyped or retired id in a hand-edited file neither fails the load nor reaches a reader.
    fn drop_unknown_telltale_toggles(&mut self, path: &Path) {
        self.telltale_toggles.retain(|id, _| {
            let known = ai_tell::pattern_ids().any(|catalogue_id| catalogue_id == id);
            if !known {
                log::warn!(
                    "ignoring the toggle for {id:?} in {}: the telltale catalogue holds no \
                     pattern with that id",
                    path.display()
                );
            }
            known
        });
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            checking_enabled: default_checking_enabled(),
            default_register: default_register_name(),
            register_map: BTreeMap::new(),
            telltale_toggles: default_telltale_toggles(),
        }
    }
}

fn default_checking_enabled() -> bool {
    true
}

fn default_register_name() -> String {
    DEFAULT_REGISTER.to_owned()
}

/// Every catalogue pattern at its default, so the file written on first start lists every pattern
/// that can be switched.
fn default_telltale_toggles() -> BTreeMap<String, bool> {
    ai_tell::pattern_ids()
        .map(|id| (id.to_owned(), telltale_enabled_by_default(id)))
        .collect()
}

fn telltale_enabled_by_default(pattern_id: &str) -> bool {
    !TELLTALES_OFF_BY_DEFAULT.contains(&pattern_id)
}

/// Loads the config from under `data_root`, writing and returning the default if no config file
/// exists yet, satisfying "a JSON config file is created on first run" without a separate
/// first-run code path: the first `load` call after install is indistinguishable from any other.
/// The root is a parameter so that a test hands in a temporary directory and never touches the
/// real one.
///
/// A file that does not parse is left as it is and returns [`StoreError::Parse`], whose source
/// carries the fault's line and column. A leading byte order mark is skipped, since some Windows
/// editors write one into a UTF-8 file.
pub fn load(data_root: &Path) -> Result<Config, StoreError> {
    let path = data_root.join(CONFIG_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let json = contents.strip_prefix('\u{feff}').unwrap_or(&contents);
            let mut config: Config =
                serde_json::from_str(json).map_err(|source| StoreError::Parse {
                    path: path.clone(),
                    source,
                })?;
            config.drop_unknown_telltale_toggles(&path);
            Ok(config)
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

    let json = serde_json::to_string_pretty(config).expect(
        "Config holds only booleans, strings and string-keyed maps, none of which can fail to \
         serialise",
    );
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
    use crate::test_support;
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

    /// A fresh data root holding a config file with `contents`, as a hand edit would leave it.
    fn root_holding(contents: &str) -> PathBuf {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE_NAME), contents).unwrap();
        dir
    }

    #[test]
    fn loading_from_an_empty_directory_writes_and_returns_the_defaults() {
        let dir = unique_temp_dir();
        let config = load(&dir).expect("loading into a fresh directory should succeed");
        assert_eq!(config, Config::default());

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(CONFIG_FILE_NAME))
                .expect("the default should have been written"),
        )
        .unwrap();
        assert_eq!(written["checking_enabled"], true);
        assert_eq!(written["default_register"], "default");
        assert_eq!(written["register_map"], serde_json::json!({}));
        let toggles = written["telltale_toggles"].as_object().unwrap();
        assert_eq!(toggles.len(), ai_tell::pattern_ids().count());
        for id in ai_tell::pattern_ids() {
            assert_eq!(toggles[id], id != "curly-quotes", "toggle for {id:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_holding_only_checking_enabled_takes_every_other_default() {
        let dir = root_holding(r#"{ "checking_enabled": false }"#);
        let config = load(&dir).expect("a file lacking fields should still parse");
        assert_eq!(
            config,
            Config {
                checking_enabled: false,
                ..Config::default()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pattern_absent_from_the_toggles_takes_its_default() {
        let dir = root_holding(r#"{ "telltale_toggles": { "emojis": false } }"#);
        let config = load(&dir).unwrap();
        assert!(!config.is_telltale_enabled("emojis"));
        assert!(!config.is_telltale_enabled("curly-quotes"));
        assert!(config.is_telltale_enabled("em-en-dashes"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_toggle_overrides_the_default_in_either_direction() {
        let dir = root_holding(
            r#"{ "telltale_toggles": { "curly-quotes": true, "em-en-dashes": false } }"#,
        );
        let config = load(&dir).unwrap();
        assert!(config.is_telltale_enabled("curly-quotes"));
        assert!(!config.is_telltale_enabled("em-en-dashes"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_toggle_for_an_unknown_pattern_is_dropped_with_one_warn_line() {
        let dir = root_holding(
            r#"{ "telltale_toggles": { "no-such-pattern": false, "emojis": false } }"#,
        );
        let (loaded, lines) = test_support::capture_logs(|| load(&dir));
        let config = loaded.expect("an unknown id should not fail the load");

        assert!(!config.telltale_toggles.contains_key("no-such-pattern"));
        assert_eq!(config.telltale_toggles.get("emojis"), Some(&false));
        let warnings: Vec<_> = lines
            .iter()
            .filter(|(level, _)| *level == log::Level::Warn)
            .collect();
        assert_eq!(warnings.len(), 1, "{lines:#?}");
        assert!(warnings[0].1.contains("\"no-such-pattern\""), "{lines:#?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_mapped_surface_key_returns_its_register_and_any_other_returns_the_default() {
        let dir = root_holding(r#"{ "register_map": { "WINWORD.EXE": "formal" } }"#);
        let config = load(&dir).unwrap();
        assert_eq!(config.register_for("WINWORD.EXE"), "formal");
        assert_eq!(config.register_for("chrome.exe"), "default");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_does_not_parse_returns_the_fault_and_is_left_as_it_is() {
        let contents = "{\n  \"checking_enabled\": true,\n}";
        let dir = root_holding(contents);
        let path = dir.join(CONFIG_FILE_NAME);

        match load(&dir) {
            Err(StoreError::Parse {
                path: fault_path,
                source,
            }) => {
                assert_eq!(fault_path, path);
                assert_eq!((source.line(), source.column()), (3, 1), "{source}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), contents.as_bytes());
        assert!(!dir.join(format!("{CONFIG_FILE_NAME}.tmp")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_opens_with_a_byte_order_mark_parses() {
        let dir = root_holding("\u{feff}{ \"checking_enabled\": false }");
        assert!(!load(&dir).unwrap().checking_enabled);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_pattern_off_by_default_is_in_the_catalogue() {
        // `curly-quotes` is the one today; a pattern renamed in the catalogue would otherwise
        // leave its default naming nothing.
        for id in TELLTALES_OFF_BY_DEFAULT {
            assert!(
                ai_tell::pattern_ids().any(|catalogue_id| catalogue_id == *id),
                "{id:?} is not a catalogue pattern id"
            );
        }
        assert!(TELLTALES_OFF_BY_DEFAULT.contains(&"curly-quotes"));
    }

    #[test]
    fn a_saved_setting_round_trips_through_a_fresh_load() {
        let dir = unique_temp_dir();
        let mut changed = Config {
            checking_enabled: false,
            default_register: "plain".to_owned(),
            ..Config::default()
        };
        changed
            .register_map
            .insert("WINWORD.EXE".to_owned(), "formal".to_owned());
        changed
            .telltale_toggles
            .insert("curly-quotes".to_owned(), true);
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
                ..Config::default()
            },
        )
        .unwrap();
        assert!(!dir.join(format!("{CONFIG_FILE_NAME}.tmp")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
