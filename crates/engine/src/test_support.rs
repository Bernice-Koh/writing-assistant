use std::path::PathBuf;

/// A file in the vendored dictionary folder. The resources stay under `src-tauri/`, where the
/// app bundles them from, so each file exists once in the repository.
pub(crate) fn dictionary_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/resources/dictionaries")
        .join(name)
}
