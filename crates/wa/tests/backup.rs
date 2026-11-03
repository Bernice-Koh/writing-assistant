//! `wa backup` run as the built binary against a temporary data root, never the real one.

mod common;

use common::{root_with_store, stderr, wa, TempDir};

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
