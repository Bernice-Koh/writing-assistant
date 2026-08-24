//! Library entry point for the Rust core. The engine modules hang off this crate so the
//! checking engine stays usable as one crate inside a larger desktop assistant later.

pub mod analyzer;
pub mod capture;
pub mod flag;
pub mod languagetool;
pub mod learning;
pub mod overlay;
pub mod rewrite;
pub mod spelling;
pub mod store;
pub mod style;

use std::sync::{Arc, Mutex};

use tauri::Manager;

use analyzer::Analyzer;
use capture::Capture;
use languagetool::LanguageToolSupervisor;
use overlay::{PositionedFlag, SharedFlags};
use spelling::SpellChecker;

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
    capture: tauri::State<'_, Arc<dyn Capture>>,
    shared: tauri::State<'_, SharedFlags>,
) -> Result<Vec<PositionedFlag>, ()> {
    Ok(overlay::relative_flags(capture.inner(), shared.inner()).await)
}

/// Builds and runs the Tauri application.
pub fn run() {
    // `info` rather than `env_logger`'s own `error` default, so a normal run shows the capture
    // backend's flow without the caller having to know to set `RUST_LOG`. The Tier 0 path's
    // per-keystroke lines stay at `debug` and so stay off until asked for.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_current_flags])
        .setup(|app| {
            overlay::create(app.handle())?;
            let backend: Arc<dyn Capture> = Arc::new(capture::native::NativeCapture::start()?);
            // Managed as well as tracked: the overlay only needs the document-view rect, but the
            // commands that will serve the Style Card and the rewrite orchestrator need the same
            // backend, and the trait object is what keeps them from naming a surface.
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

/// Loads spelling, starts LanguageTool best-effort, starts the analyzer against `capture`, and
/// starts the two background tasks that turn the analyzer's own flag stream into what the
/// overlay renders and hit-tests. Split out of `run`'s `setup` closure because it `.await`s.
async fn start_checking_pipeline(
    app: tauri::AppHandle,
    capture: Arc<dyn Capture>,
    shared_flags: SharedFlags,
) {
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
    ));
    overlay::track_flags(
        app.clone(),
        Arc::clone(&capture),
        analyzer,
        Arc::clone(&shared_flags),
    );
    overlay::track_hover(app, shared_flags);
}
