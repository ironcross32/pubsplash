//! OpenAI speech, over the REST API.
//!
//! Asked for `pcm`, which is headerless signed 16-bit little-endian mono at
//! 24 kHz — no decoding, just a rate conversion. The reference Python client
//! does the same; it is the one place it avoids an MP3 round-trip.

use crate::audio::convert::pcm16_to_engine;
use crate::config::{SpeechConfig, TtsEngineSettings};
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, client, require};

const URL: &str = "https://api.openai.com/v1/audio/speech";
const SERVICE: &str = "OpenAI";
/// The format `response_format: "pcm"` is documented to return.
const SOURCE_RATE: u32 = 24_000;
const SOURCE_CHANNELS: usize = 1;

const VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "fable", "marin", "nova", "onyx", "sage",
    "shimmer", "verse",
];

pub struct OpenAi {
    api_key: String,
}

impl OpenAi {
    pub fn new(config: &SpeechConfig) -> Self {
        Self {
            api_key: config.openai_api_key.as_str().to_string(),
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
        let body = request_body(request, voice, speed);

        let bytes = block_on(async {
            let response = client()
                .post(URL)
                .bearer_auth(key)
                .json(&body)
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

fn request_body(request: &SynthRequest, voice: &str, speed: f32) -> serde_json::Value {
    let selected = match &request.provider_settings {
        Some(TtsEngineSettings::OpenAi(settings)) => Some(settings),
        _ => None,
    };
    let model = selected
        .map(|settings| settings.model.trim())
        .filter(|model| !model.is_empty())
        .unwrap_or("tts-1");
    let mut body = serde_json::json!({
        "model": model,
        "voice": voice,
        "input": request.text,
        "speed": speed,
        "response_format": "pcm",
    });
    if model.starts_with("gpt-4o-mini-tts") {
        if let Some(instructions) = selected
            .map(|settings| settings.instructions.trim())
            .filter(|instructions| !instructions.is_empty())
        {
            body["instructions"] = serde_json::json!(instructions);
        }
    }
    body
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
                provider_settings: None,
            })
            .unwrap_err();
        assert!(matches!(error, TtsError::NotConfigured(_)), "{error}");
    }

    #[test]
    fn instructions_are_only_sent_to_the_model_that_supports_them() {
        let mut request = SynthRequest {
            text: "hello".into(),
            voice: "coral".into(),
            rate: 0,
            volume: 100,
            pitch: 0,
            provider_settings: Some(TtsEngineSettings::OpenAi(
                crate::config::OpenAiTtsSettings {
                    model: "gpt-4o-mini-tts".into(),
                    instructions: "Warm and welcoming".into(),
                },
            )),
        };
        let body = request_body(&request, "coral", 1.0);
        assert_eq!(body["instructions"], "Warm and welcoming");

        request.provider_settings = Some(TtsEngineSettings::OpenAi(
            crate::config::OpenAiTtsSettings {
                model: "tts-1-hd".into(),
                instructions: "ignored".into(),
            },
        ));
        assert!(
            request_body(&request, "coral", 1.0)
                .get("instructions")
                .is_none()
        );
    }
}

/// Authenticates the key and discovers selectable speech models and voices.
pub fn discover(config: &SpeechConfig) -> Result<crate::tts::catalog::EngineCatalog, TtsError> {
    use crate::tts::catalog::EngineCatalog;
    use crate::tts::net::body_json;
    let key = require(config.openai_api_key.as_str(), "The OpenAI API key")?;
    let body: serde_json::Value = block_on(async {
        let response = client()
            .get("https://api.openai.com/v1/models")
            .bearer_auth(key)
            .send()
            .await?;
        body_json(SERVICE, response).await
    })?;
    let models = parse_models(&body);
    Ok(EngineCatalog::from_voices(
        models,
        VOICES.iter().copied().map(Voice::plain).collect(),
    ))
}

fn parse_models(body: &serde_json::Value) -> Vec<crate::tts::catalog::CatalogModel> {
    let mut models: Vec<_> = body
        .get("data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("id").and_then(|id| id.as_str()))
        .filter(|id| id.contains("tts") || id.contains("speech"))
        .map(crate::tts::catalog::CatalogModel::plain)
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn discovery_filters_non_speech_models_and_deduplicates() {
        let models = parse_models(&serde_json::json!({"data": [
            {"id": "gpt-4o"},
            {"id": "tts-1"},
            {"id": "gpt-4o-mini-tts"},
            {"id": "tts-1"}
        ]}));
        let ids: Vec<_> = models.into_iter().map(|model| model.id).collect();
        assert_eq!(ids, vec!["gpt-4o-mini-tts", "tts-1"]);
    }
}
