//! `wa screen` run as the built binary against a temporary data root, never the real one. Each
//! startup failure is shown before LanguageTool would start, against a resource directory of stub
//! files, so it runs where LanguageTool is absent; the resource directory is shown resolved; and,
//! where the checkout holds LanguageTool and its Java runtime, one run goes end to end.

mod common;

use std::ffi::OsStr;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{root_with_store, stderr, wa_with, TempDir};
use serde_json::{json, Value};

/// One line written for the check, which a run that read its input would answer with a line.
const LINE: &str = r#"{"id": 1, "passage": "It rains at noon.", "candidate": "It rains at noon.", "register": "default"}"#;

/// The checkout's resource directory, which holds the dictionaries everywhere and LanguageTool and
/// its runtime only where the setup script has run.
fn checkout_resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("src-tauri")
        .join("resources")
}

/// A resource directory holding copies of the checkout's dictionaries and, when `languagetool` is
/// set, empty files where the Java runtime and the server jar belong, which pass the check for
/// their presence and fail to start.
fn stub_resources(languagetool: bool) -> TempDir {
    let resources = TempDir::new().created();
    let dictionaries = resources.path().join("dictionaries");
    std::fs::create_dir_all(&dictionaries).unwrap();
    for name in ["en_GB.aff", "en_GB.dic", "en_sg_supplement.txt"] {
        std::fs::copy(
            checkout_resources().join("dictionaries").join(name),
            dictionaries.join(name),
        )
        .unwrap();
    }
    if languagetool {
        for stub in [
            resources.path().join("jre").join("bin").join("java.exe"),
            resources
                .path()
                .join("languagetool")
                .join("languagetool-server.jar"),
        ] {
            std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
            std::fs::write(stub, "").unwrap();
        }
    }
    resources
}

/// Runs `wa` with `args` and `input` written to a file that is its standard input, so nothing
/// races a process that exits before reading.
fn wa_reading(args: &[&OsStr], input: &str, log_filter: Option<&str>) -> Output {
    let inputs = TempDir::new().created();
    let path = inputs.path().join("input.jsonl");
    std::fs::write(&path, input).unwrap();
    wa_with(
        args,
        Stdio::from(std::fs::File::open(&path).unwrap()),
        log_filter,
    )
}

/// Asserts that `output` is a startup failure: exit code 1, one line on standard error naming
/// `named`, and nothing on standard output.
fn assert_startup_failure(output: &Output, named: &str) {
    let stderr = stderr(output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(named), "{stderr} names no {named}");
    assert!(output.stdout.is_empty());
}

#[test]
fn a_resource_directory_lacking_a_file_exits_1_naming_it_before_reading_any_input() {
    let resources = stub_resources(false);
    let root = root_with_store();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            resources.path().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        None,
    );

    let java = resources.path().join("jre").join("bin").join("java.exe");
    assert_startup_failure(&output, &java.display().to_string());
}

#[test]
fn an_absent_store_exits_1_naming_the_file_and_creates_nothing() {
    let resources = stub_resources(true);
    let root = TempDir::new();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            resources.path().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        None,
    );

    let store = root.path().join("store.sqlite");
    assert_startup_failure(&output, &store.display().to_string());
    assert!(!root.path().exists());
}

#[test]
fn a_ruleset_version_the_store_lacks_exits_1_naming_it() {
    let resources = stub_resources(true);
    let root = root_with_store();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            resources.path().as_os_str(),
            "screen".as_ref(),
            "--ruleset-version".as_ref(),
            "4".as_ref(),
        ],
        LINE,
        None,
    );

    assert_startup_failure(&output, "the store holds no ruleset version 4");
}

#[test]
fn a_settings_file_that_does_not_parse_exits_1_naming_it_and_leaves_it_as_it_is() {
    let resources = stub_resources(true);
    let root = root_with_store();
    let settings = root.path().join("config.json");
    std::fs::write(&settings, "{ \"telltale_toggles\": ").unwrap();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            resources.path().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        None,
    );

    assert_startup_failure(&output, &settings.display().to_string());
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{ \"telltale_toggles\": "
    );
}

#[test]
fn languagetool_failing_to_start_exits_1_before_reading_any_input_and_writes_no_result() {
    let resources = stub_resources(true);
    let root = root_with_store();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            resources.path().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        None,
    );

    let java = resources.path().join("jre").join("bin").join("java.exe");
    assert_startup_failure(&output, "LanguageTool did not start");
    assert!(stderr(&output).contains(&java.display().to_string()));
}

#[test]
fn an_unknown_option_is_a_usage_error_exiting_1_and_not_2() {
    let output = wa_reading(
        &["screen".as_ref(), "--no-such-option".as_ref()],
        LINE,
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("Usage: wa"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
}

/// The directory the run's log line says the resources were read from, made canonical.
fn resources_read(output: &Output) -> PathBuf {
    const READING: &str = "reading the resources under ";
    let stderr = stderr(output);
    let line = stderr
        .lines()
        .find_map(|line| line.split_once(READING).map(|(_, dir)| dir.to_owned()))
        .unwrap_or_else(|| panic!("no line names the resource directory: {stderr}"));
    Path::new(&line).canonicalize().unwrap()
}

/// Asserts that `output` stopped at startup with nothing written, its error line naming either the
/// absent store under `root`, where the checkout holds LanguageTool and every resource file
/// passes, or a file under the checkout's resources, where it does not.
fn assert_stopped_after_reading_the_checkouts_resources(output: &Output, root: &Path) {
    let stderr = stderr(output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        resources_read(output),
        checkout_resources().canonicalize().unwrap()
    );
    let error = stderr.lines().last().unwrap();
    let store = root.join("store.sqlite").display().to_string();
    assert!(
        error.contains(&store) || error.contains("languagetool") || error.contains("java.exe"),
        "{error}"
    );
}

#[test]
fn the_resources_option_names_the_directory_the_resources_are_read_from() {
    let root = TempDir::new();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "--resources".as_ref(),
            checkout_resources().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        Some("wa=info"),
    );

    assert_stopped_after_reading_the_checkouts_resources(&output, root.path());
}

#[test]
fn without_the_resources_option_a_build_under_target_reads_the_checkouts_resources() {
    let root = TempDir::new();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "screen".as_ref(),
        ],
        LINE,
        Some("wa=info"),
    );

    assert_stopped_after_reading_the_checkouts_resources(&output, root.path());
}

/// Whether `port` on the loopback address can be bound, which it cannot while a process listens
/// on it: the probe `find_free_port` makes.
fn is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[test]
fn a_run_with_languagetool_screens_each_line_beside_a_server_on_8081_and_stops_its_own() {
    let resources = checkout_resources();
    let needed = [
        resources
            .join("languagetool")
            .join("languagetool-server.jar"),
        resources.join("jre").join("bin").join("java.exe"),
    ];
    if let Some(absent) = needed.iter().find(|path| !path.is_file()) {
        // Written past the test harness's capture, so the skip shows in every run's log.
        writeln!(
            std::io::stderr(),
            "skipped the end-to-end run of wa screen: {} is absent, as it is until \
             src-tauri/scripts/build-languagetool-jre.ps1 has run",
            absent.display()
        )
        .unwrap();
        return;
    }
    let root = root_with_store();
    // A listener held on the preferred port for the run, as a running app's server holds it. When
    // something else already holds it, the bind fails and the port is taken all the same.
    let held = TcpListener::bind(("127.0.0.1", 8081)).ok();
    // Texts written for the check: a misspelling the passage lacks, then two clean sentences.
    let lines = [
        (
            json!("misspelt"),
            "We met at the pier.",
            "We met at the peir.",
        ),
        (
            json!(2),
            "The train leaves at six.",
            "The train leaves at six.",
        ),
        (json!(null), "It rains at noon.", "It rains at noon."),
    ];
    let input: String = lines
        .iter()
        .map(|(id, passage, candidate)| {
            json!({ "id": id, "passage": passage, "candidate": candidate, "register": "default" })
                .to_string()
                + "\n"
        })
        .collect();

    let output = wa_reading(
        &[
            "--data-root".as_ref(),
            root.path().as_os_str(),
            "screen".as_ref(),
        ],
        &input,
        Some("wa=info"),
    );
    drop(held);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains('\r'));
    let screened: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(screened.len(), lines.len(), "{stdout}");
    for (line, (id, _, _)) in screened.iter().zip(&lines) {
        assert_eq!(&line["id"], id);
        assert_eq!(line.as_object().unwrap().len(), 9, "{line}");
        assert_eq!(line["rulesetVersion"], Value::Null);
    }
    assert_eq!(screened[0]["verdict"], "fail");
    assert_eq!(screened[0]["killingRule"], "spelling");
    for line in &screened[1..] {
        assert_eq!(line["verdict"], "pass", "{line}");
    }

    // The log, LanguageTool's own lines included, holds no text of a passage or a candidate.
    for (_, passage, candidate) in &lines {
        assert!(!stderr.contains(passage) && !stderr.contains(candidate));
    }

    let port: u16 = stderr
        .lines()
        .find_map(|line| line.split_once("LanguageTool is ready on port "))
        .map(|(_, port)| port.trim().parse().unwrap())
        .unwrap_or_else(|| panic!("no line names the port: {stderr}"));
    assert!(port > 8081, "{port}");
    // The server was killed and waited on before wa exited, so its port is free at once; the poll
    // allows for the operating system's own release of the socket.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !is_free(port) {
        assert!(Instant::now() < deadline, "port {port} is still held");
        std::thread::sleep(Duration::from_millis(100));
    }
}
