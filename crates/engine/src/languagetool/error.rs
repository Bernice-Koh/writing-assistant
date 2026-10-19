//! Errors from managing the LanguageTool subprocess and talking to it over HTTP. No rendering of
//! one carries the text a check was sent, so a log line may print any of them.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LanguageToolError {
    #[error("no free port found near {preferred}")]
    NoFreePort { preferred: u16 },

    #[error("could not spawn the LanguageTool subprocess at {java_bin}: {source}", java_bin = java_bin.display())]
    Spawn {
        java_bin: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("LanguageTool did not become reachable within {timeout_ms} ms of starting")]
    StartupTimeout { timeout_ms: u64 },

    /// The supervisor exhausted its restart attempts, so no check is sent for the rest of the
    /// session.
    #[error("LanguageTool is degraded, its restart attempts exhausted")]
    Degraded,

    /// Held without its URL, since a check's URL carries the text checked in its query.
    #[error("request to LanguageTool failed: {0}")]
    Request(#[source] reqwest::Error),
}

impl From<reqwest::Error> for LanguageToolError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error.without_url())
    }
}
