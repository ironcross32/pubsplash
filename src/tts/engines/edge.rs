//! Microsoft Edge's read-aloud voices.
//!
//! Free and needs no account, which makes it the best default for a user who
//! wants better-than-SAPI voices without signing up for anything. It is also
//! the most involved engine here, because it is not a REST API: it is the
//! WebSocket protocol Edge's own read-aloud feature uses.
//!
//! Three things the Python `edge-tts` package hides and we have to do
//! ourselves:
//!
//! 1. **`Sec-MS-GEC`.** Since 2024 the endpoint rejects connections without a
//!    token derived from the current time and a fixed client token — see
//!    [`gec_token`]. It changes every five minutes.
//! 2. **Framing.** Messages carry an HTTP-like header block; binary frames
//!    prefix it with a big-endian `u16` length. The audio is whatever follows.
//! 3. **Turn tracking.** The audio arrives across many frames and is only
//!    complete at `Path:turn.end`.
//!
//! Output is MP3 rather than PCM. The service nominally accepts a RIFF format,
//! but the MP3 one is what Edge itself requests and therefore what is actually
//! known to work; the decoder is already here for Google Translate.

use crate::config::SpeechConfig;
use crate::tts::clock::{self, now_unix};
use crate::tts::decode::decode_compressed;
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_json, client};
use crate::tts::ssml;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const SERVICE: &str = "Microsoft Edge";
const DEFAULT_VOICE: &str = "en-US-EmmaMultilingualNeural";

/// Published in Edge and constant across clients; not a credential.
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const WSS_BASE: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
const VOICES_URL: &str =
    "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list";
/// The Edge release this client claims to be.
///
/// **This goes stale and the service starts answering 403 when it does** — it
/// rejects versions it considers too old, and that is the single most likely
/// way this engine breaks. When it does, take the current value from
/// `edge-tts`'s `constants.py` (`CHROMIUM_FULL_VERSION`) and update it here;
/// `edge_speaks` is the ignored test that catches it.
const CHROMIUM_FULL: &str = "143.0.3650.75";

/// The major version, for the user agent. Derived rather than written twice:
/// the two drifting apart is exactly the kind of thing that earns a 403.
fn chromium_major() -> &'static str {
    CHROMIUM_FULL.split('.').next().unwrap_or(CHROMIUM_FULL)
}

fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36 Edg/{major}.0.0.0",
        major = chromium_major()
    )
}

/// The one output format Edge's own client asks for.
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

/// Seconds between the Windows and Unix epochs; the token is in Windows time.
const WINDOWS_EPOCH_OFFSET: u64 = 11_644_473_600;
/// The token is rounded down to a five-minute boundary, so it is stable for
/// that long and a small clock difference doesn't break the connection.
const TOKEN_PERIOD: u64 = 300;

pub struct Edge;

impl Edge {
    pub fn new(_config: &SpeechConfig) -> Self {
        Self
    }
}

impl SpeechEngine for Edge {
    fn id(&self) -> &'static str {
        super::EDGE
    }

    fn display_name(&self) -> &'static str {
        "Microsoft Edge"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let voice = voice_of(request);
        let document = ssml::document(
            language_of(voice),
            voice,
            &request.text,
            &format!(
                r#"pitch="{}" rate="{}" volume="{}""#,
                ssml::hertz(request.pitch.clamp(-50, 50)),
                ssml::percent(request.rate_percent()),
                ssml::percent(volume_offset(request.volume_percent()))
            ),
        );

        let mp3 = block_on(collect_audio(document))?;
        if mp3.is_empty() {
            return Err(TtsError::Decode(
                SERVICE,
                "the service sent no audio; the voice name may be wrong".into(),
            ));
        }
        // Volume is in the SSML, so it is not applied to the samples again.
        decode_compressed(SERVICE, mp3, "mp3")
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        let body: serde_json::Value = block_on(async {
            let response = client()
                .get(VOICES_URL)
                .query(&[
                    ("trustedclienttoken", TRUSTED_CLIENT_TOKEN),
                    ("Sec-MS-GEC", &gec_token(now_unix())),
                    ("Sec-MS-GEC-Version", &gec_version()),
                ])
                .header("User-Agent", user_agent())
                .send()
                .await?;
            body_json(SERVICE, response).await
        })?;
        Ok(parse_voices(&body))
    }

    // No `usage_model`: the protocol is SSML over a WebSocket, and the service
    // is free — there is no model and nothing to bill.

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

/// Runs one synthesis turn and returns the concatenated MP3 bytes.
async fn collect_audio(document: String) -> Result<Vec<u8>, TtsError> {
    let connection_id = uuid::Uuid::new_v4().simple().to_string();
    let url = format!(
        "{WSS_BASE}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}\
         &Sec-MS-GEC={}&Sec-MS-GEC-Version={}&ConnectionId={connection_id}",
        gec_token(now_unix()),
        gec_version()
    );

    let mut request = url
        .into_client_request()
        .map_err(|e| TtsError::Other(e.to_string()))?;
    {
        // The endpoint refuses connections that don't look like Edge's own
        // read-aloud client, and it checks more than the user agent — this is
        // the exact set `edge-tts` sends, and dropping any of it earns a 403.
        let headers = request.headers_mut();
        headers.insert("Pragma", "no-cache".parse().unwrap());
        headers.insert("Cache-Control", "no-cache".parse().unwrap());
        headers.insert(
            "Origin",
            "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"
                .parse()
                .unwrap(),
        );
        headers.insert(
            "Accept-Encoding",
            "gzip, deflate, br, zstd".parse().unwrap(),
        );
        headers.insert("Accept-Language", "en-US,en;q=0.9".parse().unwrap());
        headers.insert("User-Agent", user_agent().parse().unwrap());
    }

    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(connect_error)?;

    let timestamp = js_timestamp(now_unix());
    let config = format!(
        "X-Timestamp:{timestamp}\r\nContent-Type:application/json; charset=utf-8\r\n\
         Path:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":\
         {{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\
         \"outputFormat\":\"{OUTPUT_FORMAT}\"}}}}}}}}"
    );
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let ssml_message = format!(
        "X-RequestId:{request_id}\r\nContent-Type:application/ssml+xml\r\n\
         X-Timestamp:{timestamp}Z\r\nPath:ssml\r\n\r\n{document}"
    );

    for message in [config, ssml_message] {
        socket
            .send(Message::Text(message.into()))
            .await
            .map_err(|e| TtsError::Network(e.to_string()))?;
    }

    let mut audio = Vec::new();
    // A turn that never ends would otherwise hold the worker forever; the
    // client-wide HTTP timeout doesn't cover an open WebSocket.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let next = tokio::time::timeout_at(deadline, socket.next())
            .await
            .map_err(|_| TtsError::Network("the service stopped responding".into()))?;
        let Some(message) = next else {
            break;
        };
        match message.map_err(|e| TtsError::Network(e.to_string()))? {
            Message::Binary(bytes) => audio.extend_from_slice(&audio_of(&bytes)),
            Message::Text(text) => {
                if text.contains("Path:turn.end") {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = socket.close(None).await;
    Ok(audio)
}

/// A 403 here almost always means the token was rejected, which is worth
/// saying plainly — it is not something the user can fix by retyping a key.
fn connect_error(error: tokio_tungstenite::tungstenite::Error) -> TtsError {
    let text = error.to_string();
    if text.contains("403") {
        TtsError::Service {
            service: SERVICE,
            status: 403,
            detail: "the service refused the connection. This almost always means \
                     Pubsplash's Edge client version has gone stale and needs updating."
                .into(),
        }
    } else {
        TtsError::Network(text)
    }
}

/// Strips the header block from a binary frame, leaving the audio.
///
/// The frame is a big-endian `u16` header length, that many bytes of headers,
/// then the payload. A frame too short to hold its own header contributes
/// nothing rather than panicking.
fn audio_of(frame: &[u8]) -> &[u8] {
    let Some(length) = frame.get(..2) else {
        return &[];
    };
    let header_end = 2 + u16::from_be_bytes([length[0], length[1]]) as usize;
    frame.get(header_end..).unwrap_or(&[])
}

/// The `Sec-MS-GEC` token: SHA-256 of the current time in Windows 100 ns
/// ticks, rounded down to five minutes, concatenated with the client token,
/// as uppercase hex.
fn gec_token(unix_seconds: u64) -> String {
    let windows_seconds = unix_seconds + WINDOWS_EPOCH_OFFSET;
    let rounded = windows_seconds - windows_seconds % TOKEN_PERIOD;
    let ticks = rounded as u128 * 10_000_000;
    let mut hasher = Sha256::new();
    hasher.update(format!("{ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
    format!("{:X}", hasher.finalize())
}

fn gec_version() -> String {
    format!("1-{CHROMIUM_FULL}")
}

/// The JavaScript `Date.toString()` form the protocol's `X-Timestamp` uses.
fn js_timestamp(unix_seconds: u64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (year, month, day, hour, minute, second) = clock::utc_parts(unix_seconds);
    format!(
        "{} {} {day:02} {year} {hour:02}:{minute:02}:{second:02} \
         GMT+0000 (Coordinated Universal Time)",
        DAYS[clock::weekday(unix_seconds)],
        MONTHS[month as usize - 1],
    )
}

/// Edge takes volume as an SSML offset from normal, not an absolute level.
fn volume_offset(volume: u32) -> i32 {
    volume.clamp(0, 100) as i32 - 100
}

/// The locale in an Edge voice name (`en-US-AriaNeural` → `en-US`).
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
            Some(Voice::new(id, label))
        })
        .collect();
    voices.sort_by(|a, b| a.id.cmp(&b.id));
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the token derivation. If this changes, every Edge connection
    /// breaks, so it is worth catching here rather than in the field.
    #[test]
    fn the_gec_token_matches_the_reference_derivation() {
        // 2024-01-01T00:00:00Z. Windows seconds 13_346_766_336_000_000 ticks.
        let token = gec_token(1_704_067_200);
        assert_eq!(token.len(), 64, "a SHA-256 in hex");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase()),
            "must be uppercase hex: {token}"
        );

        let expected = {
            let mut hasher = Sha256::new();
            let ticks: u128 = (1_704_067_200u128 + WINDOWS_EPOCH_OFFSET as u128) * 10_000_000;
            hasher.update(format!("{ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
            format!("{:X}", hasher.finalize())
        };
        assert_eq!(token, expected);
    }

    /// The whole point of the rounding is that clients a few minutes apart
    /// still agree.
    #[test]
    fn the_token_is_stable_across_a_five_minute_window() {
        // 1_704_067_200 + 11_644_473_600 is a multiple of 300, so this is the
        // start of a window.
        let base = 1_704_067_200;
        assert_eq!(gec_token(base), gec_token(base + 299));
        assert_ne!(gec_token(base), gec_token(base + 300));
    }

    #[test]
    fn binary_frames_are_split_at_the_declared_header_length() {
        let mut frame = 4u16.to_be_bytes().to_vec();
        frame.extend_from_slice(b"headMP3DATA");
        assert_eq!(audio_of(&frame), b"MP3DATA");
    }

    /// A short or lying frame must not panic the worker thread.
    #[test]
    fn malformed_frames_contribute_no_audio() {
        assert!(audio_of(&[]).is_empty());
        assert!(audio_of(&[0x00]).is_empty());
        assert!(audio_of(&[0xff, 0xff, 0x01]).is_empty());
        // A header that exactly fills the frame leaves no audio, not a panic.
        let mut frame = 2u16.to_be_bytes().to_vec();
        frame.extend_from_slice(b"ab");
        assert!(audio_of(&frame).is_empty());
    }

    #[test]
    fn timestamps_render_in_the_javascript_date_form() {
        assert_eq!(
            js_timestamp(1_704_067_200),
            "Mon Jan 01 2024 00:00:00 GMT+0000 (Coordinated Universal Time)"
        );
        assert_eq!(
            js_timestamp(0),
            "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)"
        );
        // A leap day, and a non-zero time of day.
        assert_eq!(
            js_timestamp(1_709_209_845),
            "Thu Feb 29 2024 12:30:45 GMT+0000 (Coordinated Universal Time)"
        );
    }

    #[test]
    fn volume_becomes_an_offset_from_normal() {
        assert_eq!(volume_offset(100), 0);
        assert_eq!(volume_offset(60), -40);
        assert_eq!(volume_offset(0), -100);
    }

    #[test]
    fn the_locale_is_taken_from_the_voice_name() {
        assert_eq!(language_of("en-US-AriaNeural"), "en-US");
        assert_eq!(language_of("Aria"), "en-US");
    }

    #[test]
    fn voice_lists_are_parsed_and_sorted() {
        let body = serde_json::json!([
            {"ShortName": "en-US-AriaNeural", "Gender": "Female"},
            {"ShortName": "en-GB-RyanNeural", "Gender": "Male"},
            {"Gender": "Male"}
        ]);
        let voices = parse_voices(&body);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "en-GB-RyanNeural");
        assert_eq!(voices[1].label, "en-US-AriaNeural (Female)");
    }

    fn request(text: &str, voice: &str) -> SynthRequest {
        SynthRequest {
            text: text.into(),
            voice: voice.into(),
            rate: 0,
            volume: 100,
            pitch: 0,
            provider_settings: None,
        }
    }

    /// Live check against the real service; needs no account.
    /// `cargo test edge_speaks -- --include-ignored`
    ///
    /// This is the test that catches the `Sec-MS-GEC` derivation going stale,
    /// which is the most likely way this engine breaks.
    #[test]
    #[ignore]
    fn edge_speaks() {
        let samples = Edge
            .synth(&request("Pubsplash Edge speech is working.", ""))
            .expect("synthesis");
        let seconds = samples.len() as f32
            / (crate::audio::mixer::SAMPLE_RATE * crate::audio::mixer::CHANNELS as u32) as f32;
        assert!(seconds > 1.0, "unexpectedly short: {seconds}s");
        let peak = samples.iter().fold(0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.01, "silent audio (peak {peak})");
    }

    /// `cargo test edge_lists_voices -- --include-ignored`
    #[test]
    #[ignore]
    fn edge_lists_voices() {
        let voices = Edge.voices().expect("voice list");
        assert!(voices.len() > 100, "only {} voices", voices.len());
        assert!(voices.iter().any(|v| v.id == DEFAULT_VOICE));
    }

    #[test]
    fn the_advertised_client_version_stays_consistent() {
        assert_eq!(gec_version(), format!("1-{CHROMIUM_FULL}"));
        let agent = user_agent();
        let major = chromium_major();
        assert_eq!(major, "143");
        assert!(
            agent.contains(&format!("Edg/{major}.0.0.0"))
                && agent.contains(&format!("Chrome/{major}.0.0.0")),
            "the user agent and the advertised version must not drift apart: {agent}"
        );
    }
}
