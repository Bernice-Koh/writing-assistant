//! The web capture backend's messaging bridge: a WebSocket server the browser extension's
//! service worker connects to, standing in for the future capture contract until there is a
//! second real backend to design that contract against.
//!
//! Chosen over Chrome's Native Messaging API because this server is already the always-running
//! desktop process (the tray app), so it is the extension that should dial in rather than Chrome
//! spawning and owning a native host per connection. `extension/manifest.json`'s
//! `minimum_chrome_version` of 116 is set for this: that release is what stopped an open
//! WebSocket's own service worker from being killed by Chrome's idle timer.

mod connection;
pub mod error;
pub mod message;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use error::WebCaptureError;

/// Arbitrary and outside the ephemeral port range, so a stray unrelated process binding it by
/// chance is unlikely.
pub const PORT: u16 = 47_826;

// HACK(2026-08-15): an unpacked dev extension's ID depends on how Chrome derives it for that
// load, so this cannot be a real constant yet. Read the ID this server should trust from
// chrome://extensions after loading the unpacked extension, and pass it to `WebCapture::start`.
pub const DEV_PLACEHOLDER_ORIGIN: &str = "chrome-extension://REPLACE_WITH_LOADED_EXTENSION_ID";

/// A live bridge server: one Tokio task accepting connections until `stop` is called.
pub struct WebCapture {
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WebCapture {
    /// Binds `127.0.0.1:port` and starts accepting connections. Only handshakes whose `Origin`
    /// header equals `allowed_origin` are accepted; every other connection is rejected before
    /// any message is read.
    pub async fn start(port: u16, allowed_origin: &str) -> Result<Self, WebCaptureError> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|source| WebCaptureError::Bind { port, source })?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let allowed_origin = allowed_origin.to_owned();

        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) => {
                            let allowed_origin = allowed_origin.clone();
                            tokio::spawn(connection::handle(stream, peer, allowed_origin));
                        }
                        Err(error) => log::debug!("accept failed: {error}"),
                    },
                }
            }
        });

        Ok(Self {
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }

    /// Stops accepting new connections and waits for the listener task to end. In-flight
    /// connections finish their current read on their own tasks; this only tears down the
    /// listener.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}
