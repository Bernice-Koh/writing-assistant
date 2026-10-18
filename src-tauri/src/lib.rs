//! The Tauri app: the overlay, the commands, and the setup that starts the engine against the
//! native capture backend. The checking itself lives in `wa_engine`, which `wa` also calls.

pub mod overlay;

use std::sync::{Arc, Mutex};

use tauri::Manager;

use wa_engine::analyzer::Analyzer;
use wa_engine::capture::{self, Capture};
use wa_engine::languagetool::{self, LanguageToolSupervisor};
use wa_engine::spelling::SpellChecker;
use wa_engine::store::{self, config, Config, Store, StoreError};
use wa_engine::style::ruleset::LoadedRuleset;

use overlay::{PositionedFlag, SharedFlags};

/// Where `LanguageToolSupervisor::start` starts scanning for a free port. LanguageTool's own
/// conventional default when run standalone, kept here rather than picked arbitrarily so a
/// developer inspecting running processes recognises it; `find_free_port` already scans past it
/// if something else holds it.
const LANGUAGETOOL_PREFERRED_PORT: u16 = 8081;

/// The current, positioned flag set for the active document, relative to the overlay window's
/// own origin: the same shape [`overlay::track_flags`] emits as `flags-updated`, read here for a
/// freshly mounted frontend's initial state.
// `Result`, not a bare `Vec`, because Tauri requires it of every async command whose inputs
// borrow (`State` does): see the `AsyncCommandMustReturnResult` bound on `#[tauri::command]`.
// `Err` is never actually constructed; `relative_flags` already reports "nothing to show yet"
// as an empty `Vec`, not a failure, since that is a normal state, not an exceptional one.
#[tauri::command]
async fn get_current_flags(
    app: tauri::AppHandle,
    capture: tauri::State<'_, Arc<dyn Capture>>,
    shared: tauri::State<'_, SharedFlags>,
) -> Result<Vec<PositionedFlag>, ()> {
    Ok(overlay::relative_flags(&app, capture.inner(), shared.inner()).await)
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
            // Managed as well as tracked: `get_current_flags` reads the backend from managed
            // state, and the working card's commands, not yet built, need the same backend. The
            // trait object is what keeps a command from naming a surface.
            app.manage(Arc::clone(&backend));
            overlay::track_document_view(app.handle().clone(), Arc::clone(&backend));

            let shared_flags: SharedFlags = Arc::new(Mutex::new(Vec::new()));
            app.manage(Arc::clone(&shared_flags));

            // Spawned rather than run inline: `LanguageToolSupervisor::start` can take up to its
            // own startup timeout before giving up on a subprocess that never becomes reachable,
            // and the overlay and main windows should show immediately rather than waiting on it.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(start_checking_pipeline(app_handle, backend, shared_flags));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the webview runtime is a hard requirement, with no degraded mode to fall back to");
}

/// Loads the settings, opens the store, loads the ruleset from it, loads spelling, starts
/// LanguageTool best-effort, starts the analyzer against `capture`, and starts the two background
/// tasks that turn the analyzer's own flag stream into what the overlay renders and hit-tests.
/// Split out of `run`'s `setup` closure because it `.await`s.
async fn start_checking_pipeline(
    app: tauri::AppHandle,
    capture: Arc<dyn Capture>,
    shared_flags: SharedFlags,
) {
    // Read once, with no watcher: an edit made while the app runs takes effect at the next start.
    // Managed beside the capture backend for the readers that arrive with the telltale toggles
    // and the register.
    app.manage(load_settings());
    let store = open_store().await;
    // Read once, like the settings: a version published while the app runs is in force from the
    // next start. Managed for the analyzer passes that run its checkers (#100).
    let ruleset = Arc::new(load_ruleset(store.as_ref()).await);
    app.manage(Arc::clone(&ruleset));
    // Managed as an `Option`, so a reader meets a store that failed to open as `None` rather than
    // as unmanaged state. Nothing reads it yet.
    app.manage(store);

    let resources_dir = app.path().resource_dir().expect(
        "the resource directory is a hard requirement: without it, not even the vendored \
         dictionaries can be found",
    );

    let spelling = SpellChecker::load(
        &resources_dir.join("dictionaries").join("en_GB.aff"),
        &resources_dir.join("dictionaries").join("en_GB.dic"),
        &resources_dir
            .join("dictionaries")
            .join("en_sg_supplement.txt"),
    )
    .expect("the vendored dictionary pair and supplement are well-formed, checked in CI");

    let languagetool_paths = languagetool::default_paths(&resources_dir);
    let languagetool = match LanguageToolSupervisor::start(
        languagetool_paths,
        LANGUAGETOOL_PREFERRED_PORT,
    )
    .await
    {
        Ok(supervisor) => Some(supervisor),
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
        languagetool,
        ruleset.version(),
    ));
    overlay::track_flags(
        app.clone(),
        Arc::clone(&capture),
        analyzer,
        Arc::clone(&shared_flags),
    );
    overlay::track_hover(app, shared_flags);
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
