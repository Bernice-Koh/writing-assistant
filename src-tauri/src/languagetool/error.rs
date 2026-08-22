//! Errors from managing the LanguageTool subprocess and talking to it over HTTP.

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

    #[error("request to LanguageTool failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("could not parse LanguageTool's response: {0}")]
    Parse(#[from] serde_json::Error),
}
