//! Per-connection handshake and message loop. Split from `mod.rs` because the listener's job
//! (accept and hand off) and a connection's job (verify origin, then relay) are different
//! concerns.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

use super::message::{ClientMessage, ServerMessage};

/// Accepts one connection, rejecting the handshake unless its `Origin` header matches
/// `allowed_origin` exactly. A localhost port is reachable by anything on the machine, so the
/// origin check is what actually confines this server to the one extension it is meant for,
/// rather than the round trip itself.
pub async fn handle(stream: TcpStream, peer: SocketAddr, allowed_origin: String) {
    // tungstenite's `Callback` trait fixes this `Result`'s shape; `ErrorResponse` wraps a full
    // `http::Response` and cannot be shrunk from this side of the trait.
    #[allow(clippy::result_large_err)]
    let callback = move |request: &Request, response: Response| {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok());
        if origin == Some(allowed_origin.as_str()) {
            Ok(response)
        } else {
            log::warn!("rejected WebSocket handshake from {peer}: origin {origin:?} not allowed");
            let mut rejection = ErrorResponse::new(Some("origin not allowed".to_owned()));
            *rejection.status_mut() = StatusCode::FORBIDDEN;
            Err(rejection)
        }
    };

    let stream = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
        Ok(stream) => stream,
        Err(error) => {
            log::debug!("handshake with {peer} failed: {error}");
            return;
        }
    };
    log::info!("accepted WebSocket connection from {peer}");

    let (mut sink, mut source) = stream.split();
    while let Some(message) = source.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                log::debug!("read error from {peer}: {error}");
                break;
            }
        };
        let Ok(text) = message.into_text() else {
            continue;
        };
        // Length only, never the text itself: this channel carries the user's draft.
        log::debug!("received {} bytes from {peer}", text.len());

        let reply = match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Capture { text }) => ServerMessage::Echo {
                text: format!("[core] {text}"),
            },
            Err(error) => {
                log::debug!("malformed message from {peer}: {error}");
                continue;
            }
        };
        let payload = match serde_json::to_string(&reply) {
            Ok(payload) => payload,
            Err(error) => {
                log::debug!("failed to encode reply for {peer}: {error}");
                continue;
            }
        };
        if let Err(error) = sink.send(Message::Text(payload)).await {
            log::debug!("write error to {peer}: {error}");
            break;
        }
    }
    log::info!("connection from {peer} closed");
}
