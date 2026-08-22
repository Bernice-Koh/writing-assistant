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

use std::sync::Arc;

use tauri::Manager;

use capture::Capture;

/// Builds and runs the Tauri application.
pub fn run() {
    // `info` rather than `env_logger`'s own `error` default, so a normal run shows the capture
    // backend's flow without the caller having to know to set `RUST_LOG`. The Tier 0 path's
    // per-keystroke lines stay at `debug` and so stay off until asked for.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .setup(|app| {
            overlay::create(app.handle())?;
            let backend: Arc<dyn Capture> = Arc::new(capture::native::NativeCapture::start()?);
            // Managed as well as tracked: the overlay only needs the cursor rect, but the
            // commands that will serve the Style Card and the rewrite orchestrator need the
            // same backend, and the trait object is what keeps them from naming a surface.
            app.manage(Arc::clone(&backend));
            overlay::track_cursor(app.handle().clone(), backend);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the webview runtime is a hard requirement, with no degraded mode to fall back to");
}
