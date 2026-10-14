//! The bridge protocol between the extension's service worker and this server: a pull-capable
//! request/reply exchange, core-initiated, so the `Capture` trait's methods have something to
//! ask for text and issue replacements against, plus an unsolicited heartbeat that only keeps
//! the service worker's socket alive.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RequestText,
    Replace {
        anchor: String,
        local_start: usize,
        local_length: usize,
        replacement: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CurrentText { text: String },
    NoFocus,
    ReplaceResult { found: bool },
    Heartbeat,
}

impl ClientMessage {
    /// The variant's wire tag, which is all of a message that a log line or an error string may
    /// carry: a `CurrentText` holds the draft itself, so the whole message is never formatted
    /// into either.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CurrentText { .. } => "current_text",
            Self::NoFocus => "no_focus",
            Self::ReplaceResult { .. } => "replace_result",
            Self::Heartbeat => "heartbeat",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_text_round_trips_through_json() {
        let message = ServerMessage::RequestText;
        let json = serde_json::to_string(&message).expect("serializes");
        let parsed: ServerMessage = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, message);
    }

    #[test]
    fn replace_round_trips_through_json() {
        let message = ServerMessage::Replace {
            anchor: "sentence at the cursor".to_owned(),
            local_start: 9,
            local_length: 2,
            replacement: "near".to_owned(),
        };
        let json = serde_json::to_string(&message).expect("serializes");
        let parsed: ServerMessage = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, message);
    }

    #[test]
    fn current_text_round_trips_through_json() {
        let message = ClientMessage::CurrentText {
            text: "sentence at the cursor".to_owned(),
        };
        let json = serde_json::to_string(&message).expect("serializes");
        let parsed: ClientMessage = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, message);
    }

    #[test]
    fn no_focus_tags_its_variant() {
        let json = serde_json::to_string(&ClientMessage::NoFocus).expect("serializes");
        assert!(json.contains("\"type\":\"no_focus\""));
    }

    #[test]
    fn replace_result_round_trips_through_json() {
        let message = ClientMessage::ReplaceResult { found: true };
        let json = serde_json::to_string(&message).expect("serializes");
        let parsed: ClientMessage = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed, message);
    }

    #[test]
    fn heartbeat_tags_its_variant() {
        let json = serde_json::to_string(&ClientMessage::Heartbeat).expect("serializes");
        assert!(json.contains("\"type\":\"heartbeat\""));
    }

    #[test]
    fn kind_is_the_wire_tag_of_every_variant() {
        let messages = [
            ClientMessage::CurrentText {
                text: "sentence at the cursor".to_owned(),
            },
            ClientMessage::NoFocus,
            ClientMessage::ReplaceResult { found: true },
            ClientMessage::Heartbeat,
        ];
        for message in messages {
            let json = serde_json::to_value(&message).expect("serializes");
            assert_eq!(json["type"], message.kind());
        }
    }

    #[test]
    fn client_message_rejects_unknown_type() {
        let result: Result<ClientMessage, _> = serde_json::from_str(r#"{"type":"nonsense"}"#);
        assert!(result.is_err());
    }
}
