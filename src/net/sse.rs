//! Minimal Server-Sent Events parser for the Audio Pub `/live/{id}/events`
//! endpoint, plus the typed events it produces.

use serde::Deserialize;

/// A raw SSE event: the `event:` name and the joined `data:` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser. Feed it bytes as they arrive; it yields complete
/// events. Comment lines (`: keepalive`) and unknown fields are ignored.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    event_name: String,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();

        // Process only complete lines; keep any trailing partial line buffered.
        while let Some(newline) = self.buffer.find('\n') {
            let line: String = self.buffer.drain(..=newline).collect();
            let line = line.trim_end_matches(['\n', '\r']);

            if line.is_empty() {
                if !self.data_lines.is_empty() || !self.event_name.is_empty() {
                    events.push(SseEvent {
                        event: if self.event_name.is_empty() {
                            "message".to_string()
                        } else {
                            std::mem::take(&mut self.event_name)
                        },
                        data: self.data_lines.join("\n"),
                    });
                    self.event_name.clear();
                    self.data_lines.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => self.event_name = value.to_string(),
                "data" => self.data_lines.push(value.to_string()),
                _ => {}
            }
        }
        events
    }
}

/// Typed events from the Audio Pub live-events stream.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    State { state: String },
    Listeners { active: u32, peak: u32 },
    Chat(ChatMessage),
    ChatDeleted { chat_id: String },
    Archived,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub content: String,
    /// Unix milliseconds.
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    pub user: ChatUser,
}

/// The server sends both `name` (lowercased) and `displayName`; they must be
/// separate fields — declaring one as a serde alias of the other makes serde
/// reject the payload as a duplicate field when both are present.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatUser {
    pub id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
}

impl ChatUser {
    /// The name to show and speak: display name when set, else the plain name.
    pub fn display(&self) -> &str {
        if self.display_name.is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }
}

impl LiveEvent {
    /// Maps a raw SSE event to a typed one. Returns `None` for events we
    /// don't understand (forward compatibility).
    pub fn from_sse(raw: &SseEvent) -> Option<LiveEvent> {
        #[derive(Deserialize)]
        struct StateData {
            state: String,
        }
        #[derive(Deserialize)]
        struct ListenersData {
            #[serde(rename = "activeListeners")]
            active_listeners: u32,
            #[serde(rename = "peekListeners")]
            peek_listeners: u32,
        }
        #[derive(Deserialize)]
        struct ChatDeleteData {
            #[serde(rename = "chatId")]
            chat_id: String,
        }

        match raw.event.as_str() {
            "state" => serde_json::from_str::<StateData>(&raw.data)
                .ok()
                .map(|d| LiveEvent::State { state: d.state }),
            "listeners" => serde_json::from_str::<ListenersData>(&raw.data)
                .ok()
                .map(|d| LiveEvent::Listeners {
                    active: d.active_listeners,
                    peak: d.peek_listeners,
                }),
            "chat" => serde_json::from_str::<ChatMessage>(&raw.data)
                .ok()
                .map(LiveEvent::Chat),
            "chat_delete" => serde_json::from_str::<ChatDeleteData>(&raw.data)
                .ok()
                .map(|d| LiveEvent::ChatDeleted { chat_id: d.chat_id }),
            "archived" => Some(LiveEvent::Archived),
            "finish" => Some(LiveEvent::Finish),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let mut p = SseParser::new();
        let mut events = p.feed(b"event: listeners\ndata: {\"activeLis");
        assert!(events.is_empty());
        events.extend(p.feed(b"teners\":3,\"peekListeners\":5}\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            LiveEvent::from_sse(&events[0]),
            Some(LiveEvent::Listeners { active: 3, peak: 5 })
        );
    }

    #[test]
    fn ignores_keepalive_comments() {
        let mut p = SseParser::new();
        let events = p.feed(b": keepalive\n\nevent: finish\ndata: {}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(LiveEvent::from_sse(&events[0]), Some(LiveEvent::Finish));
    }

    #[test]
    fn parses_chat_event_with_full_user_object() {
        // Real server payload: user has BOTH name and displayName plus other
        // fields; both must deserialize without a duplicate-field error.
        let mut p = SseParser::new();
        let data = r#"{"id":"c1","content":"hi there","createdAt":1750000000000,"user":{"id":"u1","name":"alice","displayName":"Alice","bio":"","isBanned":false,"isVerified":true,"isTrusted":true}}"#;
        let events = p.feed(format!("event: chat\ndata: {data}\n\n").as_bytes());
        let Some(LiveEvent::Chat(chat)) = LiveEvent::from_sse(&events[0]) else {
            panic!("expected chat event");
        };
        assert_eq!(chat.user.display(), "Alice");
        assert_eq!(chat.content, "hi there");
    }

    #[test]
    fn chat_user_falls_back_to_plain_name() {
        let user: ChatUser = serde_json::from_str(r#"{"id":"u1","name":"alice"}"#).unwrap();
        assert_eq!(user.display(), "alice");
    }

    #[test]
    fn handles_crlf_and_state() {
        let mut p = SseParser::new();
        let events = p.feed(b"event: state\r\ndata: {\"state\":\"active\"}\r\n\r\n");
        assert_eq!(
            LiveEvent::from_sse(&events[0]),
            Some(LiveEvent::State {
                state: "active".into()
            })
        );
    }

    #[test]
    fn unknown_event_maps_to_none() {
        let raw = SseEvent {
            event: "mystery".into(),
            data: "{}".into(),
        };
        assert_eq!(LiveEvent::from_sse(&raw), None);
    }
}
