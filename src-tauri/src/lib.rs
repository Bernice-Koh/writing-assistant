//! Library entry point for the Rust core. The engine modules hang off this crate so the
//! checking engine stays usable as one crate inside a larger desktop assistant later.

pub mod analyzer;
pub mod capture;
pub mod languagetool;
pub mod learning;
pub mod overlay;
pub mod rewrite;
pub mod store;
pub mod style;

/// Builds and runs the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            overlay::create(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the webview runtime is a hard requirement, with no degraded mode to fall back to");
}
