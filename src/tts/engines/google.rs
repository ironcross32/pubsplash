//! Google Cloud Text-to-Speech, over the REST API with an API key.
//!
//! The Python client also supports Application Default Credentials, which
//! means a service-account JSON file and a full OAuth2 JWT exchange. An API
//! key does the same job here with a field the user can paste, so that is the
//! only path offered.
//!
//! `LINEAR16` at 48 kHz comes back inside a RIFF wrapper, so it goes through
//! the WAV reader rather than the raw PCM path.

use crate::audio::convert::decode_wav;
use crate::audio::mixer::SAMPLE_RATE;
use crate::config::{SpeechConfig, TtsEngineSettings};
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_json, client, require};

const API_BASE: &str = "https://texttospeech.googleapis.com/v1";
const SERVICE: &str = "Google Cloud";
const DEFAULT_VOICE: &str = "en-US-Wavenet-C";

pub const EFFECTS_PROFILES: &[(&str, &str)] = &[
    ("wearable-class-device", "Wearable device"),
    ("handset-class-device", "Handset"),
    ("headphone-class-device", "Headphones"),
    (
        "small-bluetooth-speaker-class-device",
        "Small Bluetooth speaker",
    ),
    (
        "medium-bluetooth-speaker-class-device",
        "Medium Bluetooth speaker",
    ),
    (
        "large-home-entertainment-class-device",
        "Home entertainment system",
    ),
    ("large-automotive-class-device", "Automotive system"),
    ("telephony-class-application", "Telephone audio"),
];

pub struct GoogleCloud {
    api_key: String,
    language_code: String,
}

impl GoogleCloud {
    pub fn new(config: &SpeechConfig) -> Self {
        let language = config.google_language_code.trim();
        Self {
            api_key: config.google_api_key.as_str().to_string(),
            language_code: if language.is_empty() {
                "en-US".into()
            } else {
                language.to_string()
            },
        }
    }
}

impl SpeechEngine for GoogleCloud {
    fn id(&self) -> &'static str {
        super::GOOGLE
    }

    fn display_name(&self) -> &'static str {
        "Google Cloud"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let key = require(&self.api_key, "The Google Cloud API key")?;
        let voice = voice_of(request);

        let body = request_body(request, voice, &self.language_code);

        let response: serde_json::Value = block_on(async {
            let response = client()
                .post(format!("{API_BASE}/text:synthesize"))
                .query(&[("key", key)])
                .json(&body)
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;

        // The audio comes back base64-encoded inside JSON.
        let encoded = response
            .get("audioContent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                TtsError::Decode(SERVICE, "the response carried no audioContent".into())
            })?;
        let wav = crate::b64::decode(encoded)
            .ok_or_else(|| TtsError::Decode(SERVICE, "audioContent was not valid base64".into()))?;
        decode_wav(&wav).map_err(|e| TtsError::Decode(SERVICE, e))
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        let key = require(&self.api_key, "The Google Cloud API key")?;
        let body: serde_json::Value = block_on(async {
            let response = client()
                .get(format!("{API_BASE}/voices"))
                .query(&[("key", key)])
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;
        Ok(parse_voices(&body))
    }

    // No `usage_model`: the request has no model field. Google prices by voice
    // family — Standard, WaveNet, Neural2 — which the voice name encodes.

    fn usage_voice(&self, request: &SynthRequest) -> Option<String> {
        Some(voice_of(request).to_string())
    }
}

fn voice_of(request: &SynthRequest) -> &str {
    if request.voice.is_empty() {
        DEFAULT_VOICE
    } else {
        &request.voice
    }
}

fn request_body(request: &SynthRequest, voice: &str, legacy_language: &str) -> serde_json::Value {
    let selected = match &request.provider_settings {
        Some(TtsEngineSettings::Google(settings)) => Some(settings),
        _ => None,
    };
    let configured_language = match (&request.provider_settings, selected) {
        (None, _) => legacy_language,
        (_, Some(settings)) if !settings.language_code.trim().is_empty() => {
            settings.language_code.trim()
        }
        _ => "en-US",
    };
    let mut body = serde_json::json!({
        "input": { "text": request.text },
        "voice": { "languageCode": language_of(voice, configured_language), "name": voice },
        "audioConfig": {
            "audioEncoding": "LINEAR16",
            "sampleRateHertz": SAMPLE_RATE,
            // Documented ranges; outside them the request is rejected.
            "speakingRate": request.rate_multiplier().clamp(0.25, 4.0),
            "pitch": (request.pitch as f64 / 2.5).clamp(-20.0, 20.0),
            "volumeGainDb": volume_gain_db(request.volume_percent()),
        }
    });
    if let Some(profile) = selected
        .map(|settings| settings.effects_profile.trim())
        .filter(|profile| !profile.is_empty())
    {
        body["audioConfig"]["effectsProfileId"] = serde_json::json!([profile]);
    }
    body
}

/// `volumeGainDb` is a gain, not a percentage: 0 is unity and the field is
/// clamped to -96..=16. A volume of 0 has to mean silence, which -96 dB is.
fn volume_gain_db(volume: u32) -> f64 {
    if volume == 0 {
        return -96.0;
    }
    (20.0 * (volume as f64 / 100.0).log10()).clamp(-96.0, 16.0)
}

/// The locale for a Google voice name (`en-US-Wavenet-C` → `en-US`), falling
/// back to the configured language when the name isn't in that shape.
fn language_of<'a>(voice: &'a str, configured: &'a str) -> &'a str {
    let mut boundaries = voice.match_indices('-');
    match (boundaries.next(), boundaries.next()) {
        (Some(_), Some((second, _))) => &voice[..second],
        _ => configured,
    }
}

fn parse_voices(body: &serde_json::Value) -> Vec<Voice> {
    let mut voices: Vec<Voice> = body
        .get("voices")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let id = entry.get("name")?.as_str()?;
                    let gender = entry
                        .get("ssmlGender")
                        .and_then(|g| g.as_str())
                        .unwrap_or("");
                    let label = if gender.is_empty() {
                        id.to_string()
                    } else {
                        format!("{id} ({})", gender.to_lowercase())
                    };
                    Some(Voice::new(id, label))
                })
                .collect()
        })
        .unwrap_or_default();
    voices.sort_by(|a, b| a.id.cmp(&b.id));
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_maps_onto_a_decibel_gain_with_silence_at_zero() {
        assert_eq!(volume_gain_db(0), -96.0);
        assert!(
            (volume_gain_db(100) - 0.0).abs() < 1e-9,
            "100 must be unity"
        );
        let half = volume_gain_db(50);
        assert!((half - -6.02).abs() < 0.01, "got {half}");
    }

    #[test]
    fn the_locale_comes_from_the_voice_and_falls_back_to_the_setting() {
        assert_eq!(language_of("en-US-Wavenet-C", "de-DE"), "en-US");
        assert_eq!(language_of("cmn-TW-Wavenet-A", "de-DE"), "cmn-TW");
        assert_eq!(language_of("odd", "de-DE"), "de-DE");
    }

    #[test]
    fn voice_lists_are_parsed_and_sorted() {
        let body = serde_json::json!({
            "voices": [
                {"name": "en-US-Wavenet-C", "ssmlGender": "FEMALE"},
                {"name": "en-GB-Standard-A"},
                {"ssmlGender": "MALE"}
            ]
        });
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "en-GB-Standard-A");
        assert_eq!(voices[1].label, "en-US-Wavenet-C (female)");
    }

    #[test]
    fn a_blank_configured_language_falls_back_rather_than_sending_an_empty_tag() {
        let config = SpeechConfig {
            google_language_code: "   ".into(),
            ..Default::default()
        };
        assert_eq!(GoogleCloud::new(&config).language_code, "en-US");
    }

    #[test]
    fn effects_profile_and_language_override_are_sent_per_source() {
        let request = SynthRequest {
            text: "hello".into(),
            voice: "odd".into(),
            rate: 0,
            volume: 100,
            pitch: 0,
            provider_settings: Some(TtsEngineSettings::Google(
                crate::config::GoogleTtsSettings {
                    language_code: "de-DE".into(),
                    effects_profile: "headphone-class-device".into(),
                },
            )),
        };
        let body = request_body(&request, "odd", "en-US");
        assert_eq!(body["voice"]["languageCode"], "de-DE");
        assert_eq!(
            body["audioConfig"]["effectsProfileId"][0],
            "headphone-class-device"
        );
    }

    #[test]
    fn a_missing_key_is_reported_before_any_request() {
        let engine = GoogleCloud::new(&SpeechConfig::default());
        assert!(matches!(
            engine.voices().unwrap_err(),
            TtsError::NotConfigured(_)
        ));
    }
}
