//! The resource directory `wa screen` reads the dictionaries, LanguageTool and its Java runtime
//! from, each file found through the engine function the app finds it through, and the check that
//! every one exists before any input is read. The telltale catalogue and the spelling variant table
//! are compiled into the engine and come from no directory.

use std::path::{Path, PathBuf};

use wa_engine::{languagetool, spelling};

/// The resource directory when `--resources` names none, for the executable at `executable`. A
/// build under a checkout's `target/` folder finds `src-tauri/resources` of the nearest directory
/// above it that holds a `Cargo.toml` and that folder, the checkout's root. An executable in no
/// checkout gets its own directory, where the app's bundle places each resource family.
pub fn default_dir(executable: &Path) -> PathBuf {
    let executable_dir = executable.parent().unwrap_or(executable);
    executable_dir
        .ancestors()
        .map(|dir| (dir, dir.join("src-tauri").join("resources")))
        .find(|(dir, resources)| dir.join("Cargo.toml").is_file() && resources.is_dir())
        .map_or_else(|| executable_dir.to_path_buf(), |(_, resources)| resources)
}

/// Every file `wa screen` reads under `resources`, in the order [`first_missing`] checks them.
fn needed_files(resources: &Path) -> [PathBuf; 5] {
    let dictionaries = spelling::default_paths(resources);
    let languagetool = languagetool::default_paths(resources);
    [
        dictionaries.aff,
        dictionaries.dic,
        dictionaries.supplement,
        languagetool.java_bin,
        languagetool.jar,
    ]
}

/// The first file `wa screen` reads under `resources` that does not exist.
pub fn first_missing(resources: &Path) -> Option<PathBuf> {
    needed_files(resources)
        .into_iter()
        .find(|path| !path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// Creates an empty file at `path`, and the directories above it.
    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn a_build_under_a_checkouts_target_folder_finds_its_src_tauri_resources() {
        let checkout = TempDir::new();
        touch(&checkout.path().join("Cargo.toml"));
        let resources = checkout.path().join("src-tauri").join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        // A member crate's own manifest, which holds no resource folder, is passed over.
        touch(&checkout.path().join("crates").join("wa").join("Cargo.toml"));

        for executable in [
            checkout.path().join("target").join("debug").join("wa.exe"),
            checkout
                .path()
                .join("crates")
                .join("wa")
                .join("target")
                .join("release")
                .join("wa.exe"),
        ] {
            assert_eq!(
                default_dir(&executable),
                resources,
                "{}",
                executable.display()
            );
        }
    }

    #[test]
    fn an_executable_in_no_checkout_finds_its_own_directory() {
        let installed = TempDir::new();
        std::fs::create_dir_all(installed.path()).unwrap();
        let executable = installed.path().join("wa.exe");

        assert_eq!(default_dir(&executable), installed.path());
    }

    #[test]
    fn the_first_missing_file_is_named_until_every_one_exists() {
        let resources = TempDir::new();
        let needed = needed_files(resources.path());
        assert_eq!(
            needed
                .clone()
                .map(|path| path.strip_prefix(resources.path()).unwrap().to_path_buf()),
            [
                Path::new("dictionaries/en_GB.aff"),
                Path::new("dictionaries/en_GB.dic"),
                Path::new("dictionaries/en_sg_supplement.txt"),
                Path::new("jre/bin/java.exe"),
                Path::new("languagetool/languagetool-server.jar"),
            ]
        );

        for (index, path) in needed.iter().enumerate() {
            assert_eq!(
                first_missing(resources.path()).as_ref(),
                Some(path),
                "{index}"
            );
            touch(path);
        }
        assert_eq!(first_missing(resources.path()), None);

        // A directory where a file belongs is missing all the same.
        std::fs::remove_file(&needed[4]).unwrap();
        std::fs::create_dir(&needed[4]).unwrap();
        assert_eq!(first_missing(resources.path()), Some(needed[4].clone()));
    }
}
