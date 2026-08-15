//! Typed failures for the web capture backend, raised at the deepest layer that can name them.

#[derive(Debug, thiserror::Error)]
pub enum WebCaptureError {
    #[error("could not bind the local WebSocket server to 127.0.0.1:{port}: {source}")]
    Bind { port: u16, source: std::io::Error },
}
