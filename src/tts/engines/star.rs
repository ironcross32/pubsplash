//! Star coagulator client.
//!
//! Star is a self-hosted synthesis server that speaks a small WebSocket
//! protocol: send a JSON request, get one binary frame back holding a
//! little-endian `u16` metadata length, that many bytes of JSON, then the
//! encoded audio. The metadata's `extension` field names the format.

use crate::config::SpeechConfig;
use crate::tts::decode::decode_compressed;
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::block_on;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

const SERVICE: &str = "Star";
/// The protocol's client identifier; the reference client sends 4.
const USER: u32 = 4;

pub struct Star {
    host: String,
}

impl Star {
    pub fn new(config: &SpeechConfig) -> Self {
        Self {
            host: config.star_host.trim().to_string(),
        }
    }

    fn host(&self) -> Result<&str, TtsError> {
        if self.host.is_empty() {
            return Err(TtsError::NotConfigured(
                "The Star server URL is not set. Enter it in Preferences, Speech.".into(),
            ));
        }
        Ok(&self.host)
    }

    /// Sends one request and returns the single reply frame.
    async fn exchange(&self, payload: String) -> Result<Message, TtsError> {
        let host = self.host()?;
        let (mut socket, _) = tokio_tungstenite::connect_async(host)
            .await
            .map_err(|e| TtsError::Network(format!("could not reach {host}: {e}")))?;
        socket
            .send(Message::Text(payload.into()))
            .await
            .map_err(|e| TtsError::Network(e.to_string()))?;
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| TtsError::Network("the server closed the connection".into()))?
                .map_err(|e| TtsError::Network(e.to_string()))?;
            match message {
                // Keepalives arrive before the payload on a busy server.
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(TtsError::Network("the server closed the connection".into()));
                }
                other => return Ok(other),
            }
        }
    }
}

impl SpeechEngine for Star {
    fn id(&self) -> &'static str {
        super::STAR
    }

    fn display_name(&self) -> &'static str {
        "Star"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        self.host()?;
        // The voice is addressed by prefixing the text, not by a field.
        let spoken = if request.voice.is_empty() {
            request.text.clone()
        } else {
            format!("{}: {}", request.voice, request.text)
        };
        let payload = serde_json::json!({ "user": USER, "request": [spoken] }).to_string();

        let message = block_on(self.exchange(payload))?;
        let bytes = match message {
            Message::Binary(bytes) => bytes.to_vec(),
            // A text reply is how the server reports an error.
            Message::Text(text) => {
                return Err(TtsError::Service {
                    service: SERVICE,
                    status: 0,
                    detail: text.to_string(),
                });
            }
            _ => return Err(TtsError::Network("unexpected reply from the server".into())),
        };

        let (extension, audio) = split_frame(&bytes)?;
        let mut samples = decode_compressed(SERVICE, audio, &extension)?;
        request.apply_volume(&mut samples);
        Ok(samples)
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        self.host()?;
        let payload = serde_json::json!({ "user": USER }).to_string();
        let message = block_on(self.exchange(payload))?;
        let Message::Text(text) = message else {
            return Ok(Vec::new());
        };
        let body: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| TtsError::Other(e.to_string()))?;
        Ok(body
            .get("voices")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str())
                    .map(Voice::plain)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Splits a reply frame into its declared file extension and the audio bytes.
fn split_frame(bytes: &[u8]) -> Result<(String, Vec<u8>), TtsError> {
    let malformed = || TtsError::Decode(SERVICE, "the reply frame was malformed".into());
    let length = u16::from_le_bytes([
        *bytes.first().ok_or_else(malformed)?,
        *bytes.get(1).ok_or_else(malformed)?,
    ]) as usize;
    let meta_end = 2usize.checked_add(length).ok_or_else(malformed)?;
    let meta = bytes.get(2..meta_end).ok_or_else(malformed)?;
    let audio = bytes.get(meta_end..).ok_or_else(malformed)?.to_vec();

    // A missing or unreadable extension is not fatal: the decoder sniffs.
    let extension = serde_json::from_slice::<serde_json::Value>(meta)
        .ok()
        .and_then(|v| v.get("extension")?.as_str().map(str::to_string))
        .unwrap_or_default();
    Ok((extension, audio))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(meta: &str, audio: &[u8]) -> Vec<u8> {
        let mut bytes = (meta.len() as u16).to_le_bytes().to_vec();
        bytes.extend_from_slice(meta.as_bytes());
        bytes.extend_from_slice(audio);
        bytes
    }

    #[test]
    fn a_well_formed_frame_yields_its_extension_and_audio() {
        let bytes = frame(r#"{"extension":"ogg"}"#, b"AUDIO");
        let (extension, audio) = split_frame(&bytes).unwrap();
        assert_eq!(extension, "ogg");
        assert_eq!(audio, b"AUDIO");
    }

    /// A server that omits the extension must still get its audio played.
    #[test]
    fn a_frame_without_an_extension_still_yields_audio() {
        let (extension, audio) = split_frame(&frame("{}", b"AUDIO")).unwrap();
        assert!(extension.is_empty());
        assert_eq!(audio, b"AUDIO");

        let (extension, audio) = split_frame(&frame("not json", b"AUDIO")).unwrap();
        assert!(extension.is_empty());
        assert_eq!(audio, b"AUDIO");
    }

    /// A length field longer than the frame must not index out of bounds.
    #[test]
    fn a_truncated_or_lying_frame_is_an_error_not_a_panic() {
        assert!(split_frame(&[]).is_err());
        assert!(split_frame(&[0x01]).is_err());
        assert!(split_frame(&[0xff, 0xff, 0x00]).is_err());
        // Exactly the header, no metadata and no audio.
        let (extension, audio) = split_frame(&[0x00, 0x00]).unwrap();
        assert!(extension.is_empty() && audio.is_empty());
    }

    #[test]
    fn a_blank_host_is_reported_rather_than_dialled() {
        let mut config = SpeechConfig::default();
        config.star_host = "  ".into();
        let engine = Star::new(&config);
        assert!(matches!(
            engine.voices().unwrap_err(),
            TtsError::NotConfigured(_)
        ));
    }
}
