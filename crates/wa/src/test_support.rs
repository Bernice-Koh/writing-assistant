//! What `wa`'s in-process tests share.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A directory under the OS temp directory unique to this call, not yet created, and removed with
/// everything in it when the guard drops. The process id separates concurrent test binaries and the
/// counter concurrent tests within this one.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        Self(std::env::temp_dir().join(format!(
            "writing-assistant-wa-unit-{}-{id}",
            std::process::id()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}
