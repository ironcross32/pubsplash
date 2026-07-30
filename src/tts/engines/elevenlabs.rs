//! ElevenLabs, over the REST API.
//!
//! Asked for `pcm_24000` rather than the default MP3: the reference Python
//! client decodes MP3 here for no reason, and the raw form is both faster and
//! lossless.

use crate::audio::convert::pcm16_to_engine;
use crate::config::{SpeechConfig, TtsEngineSettings};
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
    "eleven_flash_v2_5",
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
        let body = request_body(&self.model, request);

        let bytes = block_on(async {
            let response = client()
                .post(format!("{API_BASE}/text-to-speech/{voice}"))
                .header("xi-api-key", key)
                .query(&[("output_format", "pcm_24000")])
                .json(&body)
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

/// Converts the integer UI rate directly to `f64` so JSON serialization does
/// not expose an `f32` boundary as slightly outside ElevenLabs' accepted range.
fn speed_for_request(request: &SynthRequest) -> f64 {
    (1.0 + request.rate_percent() as f64 / 100.0).clamp(0.7, 1.2)
}

fn request_body(legacy_model: &str, request: &SynthRequest) -> serde_json::Value {
    let selected = match &request.provider_settings {
        Some(TtsEngineSettings::ElevenLabs(settings)) => Some(settings),
        _ => None,
    };
    let mut voice_settings = serde_json::Map::new();
    voice_settings.insert(
        "speed".into(),
        serde_json::json!(speed_for_request(request)),
    );
    if let Some(settings) = selected {
        if let Some(value) = settings.stability {
            voice_settings.insert("stability".into(), serde_json::json!(value.clamp(0.0, 1.0)));
        }
        let is_v3 = settings.model.trim() == "eleven_v3";
        if !is_v3 {
            if let Some(value) = settings.similarity_boost {
                voice_settings.insert(
                    "similarity_boost".into(),
                    serde_json::json!(value.clamp(0.0, 1.0)),
                );
            }
            if let Some(value) = settings.speaker_boost {
                voice_settings.insert("use_speaker_boost".into(), serde_json::json!(value));
            }
        }
        if let Some(value) = settings.style {
            voice_settings.insert("style".into(), serde_json::json!(value.clamp(0.0, 1.0)));
        }
    }

    let mut body = serde_json::Map::new();
    body.insert("text".into(), serde_json::json!(request.text));
    body.insert(
        "voice_settings".into(),
        serde_json::Value::Object(voice_settings),
    );
    let model = match (&request.provider_settings, selected) {
        (None, _) => legacy_model.trim(),
        (_, Some(settings)) => settings.model.trim(),
        _ => "",
    };
    if !model.is_empty() {
        body.insert("model_id".into(), serde_json::json!(model));
    }
    if let Some(settings) = selected {
        let language = settings.language_code.trim();
        if !language.is_empty() {
            body.insert("language_code".into(), serde_json::json!(language));
        }
    }
    serde_json::Value::Object(body)
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
                    let mut voice = Voice::new(id, name);
                    for compatibility in [
                        entry.get("high_quality_base_model_ids"),
                        entry.pointer("/fine_tuning/models"),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        match compatibility {
                            serde_json::Value::Array(models) => voice.supported_engines.extend(
                                models
                                    .iter()
                                    .filter_map(|model| model.as_str().map(str::to_string)),
                            ),
                            serde_json::Value::Object(models) => {
                                voice.supported_engines.extend(models.keys().cloned())
                            }
                            _ => {}
                        }
                    }
                    voice.supported_engines.sort();
                    voice.supported_engines.dedup();
                    Some(voice)
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

    fn request_at_rate(rate: i32) -> SynthRequest {
        SynthRequest {
            text: String::new(),
            voice: String::new(),
            rate,
            volume: 100,
            pitch: 0,
            provider_settings: None,
        }
    }

    fn serialized_speed(rate: i32) -> String {
        serde_json::json!({ "speed": speed_for_request(&request_at_rate(rate)) }).to_string()
    }

    #[test]
    fn speed_serializes_within_elevenlabs_limits() {
        assert_eq!(serialized_speed(-10), r#"{"speed":0.7}"#);
        assert_eq!(serialized_speed(-6), r#"{"speed":0.7}"#);
        assert_eq!(serialized_speed(-5), r#"{"speed":0.75}"#);
        assert_eq!(serialized_speed(0), r#"{"speed":1.0}"#);
        assert_eq!(serialized_speed(2), r#"{"speed":1.2}"#);
        assert_eq!(serialized_speed(10), r#"{"speed":1.2}"#);
    }

    #[test]
    fn provider_defaults_are_omitted_and_voice_overrides_are_clamped() {
        let mut request = request_at_rate(0);
        request.provider_settings = Some(TtsEngineSettings::ElevenLabs(
            crate::config::ElevenLabsTtsSettings {
                stability: Some(2.0),
                similarity_boost: Some(-1.0),
                speaker_boost: Some(false),
                language_code: "fr".into(),
                ..Default::default()
            },
        ));
        let body = request_body("legacy-model", &request);
        assert!(body.get("model_id").is_none());
        assert_eq!(body["language_code"], "fr");
        assert_eq!(body["voice_settings"]["stability"], 1.0);
        assert_eq!(body["voice_settings"]["similarity_boost"], 0.0);
        assert_eq!(body["voice_settings"]["use_speaker_boost"], false);
    }

    #[test]
    fn voice_lists_are_parsed_and_sorted_by_name() {
        let body = serde_json::json!({
            "voices": [
                {"voice_id": "b", "name": "Zoe"},
                {"voice_id": "a", "name": "adam"},
                {"voice_id": "c", "high_quality_base_model_ids": ["eleven_v3", "eleven_v3"]},
                {"name": "no id here"}
            ]
        });
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 3, "the entry without an id must be skipped");
        assert_eq!(voices[0].id, "a");
        assert_eq!(voices[1].label, "c", "a missing name falls back to the id");
        assert_eq!(voices[1].supported_engines, vec!["eleven_v3"]);
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
                provider_settings: None,
            })
            .unwrap_err();
        assert!(matches!(error, TtsError::NotConfigured(_)), "{error}");
    }
}

/// Authenticates the key and discovers TTS-capable models and all voices.
pub fn discover(config: &SpeechConfig) -> Result<crate::tts::catalog::EngineCatalog, TtsError> {
    use crate::tts::catalog::{CatalogModel, EngineCatalog};
    let engine = ElevenLabs::new(config);
    let key = require(&engine.api_key, "The ElevenLabs API key")?;
    let body: serde_json::Value = block_on(async {
        let response = client()
            .get(format!("{API_BASE}/models"))
            .header("xi-api-key", key)
            .send()
            .await?;
        body_json(SERVICE, response).await
    })?;
    let mut models: Vec<_> = body
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| {
            model
                .get("can_do_text_to_speech")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|model| {
            let id = model.get("model_id")?.as_str()?;
            let label = model.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            Some(CatalogModel {
                id: id.into(),
                label: label.into(),
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    let voices = block_on(async {
        let mut voices = Vec::new();
        let mut page_token = String::new();
        loop {
            let mut request = client()
                .get(format!("{API_BASE}/voices"))
                .header("xi-api-key", key)
                .query(&[("page_size", "100")]);
            if !page_token.is_empty() {
                request = request.query(&[("next_page_token", page_token.as_str())]);
            }
            let body = body_json(SERVICE, request.send().await?).await?;
            voices.extend(parse_voices(&body));
            let has_more = body
                .get("has_more")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            page_token = body
                .get("next_page_token")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if !has_more || page_token.is_empty() {
                break;
            }
        }
        Ok::<_, TtsError>(voices)
    })?;
    Ok(EngineCatalog::from_voices(models, voices))
}
