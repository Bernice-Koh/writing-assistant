//! Typed failures for the web capture backend, raised at the deepest layer that can name them,
//! degrading rather than crashing the caller.

#[derive(Debug, thiserror::Error)]
pub enum WebCaptureError {
    #[error("could not bind the local WebSocket server to 127.0.0.1:{port}: {source}")]
    Bind { port: u16, source: std::io::Error },
    #[error("no extension is currently connected")]
    NoConnection,
    #[error("a request to the extension is already in flight")]
    RequestInFlight,
    #[error("the extension disconnected before replying")]
    Disconnected,
    #[error("the request hub is not running")]
    HubStopped,
    #[error("the extension sent an unexpected reply: {0}")]
    UnexpectedReply(String),
}

/// Maps this backend's own, specific failure modes onto the `Capture` trait's shared
/// vocabulary at the trait boundary, per #19: a caller of the trait never matches on
/// `WebCaptureError` directly. Every variant becomes `Communication`: none of them are the
/// per-request `Unsupported`, `TextNotFound`, or `NoFocus` cases, which the trait methods
/// already produce by reading the extension's reply directly rather than through this type.
/// What is left is uniformly "the bridge to the extension itself is not working right now".
impl From<WebCaptureError> for crate::capture::CaptureError {
    fn from(error: WebCaptureError) -> Self {
        crate::capture::CaptureError::Communication(error.to_string())
    }
}
