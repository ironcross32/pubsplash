//! ElevenLabs, over the REST API.
//!
//! Asked for `pcm_24000` rather than the default MP3: the reference Python
//! client decodes MP3 here for no reason, and the raw form is both faster and
//! lossless.

use crate::audio::convert::pcm16_to_engine;
use crate::config::SpeechConfig;
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, body_json, client, require};

const API_BASE: &str = "https://api.elevenlabs.io/v1";
const SERVICE: &str = "ElevenLabs";
const DEFAULT_VOICE: &str = "21m00Tcm4TlvDq8ikWAM"; // Rachel
const SOURCE_RATE: u32 = 24_000;
const SOURCE_CHANNELS: usize = 1;

pub const MODELS: &[&str] = &[
    "eleven_multilingual_v2",
    "eleven_v3",
    "eleven_turbo_v2_5",
    "eleven_turbo_v2",
    "eleven_monolingual_v1",
];

pub struct ElevenLabs {
    api_key: String,
    model: String,
}

impl ElevenLabs {
    pub fn new(config: &SpeechConfig) -> Self {
        let model = config.elevenlabs_model.trim();
        Self {
            api_key: config.elevenlabs_api_key.as_str().to_string(),
            model: if model.is_empty() {
                MODELS[0].to_string()
            } else {
                model.to_string()
            },
        }
    }
}

impl SpeechEngine for ElevenLabs {
    fn id(&self) -> &'static str {
        super::ELEVENLABS
    }

    fn display_name(&self) -> &'static str {
        "ElevenLabs"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let key = require(&self.api_key, "The ElevenLabs API key")?;
        let voice = if request.voice.is_empty() {
            DEFAULT_VOICE
        } else {
            &request.voice
        };
        // The API rejects speeds outside this window.
        let speed = request.rate_multiplier().clamp(0.7, 1.2);

        let bytes = block_on(async {
            let response = client()
                .post(format!("{API_BASE}/text-to-speech/{voice}"))
                .header("xi-api-key", key)
                .query(&[("output_format", "pcm_24000")])
                .json(&serde_json::json!({
                    "text": request.text,
                    "model_id": self.model,
                    "voice_settings": { "speed": speed },
                }))
                .send()
                .await?;
            body_bytes(SERVICE, response).await
        })?;

        let mut samples = pcm16_to_engine(&bytes, SOURCE_RATE, SOURCE_CHANNELS);
        request.apply_volume(&mut samples);
        Ok(samples)
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        let key = require(&self.api_key, "The ElevenLabs API key")?;
        let body: serde_json::Value = block_on(async {
            let response = client()
                .get(format!("{API_BASE}/voices"))
                .header("xi-api-key", key)
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;
        Ok(parse_voices(&body))
    }
}

fn parse_voices(body: &serde_json::Value) -> Vec<Voice> {
    let mut voices: Vec<Voice> = body
        .get("voices")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let id = entry.get("voice_id")?.as_str()?;
                    let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or(id);
                    Some(Voice::new(id, name))
                })
                .collect()
        })
        .unwrap_or_default();
    voices.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_lists_are_parsed_and_sorted_by_name() {
        let body = serde_json::json!({
            "voices": [
                {"voice_id": "b", "name": "Zoe"},
                {"voice_id": "a", "name": "adam"},
                {"voice_id": "c"},
                {"name": "no id here"}
            ]
        });
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 3, "the entry without an id must be skipped");
        assert_eq!(voices[0].id, "a");
        assert_eq!(voices[1].label, "c", "a missing name falls back to the id");
        assert_eq!(voices[2].label, "Zoe");
    }

    #[test]
    fn an_unexpected_shape_yields_no_voices_rather_than_panicking() {
        assert!(parse_voices(&serde_json::json!({})).is_empty());
        assert!(parse_voices(&serde_json::json!({"voices": "nope"})).is_empty());
    }

    #[test]
    fn a_blank_configured_model_falls_back_to_the_default() {
        let mut config = SpeechConfig::default();
        config.elevenlabs_model = "  ".into();
        assert_eq!(ElevenLabs::new(&config).model, MODELS[0]);
    }

    #[test]
    fn a_missing_key_is_reported_for_both_synthesis_and_voice_listing() {
        let engine = ElevenLabs::new(&SpeechConfig::default());
        assert!(matches!(
            engine.voices().unwrap_err(),
            TtsError::NotConfigured(_)
        ));
        let error = engine
            .synth(&SynthRequest {
                text: "hi".into(),
                voice: String::new(),
                rate: 0,
                volume: 100,
                pitch: 0,
            })
            .unwrap_err();
        assert!(matches!(error, TtsError::NotConfigured(_)), "{error}");
    }
}
