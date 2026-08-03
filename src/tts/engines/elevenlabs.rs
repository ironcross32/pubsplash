//! ElevenLabs, over the REST API.
//!
//! Asked for `pcm_24000` rather than the default MP3: the reference Python
//! client decodes MP3 here for no reason, and the raw form is both faster and
//! lossless.
//!
//! The `/stream` endpoint returns those same bytes chunked as they are
//! generated, and [`ElevenLabs::synth_to`] feeds them to the mixer as they
//! arrive — which is the whole latency win, since a chat message otherwise
//! stays silent until the last word has been synthesized. It is a per-source
//! setting (`ElevenLabsTtsSettings::stream`), on by default.

use crate::audio::convert::{ENGINE_SAMPLE_RATE, Pcm16Stream, pcm16_to_engine};
use crate::config::{SpeechConfig, TtsEngineSettings};
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, body_json, check_status, client, require};
use futures_util::StreamExt;

const API_BASE: &str = "https://api.elevenlabs.io/v1";

/// The voice list is the one endpoint taken from v2 rather than [`API_BASE`].
///
/// v1 returns the whole list in a single response with no `has_more` or
/// `next_page_token`, so the paging loop in [`discover`] read a missing field
/// as "no more pages" and always stopped after one — silently truncating an
/// account larger than whatever v1 hands back. v2 is the version that actually
/// implements the `page_size`/`next_page_token` contract that loop is written
/// against. The entries still carry `voice_id` and `name`, so [`parse_voices`]
/// is shared between both.
const VOICES_URL: &str = "https://api.elevenlabs.io/v2/voices";
const SERVICE: &str = "ElevenLabs";
const DEFAULT_VOICE: &str = "21m00Tcm4TlvDq8ikWAM"; // Rachel
const SOURCE_RATE: u32 = 24_000;
const SOURCE_CHANNELS: usize = 1;

/// The model that has no streaming endpoint. Its request also drops similarity
/// boost and speaker boost; the dialog greys all three out together.
const NO_STREAMING_MODEL: &str = "eleven_v3";

/// Whole-request timeout for a streamed synthesis, replacing the shared
/// client's 30 seconds ([`crate::tts::net`]). That budget assumes a request
/// ends when the download does; here it ends when the audio has finished
/// *playing*, because the sink blocks while the mixer drains its ring. Still
/// bounded, so one wedged request cannot hold the queue forever — and
/// `SpeechConfig::max_chars` bounds the utterance well inside it.
const STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How much audio is held back before the first chunk is played, as a jitter
/// cushion. Once playback has started this much stays queued ahead of it, so a
/// chunk that arrives late is covered rather than heard as a gap — chopped
/// speech is worse than speech that starts a fraction of a second later, and
/// this is a small fraction of the delay streaming removes.
const PREROLL_SAMPLES: usize =
    (ENGINE_SAMPLE_RATE as usize * 300 / 1_000) * crate::audio::convert::ENGINE_CHANNELS;

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
        let voice = voice_of(request);
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

    fn synth_to(
        &self,
        request: &SynthRequest,
        sink: &mut dyn FnMut(&[f32]),
    ) -> Result<(), TtsError> {
        if !self.streaming_enabled(request) {
            let samples = self.synth(request)?;
            if !samples.is_empty() {
                sink(&samples);
            }
            return Ok(());
        }

        let key = require(&self.api_key, "The ElevenLabs API key")?;
        let voice = voice_of(request);
        let body = request_body(&self.model, request);

        block_on(async {
            let response = client()
                .post(format!("{API_BASE}/text-to-speech/{voice}/stream"))
                .timeout(STREAM_TIMEOUT)
                .header("xi-api-key", key)
                .query(&[("output_format", "pcm_24000")])
                .json(&body)
                .send()
                .await?;
            let mut chunks = check_status(SERVICE, response).await?.bytes_stream();
            // Chunk boundaries fall wherever the network puts them, so the
            // conversion has to carry state across them; see `Pcm16Stream`.
            let mut pcm = Pcm16Stream::new(SOURCE_RATE, SOURCE_CHANNELS);
            let feed = |samples: &mut Vec<f32>, sink: &mut dyn FnMut(&[f32])| {
                if !samples.is_empty() {
                    request.apply_volume(samples);
                    sink(samples);
                    samples.clear();
                }
            };
            let mut buffered = Vec::new();
            let mut started = false;
            while let Some(chunk) = chunks.next().await {
                buffered.extend_from_slice(&pcm.push(&chunk?));
                if !started && buffered.len() < PREROLL_SAMPLES {
                    continue;
                }
                started = true;
                feed(&mut buffered, sink);
            }
            buffered.extend_from_slice(&pcm.finish());
            feed(&mut buffered, sink);
            Ok::<_, TtsError>(())
        })
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        let key = require(&self.api_key, "The ElevenLabs API key")?;
        block_on(fetch_voices(key))
    }

    fn usage_model(&self, request: &SynthRequest) -> Option<String> {
        // Empty means ElevenLabs picks, and there is no honest name to record.
        let model = model_for(&self.model, request);
        (!model.is_empty()).then(|| model.to_string())
    }

    fn usage_voice(&self, request: &SynthRequest) -> Option<String> {
        Some(voice_of(request).to_string())
    }
}

impl ElevenLabs {
    /// Whether this utterance should go to the streaming endpoint.
    ///
    /// A source saved before streaming existed deserializes with it on (see
    /// `ElevenLabsTtsSettings::default`), and a *legacy* source — no
    /// per-source settings at all, taking the global model — gets it too. The
    /// one hard exclusion is [`NO_STREAMING_MODEL`], which has no `/stream`.
    fn streaming_enabled(&self, request: &SynthRequest) -> bool {
        match &request.provider_settings {
            Some(TtsEngineSettings::ElevenLabs(settings)) => {
                settings.stream && settings.model.trim() != NO_STREAMING_MODEL
            }
            _ => self.model.trim() != NO_STREAMING_MODEL,
        }
    }
}

fn voice_of(request: &SynthRequest) -> &str {
    if request.voice.is_empty() {
        DEFAULT_VOICE
    } else {
        &request.voice
    }
}

/// Converts the integer UI rate directly to `f64` so JSON serialization does
/// not expose an `f32` boundary as slightly outside ElevenLabs' accepted range.
fn speed_for_request(request: &SynthRequest) -> f64 {
    (1.0 + request.rate_percent() as f64 / 100.0).clamp(0.7, 1.2)
}

/// The `model_id` this request will carry, or empty to let ElevenLabs choose.
///
/// A source saved before per-source settings existed (`provider_settings:
/// None`) falls back to the global `SpeechConfig::elevenlabs_model`, which
/// `ElevenLabs::new` has already defaulted; a source carrying some *other*
/// engine's settings names no model at all.
fn model_for<'a>(legacy_model: &'a str, request: &'a SynthRequest) -> &'a str {
    match &request.provider_settings {
        None => legacy_model.trim(),
        Some(TtsEngineSettings::ElevenLabs(settings)) => settings.model.trim(),
        Some(_) => "",
    }
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
    let model = model_for(legacy_model, request);
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

/// Parses a `/voices` payload.
///
/// `Voice::supported_engines` is deliberately left empty, which
/// [`crate::tts::catalog::voices`] reads as "this voice is not restricted to
/// any model" — and on ElevenLabs that is the truth. The model is only a
/// `model_id` field in the synthesis body (see [`request_body`]); every voice
/// in the account works with every TTS model.
///
/// The payload does carry two model lists, and **neither is a compatibility
/// allowlist**: `high_quality_base_model_ids` names the models ElevenLabs holds
/// a high-quality rendition of the voice for, and `fine_tuning.models` names
/// the models a *cloned* voice was fine-tuned against. Both were once read as
/// the set of usable models, which broke the moment a model newer than the
/// fields shipped: `eleven_v3` appears in `high_quality_base_model_ids` for
/// essentially no premade voice, so selecting v3 emptied the picker of
/// everything except the user's own clones — whose `fine_tuning.models` does
/// name it. Don't reintroduce them as a filter.
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
    voices.sort_by_key(|a| a.label.to_lowercase());
    voices
}

/// Every voice on the account, following [`VOICES_URL`]'s paging to the end.
///
/// Shared by [`ElevenLabs::voices`] and [`discover`] so the two cannot disagree
/// about how many voices the account has — they did, when only one of them
/// paged.
async fn fetch_voices(key: &str) -> Result<Vec<Voice>, TtsError> {
    let mut voices = Vec::new();
    let mut page_token = String::new();
    loop {
        let mut request = client()
            .get(VOICES_URL)
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
    Ok(voices)
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
    let voices = block_on(fetch_voices(key))?;
    Ok(EngineCatalog::from_voices(models, voices))
}

/// Reads the account's character allowance.
///
/// ElevenLabs is the only engine Pubsplash talks to that publishes a balance
/// against the credentials the app already holds — the rest keep theirs behind
/// a cloud-billing API with its own scope. Bills one credit per character, so
/// `character_count`/`character_limit` are directly comparable with the
/// per-session character tally in [`crate::tts::usage`].
pub fn subscription(config: &SpeechConfig) -> Result<crate::tts::usage::Balance, TtsError> {
    let engine = ElevenLabs::new(config);
    let key = require(&engine.api_key, "The ElevenLabs API key")?;
    let body: serde_json::Value = block_on(async {
        let response = client()
            .get(format!("{API_BASE}/user/subscription"))
            .header("xi-api-key", key)
            .send()
            .await?;
        body_json(SERVICE, response).await
    })?;
    Ok(parse_subscription(&body))
}

/// A missing field is reported as zero rather than as an error: the response
/// shape has grown fields over time, and a balance that reads "0 of 0" is
/// obviously wrong to a user in a way a failed refresh is not.
fn parse_subscription(body: &serde_json::Value) -> crate::tts::usage::Balance {
    let number = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    crate::tts::usage::Balance {
        used: number("character_count"),
        limit: number("character_limit"),
        tier: body
            .get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        resets_unix: body
            .get("next_character_count_reset_unix")
            .and_then(|v| v.as_i64()),
    }
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
        assert_eq!(voices[2].label, "Zoe");
    }

    /// The payload's model lists are hints about rendition quality and cloning,
    /// not the set of models a voice may be used with — so no voice may come
    /// back carrying a model restriction. Reading them as an allowlist is what
    /// emptied the picker under `eleven_v3`; see [`parse_voices`].
    #[test]
    fn a_compatibility_hint_never_becomes_a_model_restriction() {
        let body = serde_json::json!({
            "voices": [
                // A premade voice: the hint predates v3 and never names it.
                {
                    "voice_id": "premade",
                    "name": "Rachel",
                    "high_quality_base_model_ids": ["eleven_multilingual_v2", "eleven_turbo_v2"],
                },
                // A cloned voice, fine-tuned against v3 in both shapes the
                // field has been seen in.
                {
                    "voice_id": "cloned",
                    "name": "Mine",
                    "fine_tuning": { "models": { "eleven_v3": true } },
                },
            ]
        });
        for voice in parse_voices(&body) {
            assert!(
                voice.supported_engines.is_empty(),
                "{} carries a model restriction",
                voice.id
            );
        }
    }

    #[test]
    fn an_unexpected_shape_yields_no_voices_rather_than_panicking() {
        assert!(parse_voices(&serde_json::json!({})).is_empty());
        assert!(parse_voices(&serde_json::json!({"voices": "nope"})).is_empty());
    }

    fn with_settings(settings: crate::config::ElevenLabsTtsSettings) -> SynthRequest {
        let mut request = request_at_rate(0);
        request.provider_settings = Some(TtsEngineSettings::ElevenLabs(settings));
        request
    }

    #[test]
    fn streaming_is_the_default_and_the_checkbox_turns_it_off() {
        let engine = ElevenLabs::new(&SpeechConfig::default());
        // A legacy source: no per-source settings at all.
        assert!(engine.streaming_enabled(&request_at_rate(0)));
        assert!(engine.streaming_enabled(&with_settings(Default::default())));
        assert!(
            !engine.streaming_enabled(&with_settings(crate::config::ElevenLabsTtsSettings {
                stream: false,
                ..Default::default()
            }))
        );
    }

    /// Eleven v3 has no streaming endpoint, so the flag cannot reach it —
    /// through a per-source model or through the legacy global one.
    #[test]
    fn eleven_v3_never_streams() {
        let engine = ElevenLabs::new(&SpeechConfig::default());
        assert!(
            !engine.streaming_enabled(&with_settings(crate::config::ElevenLabsTtsSettings {
                model: "eleven_v3".into(),
                stream: true,
                ..Default::default()
            }))
        );
        let config = SpeechConfig {
            elevenlabs_model: "eleven_v3".into(),
            ..Default::default()
        };
        assert!(!ElevenLabs::new(&config).streaming_enabled(&request_at_rate(0)));
    }

    #[test]
    fn a_blank_configured_model_falls_back_to_the_default() {
        let config = SpeechConfig {
            elevenlabs_model: "  ".into(),
            ..Default::default()
        };
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

    #[test]
    fn a_subscription_payload_becomes_a_balance() {
        let balance = parse_subscription(&serde_json::json!({
            "tier": "creator",
            "character_count": 1_204,
            "character_limit": 10_000,
            "next_character_count_reset_unix": 1_770_000_000i64,
        }));
        assert_eq!(balance.used, 1_204);
        assert_eq!(balance.limit, 10_000);
        assert_eq!(balance.remaining(), 8_796);
        assert_eq!(balance.tier, "creator");
        assert_eq!(balance.resets_unix, Some(1_770_000_000));
    }

    #[test]
    fn a_subscription_missing_fields_reads_as_zero_rather_than_failing() {
        let balance = parse_subscription(&serde_json::json!({}));
        assert_eq!(balance, crate::tts::usage::Balance::default());
    }

    #[test]
    fn the_billed_model_is_the_one_the_source_selected() {
        // Legacy source: the global model, which `new` has already defaulted.
        let engine = ElevenLabs::new(&SpeechConfig::default());
        assert_eq!(
            engine.usage_model(&request_at_rate(0)).as_deref(),
            Some(MODELS[0])
        );
        // Per-source settings win over the global.
        let request = with_settings(crate::config::ElevenLabsTtsSettings {
            model: "eleven_flash_v2_5".into(),
            ..Default::default()
        });
        assert_eq!(
            engine.usage_model(&request).as_deref(),
            Some("eleven_flash_v2_5")
        );
        // Blank per-source model means ElevenLabs picks; nothing to record.
        let request = with_settings(crate::config::ElevenLabsTtsSettings::default());
        assert_eq!(engine.usage_model(&request), None);
    }

    #[test]
    fn the_billed_voice_resolves_the_default() {
        let engine = ElevenLabs::new(&SpeechConfig::default());
        assert_eq!(
            engine.usage_voice(&request_at_rate(0)).as_deref(),
            Some(DEFAULT_VOICE)
        );
        let mut request = request_at_rate(0);
        request.voice = "abc123".into();
        assert_eq!(engine.usage_voice(&request).as_deref(), Some("abc123"));
    }
}
