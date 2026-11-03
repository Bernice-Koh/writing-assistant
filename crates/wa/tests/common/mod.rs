//! What the tests that run the built `wa` share. Each test binary compiles this module and uses a
//! part of it, so an item one binary leaves unused is not dead code.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use wa_engine::store::Store;

/// A directory under the OS temp directory unique to this call, removed with everything in it when
/// the guard drops. The process id separates concurrent test binaries and the counter concurrent
/// tests within this one.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        Self(std::env::temp_dir().join(format!(
            "writing-assistant-wa-test-{}-{id}",
            std::process::id()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn created(self) -> Self {
        std::fs::create_dir_all(&self.0).unwrap();
        self
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A data root holding a store the engine created, as the app does at its first start.
pub fn root_with_store() -> TempDir {
    let root = TempDir::new();
    drop(Store::open(root.path()).unwrap());
    root
}

/// Runs the built `wa` with `args` and `stdin` as its standard input, with no `RUST_LOG` but the
/// one `log_filter` names, so its standard error holds only what a test expects.
pub fn wa_with(args: &[&OsStr], stdin: Stdio, log_filter: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wa"));
    command.args(args).stdin(stdin).env_remove("RUST_LOG");
    if let Some(filter) = log_filter {
        command.env("RUST_LOG", filter);
    }
    command.output().unwrap()
}

/// Runs the built `wa` with `args` and nothing on its standard input.
pub fn wa(args: &[&OsStr]) -> Output {
    wa_with(args, Stdio::null(), None)
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}
