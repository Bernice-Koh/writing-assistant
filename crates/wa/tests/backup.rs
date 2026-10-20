//! `wa backup` run as the built binary against a temporary data root, never the real one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use wa_engine::store::Store;

/// A directory under the OS temp directory unique to this call, removed with everything in it when
/// the guard drops. The process id separates concurrent test binaries and the counter concurrent
/// tests within this one.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        Self(std::env::temp_dir().join(format!(
            "writing-assistant-wa-test-{}-{id}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn created(self) -> Self {
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
fn root_with_store() -> TempDir {
    let root = TempDir::new();
    drop(Store::open(root.path()).unwrap());
    root
}

fn wa(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wa"))
        .args(args)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn no_destination_prints_the_usage_and_exits_non_zero() {
    let root = root_with_store();

    let output = wa(&[
        "--data-root".as_ref(),
        root.path().as_os_str(),
        "backup".as_ref(),
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("Usage: wa"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
}

#[test]
fn a_data_root_with_no_store_is_refused_by_name_and_nothing_is_created() {
    let root = TempDir::new();
    let destinations = TempDir::new().created();
    let destination = destinations.path().join("copy.sqlite");

    let output = wa(&[
        "--data-root".as_ref(),
        root.path().as_os_str(),
        "backup".as_ref(),
        destination.as_os_str(),
    ]);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&root.path().join("store.sqlite").display().to_string()),
        "{stderr}"
    );
    assert!(!root.path().exists());
    assert!(!destination.exists());
}

#[test]
fn an_existing_destination_is_refused_by_name_and_left_as_it_is() {
    let root = root_with_store();
    let destinations = TempDir::new().created();
    let destination = destinations.path().join("existing.sqlite");
    std::fs::write(&destination, "an earlier backup").unwrap();

    let output = wa(&[
        "--data-root".as_ref(),
        root.path().as_os_str(),
        "backup".as_ref(),
        destination.as_os_str(),
    ]);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&destination.display().to_string()),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "an earlier backup"
    );
}

#[test]
fn a_backup_prints_the_destination_and_exits_zero() {
    let root = root_with_store();
    let destinations = TempDir::new().created();
    let destination = destinations.path().join("copy.sqlite");

    let output = wa(&[
        "--data-root".as_ref(),
        root.path().as_os_str(),
        "backup".as_ref(),
        destination.as_os_str(),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        destination.display().to_string()
    );
    assert!(destination.is_file());
}
