use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Once;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// A file in the vendored dictionary folder. The resources stay under `src-tauri/`, where the
/// app bundles them from, so each file exists once in the repository.
pub(crate) fn dictionary_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/resources/dictionaries")
        .join(name)
}

/// The app's Tauri configuration, which declares the bundle identifier the data root is named by.
pub(crate) fn tauri_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/tauri.conf.json")
}

thread_local! {
    static CAPTURED: RefCell<Option<Vec<(Level, String)>>> = const { RefCell::new(None) };
}

/// Records each log line on the thread that emits it, and only while that thread is inside
/// [`capture_logs`]. `cargo test` runs each test on its own thread, so concurrent tests never see
/// each other's lines.
struct ThreadLogger;

impl Log for ThreadLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        CAPTURED.with(|captured| {
            if let Some(lines) = captured.borrow_mut().as_mut() {
                lines.push((record.level(), record.args().to_string()));
            }
        });
    }

    fn flush(&self) {}
}

static LOGGER: ThreadLogger = ThreadLogger;

/// Runs `body` and returns its result with every log line it emitted on the calling thread.
pub(crate) fn capture_logs<T>(body: impl FnOnce() -> T) -> (T, Vec<(Level, String)>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        log::set_logger(&LOGGER).expect("no other code in the engine's tests installs a logger");
        log::set_max_level(LevelFilter::Trace);
    });
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
    let result = body();
    let lines = CAPTURED
        .with(|captured| captured.borrow_mut().take())
        .expect("set above on this thread");
    (result, lines)
}
