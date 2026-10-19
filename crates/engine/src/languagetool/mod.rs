//! Manages the LanguageTool subprocess (launch, readiness, bounded restart on an unexpected
//! exit) and runs the grammar stage's checks against it, in LanguageTool's `en-GB` variant and its
//! picky mode. `process` covers the subprocess itself; `client` the HTTP API, the shape of a
//! response and the route of each of LanguageTool's categories; `routing` the turn of one
//! sentence's matches into [`crate::flag::Flag`]s through the loaded ruleset.

mod client;
pub mod error;
mod process;
mod routing;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tokio::process::Child;

pub use client::LanguageToolClient;
pub use error::LanguageToolError;
pub use process::{default_paths, LanguageToolPaths};

use crate::flag::Flag;
use crate::style::checker::FormMode;
use crate::style::ruleset::LoadedRuleset;

/// How many times [`supervise`] restarts the subprocess after an unexpected exit before giving
/// up and staying degraded for the rest of the session. Three gives a real subprocess crash-loop
/// (a bad JVM flag, a corrupt bundled jar) a couple of chances to be transient before the app
/// commits to running without grammar checking rather than restarting forever.
const MAX_RESTART_ATTEMPTS: u32 = 3;

/// Owns a running LanguageTool subprocess and the client talking to it. Cloning is cheap
/// (`Arc`-backed) and safe to share across the async runtime: [`Self::check`] takes `&self`.
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
}

impl LanguageToolSupervisor {
    /// Spawns the subprocess at `paths` on the first free port at or after `preferred_port`, and
    /// waits for it to answer before returning. A background task then takes over supervising it
    /// for the rest of this supervisor's life, restarting it up to `MAX_RESTART_ATTEMPTS` times
    /// on an unexpected exit.
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

        let inner = Arc::new(Inner {
            paths,
            port,
            client,
            restart_attempts: AtomicU32::new(0),
            degraded: AtomicBool::new(false),
        });

        tokio::spawn(supervise(Arc::clone(&inner), child));

        Ok(Self { inner })
    }

    /// Runs the grammar stage's check of `sentence`, one sentence of form mode `mode` under
    /// `register`, and routes LanguageTool's matches through `ruleset` into flags. An empty list
    /// is a check that found nothing to flag.
    ///
    /// # Errors
    /// When the subprocess is degraded or the request fails, which fails the check for this
    /// sentence. The caller treats a failure as no grammar flags this time and keeps serving the
    /// other sources' flags, so a down LanguageTool never stops checking.
    pub async fn check(
        &self,
        sentence: &str,
        ruleset: &LoadedRuleset,
        mode: FormMode,
        register: &str,
    ) -> Result<Vec<Flag>, LanguageToolError> {
        if self.inner.degraded.load(Ordering::Relaxed) {
            return Err(LanguageToolError::Degraded);
        }
        let matches = self.inner.client.check(sentence).await?;
        Ok(routing::route(matches, ruleset, mode, register))
    }
}

/// Waits for `child` to exit, then restarts it through [`respawn`] and waits again, for as long
/// as restarts remain. Ends once [`respawn`] reports the restart budget exhausted.
async fn supervise(inner: Arc<Inner>, mut current: Child) {
    loop {
        let exit_status = current.wait().await;
        log::warn!("LanguageTool subprocess exited: {exit_status:?}");

        match respawn(&inner).await {
            Some(child) => current = child,
            None => return,
        }
    }
}

/// Attempts a restart, retrying immediately on a failed spawn or a failed readiness wait, up to
/// [`MAX_RESTART_ATTEMPTS`] attempts shared across the supervisor's whole lifetime, not reset per
/// call. Returns the new child once one attempt both spawns and becomes ready, or `None` once the
/// budget is exhausted, after which [`Inner::degraded`] is set and every later
/// [`LanguageToolSupervisor::check`] call answers `None` without trying the subprocess again.
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
