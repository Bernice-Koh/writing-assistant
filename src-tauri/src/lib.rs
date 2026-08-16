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
    // `info` rather than `env_logger`'s own `error` default, so a normal run shows the capture
    // backend's flow without the caller having to know to set `RUST_LOG`. The Tier 0 path's
    // per-keystroke lines stay at `debug` and so stay off until asked for.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .setup(|app| {
            overlay::create(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the webview runtime is a hard requirement, with no degraded mode to fall back to");
}
