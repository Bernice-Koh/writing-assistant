use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Once;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// A directory under the OS temp directory unique to this call, not yet created, handed to a test
/// as its data root so that no test touches the real one, and concurrently running tests never
/// read or write each other's files. `std::process::id()` separates concurrent test binaries, and
/// the atomic counter separates concurrent tests within one binary, which `cargo test` runs in
/// parallel by default.
pub(crate) fn unique_temp_dir() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "writing-assistant-store-test-{}-{id}",
        std::process::id()
    ))
}

/// A [`unique_temp_dir`] removed with everything in it when the guard drops, a failing test's
/// unwinding included. Declared before anything that holds a file open under it, so that it drops
/// after those do.
pub(crate) struct TempRoot(PathBuf);

impl TempRoot {
    pub(crate) fn new() -> Self {
        Self(unique_temp_dir())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

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
