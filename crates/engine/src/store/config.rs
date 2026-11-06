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
//! - `register_map`: surface key to register, empty by default. A key is the executable name of
//!   the focused document's process, such as `WINWORD.EXE`, or that name, a space and a page's
//!   host, such as `msedge.exe github.com`, which gives the pages on that host a register of their
//!   own. The process name matches ignoring ASCII case, since Windows treats executable names as
//!   case-insensitive, so `winword.exe` matches Word's process; the host matches as a URL parser
//!   normalises it. A key keeps the spelling it was written with. The analyzer resolves the
//!   register at each recheck by [`Config::register_for`].
//! - `telltale_toggles`: telltale pattern id to on or off. The file written on first start lists
//!   every catalogue pattern. A pattern the map omits takes its default, off for `curly-quotes`
//!   and on for every other pattern, and an id the catalogue does not hold is dropped on load.
//!
//! Every field has a default, so a file that lacks any of them still parses. A top-level key that
//! names no field is ignored, with one `warn` line naming it, so a misspelt key does not silently
//! leave its setting at the default. No field holds a credential or any text of a draft.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use super::error::StoreError;
use crate::capture::Surface;
use crate::style::ai_tell;

const CONFIG_FILE_NAME: &str = "config.json";

const DEFAULT_REGISTER: &str = "default";

/// The top-level keys of the file, one per field of [`Config`].
const KNOWN_KEYS: [&str; 4] = [
    "checking_enabled",
    "default_register",
    "register_map",
    "telltale_toggles",
];

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
    /// The register a check on `surface` runs under: the register map's entry for the surface's
    /// process name, a space and its page URL's host, then its entry for the process name alone,
    /// then the default register. A surface with no page URL, or with a URL that has no host, such
    /// as a `file:` URL of a local path, is read by its process name alone.
    ///
    /// A key's process name matches ignoring ASCII case, and its host matches once the URL parser
    /// has normalised it as it normalises the page URL's, so the map is scanned rather than looked
    /// up by exact key: it holds a handful of entries and is read once per recheck. Of two keys that
    /// match one surface in the same form, the first in the map's order wins.
    pub fn register_for(&self, surface: &Surface) -> &str {
        let process = surface.process_name.as_str();
        surface
            .page_host()
            .and_then(|host| {
                self.registers().find_map(|(key, register)| {
                    let (key_process, key_host) = key.split_once(' ')?;
                    let host_matches =
                        normalised_host(key_host).is_some_and(|normalised| normalised == host);
                    (key_process.eq_ignore_ascii_case(process) && host_matches).then_some(register)
                })
            })
            .or_else(|| {
                self.registers().find_map(|(key, register)| {
                    (!key.contains(' ') && key.eq_ignore_ascii_case(process)).then_some(register)
                })
            })
            .unwrap_or(&self.default_register)
    }

    /// The register map's entries, in the map's order.
    fn registers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.register_map
            .iter()
            .map(|(key, register)| (key.as_str(), register.as_str()))
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

/// `host`, a register map key's host, as the URL parser normalises a web page's host, so that it
/// compares with [`Surface::page_host`]; `None` when it is not a host.
fn normalised_host(host: &str) -> Option<String> {
    url::Host::parse(host).ok().map(|parsed| parsed.to_string())
}

/// Loads the config from under `data_root`, writing and returning the default if no config file
/// exists yet, satisfying "a JSON config file is created on first run" without a separate
/// first-run code path: the first `load` call after install is indistinguishable from any other.
/// The root is a parameter so that a test hands in a temporary directory and never touches the
/// real one.
///
/// A file that does not parse is left as it is and returns [`StoreError::Parse`], whose source
/// carries the fault's line and column. A leading byte order mark is skipped, since some Windows
/// editors write one into a UTF-8 file. A top-level key that names no setting is ignored with one
/// `warn` line, and the load succeeds.
pub fn load(data_root: &Path) -> Result<Config, StoreError> {
    match load_existing(data_root)? {
        Some(config) => Ok(config),
        None => {
            let config = Config::default();
            save(data_root, &config)?;
            Ok(config)
        }
    }
}

/// The config under `data_root` read as [`load`] reads it, or `None` when no config file exists,
/// in which case nothing is written. `wa` reads the settings through this, so the app stays the
/// one writer of the defaults, as it is the one creator of the store.
///
/// # Errors
/// As [`load`], short of a failed write.
pub fn load_existing(data_root: &Path) -> Result<Option<Config>, StoreError> {
    let path = data_root.join(CONFIG_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let json = contents.strip_prefix('\u{feff}').unwrap_or(&contents);
            let mut config: Config =
                serde_json::from_str(json).map_err(|source| StoreError::Parse {
                    path: path.clone(),
                    source,
                })?;
            warn_of_unknown_keys(json, &path);
            config.drop_unknown_telltale_toggles(&path);
            Ok(Some(config))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StoreError::Read { path, source }),
    }
}

/// Logs one `warn` line for each top-level key of `json`, the settings file at `path`, that names
/// no setting, naming the key and the file.
fn warn_of_unknown_keys(json: &str, path: &Path) {
    let Ok(top_level) = serde_json::from_str::<BTreeMap<String, IgnoredAny>>(json) else {
        log::debug!(
            "the settings file {} parsed without being a JSON object, so it has no keys to check",
            path.display()
        );
        return;
    };
    for key in top_level
        .keys()
        .filter(|key| !KNOWN_KEYS.contains(&key.as_str()))
    {
        log::warn!(
            "ignoring the key {key:?} in {}: the settings hold no setting of that name",
            path.display()
        );
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
    use crate::test_support::{self, unique_temp_dir};
    use std::path::PathBuf;

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
    fn an_unknown_top_level_key_warns_once_naming_it_and_the_file_and_the_load_succeeds() {
        // A misspelling of `telltale_toggles`, whose toggle would otherwise go unread unnoticed.
        let dir = root_holding(
            r#"{ "checking_enabled": false, "telltale_toggle": { "curly-quotes": true } }"#,
        );
        let path = dir.join(CONFIG_FILE_NAME);
        let (loaded, lines) = test_support::capture_logs(|| load(&dir));
        let config = loaded.expect("an unknown key should not fail the load");

        assert_eq!(
            config,
            Config {
                checking_enabled: false,
                ..Config::default()
            }
        );
        let warnings: Vec<&String> = lines
            .iter()
            .filter(|(level, _)| *level == log::Level::Warn)
            .map(|(_, line)| line)
            .collect();
        assert_eq!(warnings.len(), 1, "{lines:#?}");
        assert!(warnings[0].contains("\"telltale_toggle\""), "{lines:#?}");
        assert!(
            warnings[0].contains(&path.display().to_string()),
            "{lines:#?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_of_known_keys_alone_warns_of_none() {
        let dir = unique_temp_dir();
        save(&dir, &Config::default()).unwrap();
        let (loaded, lines) = test_support::capture_logs(|| load(&dir));
        loaded.unwrap();
        assert!(
            lines.iter().all(|(level, _)| *level > log::Level::Warn),
            "{lines:#?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_known_keys_are_the_keys_the_settings_serialise_with() {
        let serialised = serde_json::to_value(Config::default()).unwrap();
        let keys: Vec<&str> = serialised
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut known = KNOWN_KEYS.to_vec();
        known.sort_unstable();
        assert_eq!(keys, known);
    }

    /// The settings with a register map of `entries` and the default register `"default"`.
    fn mapping(entries: &[(&str, &str)]) -> Config {
        Config {
            register_map: entries
                .iter()
                .map(|(key, register)| ((*key).to_owned(), (*register).to_owned()))
                .collect(),
            ..Config::default()
        }
    }

    fn surface(process_name: &str, page_url: Option<&str>) -> Surface {
        Surface {
            process_name: process_name.to_owned(),
            page_url: page_url.map(str::to_owned),
            document_key: None,
        }
    }

    #[test]
    fn a_register_map_key_of_a_process_name_maps_every_document_in_that_process() {
        let dir = root_holding(r#"{ "register_map": { "WINWORD.EXE": "formal" } }"#);
        let config = load(&dir).unwrap();
        assert_eq!(config.register_for(&surface("WINWORD.EXE", None)), "formal");
        std::fs::remove_dir_all(&dir).ok();

        let config = mapping(&[("msedge.exe", "chat")]);
        assert_eq!(
            config.register_for(&surface("msedge.exe", Some("https://example.com/notes"))),
            "chat"
        );
    }

    #[test]
    fn a_key_of_the_process_name_and_the_host_wins_over_the_process_names_key() {
        let config = mapping(&[
            ("msedge.exe", "chat"),
            ("msedge.exe mail.example.com", "email"),
        ]);
        assert_eq!(
            config.register_for(&surface(
                "msedge.exe",
                Some("https://mail.example.com/compose?to=a#draft")
            )),
            "email"
        );
        // Another host, and the same host in another process, fall to the process name's key.
        assert_eq!(
            config.register_for(&surface("msedge.exe", Some("https://example.com/"))),
            "chat"
        );
        assert_eq!(
            config.register_for(&surface("chrome.exe", Some("https://mail.example.com/"))),
            "default"
        );
    }

    #[test]
    fn a_surface_no_key_matches_takes_the_default_register() {
        let config = Config {
            default_register: "plain".to_owned(),
            ..mapping(&[
                ("WINWORD.EXE", "formal"),
                ("msedge.exe example.com", "email"),
            ])
        };
        assert_eq!(config.register_for(&surface("Notepad.exe", None)), "plain");
        assert_eq!(
            config.register_for(&surface("Notepad.exe", Some("https://example.com/"))),
            "plain"
        );
    }

    #[test]
    fn a_keys_process_name_matches_in_another_case_and_its_host_as_the_parser_normalises_it() {
        let config = mapping(&[
            ("winword.exe", "formal"),
            ("MSEDGE.EXE", "chat"),
            ("MsEdge.exe Mail.Example.COM", "email"),
        ]);
        assert_eq!(config.register_for(&surface("WINWORD.EXE", None)), "formal");
        assert_eq!(
            config.register_for(&surface(
                "msedge.exe",
                Some("https://mail.example.com/inbox")
            )),
            "email"
        );
        assert_eq!(
            config.register_for(&surface("msedge.exe", Some("https://example.com/"))),
            "chat"
        );
        // The keys keep the spelling they were written with.
        assert!(config.register_map.contains_key("winword.exe"));
        assert!(config
            .register_map
            .contains_key("MsEdge.exe Mail.Example.COM"));
    }

    #[test]
    fn a_page_url_with_no_host_goes_straight_to_the_process_name() {
        let page = "file:///C:/pages/check.html";
        let config = mapping(&[("msedge.exe", "chat"), ("msedge.exe ", "email")]);
        assert_eq!(
            config.register_for(&surface("msedge.exe", Some(page))),
            "chat"
        );
        let unmapped = mapping(&[("msedge.exe ", "email")]);
        assert_eq!(
            unmapped.register_for(&surface("msedge.exe", Some(page))),
            "default"
        );
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
    fn reading_an_existing_config_writes_nothing_where_there_is_none() {
        let dir = unique_temp_dir();
        assert_eq!(load_existing(&dir).unwrap(), None);
        assert!(!dir.exists());

        let held = root_holding(r#"{ "telltale_toggles": { "emojis": false } }"#);
        assert_eq!(load_existing(&held).unwrap(), Some(load(&held).unwrap()));
        std::fs::remove_dir_all(&held).ok();

        let malformed = root_holding("{ \"checking_enabled\": tru }");
        assert!(matches!(
            load_existing(&malformed),
            Err(StoreError::Parse { .. })
        ));
        std::fs::remove_dir_all(&malformed).ok();
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
