//! The bridge protocol between the extension's service worker and this server. Minimal by
//! design: this spike proves a round trip, not the capture contract the real web backend will
//! eventually speak.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Capture { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Echo { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_round_trips_through_json() {
        let message = ClientMessage::Capture {
            text: "sentence at the cursor".to_owned(),
        };
        let json = serde_json::to_string(&message).expect("serializes");
        let parsed: ClientMessage = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, message);
    }

    #[test]
    fn server_message_tags_its_variant() {
        let message = ServerMessage::Echo {
            text: "[core] sentence at the cursor".to_owned(),
        };
        let json = serde_json::to_string(&message).expect("serializes");
        assert!(json.contains("\"type\":\"echo\""));
    }

    #[test]
    fn client_message_rejects_unknown_type() {
        let result: Result<ClientMessage, _> = serde_json::from_str(r#"{"type":"nonsense"}"#);
        assert!(result.is_err());
    }
}
