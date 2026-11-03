//! Manages the LanguageTool subprocess (launch, readiness, bounded restart on an unexpected
//! exit, shutdown) and runs the grammar stage's checks against it, in LanguageTool's `en-GB` variant and its
//! picky mode. `process` covers the subprocess itself; `client` the HTTP API, the shape of a
//! response and the route of each of LanguageTool's categories; `routing` the turn of one
//! sentence's matches into [`crate::flag::Flag`]s through the loaded ruleset. The analyzer and the
//! screen reach the checks through [`GrammarSource`], which the supervisor implements.

mod client;
pub mod error;
mod process;
mod routing;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::process::Child;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[cfg(test)]
pub(crate) use client::fixtures as captured_responses;
pub use client::LanguageToolClient;
pub use error::LanguageToolError;
pub use process::{default_paths, LanguageToolPaths};

use crate::flag::Flag;
use crate::style::checker::FormMode;
use crate::style::ruleset::LoadedRuleset;

/// The flags `response`, a response captured from LanguageTool to one sentence of `mode` under
/// `register`, routes to through `ruleset` by the routing the supervisor runs, so a fake grammar
/// source answers through it with no subprocess.
#[cfg(test)]
pub(crate) fn route_captured_response(
    response: &str,
    ruleset: &LoadedRuleset,
    mode: FormMode,
    register: &str,
) -> Vec<Flag> {
    routing::route(
        client::fixtures::matches_in(response),
        ruleset,
        mode,
        register,
    )
}

/// How many times [`supervise`] restarts the subprocess after an unexpected exit before giving
/// up and staying degraded for the rest of the session. Three gives a real subprocess crash-loop
/// (a bad JVM flag, a corrupt bundled jar) a couple of chances to be transient before the app
/// commits to running without grammar checking rather than restarting forever.
const MAX_RESTART_ATTEMPTS: u32 = 3;

/// The port the app and `wa` hand [`LanguageToolSupervisor::start`], which scans upward from it for
/// a free one, so two processes started at it both run, the second on the next free port.
/// LanguageTool's own default when run standalone, so its server is recognisable among a
/// machine's listening ports.
pub const PREFERRED_PORT: u16 = 8081;

/// The source of one sentence's grammar flags, which the analyzer's grammar stage and the screen
/// call. A trait object rather than the supervisor itself, so their tests answer, hold, fail and
/// time out checks with no LanguageTool subprocess. [`LanguageToolSupervisor`] is the
/// implementation the app runs.
#[async_trait::async_trait]
pub trait GrammarSource: Send + Sync {
    /// Runs the grammar stage's check of `sentence`, one sentence of form mode `mode` under
    /// `register`, and routes LanguageTool's matches through `ruleset` into flags. An empty list
    /// is a check that found nothing to flag.
    ///
    /// # Errors
    /// When the subprocess is degraded or the request fails or times out, which fails the check for
    /// this sentence. The analyzer treats a failure as no grammar flags this time, leaves the
    /// sentence's grammar part incomplete so that the next recheck sends it again, and keeps
    /// serving the other sources' flags, so a down LanguageTool never stops checking. The screen
    /// treats a failure as a run that did not happen, since a screening without grammar would read
    /// as clean.
    async fn check(
        &self,
        sentence: &str,
        ruleset: &LoadedRuleset,
        mode: FormMode,
        register: &str,
    ) -> Result<Vec<Flag>, LanguageToolError>;
}

/// Owns a running LanguageTool subprocess and the client talking to it. Cloning is cheap
/// (`Arc`-backed) and safe to share across the async runtime: [`GrammarSource::check`] takes
/// `&self`.
#[derive(Clone)]
pub struct LanguageToolSupervisor {
    inner: Arc<Inner>,
}

struct Inner {
    paths: LanguageToolPaths,
    port: u16,
    client: LanguageToolClient,
    restart_attempts: AtomicU32,
    degraded: AtomicBool,
    /// Set once, by [`LanguageToolSupervisor::shutdown`]. The supervising task watches it.
    shut_down: watch::Sender<bool>,
    /// The supervising task, taken by the first shutdown, which waits for it to end.
    supervision: Mutex<Option<JoinHandle<()>>>,
}

impl LanguageToolSupervisor {
    /// Spawns the subprocess at `paths` on the first free port at or after `preferred_port`, and
    /// waits for it to answer before returning. A background task then takes over supervising it
    /// until [`shutdown`](Self::shutdown), restarting it up to `MAX_RESTART_ATTEMPTS` times on an
    /// unexpected exit.
    ///
    /// Returns `Err` if the subprocess cannot be spawned at all, or does not become reachable
    /// within its startup timeout; this initial startup failure is distinct from, and not bounded
    /// by, the restart budget the background task applies to a later unexpected exit.
    pub async fn start(
        paths: LanguageToolPaths,
        preferred_port: u16,
    ) -> Result<Self, LanguageToolError> {
        let port = process::find_free_port(preferred_port)?;
        let child = process::spawn(&paths, port)?;
        let client = LanguageToolClient::new(port);
        process::wait_until_ready(&client).await?;
        process::warm_up(&client).await;
        Ok(Self::supervising(paths, port, client, child))
    }

    /// Hands `child`, the subprocess serving on `port`, to a background task that supervises it
    /// until shutdown.
    fn supervising(
        paths: LanguageToolPaths,
        port: u16,
        client: LanguageToolClient,
        child: Child,
    ) -> Self {
        let inner = Arc::new(Inner {
            paths,
            port,
            client,
            restart_attempts: AtomicU32::new(0),
            degraded: AtomicBool::new(false),
            shut_down: watch::channel(false).0,
            supervision: Mutex::new(None),
        });
        let task = tokio::spawn(supervise(Arc::clone(&inner), child));
        *inner
            .supervision
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(task);
        Self { inner }
    }

    /// The port the subprocess serves on: the first that was free at or after the preferred one.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Ends supervision and stops the subprocess, returning once it has exited. No restart follows,
    /// and every check through this supervisor or any clone of it answers
    /// [`LanguageToolError::ShutDown`] from then on. A call after the first returns at once.
    ///
    /// A process that leaves without calling this relies on `kill_on_drop`, which kills the
    /// subprocess when the runtime drops the supervising task and does not wait for it to exit.
    pub async fn shutdown(&self) {
        self.inner.shut_down.send_replace(true);
        let supervision = self
            .inner
            .supervision
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = supervision {
            if let Err(error) = task.await {
                log::warn!("the task supervising LanguageTool ended abnormally: {error}");
            }
        }
    }
}

#[async_trait::async_trait]
impl GrammarSource for LanguageToolSupervisor {
    async fn check(
        &self,
        sentence: &str,
        ruleset: &LoadedRuleset,
        mode: FormMode,
        register: &str,
    ) -> Result<Vec<Flag>, LanguageToolError> {
        // Checked before the request, since after a shutdown another process can hold the port.
        if *self.inner.shut_down.borrow() {
            return Err(LanguageToolError::ShutDown);
        }
        if self.inner.degraded.load(Ordering::Relaxed) {
            return Err(LanguageToolError::Degraded);
        }
        let matches = self.inner.client.check(sentence).await?;
        Ok(routing::route(matches, ruleset, mode, register))
    }
}

/// Waits for `child` to exit, then restarts it through [`respawn`] and waits again, for as long
/// as restarts remain. Ends once [`respawn`] reports the restart budget exhausted, or at shutdown,
/// when it kills the subprocess and waits for it to exit. A shutdown during a restart drops the
/// restart, and with it any subprocess the restart has spawned, which `kill_on_drop` kills.
async fn supervise(inner: Arc<Inner>, mut current: Child) {
    let mut shut_down = inner.shut_down.subscribe();
    loop {
        tokio::select! {
            exit_status = current.wait() => {
                log::warn!("LanguageTool subprocess exited: {exit_status:?}");
            }
            () = shutdown_requested(&mut shut_down) => {
                if let Err(error) = current.kill().await {
                    log::warn!("could not stop the LanguageTool subprocess: {error}");
                }
                return;
            }
        }

        tokio::select! {
            respawned = respawn(&inner) => match respawned {
                Some(child) => current = child,
                None => return,
            },
            () = shutdown_requested(&mut shut_down) => return,
        }
    }
}

/// Returns once shutdown has been requested, at once if it already has.
async fn shutdown_requested(shut_down: &mut watch::Receiver<bool>) {
    // The sender lives in `Inner`, which the supervising task holds, so it outlives this wait, and
    // the wait ends only on the flag.
    let _ = shut_down.wait_for(|shut_down| *shut_down).await;
}

/// Attempts a restart, retrying immediately on a failed spawn or a failed readiness wait, up to
/// [`MAX_RESTART_ATTEMPTS`] attempts shared across the supervisor's whole lifetime, not reset per
/// call. Returns the new child once one attempt both spawns and becomes ready, or `None` once the
/// budget is exhausted, after which [`Inner::degraded`] is set and every later
/// [`GrammarSource::check`] call answers [`LanguageToolError::Degraded`] without trying the
/// subprocess again.
async fn respawn(inner: &Inner) -> Option<Child> {
    loop {
        let attempts = inner.restart_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempts > MAX_RESTART_ATTEMPTS {
            log::error!(
                "LanguageTool exited and {MAX_RESTART_ATTEMPTS} restart attempts are exhausted; \
                 degrading to spelling and style flags alone for the rest of this session"
            );
            inner.degraded.store(true, Ordering::SeqCst);
            return None;
        }
        log::warn!("restarting LanguageTool, attempt {attempts} of {MAX_RESTART_ATTEMPTS}");

        let child = match process::spawn(&inner.paths, inner.port) {
            Ok(child) => child,
            Err(error) => {
                log::warn!("restart attempt {attempts} failed to spawn: {error}");
                continue;
            }
        };
        if let Err(error) = process::wait_until_ready(&inner.client).await {
            log::warn!("restart attempt {attempts} spawned but did not become ready: {error}");
            // `child` is dropped here uncollected; `process::spawn` sets `kill_on_drop`, so the
            // orphaned process is killed rather than left running unsupervised.
            continue;
        }
        process::warm_up(&inner.client).await;
        return Some(child);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Stdio;

    use super::*;

    /// A port nothing listens on and no check here reaches, clear of `process`'s test ports.
    const UNUSED_PORT: u16 = 45991;

    /// A stand-in for the server, which runs for a minute unless killed, spawned with
    /// `kill_on_drop` as the server is.
    fn long_running_child() -> Child {
        tokio::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    /// Whether `tasklist` lists a process with id `pid`.
    fn is_running(pid: u32) -> bool {
        let listed = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
    }

    /// A supervisor over `child` whose paths no restart can spawn from.
    fn supervisor_of(child: Child) -> LanguageToolSupervisor {
        let paths = LanguageToolPaths {
            java_bin: PathBuf::from("no-such-java.exe"),
            jar: PathBuf::from("no-such-server.jar"),
        };
        LanguageToolSupervisor::supervising(
            paths,
            UNUSED_PORT,
            LanguageToolClient::new(UNUSED_PORT),
            child,
        )
    }

    #[tokio::test]
    async fn a_shutdown_kills_the_subprocess_and_returns_once_it_has_exited() {
        let child = long_running_child();
        let pid = child.id().unwrap();
        assert!(is_running(pid));
        let supervisor = supervisor_of(child);

        supervisor.shutdown().await;

        assert!(!is_running(pid));
        assert!(supervisor
            .inner
            .supervision
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none());
    }

    #[tokio::test]
    async fn after_a_shutdown_every_clone_answers_that_it_was_shut_down_and_a_second_call_returns()
    {
        let supervisor = supervisor_of(long_running_child());
        let clone = supervisor.clone();

        supervisor.shutdown().await;

        let checked = clone
            .check(
                "A sentence.",
                &LoadedRuleset::empty(),
                FormMode::Prose,
                "default",
            )
            .await;
        assert!(
            matches!(checked, Err(LanguageToolError::ShutDown)),
            "{checked:?}"
        );
        clone.shutdown().await;
        supervisor.shutdown().await;
    }
}
