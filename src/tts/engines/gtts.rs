//! Google Translate's read-aloud voice — free, no key, MP3 only.
//!
//! This is the endpoint the `gTTS` Python package drives. It is not a
//! supported API: it rate-limits, it caps each request at 200 characters, and
//! it can change without notice. It earns its place by needing no setup at
//! all, which for a user who just wants chat read out loud is the whole point.
//!
//! Requests are split at the 200-character limit and the MP3 fragments
//! concatenated, which is what gTTS itself does.

use crate::tts::decode::decode_compressed;
use crate::tts::engine::{SpeechEngine, SynthRequest, TtsError, Voice};
use crate::tts::net::{block_on, body_bytes, client};

const URL: &str = "https://translate.google.com/translate_tts";
const SERVICE: &str = "Google Translate";
/// The endpoint refuses anything longer in a single request.
const MAX_CHUNK: usize = 200;

/// Languages the endpoint serves, as (code, English name).
const LANGUAGES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("ca", "Catalan"),
    ("cs", "Czech"),
    ("cy", "Welsh"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Spanish"),
    ("et", "Estonian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("gu", "Gujarati"),
    ("hi", "Hindi"),
    ("hr", "Croatian"),
    ("hu", "Hungarian"),
    ("hy", "Armenian"),
    ("id", "Indonesian"),
    ("is", "Icelandic"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("jw", "Javanese"),
    ("km", "Khmer"),
    ("kn", "Kannada"),
    ("ko", "Korean"),
    ("la", "Latin"),
    ("lv", "Latvian"),
    ("mk", "Macedonian"),
    ("ml", "Malayalam"),
    ("mr", "Marathi"),
    ("my", "Myanmar"),
    ("ne", "Nepali"),
    ("nl", "Dutch"),
    ("no", "Norwegian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("si", "Sinhala"),
    ("sk", "Slovak"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("su", "Sundanese"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("tl", "Filipino"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("vi", "Vietnamese"),
    ("zh-CN", "Chinese (Simplified)"),
    ("zh-TW", "Chinese (Traditional)"),
];

pub struct GoogleTranslate;

impl GoogleTranslate {
    pub fn new() -> Self {
        Self
    }
}

impl SpeechEngine for GoogleTranslate {
    fn id(&self) -> &'static str {
        super::GTTS
    }

    fn display_name(&self) -> &'static str {
        "Google Translate"
    }

    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError> {
        let language = if request.voice.is_empty() {
            "en"
        } else {
            &request.voice
        };
        // The endpoint has no rate control beyond a "slow" mode, so a negative
        // rate is the closest thing to slowing it down that exists.
        let speed = if request.rate < 0 { "0.24" } else { "1" };
        let chunks = split_for_endpoint(&request.text);
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let mp3 = block_on(async {
            let mut combined: Vec<u8> = Vec::new();
            for (index, chunk) in chunks.iter().enumerate() {
                let response = client()
                    .get(URL)
                    .query(&[
                        ("ie", "UTF-8"),
                        ("client", "tw-ob"),
                        ("tl", language),
                        ("ttsspeed", speed),
                        ("total", &chunks.len().to_string()),
                        ("idx", &index.to_string()),
                        ("textlen", &chunk.chars().count().to_string()),
                        ("q", chunk),
                    ])
                    .send()
                    .await?;
                let bytes = body_bytes(SERVICE, response).await?;
                combined.extend_from_slice(&bytes);
            }
            Ok::<_, TtsError>(combined)
        })?;

        let mut samples = decode_compressed(SERVICE, mp3, "mp3")?;
        request.apply_volume(&mut samples);
        Ok(samples)
    }

    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        Ok(LANGUAGES
            .iter()
            .map(|(code, name)| Voice::new(*code, format!("{name} ({code})")))
            .collect())
    }
}

/// Splits text into pieces the endpoint will accept, preferring to break at a
/// space so words aren't cut in half mid-sound.
fn split_for_endpoint(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        // A single word past the limit has to be cut; there is no break to use.
        if word.chars().count() > MAX_CHUNK {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut rest: Vec<char> = word.chars().collect();
            while rest.len() > MAX_CHUNK {
                chunks.push(rest.drain(..MAX_CHUNK).collect());
            }
            current = rest.into_iter().collect();
            continue;
        }
        let extra = if current.is_empty() { 0 } else { 1 };
        if current.chars().count() + extra + word.chars().count() > MAX_CHUNK {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_a_single_request() {
        assert_eq!(split_for_endpoint("hello there"), vec!["hello there"]);
        assert!(split_for_endpoint("   ").is_empty());
    }

    #[test]
    fn long_text_is_split_at_spaces_within_the_limit() {
        let text = "word ".repeat(100);
        let chunks = split_for_endpoint(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= MAX_CHUNK, "{}", chunk.len());
            assert!(!chunk.starts_with(' ') && !chunk.ends_with(' '));
        }
        // Nothing may be dropped on the floor.
        assert_eq!(chunks.join(" "), text.trim());
    }

    /// A pathological single token still has to go out somehow.
    #[test]
    fn an_overlong_word_is_cut_rather_than_dropped() {
        let word = "a".repeat(450);
        let chunks = split_for_endpoint(&word);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), word);
        assert!(chunks.iter().all(|c| c.chars().count() <= MAX_CHUNK));
    }

    /// Live check against the real endpoint; needs no account. Also the only
    /// end-to-end exercise of the MP3 decode path.
    /// `cargo test gtts_speaks -- --include-ignored`
    #[test]
    #[ignore]
    fn gtts_speaks() {
        let samples = GoogleTranslate::new()
            .synth(&SynthRequest {
                text: "Pubsplash Google Translate speech is working.".into(),
                voice: "en".into(),
                rate: 0,
                volume: 100,
                pitch: 0,
            })
            .expect("synthesis");
        let seconds = samples.len() as f32
            / (crate::audio::mixer::SAMPLE_RATE * crate::audio::mixer::CHANNELS as u32) as f32;
        assert!(seconds > 1.0, "unexpectedly short: {seconds}s");
        let peak = samples.iter().fold(0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.01, "silent audio (peak {peak})");
    }

    #[test]
    fn languages_are_offered_as_voices() {
        let voices = GoogleTranslate::new().voices().unwrap();
        assert_eq!(voices.len(), LANGUAGES.len());
        let english = voices.iter().find(|v| v.id == "en").unwrap();
        assert_eq!(english.label, "English (en)");
    }
}
