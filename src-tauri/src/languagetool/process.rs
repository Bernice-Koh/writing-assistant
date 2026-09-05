//! Spawns and supervises the LanguageTool subprocess: launch, readiness poll, and a background
//! health check that restarts it a bounded number of times before giving up.
//!
//! Verified directly against LanguageTool 6.6's official server distribution (jar plus a `libs/`
//! directory of 138 dependency jars, not one fat jar): `languagetool-server.jar`'s own
//! `META-INF/MANIFEST.MF` declares a `Class-Path` entry naming every jar under `libs/` it needs,
//! resolved by the JVM relative to `languagetool-server.jar`'s own location regardless of the
//! subprocess's working directory. `-cp <jar path>` alone is therefore enough, and nothing here
//! builds the classpath by hand, but only when the jar's own directory holds the rest of the
//! distribution root as well. That `Class-Path` entry begins `./`, and the language modules live
//! there as loose files, `org/languagetool/language/English.class` among them, rather than inside
//! any jar under `libs/`. With `libs/` alone beside the jar the server starts and then dies in
//! `LanguageIdentifier`'s static initialiser, reporting `xx-XX`, LanguageTool's own test fixture
//! language, as the only language code it knows. `scripts/build-languagetool-jre.ps1` is what
//! copies `org/` and `META-INF/` across alongside the jar and `libs/`.
//!
//! The jlink module list bundled for the trimmed runtime this spawns
//! (`java.base,java.compiler,java.desktop,java.instrument,java.naming,java.scripting,java.sql,
//! jdk.attach,jdk.httpserver,jdk.jdi,jdk.management,jdk.unsupported`) was derived by running
//! `jdeps --print-module-deps --multi-release 21 --class-path "libs/*" --ignore-missing-deps
//! languagetool-server.jar` against the real 6.6 distribution (the ignored deps are optional
//! metrics integrations, resilience4j and OpenTelemetry, that this server never exercises) and
//! then verified by actually running the server under a JRE built from exactly that module list
//! and checking real English (GB) text against it. The build recipe is recorded in
//! `scripts/build-languagetool-jre.ps1`; this module only consumes whatever `java_bin` it is
//! given and does not build the JRE itself.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::Instant;

use super::client::LanguageToolClient;
use super::error::LanguageToolError;

/// How many ports past `preferred` to try before giving up. A handful is enough in practice:
/// this is only needed at all because a previous run's subprocess, or something unrelated, might
/// still be holding the preferred port.
const PORT_SCAN_ATTEMPTS: u16 = 10;

/// Bound on how long startup is allowed to take before [`wait_until_ready`] gives up. Measured
/// startup under the trimmed jlink runtime, on the machine this was verified against, was
/// consistently under 3 seconds; this leaves generous room for a slower disk or a cold page
/// cache without leaving the app hanging indefinitely on a subprocess that will never come up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// How often [`wait_until_ready`] polls while waiting for the subprocess to become reachable.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Where to find the LanguageTool subprocess's own executable pieces. `jar` and `java_bin` are
/// both required to exist; `libs/` is expected to sit next to `jar` on disk (see this module's
/// own doc comment for why nothing here references it directly).
#[derive(Debug, Clone)]
pub struct LanguageToolPaths {
    pub java_bin: PathBuf,
    pub jar: PathBuf,
}

/// Finds a free TCP port at or after `preferred`, by binding and immediately releasing it. This
/// is a probe, not a reservation: nothing stops another process from taking the port between the
/// probe and the subprocess actually binding it. That race is accepted rather than engineered
/// around, since LanguageTool's own HTTP server gives no other way to ask it to pick its own free
/// port and report which one it chose.
pub(crate) fn find_free_port(preferred: u16) -> Result<u16, LanguageToolError> {
    (0..PORT_SCAN_ATTEMPTS)
        .filter_map(|offset| preferred.checked_add(offset))
        .find(|&port| TcpListener::bind(("127.0.0.1", port)).is_ok())
        .ok_or(LanguageToolError::NoFreePort { preferred })
}

/// Spawns the LanguageTool HTTP server subprocess on `port`, without waiting for it to become
/// ready; callers that need readiness should follow this with [`wait_until_ready`].
pub(crate) fn spawn(paths: &LanguageToolPaths, port: u16) -> Result<Child, LanguageToolError> {
    Command::new(&paths.java_bin)
        .arg("-cp")
        .arg(&paths.jar)
        .arg("org.languagetool.server.HTTPServer")
        .arg("--port")
        .arg(port.to_string())
        // Neither stdout nor stderr carries user draft text: LanguageTool's own process log is
        // startup diagnostics and per-request timing, per its own logging configuration, not the
        // text it was asked to check.
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| LanguageToolError::Spawn {
            java_bin: paths.java_bin.clone(),
            source,
        })
}

/// Polls `client` until it answers a lightweight request or [`STARTUP_TIMEOUT`] elapses.
/// `/v2/languages` is used rather than `/v2/check`: it does no grammar analysis, so it answers as
/// soon as the HTTP listener is up, without waiting on LanguageTool's own rule-loading to finish
/// for a specific check.
pub(crate) async fn wait_until_ready(client: &LanguageToolClient) -> Result<(), LanguageToolError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if client.languages_reachable().await {
            return Ok(());
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL).await;
    }
    Err(LanguageToolError::StartupTimeout {
        timeout_ms: STARTUP_TIMEOUT.as_millis() as u64,
    })
}

/// A minimal sentence used only to force LanguageTool to load its `en-GB` rule set now, during
/// startup, rather than on whichever real check happens to run first. Verified directly against a
/// running server: LanguageTool loads rules lazily, so a cold first check took 5.5 seconds against
/// well under 100 ms once warm, an entirely one-time cost with no per-request work in it. Paying
/// that cost here, once, keeps every check a real caller makes afterward inside Tier 0.5's
/// hundreds-of-milliseconds budget instead of one unlucky keystroke absorbing it.
const WARM_UP_TEXT: &str = "Hello.";

/// Sends [`WARM_UP_TEXT`] through `client` and discards the result. A failure here is logged, not
/// propagated: it costs the next real check the cold-load penalty this exists to avoid, not
/// correctness, so it does not fail [`crate::languagetool::LanguageToolSupervisor::start`] or a
/// restart over it.
pub(crate) async fn warm_up(client: &LanguageToolClient) {
    if let Err(error) = client.check(WARM_UP_TEXT).await {
        log::warn!(
            "LanguageTool warm-up check failed; the next real check will pay the cold-load cost \
             instead: {error}"
        );
    }
}

/// The default location of the bundled JRE's `java` binary and the bundled server jar, both
/// resolved relative to `resources_dir` (the Tauri resource directory at runtime). Kept separate
/// from [`spawn`] and [`wait_until_ready`] so tests can supply their own paths instead.
pub fn default_paths(resources_dir: &Path) -> LanguageToolPaths {
    let resources_dir = without_extended_length_prefix(resources_dir);
    LanguageToolPaths {
        // `.exe` unconditionally: per README's Requirements section this app only ships for
        // Windows, so there is no second platform's binary name to branch on.
        java_bin: resources_dir.join("jre").join("bin").join("java.exe"),
        jar: resources_dir
            .join("languagetool")
            .join("languagetool-server.jar"),
    }
}

/// Strips a `\\?\` extended-length prefix, which `AppHandle::path().resource_dir()` returns and
/// the JVM will not accept as a classpath entry. `CreateProcess` does accept the prefixed form,
/// so `java.exe` launches from it and the rejection surfaces one layer later as
/// `ClassNotFoundException` on the main class rather than as a spawn failure. Verified side by
/// side against the bundled 6.6 jar: `-cp C:\...\languagetool-server.jar` loads the server,
/// `-cp \\?\C:\...\languagetool-server.jar` loads nothing at all.
///
/// The prefix is what lifts Windows' 260-character path limit, so a resource directory nested
/// deeply enough to need it cannot run LanguageTool. Nothing here can recover that case: the JVM
/// takes neither form of such a path.
fn without_extended_length_prefix(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    match text.strip_prefix(r"\\?\UNC\") {
        Some(share) => PathBuf::from(format!(r"\\{share}")),
        None => PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed ports, below Windows' ephemeral range (49152-65535) and clear of each other's scan
    /// window, rather than ports discovered by binding to 0. Two reasons, one per test below.
    /// `cargo test` runs tests in parallel, so a dropped OS-assigned port can be grabbed by
    /// another test before the test that dropped it re-probes it. And an OS-assigned port lands
    /// inside the range every other socket on the machine is churning, where `find_free_port`'s
    /// [`PORT_SCAN_ATTEMPTS`]-port upward scan can find all ten busy: that made
    /// `scans_past_a_port_already_in_use` fail roughly one run in four. `find_free_port` itself
    /// documents accepting that race for its real scanning behaviour; these tests should not
    /// depend on it to prove anything.
    const FREE_PORT: u16 = 45817;
    const TAKEN_PORT: u16 = 45903;

    #[test]
    fn finds_the_preferred_port_when_it_is_free() {
        assert_eq!(find_free_port(FREE_PORT).unwrap(), FREE_PORT);
    }

    #[test]
    fn scans_past_a_port_already_in_use() {
        let listener = TcpListener::bind(("127.0.0.1", TAKEN_PORT)).unwrap();
        let found = find_free_port(TAKEN_PORT).unwrap();
        assert_ne!(found, TAKEN_PORT);
        assert!(found > TAKEN_PORT);
        // Kept alive to here so the port stays genuinely taken for the whole scan above.
        drop(listener);
    }

    #[test]
    fn default_paths_strip_an_extended_length_prefix_the_jvm_would_reject() {
        let paths = default_paths(Path::new(r"\\?\C:\Users\b\app\resources"));
        assert_eq!(
            paths.jar,
            Path::new(r"C:\Users\b\app\resources\languagetool\languagetool-server.jar")
        );
        assert_eq!(
            paths.java_bin,
            Path::new(r"C:\Users\b\app\resources\jre\bin\java.exe")
        );
    }

    #[test]
    fn default_paths_restore_a_unc_share_rather_than_leaving_it_headless() {
        let paths = default_paths(Path::new(r"\\?\UNC\host\share\resources"));
        assert_eq!(
            paths.jar,
            Path::new(r"\\host\share\resources\languagetool\languagetool-server.jar")
        );
    }

    #[test]
    fn default_paths_resolve_under_the_given_resources_dir() {
        let paths = default_paths(Path::new("C:/resources"));
        assert_eq!(paths.java_bin, Path::new("C:/resources/jre/bin/java.exe"));
        assert_eq!(
            paths.jar,
            Path::new("C:/resources/languagetool/languagetool-server.jar")
        );
    }
}
