//! The Tauri app: the overlay, the commands, and the setup that starts the engine against the
//! native capture backend. The checking itself lives in `wa_engine`, which `wa` also calls.

pub mod overlay;

use std::sync::{Arc, Mutex};

use tauri::Manager;

use wa_engine::analyzer::Analyzer;
use wa_engine::capture::{self, Capture};
use wa_engine::languagetool::{self, GrammarSource, LanguageToolSupervisor};
use wa_engine::spelling::{self, SpellChecker};
use wa_engine::store::{self, config, Config, Store, StoreError};
use wa_engine::style::ai_tell::{self, Catalogue};
use wa_engine::style::ruleset::LoadedRuleset;

use overlay::{OverlayState, PositionedFlag, SharedOverlay};

/// The current, positioned flag set for the active document, relative to the overlay window's
/// own origin: the same shape [`overlay::track_flags`] emits as `flags-updated`, read here for a
/// freshly mounted frontend's initial state.
#[tauri::command]
fn get_current_flags(
    app: tauri::AppHandle,
    shared: tauri::State<'_, SharedOverlay>,
) -> Vec<PositionedFlag> {
    overlay::relative_flags(&app, shared.inner())
}

/// Builds and runs the Tauri application.
pub fn run() {
    // `info` rather than `env_logger`'s own `error` default, so a normal run shows the capture
    // backend's flow without the caller having to know to set `RUST_LOG`. The capture backend's
    // high-frequency lines stay at `debug` and so stay off until asked for.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_current_flags])
        .setup(|app| {
            overlay::create(app.handle())?;
            let backend: Arc<dyn Capture> = Arc::new(capture::native::NativeCapture::start()?);
            // Managed as well as tracked: the working card's commands, not yet built, need the
            // same backend. The trait object is what keeps a command from naming a surface.
            app.manage(Arc::clone(&backend));

            let shared_overlay: SharedOverlay = Arc::new(Mutex::new(OverlayState::default()));
            app.manage(Arc::clone(&shared_overlay));
            overlay::track_document_view(
                app.handle().clone(),
                Arc::clone(&backend),
                Arc::clone(&shared_overlay),
            );

            // Spawned rather than run inline: `LanguageToolSupervisor::start` can take up to its
            // own startup timeout before giving up on a subprocess that never becomes reachable,
            // and the overlay and main windows should show immediately rather than waiting on it.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(start_checking_pipeline(
                app_handle,
                backend,
                shared_overlay,
            ));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the webview runtime is a hard requirement, with no degraded mode to fall back to");
}

/// Loads the settings, compiles the telltale catalogue from them, opens the store, loads the
/// ruleset from it, loads spelling, starts LanguageTool best-effort, starts the analyzer against
/// `capture`, and starts the two background tasks that turn the analyzer's own flag stream into
/// what the overlay renders and hit-tests. Split out of `run`'s `setup` closure because it
/// `.await`s.
async fn start_checking_pipeline(
    app: tauri::AppHandle,
    capture: Arc<dyn Capture>,
    shared_overlay: SharedOverlay,
) {
    // Read once, with no watcher: an edit made while the app runs takes effect at the next start.
    // Managed beside the capture backend for the reader that arrives with the register map. The
    // analyzer checks every sentence under the default register, against the telltale catalogue
    // compiled from the toggles.
    let settings = load_settings();
    let register = settings.default_register.clone();
    let catalogue = Arc::new(load_catalogue(&settings));
    app.manage(settings);
    let store = open_store().await;
    // Read once, like the settings: a version published while the app runs is in force from the
    // next start. Handed to the analyzer, which runs its checkers and routes LanguageTool's matches
    // through it.
    let ruleset = Arc::new(load_ruleset(store.as_ref()).await);
    // Managed as an `Option`, so a reader meets a store that failed to open as `None` rather than
    // as unmanaged state. Nothing reads it yet.
    app.manage(store);

    let resources_dir = app.path().resource_dir().expect(
        "the resource directory is a hard requirement: without it, not even the vendored \
         dictionaries can be found",
    );

    let dictionaries = spelling::default_paths(&resources_dir);
    let spelling = SpellChecker::load(
        &dictionaries.aff,
        &dictionaries.dic,
        &dictionaries.supplement,
    )
    .expect("the vendored dictionary pair and supplement are well-formed, checked in CI");

    let languagetool_paths = languagetool::default_paths(&resources_dir);
    let grammar: Option<Arc<dyn GrammarSource>> =
        match LanguageToolSupervisor::start(languagetool_paths, languagetool::PREFERRED_PORT).await
        {
            Ok(supervisor) => Some(Arc::new(supervisor)),
            Err(error) => {
                log::warn!(
                    "LanguageTool could not start, degrading to spelling and AI-tell flags only: \
                     {error}"
                );
                None
            }
        };

    let analyzer = Arc::new(Analyzer::start(
        Arc::clone(&capture),
        spelling,
        grammar,
        ruleset,
        catalogue,
        register,
    ));
    overlay::track_flags(
        app.clone(),
        Arc::clone(&capture),
        analyzer,
        Arc::clone(&shared_overlay),
    );
    overlay::track_hover(app, shared_overlay);
}

/// The settings under the data root, or the defaults when they cannot be loaded, since a fault in
/// the settings file degrades the settings and never stops checking. A file that does not parse
/// is left as it is for the owner to correct.
fn load_settings() -> Config {
    let loaded = store::data_root().and_then(|root| {
        let settings = config::load(&root)?;
        log::info!("loaded the settings file under {}", root.display());
        Ok(settings)
    });
    match loaded {
        Ok(settings) => settings,
        Err(StoreError::Parse { path, source }) => {
            log::error!(
                "the settings file {} does not parse at line {}, column {}, so checking starts on \
                 the default settings and the file is left as it is: {source}",
                path.display(),
                source.line(),
                source.column(),
            );
            Config::default()
        }
        Err(error) => {
            log::error!("{error}, so checking starts on the default settings");
            Config::default()
        }
    }
}

/// The telltale catalogue's patterns that `settings` switch on, compiled once. Logs one `info` line
/// naming how many are on, each pattern switched off, and the rule data's content hash.
fn load_catalogue(settings: &Config) -> Catalogue {
    let catalogue = Catalogue::load(settings);
    let off: Vec<&str> = ai_tell::pattern_ids()
        .filter(|id| {
            !catalogue
                .enabled_pattern_ids()
                .any(|enabled| enabled == *id)
        })
        .collect();
    log::info!(
        "loaded the telltale catalogue with {} of {} patterns on, off: {}; rule data hash {}",
        catalogue.enabled_pattern_ids().count(),
        ai_tell::pattern_ids().count(),
        if off.is_empty() {
            "none".to_owned()
        } else {
            off.join(", ")
        },
        catalogue.hash().as_str()
    );
    catalogue
}

/// The store under the data root, or `None` when it cannot be opened, since a fault in the store
/// degrades what the store records and never stops checking. Opened on a blocking thread because
/// the open waits for the store thread to report, and can wait out the busy timeout while `wa`
/// holds the write lock.
async fn open_store() -> Option<Store> {
    let opened = tauri::async_runtime::spawn_blocking(|| {
        store::data_root().and_then(|root| Store::open(&root))
    })
    .await;
    match opened {
        Ok(Ok(store)) => Some(store),
        Ok(Err(error)) => {
            log::error!("{error}, so checking continues without the store");
            None
        }
        Err(error) => {
            log::error!(
                "the store open did not finish ({error}), so checking continues without the store"
            );
            None
        }
    }
}

/// The latest ruleset in `store`, or an empty one with no version when the store did not open or
/// cannot be read, since a fault in the ruleset degrades style checking and never stops checking.
/// Logs one `info` line naming the version in force.
async fn load_ruleset(store: Option<&Store>) -> LoadedRuleset {
    let ruleset = match store {
        Some(store) => LoadedRuleset::load(store).await.unwrap_or_else(|error| {
            log::error!("{error}, so checking continues without the ruleset");
            LoadedRuleset::empty()
        }),
        None => LoadedRuleset::empty(),
    };
    match ruleset.version() {
        Some(version) => {
            let compiled = ruleset.checkers().len();
            log::info!(
                "loaded ruleset version {} with {compiled} compiled {}",
                version.0,
                if compiled == 1 { "checker" } else { "checkers" }
            );
        }
        None => log::info!("loaded no ruleset version"),
    }
    ruleset
}
