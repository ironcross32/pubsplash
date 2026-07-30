//! Azure Cognitive Services speech, over the REST API.
//!
//! The reference Python client uses the Microsoft Speech SDK; there is no Rust
//! binding, and none is needed — the REST endpoint takes the subscription key
//! directly and will render straight to the mixer's own format, so this is the
//! one engine that needs neither decoding nor resampling.

use crate::audio::convert::pcm16_to_engine;
use crate::audio::mixer::SAMPLE_RATE;
use crate::config::{SpeechConfig, TtsEngineSettings};
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, body_json, client, require};
use crate::tts::ssml;

const SERVICE: &str = "Azure";
const DEFAULT_VOICE: &str = "en-US-JennyNeural";
/// Rendered at the mixer rate, so `pcm16_to_engine` only has to widen to stereo.
const OUTPUT_FORMAT: &str = "raw-48khz-16bit-mono-pcm";
const SOURCE_CHANNELS: usize = 1;

pub struct Azure {
    key: String,
    region: String,
}

impl Azure {
    pub fn new(config: &SpeechConfig) -> Self {
        Self {
            key: config.azure_key.as_str().to_string(),
            region: config.azure_region.trim().to_lowercase(),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("https://{}.tts.speech.microsoft.com/{path}", self.region)
    }

    fn credentials(&self) -> Result<(&str, &str), TtsError> {
        let key = require(&self.key, "The Azure subscription key")?;
        let region = require(&self.region, "The Azure region")?;
        Ok((key, region))
    }
}

impl SpeechEngine for Azure {
    fn id(&self) -> &'static str {
        super::AZURE
    }

    fn display_name(&self) -> &'static str {
        "Azure"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let (key, _) = self.credentials()?;
        let voice = if request.voice.is_empty() {
            DEFAULT_VOICE
        } else {
            &request.voice
        };
        let body = request_ssml(request, voice);

        let bytes = block_on(async {
            let response = client()
                .post(self.endpoint("cognitiveservices/v1"))
                .header("Ocp-Apim-Subscription-Key", key)
                .header("Content-Type", "application/ssml+xml")
                .header("X-Microsoft-OutputFormat", OUTPUT_FORMAT)
                .header("User-Agent", "Pubsplash")
                .body(body)
                .send()
                .await?;
            body_bytes(SERVICE, response).await
        })?;

        // Volume is already in the SSML, so it is not applied again here.
        Ok(pcm16_to_engine(&bytes, SAMPLE_RATE, SOURCE_CHANNELS))
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        let (key, _) = self.credentials()?;
        let body: serde_json::Value = block_on(async {
            let response = client()
                .get(self.endpoint("cognitiveservices/voices/list"))
                .header("Ocp-Apim-Subscription-Key", key)
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;
        Ok(parse_voices(&body))
    }
}

fn request_ssml(request: &SynthRequest, voice: &str) -> String {
    let prosody = format!(
        r#"<prosody rate="{}" pitch="{}" volume="{}">{}</prosody>"#,
        ssml::percent(request.rate_percent()),
        ssml::percent(request.pitch.clamp(-50, 50)),
        request.volume_percent(),
        ssml::escape(&request.text)
    );
    let settings = match &request.provider_settings {
        Some(TtsEngineSettings::Azure(settings)) => Some(settings),
        _ => None,
    };
    let content = if let Some(settings) = settings {
        let style = settings.style.trim();
        let role = settings.role.trim();
        if !style.is_empty() || !role.is_empty() {
            let mut attributes = String::new();
            if !style.is_empty() {
                attributes.push_str(&format!(
                    r#" style="{}" styledegree="{:.2}""#,
                    ssml::escape(style),
                    settings.style_degree.clamp(0.01, 2.0)
                ));
            }
            if !role.is_empty() {
                attributes.push_str(&format!(r#" role="{}""#, ssml::escape(role)));
            }
            format!(r#"<mstts:express-as{attributes}>{prosody}</mstts:express-as>"#)
        } else {
            prosody
        }
    } else {
        prosody
    };
    format!(
        concat!(
            r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" "#,
            r#"xmlns:mstts="https://www.w3.org/2001/mstts" xml:lang="{}">"#,
            r#"<voice name="{}">{}</voice></speak>"#
        ),
        ssml::escape(language_of(voice)),
        ssml::escape(voice),
        content
    )
}

/// The locale embedded in an Azure voice name (`en-US-JennyNeural` → `en-US`).
///
/// The document's `xml:lang` has to be a well-formed tag or the service
/// rejects it, and taking it from the voice keeps the two consistent.
fn language_of(voice: &str) -> &str {
    let mut boundaries = voice.match_indices('-');
    match (boundaries.next(), boundaries.next()) {
        (Some(_), Some((second, _))) => &voice[..second],
        _ => "en-US",
    }
}

fn parse_voices(body: &serde_json::Value) -> Vec<Voice> {
    let Some(list) = body.as_array() else {
        return Vec::new();
    };
    let mut voices: Vec<Voice> = list
        .iter()
        .filter_map(|entry| {
            let id = entry.get("ShortName")?.as_str()?;
            let gender = entry.get("Gender").and_then(|g| g.as_str()).unwrap_or("");
            let label = if gender.is_empty() {
                id.to_string()
            } else {
                format!("{id} ({gender})")
            };
            let mut voice = Voice::new(id, label);
            voice.styles = entry
                .get("StyleList")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            voice.roles = entry
                .get("RolePlayList")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(voice)
        })
        .collect();
    voices.sort_by(|a, b| a.id.cmp(&b.id));
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_locale_is_taken_from_the_voice_name() {
        assert_eq!(language_of("en-US-JennyNeural"), "en-US");
        assert_eq!(language_of("zh-CN-XiaoxiaoNeural"), "zh-CN");
        // Anything unexpected must still produce a legal tag.
        assert_eq!(language_of("Jenny"), "en-US");
        assert_eq!(language_of("en-US"), "en-US");
        assert_eq!(language_of(""), "en-US");
    }

    #[test]
    fn voice_lists_are_parsed_and_sorted() {
        let body = serde_json::json!([
            {"ShortName": "en-US-ZoeNeural", "Gender": "Female", "StyleList": ["chat"], "RolePlayList": ["YoungAdultFemale"]},
            {"ShortName": "en-GB-RyanNeural"},
            {"Gender": "Male"}
        ]);
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 2, "the entry without a ShortName is skipped");
        assert_eq!(voices[0].id, "en-GB-RyanNeural");
        assert_eq!(voices[0].label, "en-GB-RyanNeural");
        assert_eq!(voices[1].label, "en-US-ZoeNeural (Female)");
        assert_eq!(voices[1].styles, ["chat"]);
        assert_eq!(voices[1].roles, ["YoungAdultFemale"]);
    }

    #[test]
    fn expressive_ssml_is_escaped_and_clamped() {
        let request = SynthRequest {
            text: "hello <chat>".into(),
            voice: "en-US-JennyNeural".into(),
            rate: 0,
            volume: 100,
            pitch: 0,
            provider_settings: Some(TtsEngineSettings::Azure(crate::config::AzureTtsSettings {
                style: "cheerful".into(),
                style_degree: 9.0,
                role: "YoungAdultFemale".into(),
            })),
        };
        let body = request_ssml(&request, &request.voice);
        assert!(body.contains(r#"style="cheerful" styledegree="2.00""#));
        assert!(body.contains(r#"role="YoungAdultFemale""#));
        assert!(body.contains("hello &lt;chat&gt;"));
    }

    #[test]
    fn both_the_key_and_the_region_are_required_by_name() {
        let mut config = SpeechConfig::default();
        config.azure_key = crate::secret::Secret::new("k");
        let error = Azure::new(&config).voices().unwrap_err();
        assert!(error.to_string().contains("region"), "{error}");

        let config = SpeechConfig::default();
        let error = Azure::new(&config).voices().unwrap_err();
        assert!(error.to_string().contains("subscription key"), "{error}");
    }

    /// The region goes into a URL; a stray space or capital would break it.
    #[test]
    fn the_region_is_normalised_for_the_endpoint_host() {
        let mut config = SpeechConfig::default();
        config.azure_region = "  EastUS  ".into();
        let engine = Azure::new(&config);
        assert_eq!(
            engine.endpoint("cognitiveservices/v1"),
            "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1"
        );
    }
}
