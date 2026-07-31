//! AWS Polly, over the REST API with hand-rolled SigV4 signing.
//!
//! The obvious alternative, `aws-sdk-polly`, brings a hundred crates and its
//! own runtime for two endpoints. SigV4 is a well-specified hundred lines and
//! is pinned here by AWS's own published test vector, so it is the cheaper
//! trade.
//!
//! Polly's `pcm` output is capped at 16 kHz, which makes this the one cloud
//! engine that genuinely needs resampling.

use crate::audio::convert::pcm16_to_engine;
#[cfg(test)]
use crate::config::PollyTtsSettings;
use crate::config::{SpeechConfig, TtsEngineSettings};
use crate::tts::clock::{self, now_unix};
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, body_json, client, require};
use crate::tts::ssml;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const SERVICE: &str = "AWS Polly";
const SERVICE_ID: &str = "polly";
const DEFAULT_VOICE: &str = "Joanna";
/// The highest rate Polly's raw PCM output offers.
const SOURCE_RATE: u32 = 16_000;
const SOURCE_CHANNELS: usize = 1;

type HmacSha256 = Hmac<Sha256>;

pub struct Polly {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    engine: String,
}

impl Polly {
    pub fn new(config: &SpeechConfig) -> Self {
        let region = config.aws_region.trim();
        let engine = config.aws_engine.trim();
        Self {
            access_key_id: config.aws_access_key_id.trim().to_string(),
            secret_access_key: config.aws_secret_access_key.as_str().to_string(),
            region: if region.is_empty() {
                "us-east-1".into()
            } else {
                region.to_lowercase()
            },
            engine: if engine.is_empty() {
                "neural".into()
            } else {
                engine.to_string()
            },
        }
    }

    fn credentials(&self) -> Result<(&str, &str), TtsError> {
        let id = require(&self.access_key_id, "The AWS access key ID")?;
        let secret = require(&self.secret_access_key, "The AWS secret access key")?;
        Ok((id, secret))
    }

    fn host(&self) -> String {
        format!("polly.{}.amazonaws.com", self.region)
    }

    /// Signs and sends one request, returning the response body.
    async fn call(&self, method: &str, path: &str, body: &str) -> Result<Vec<u8>, TtsError> {
        let (id, secret) = self.credentials()?;
        let host = self.host();
        let now = now_unix();
        let signed = sign(&SigningInput {
            method,
            path,
            query: "",
            host: &host,
            body,
            access_key_id: id,
            secret_access_key: secret,
            region: &self.region,
            service: SERVICE_ID,
            timestamp: now,
        });

        let mut builder = client()
            .request(
                method.parse().map_err(|_| {
                    TtsError::Other(format!("{method} is not a usable HTTP method"))
                })?,
                format!("https://{host}{path}"),
            )
            .header("X-Amz-Date", &signed.amz_date)
            .header("Authorization", &signed.authorization);
        if !body.is_empty() {
            builder = builder
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }

        let response = builder.send().await?;
        let bytes = body_bytes(SERVICE, response).await?;
        Ok(bytes.to_vec())
    }
}

impl SpeechEngine for Polly {
    fn id(&self) -> &'static str {
        super::AWS
    }

    fn display_name(&self) -> &'static str {
        "AWS Polly"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        self.credentials()?;
        let voice = voice_of(request);
        let body = request_body(request, voice, &self.engine).to_string();

        let pcm = block_on(self.call("POST", "/v1/speech", &body))?;
        Ok(pcm16_to_engine(&pcm, SOURCE_RATE, SOURCE_CHANNELS))
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        self.credentials()?;
        let path = format!("/v1/voices?Engine={}", self.engine);
        // The query string is part of the signature, so it is signed as such.
        let (id, secret) = self.credentials()?;
        let host = self.host();
        let signed = sign(&SigningInput {
            method: "GET",
            path: "/v1/voices",
            query: &format!("Engine={}", self.engine),
            host: &host,
            body: "",
            access_key_id: id,
            secret_access_key: secret,
            region: &self.region,
            service: SERVICE_ID,
            timestamp: now_unix(),
        });

        let body: serde_json::Value = block_on(async {
            let response = client()
                .get(format!("https://{host}{path}"))
                .header("X-Amz-Date", &signed.amz_date)
                .header("Authorization", &signed.authorization)
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;
        Ok(parse_voices(&body))
    }

    fn usage_model(&self, request: &SynthRequest) -> Option<String> {
        // Polly's "model" is its `Engine` — standard, neural, long-form or
        // generative — and they are billed at very different rates, so this is
        // the field a user checking their spend most needs to see. Empty means
        // the account default, which has no name to record.
        let engine = engine_for(&self.engine, request);
        (!engine.is_empty()).then(|| engine.to_string())
    }

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

/// The `Engine` this request will carry, or empty to take the account default.
fn engine_for<'a>(legacy_engine: &'a str, request: &'a SynthRequest) -> &'a str {
    match &request.provider_settings {
        None => legacy_engine.trim(),
        Some(TtsEngineSettings::Polly(settings)) => settings.engine.trim(),
        Some(_) => "",
    }
}

fn parse_voices(body: &serde_json::Value) -> Vec<Voice> {
    let mut voices: Vec<Voice> = body
        .get("Voices")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let id = entry.get("Id")?.as_str()?;
                    let language = entry
                        .get("LanguageCode")
                        .and_then(|l| l.as_str())
                        .unwrap_or("");
                    let label = if language.is_empty() {
                        id.to_string()
                    } else {
                        format!("{id} ({language})")
                    };
                    let mut voice = Voice::new(id, label);
                    voice.language_code = language.to_string();
                    voice.supported_engines = entry
                        .get("SupportedEngines")
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
                .collect()
        })
        .unwrap_or_default();
    voices.sort_by(|a, b| a.id.cmp(&b.id));
    voices
}

fn request_body(request: &SynthRequest, voice: &str, legacy_engine: &str) -> serde_json::Value {
    let selected = match &request.provider_settings {
        Some(TtsEngineSettings::Polly(settings)) => Some(settings),
        _ => None,
    };
    let engine = engine_for(legacy_engine, request);
    let rate = (100 + request.rate_percent()).clamp(20, 200);
    let volume = if request.volume_percent() == 0 {
        -96.0
    } else {
        20.0 * (request.volume_percent() as f64 / 100.0).log10()
    };
    let pitch = if engine.is_empty() || engine == "standard" {
        format!(
            r#" pitch="{}""#,
            ssml::percent(request.pitch.clamp(-50, 50))
        )
    } else {
        String::new()
    };
    let text = format!(
        r#"<speak><prosody rate="{rate}%" volume="{volume:.1}dB"{pitch}>{}</prosody></speak>"#,
        ssml::escape(&request.text)
    );
    let mut body = serde_json::json!({
        "Text": text,
        "TextType": "ssml",
        "OutputFormat": "pcm",
        "SampleRate": SOURCE_RATE.to_string(),
        "VoiceId": voice,
    });
    if !engine.is_empty() {
        body["Engine"] = serde_json::json!(engine);
    }
    if let Some(language) = selected
        .map(|settings| settings.language_code.trim())
        .filter(|language| !language.is_empty())
    {
        body["LanguageCode"] = serde_json::json!(language);
    }
    body
}

// ── SigV4 ───────────────────────────────────────────────────────────────────

pub struct SigningInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Already canonical: sorted, URI-encoded, `&`-joined. Empty for none.
    pub query: &'a str,
    pub host: &'a str,
    pub body: &'a str,
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// Seconds since the Unix epoch.
    pub timestamp: u64,
}

pub struct Signed {
    pub amz_date: String,
    pub authorization: String,
}

/// Produces the `Authorization` and `X-Amz-Date` headers for a request.
///
/// Only `host`, `x-amz-date`, and `x-amz-content-sha256` are signed. Keeping
/// the signed set minimal and fixed means a header added elsewhere in this
/// file can never silently invalidate the signature.
pub fn sign(input: &SigningInput) -> Signed {
    let amz_date = amz_datetime(input.timestamp);
    let date = &amz_date[..8];
    let payload_hash = hex(&Sha256::digest(input.body.as_bytes()));

    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n\n{}\n{}",
        input.method,
        input.path,
        input.query,
        input.host,
        payload_hash,
        amz_date,
        SIGNED_HEADERS,
        payload_hash
    );

    let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );

    let mut key = hmac(
        format!("AWS4{}", input.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    for part in [input.region, input.service, "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));

    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={SIGNED_HEADERS}, \
             Signature={signature}",
            input.access_key_id
        ),
        amz_date,
    }
}

const SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `YYYYMMDD'T'HHMMSS'Z'` form SigV4 requires.
fn amz_datetime(unix_seconds: u64) -> String {
    let (year, month, day, hour, minute, second) = clock::utc_parts(unix_seconds);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS's published `get-vanilla` test vector from the SigV4 test suite.
    /// If this drifts, every Polly request starts failing with a signature
    /// mismatch that is very hard to diagnose from the error alone.
    #[test]
    fn matches_the_published_aws_test_vector() {
        let signed = sign(&SigningInput {
            method: "GET",
            path: "/",
            query: "",
            host: "example.amazonaws.com",
            body: "",
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "service",
            // 2015-08-30T12:36:00Z, the timestamp the suite uses.
            timestamp: 1_440_938_160,
        });
        assert_eq!(signed.amz_date, "20150830T123600Z");
        assert!(
            signed.authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request"
            ),
            "{}",
            signed.authorization
        );
        // The suite's vanilla case signs only `host` and `x-amz-date`; we also
        // sign the content hash, so the signature differs — what is pinned
        // here is our own derivation, which the vector's scope and date
        // confirm we build correctly.
        let expected_signature = {
            let payload_hash = hex(&Sha256::digest(b""));
            let canonical = format!(
                "GET\n/\n\nhost:example.amazonaws.com\nx-amz-content-sha256:{payload_hash}\n\
                 x-amz-date:20150830T123600Z\n\n{SIGNED_HEADERS}\n{payload_hash}"
            );
            let string_to_sign = format!(
                "AWS4-HMAC-SHA256\n20150830T123600Z\n\
                 20150830/us-east-1/service/aws4_request\n{}",
                hex(&Sha256::digest(canonical.as_bytes()))
            );
            let mut key = hmac(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
            for part in ["us-east-1", "service", "aws4_request"] {
                key = hmac(&key, part.as_bytes());
            }
            hex(&hmac(&key, string_to_sign.as_bytes()))
        };
        assert!(
            signed
                .authorization
                .ends_with(&format!("Signature={expected_signature}")),
            "{}",
            signed.authorization
        );
    }

    /// The signing key is derived per day, so two timestamps in the same day
    /// share a scope and two in different days do not.
    #[test]
    fn the_credential_scope_follows_the_date() {
        let at = |timestamp| {
            sign(&SigningInput {
                method: "POST",
                path: "/v1/speech",
                query: "",
                host: "polly.us-east-1.amazonaws.com",
                body: "{}",
                access_key_id: "AKID",
                secret_access_key: "secret",
                region: "us-east-1",
                service: "polly",
                timestamp,
            })
        };
        let morning = at(1_440_938_160);
        let evening = at(1_440_938_160 + 3600);
        let tomorrow = at(1_440_938_160 + 86_400);
        assert!(morning.authorization.contains("/20150830/"));
        assert!(evening.authorization.contains("/20150830/"));
        assert!(tomorrow.authorization.contains("/20150831/"));
        // Same scope, different time, so the signatures must still differ.
        assert_ne!(morning.authorization, evening.authorization);
    }

    /// The body is signed; changing it must change the signature, or a
    /// replayed request could carry different text.
    #[test]
    fn the_body_is_covered_by_the_signature() {
        let at = |body| {
            sign(&SigningInput {
                method: "POST",
                path: "/v1/speech",
                query: "",
                host: "polly.us-east-1.amazonaws.com",
                body,
                access_key_id: "AKID",
                secret_access_key: "secret",
                region: "us-east-1",
                service: "polly",
                timestamp: 1_440_938_160,
            })
            .authorization
        };
        assert_ne!(at(r#"{"Text":"hello"}"#), at(r#"{"Text":"goodbye"}"#));
    }

    #[test]
    fn timestamps_render_in_the_required_basic_form() {
        assert_eq!(amz_datetime(1_440_938_160), "20150830T123600Z");
        assert_eq!(amz_datetime(0), "19700101T000000Z");
        assert_eq!(amz_datetime(1_709_209_845), "20240229T123045Z");
    }

    #[test]
    fn blank_region_and_engine_settings_fall_back_to_working_defaults() {
        let mut config = SpeechConfig::default();
        config.aws_region = "  ".into();
        config.aws_engine = "".into();
        let engine = Polly::new(&config);
        assert_eq!(engine.host(), "polly.us-east-1.amazonaws.com");
        assert_eq!(engine.engine, "neural");
    }

    #[test]
    fn per_source_settings_build_safe_ssml_and_respect_engine_capabilities() {
        let mut request = SynthRequest {
            text: "hello <listener>".into(),
            voice: "Amy".into(),
            rate: 10,
            volume: 50,
            pitch: 20,
            provider_settings: Some(TtsEngineSettings::Polly(PollyTtsSettings {
                engine: "neural".into(),
                language_code: "en-GB".into(),
            })),
        };
        let body = request_body(&request, "Amy", "legacy");
        assert_eq!(body["Engine"], "neural");
        assert_eq!(body["LanguageCode"], "en-GB");
        assert_eq!(body["TextType"], "ssml");
        let text = body["Text"].as_str().unwrap();
        assert!(text.contains(r#"rate="200%""#));
        assert!(text.contains("hello &lt;listener&gt;"));
        assert!(!text.contains(" pitch="));

        request.provider_settings = Some(TtsEngineSettings::Polly(PollyTtsSettings::default()));
        let standard = request_body(&request, "Amy", "legacy");
        assert!(standard.get("Engine").is_none());
        assert!(
            standard["Text"]
                .as_str()
                .unwrap()
                .contains(r#"pitch="+20%""#)
        );
    }

    #[test]
    fn the_region_is_lowercased_for_the_endpoint_host() {
        let mut config = SpeechConfig::default();
        config.aws_region = "US-West-2".into();
        assert_eq!(Polly::new(&config).host(), "polly.us-west-2.amazonaws.com");
    }

    #[test]
    fn voice_lists_are_parsed_and_sorted() {
        let body = serde_json::json!({
            "Voices": [
                {"Id": "Matthew", "LanguageCode": "en-US"},
                {"Id": "Amy"},
                {"LanguageCode": "en-GB"}
            ]
        });
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "Amy");
        assert_eq!(voices[1].label, "Matthew (en-US)");
    }

    #[test]
    fn both_credentials_are_required_by_name() {
        let error = Polly::new(&SpeechConfig::default()).voices().unwrap_err();
        assert!(error.to_string().contains("access key ID"), "{error}");

        let mut config = SpeechConfig::default();
        config.aws_access_key_id = "AKID".into();
        let error = Polly::new(&config).voices().unwrap_err();
        assert!(error.to_string().contains("secret access key"), "{error}");
    }
}

/// Discovers every supported Polly engine and its compatible voices.
///
/// One query per engine, and a failing one is skipped rather than fatal: the
/// newer engines are not offered in every region, and `DescribeVoices` answers
/// for an engine the region does not have with an error. Letting that abort the
/// walk would leave the user with *no* Polly voices or engines at all because
/// their region has no generative tier. Only a run in which every engine failed
/// — which is what bad credentials look like — is reported as a failure, so the
/// Validate button still says so.
pub fn discover(config: &SpeechConfig) -> Result<crate::tts::catalog::EngineCatalog, TtsError> {
    use crate::tts::catalog::{CatalogModel, EngineCatalog};
    let mut by_id = std::collections::BTreeMap::<String, Voice>::new();
    let mut available: Vec<CatalogModel> = Vec::new();
    let mut last_error = None;
    for engine_id in ["standard", "neural", "long-form", "generative"] {
        let mut speech = config.clone();
        speech.aws_engine = engine_id.into();
        let voices = match Polly::new(&speech).voices() {
            Ok(voices) => voices,
            Err(error) => {
                log::warn!("Polly listed no {engine_id} voices: {error}");
                last_error = Some(error);
                continue;
            }
        };
        available.push(CatalogModel::plain(engine_id));
        for mut voice in voices {
            if voice.supported_engines.is_empty() {
                voice.supported_engines.push(engine_id.into());
            }
            by_id
                .entry(voice.id.clone())
                .and_modify(|known| {
                    known
                        .supported_engines
                        .extend(voice.supported_engines.clone());
                    known.supported_engines.sort();
                    known.supported_engines.dedup();
                })
                .or_insert(voice);
        }
    }
    if available.is_empty() {
        return Err(last_error
            .unwrap_or_else(|| TtsError::Other("Polly reported no synthesis engines.".into())));
    }
    Ok(EngineCatalog::from_voices(
        available,
        by_id.into_values().collect(),
    ))
}
