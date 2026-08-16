//! The web capture backend's messaging bridge: a WebSocket server the browser extension's
//! service worker connects to, and the request hub that lets [`WebCapture`] answer `Capture`
//! trait calls by routing them to whichever connection is currently open.
//!
//! Chosen over Chrome's Native Messaging API because this server is already the always-running
//! desktop process (the tray app), so it is the extension that should dial in rather than Chrome
//! spawning and owning a native host per connection. `extension/manifest.json`'s
//! `minimum_chrome_version` of 116 is set for this: that release is what stopped an open
//! WebSocket's own service worker from being killed by Chrome's idle timer.
//!
//! One extension, one tab, one focused element at a time, the same assumption the native
//! backend's single `current_scope` already makes (`capture::native`): the hub tracks at most
//! one open connection and at most one in-flight `Capture` trait call, rather than queueing or
//! multiplexing either.

mod connection;
pub mod error;
pub mod message;

use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::capture::{Capture, CaptureError, CursorRect};
use error::WebCaptureError;
use message::{ClientMessage, ServerMessage};

/// Arbitrary and outside the ephemeral port range, so a stray unrelated process binding it by
/// chance is unlikely.
pub const PORT: u16 = 47_826;

// HACK(2026-08-15): an unpacked dev extension's ID depends on how Chrome derives it for that
// load, so this cannot be a real constant yet. Read the ID this server should trust from
// chrome://extensions after loading the unpacked extension, and pass it to `WebCapture::start`.
pub const DEV_PLACEHOLDER_ORIGIN: &str = "chrome-extension://REPLACE_WITH_LOADED_EXTENSION_ID";

/// Events the hub task multiplexes onto one channel: a connection opening or closing, a reply
/// arriving from the extension, and an outgoing request from a `Capture` trait call.
enum HubEvent {
    Connected(mpsc::UnboundedSender<ServerMessage>),
    Disconnected,
    Inbound(ClientMessage),
    Request(
        ServerMessage,
        oneshot::Sender<Result<ClientMessage, WebCaptureError>>,
    ),
}

/// A live bridge server: one Tokio task accepting connections, and a hub task routing `Capture`
/// trait calls to whichever connection is currently open, until `stop` is called.
pub struct WebCapture {
    local_addr: SocketAddr,
    events: mpsc::UnboundedSender<HubEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WebCapture {
    /// Binds `127.0.0.1:port` (`0` for an OS-assigned port), starts accepting connections, and
    /// starts the hub. Only handshakes whose `Origin` header equals `allowed_origin` are
    /// accepted; every other connection is rejected before any message is read.
    pub async fn start(port: u16, allowed_origin: &str) -> Result<Self, WebCaptureError> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|source| WebCaptureError::Bind { port, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| WebCaptureError::Bind { port, source })?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_hub(events_rx));

        let allowed_origin = allowed_origin.to_owned();
        let accept_events = events_tx.clone();
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) => {
                            let allowed_origin = allowed_origin.clone();
                            let events = accept_events.clone();
                            tokio::spawn(connection::handle(stream, peer, allowed_origin, events));
                        }
                        Err(error) => log::debug!("accept failed: {error}"),
                    },
                }
            }
        });

        Ok(Self {
            local_addr,
            events: events_tx,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }

    /// The address this server actually bound, useful when `start` was called with port `0`.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops accepting new connections and waits for the listener task to end. In-flight
    /// connections finish their current read on their own tasks; this only tears down the
    /// listener. The hub keeps running for as long as any connection task still holds a sender
    /// to it.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }

    /// Sends `message` to the hub and awaits the extension's reply, mapping every failure mode
    /// that isn't a per-request answer (no connection, one already in flight, a disconnect
    /// mid-request, the hub itself gone) onto [`CaptureError::Communication`].
    async fn request(&self, message: ServerMessage) -> Result<ClientMessage, CaptureError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.events
            .send(HubEvent::Request(message, reply_tx))
            .map_err(|_| CaptureError::from(WebCaptureError::HubStopped))?;
        let reply = reply_rx
            .await
            .map_err(|_| CaptureError::from(WebCaptureError::HubStopped))?;
        reply.map_err(CaptureError::from)
    }
}

#[async_trait::async_trait]
impl Capture for WebCapture {
    async fn current_text(&self) -> Result<String, CaptureError> {
        match self.request(ServerMessage::RequestText).await? {
            ClientMessage::CurrentText { text } => Ok(text),
            ClientMessage::NoFocus => Err(CaptureError::NoFocus),
            other => Err(unexpected_reply(&other)),
        }
    }

    async fn cursor_rect(&self) -> Result<CursorRect, CaptureError> {
        Err(CaptureError::Unsupported)
    }

    async fn replace(
        &self,
        anchor: &str,
        local_start: usize,
        local_length: usize,
        replacement: &str,
    ) -> Result<(), CaptureError> {
        let message = ServerMessage::Replace {
            anchor: anchor.to_owned(),
            local_start,
            local_length,
            replacement: replacement.to_owned(),
        };
        match self.request(message).await? {
            ClientMessage::ReplaceResult { found: true } => Ok(()),
            ClientMessage::ReplaceResult { found: false } => Err(CaptureError::TextNotFound),
            other => Err(unexpected_reply(&other)),
        }
    }
}

fn unexpected_reply(message: &ClientMessage) -> CaptureError {
    CaptureError::from(WebCaptureError::UnexpectedReply(format!("{message:?}")))
}

/// Owns the hub's state: at most one open connection, at most one in-flight request. Runs until
/// every [`HubEvent`] sender (the `WebCapture` handle and every live connection task) is
/// dropped.
async fn run_hub(mut events: mpsc::UnboundedReceiver<HubEvent>) {
    let mut active: Option<mpsc::UnboundedSender<ServerMessage>> = None;
    let mut pending: Option<oneshot::Sender<Result<ClientMessage, WebCaptureError>>> = None;

    while let Some(event) = events.recv().await {
        match event {
            HubEvent::Connected(outbound) => {
                log::info!("extension connected");
                active = Some(outbound);
            }
            HubEvent::Disconnected => {
                log::info!("extension disconnected");
                active = None;
                if let Some(reply) = pending.take() {
                    let _ = reply.send(Err(WebCaptureError::Disconnected));
                }
            }
            HubEvent::Inbound(ClientMessage::Heartbeat) => {
                log::debug!("heartbeat received");
            }
            HubEvent::Inbound(message) => match pending.take() {
                Some(reply) => {
                    let _ = reply.send(Ok(message));
                }
                None => log::debug!("unsolicited reply from extension: {message:?}"),
            },
            HubEvent::Request(message, reply) => {
                if pending.is_some() {
                    let _ = reply.send(Err(WebCaptureError::RequestInFlight));
                    continue;
                }
                let Some(outbound) = &active else {
                    let _ = reply.send(Err(WebCaptureError::NoConnection));
                    continue;
                };
                if outbound.send(message).is_err() {
                    let _ = reply.send(Err(WebCaptureError::Disconnected));
                    continue;
                }
                pending = Some(reply);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    use tokio_tungstenite::tungstenite::http::Request as HttpRequest;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::MaybeTlsStream;

    use super::*;

    const TEST_ORIGIN: &str = "chrome-extension://test";
    type TestClient = tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    async fn connect_test_client(addr: SocketAddr) -> TestClient {
        let request = HttpRequest::builder()
            .uri(format!("ws://{addr}/"))
            .header("Host", addr.to_string())
            .header("Origin", TEST_ORIGIN)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .body(())
            .expect("valid handshake request");
        let (stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("client connects");
        stream
    }

    async fn next_server_message(client: &mut TestClient) -> ServerMessage {
        let message = client
            .next()
            .await
            .expect("connection open")
            .expect("read succeeds");
        serde_json::from_str(&message.into_text().expect("text frame")).expect("parses")
    }

    async fn reply_with(client: &mut TestClient, message: ClientMessage) {
        let payload = serde_json::to_string(&message).expect("serializes");
        client.send(Message::Text(payload)).await.expect("sends");
    }

    #[tokio::test]
    async fn cursor_rect_is_always_unsupported() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let result = capture.cursor_rect().await;
        assert!(matches!(result, Err(CaptureError::Unsupported)));
    }

    #[tokio::test]
    async fn current_text_round_trips_with_a_connected_extension() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let mut client = connect_test_client(capture.local_addr()).await;
        let call = tokio::spawn(async move { capture.current_text().await });

        assert_eq!(
            next_server_message(&mut client).await,
            ServerMessage::RequestText
        );
        reply_with(
            &mut client,
            ClientMessage::CurrentText {
                text: "draft text".to_owned(),
            },
        )
        .await;

        let text = call.await.expect("task joins").expect("succeeds");
        assert_eq!(text, "draft text");
    }

    #[tokio::test]
    async fn current_text_no_focus_maps_to_no_focus_error() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let mut client = connect_test_client(capture.local_addr()).await;
        let call = tokio::spawn(async move { capture.current_text().await });

        next_server_message(&mut client).await;
        reply_with(&mut client, ClientMessage::NoFocus).await;

        let result = call.await.expect("task joins");
        assert!(matches!(result, Err(CaptureError::NoFocus)));
    }

    #[tokio::test]
    async fn replace_found_succeeds() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let mut client = connect_test_client(capture.local_addr()).await;
        let call = tokio::spawn(async move { capture.replace("anchor", 0, 6, "target").await });

        let request = next_server_message(&mut client).await;
        assert_eq!(
            request,
            ServerMessage::Replace {
                anchor: "anchor".to_owned(),
                local_start: 0,
                local_length: 6,
                replacement: "target".to_owned(),
            }
        );
        reply_with(&mut client, ClientMessage::ReplaceResult { found: true }).await;

        call.await.expect("task joins").expect("succeeds");
    }

    #[tokio::test]
    async fn replace_not_found_maps_to_text_not_found() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let mut client = connect_test_client(capture.local_addr()).await;
        let call = tokio::spawn(async move { capture.replace("missing", 0, 1, "x").await });

        next_server_message(&mut client).await;
        reply_with(&mut client, ClientMessage::ReplaceResult { found: false }).await;

        let result = call.await.expect("task joins");
        assert!(matches!(result, Err(CaptureError::TextNotFound)));
    }

    #[tokio::test]
    async fn no_connection_yields_communication_error() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let result = capture.current_text().await;
        assert!(matches!(result, Err(CaptureError::Communication(_))));
    }

    #[tokio::test]
    async fn disconnect_mid_request_resolves_pending_with_an_error() {
        let capture = WebCapture::start(0, TEST_ORIGIN).await.expect("starts");
        let mut client = connect_test_client(capture.local_addr()).await;
        let call = tokio::spawn(async move { capture.current_text().await });

        next_server_message(&mut client).await;
        drop(client);

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), call)
            .await
            .expect("does not hang")
            .expect("task joins");
        assert!(matches!(result, Err(CaptureError::Communication(_))));
    }
}
