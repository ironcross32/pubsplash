//! OpenAI speech, over the REST API.
//!
//! Asked for `pcm`, which is headerless signed 16-bit little-endian mono at
//! 24 kHz — no decoding, just a rate conversion. The reference Python client
//! does the same; it is the one place it avoids an MP3 round-trip.

use crate::audio::convert::pcm16_to_engine;
use crate::config::SpeechConfig;
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, client, require};

const URL: &str = "https://api.openai.com/v1/audio/speech";
const SERVICE: &str = "OpenAI";
/// The format `response_format: "pcm"` is documented to return.
const SOURCE_RATE: u32 = 24_000;
const SOURCE_CHANNELS: usize = 1;

const VOICES: &[&str] = &[
    "alloy", "ash", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer",
];

pub struct OpenAi {
    api_key: String,
    model: String,
}

impl OpenAi {
    pub fn new(config: &SpeechConfig) -> Self {
        Self {
            api_key: config.openai_api_key.as_str().to_string(),
            model: "tts-1".into(),
        }
    }
}

impl SpeechEngine for OpenAi {
    fn id(&self) -> &'static str {
        super::OPENAI
    }

    fn display_name(&self) -> &'static str {
        "OpenAI"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let key = require(&self.api_key, "The OpenAI API key")?;
        let voice = if request.voice.is_empty() {
            "alloy"
        } else {
            &request.voice
        };
        // The API rejects anything outside 0.25..=4.0 outright.
        let speed = request.rate_multiplier().clamp(0.25, 4.0);

        let bytes = block_on(async {
            let response = client()
                .post(URL)
                .bearer_auth(key)
                .json(&serde_json::json!({
                    "model": self.model,
                    "voice": voice,
                    "input": request.text,
                    "speed": speed,
                    "response_format": "pcm",
                }))
                .send()
                .await?;
            body_bytes(SERVICE, response).await
        })?;

        let mut samples = pcm16_to_engine(&bytes, SOURCE_RATE, SOURCE_CHANNELS);
        // OpenAI has no volume parameter, so the slider is applied here.
        request.apply_volume(&mut samples);
        Ok(samples)
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        Ok(VOICES.iter().copied().map(Voice::plain).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices_are_the_documented_fixed_set_and_need_no_network() {
        let engine = OpenAi::new(&SpeechConfig::default());
        let voices = engine.voices().unwrap();
        assert_eq!(voices.len(), VOICES.len());
        assert!(voices.iter().any(|v| v.id == "alloy"));
    }

    #[test]
    fn a_missing_key_is_reported_before_any_request() {
        let engine = OpenAi::new(&SpeechConfig::default());
        let error = engine
            .synth(&SynthRequest {
                text: "hello".into(),
                voice: String::new(),
                rate: 0,
                volume: 100,
                pitch: 0,
            })
            .unwrap_err();
        assert!(matches!(error, TtsError::NotConfigured(_)), "{error}");
    }
}
