//! Text-to-speech engines behind a common interface.
//!
//! V1 ships SAPI; OneCore, Microsoft Edge voices, and Piper are planned and
//! will plug in behind the same functions.

pub mod sapi;

use std::sync::OnceLock;

/// Voice enumeration walks the registry via a subprocess and takes seconds,
/// so the result is computed once and cached for the life of the session.
/// Voices installed mid-session appear on next launch.
static VOICE_CACHE: OnceLock<Vec<String>> = OnceLock::new();

/// Engine identifiers as stored in the config.
pub fn engine_names() -> Vec<&'static str> {
    vec!["sapi"]
}

/// Voice names for an engine (empty when the engine is unavailable).
pub fn voices_for(engine: &str) -> Vec<String> {
    match engine {
        "sapi" => VOICE_CACHE.get_or_init(sapi::voice_names).clone(),
        _ => Vec::new(),
    }
}

/// Fills the voice cache on a background thread so the first source-edit
/// dialog doesn't block the UI on enumeration.
pub fn prewarm_voices() {
    let _ = std::thread::Builder::new()
        .name("tts-voice-scan".into())
        .spawn(|| {
            VOICE_CACHE.get_or_init(sapi::voice_names);
        });
}
