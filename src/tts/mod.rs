//! Text-to-speech engines behind a common interface.
//!
//! SAPI runs on a COM apartment thread and speaks locally as well as feeding
//! the mixer; every other engine is a blocking network call rendered on the
//! `tts-net` worker. [`speaker::Speaker`] routes between the two — see the
//! module docs there.
//!
//! Adding an engine means a file under [`engines`], a row in
//! [`engines::ALL`], and an arm in [`engines::build`]. Nothing else in the
//! app hard-codes an engine name.

pub mod clock;
pub mod decode;
pub mod engine;
pub mod engines;
pub mod net;
pub mod queue;
pub mod sapi;
pub mod speaker;
pub mod ssml;

use engine::Voice;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Voice lists, keyed by engine id.
///
/// SAPI enumeration walks the registry through a subprocess and takes seconds,
/// so it is worth caching; the network engines are cached because their lists
/// run to hundreds of entries and a user reopening the dialog should not pay
/// for a second round trip. Voices installed — or published — mid-session
/// appear after [`forget_voices`] or a restart.
static VOICE_CACHE: OnceLock<Mutex<HashMap<String, Vec<Voice>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Vec<Voice>>> {
    VOICE_CACHE.get_or_init(Default::default)
}

/// Engine ids and display names, in picker order.
pub fn engine_names() -> Vec<(&'static str, &'static str)> {
    engines::ALL.to_vec()
}

/// Cached voices for an engine, or `None` if they have not been fetched yet.
///
/// SAPI is filled in on demand because it needs no credentials and cannot
/// fail; the network engines return `None` until the user presses the fetch
/// button, so opening the dialog never blocks on a round trip.
pub fn cached_voices(engine: &str) -> Option<Vec<Voice>> {
    let id = engines::resolve_id(engine);
    if let Ok(cache) = cache().lock() {
        if let Some(voices) = cache.get(id) {
            return Some(voices.clone());
        }
    }
    if id == engines::SAPI {
        let voices: Vec<Voice> = sapi::voice_names().into_iter().map(Voice::plain).collect();
        store_voices(id, voices.clone());
        return Some(voices);
    }
    None
}

/// How many voices are cached for an engine; `None` means "never fetched",
/// which is a different thing from `Some(0)` ("the service reported none").
///
/// Unlike [`cached_voices`] this never clones the list and never falls back to
/// enumerating SAPI — it answers only from what is already cached. The voice
/// count is refreshed every time the engine picker settles on a new engine, and
/// deep-copying hundreds of `Voice`s (or shelling out to `reg query`) to learn a
/// single integer is exactly the cost that made that picker lag.
pub fn voice_count(engine: &str) -> Option<usize> {
    let id = engines::resolve_id(engine);
    cache().lock().ok()?.get(id).map(Vec::len)
}

/// Records a fetched voice list so reopening the dialog is instant.
pub fn store_voices(engine: &str, voices: Vec<Voice>) {
    if let Ok(mut cache) = cache().lock() {
        cache.insert(engines::resolve_id(engine).to_string(), voices);
    }
}

/// Drops an engine's cached voices, so the next look-up refetches. Used when
/// credentials change — a voice list fetched under the old key is stale.
pub fn forget_voices(engine: &str) {
    if let Ok(mut cache) = cache().lock() {
        cache.remove(engines::resolve_id(engine));
    }
}

/// Fills the SAPI voice cache on a background thread so the first source-edit
/// dialog doesn't block the UI on registry enumeration.
pub fn prewarm_voices() {
    let _ = std::thread::Builder::new()
        .name("tts-voice-scan".into())
        .spawn(|| {
            cached_voices(engines::SAPI);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_voices_come_back_for_the_engine_that_stored_them() {
        store_voices("openai", vec![Voice::plain("alloy")]);
        assert_eq!(cached_voices("openai"), Some(vec![Voice::plain("alloy")]));
        forget_voices("openai");
        assert_eq!(cached_voices("openai"), None);
    }

    /// The cache is keyed by engine, not global — the bug the single
    /// `OnceLock<Vec<String>>` it replaced would have had.
    #[test]
    fn engines_do_not_share_a_voice_list() {
        store_voices("elevenlabs", vec![Voice::plain("rachel")]);
        store_voices("azure", vec![Voice::plain("en-US-JennyNeural")]);
        assert_eq!(cached_voices("elevenlabs").unwrap()[0].id, "rachel");
        assert_eq!(cached_voices("azure").unwrap()[0].id, "en-US-JennyNeural");
        forget_voices("elevenlabs");
        assert!(cached_voices("azure").is_some(), "unrelated engine evicted");
        forget_voices("azure");
    }

    /// An unknown engine resolves to SAPI, so it must not land in the cache
    /// under its own name and shadow a real engine later.
    #[test]
    fn unknown_engine_ids_are_normalised_before_caching() {
        store_voices("not-an-engine", vec![Voice::plain("x")]);
        assert_eq!(cached_voices(engines::SAPI).unwrap()[0].id, "x");
        forget_voices(engines::SAPI);
    }

    /// The count label leans on "never fetched" and "fetched, but empty" being
    /// distinguishable — they are different messages to the user.
    #[test]
    fn voice_count_separates_never_fetched_from_empty() {
        forget_voices("star");
        assert_eq!(voice_count("star"), None);
        store_voices("star", Vec::new());
        assert_eq!(voice_count("star"), Some(0));
        store_voices("star", vec![Voice::plain("a"), Voice::plain("b")]);
        assert_eq!(voice_count("star"), Some(2));
        forget_voices("star");
    }

    #[test]
    fn the_picker_list_carries_ids_and_display_names() {
        let names = engine_names();
        assert!(
            names
                .iter()
                .any(|(id, name)| *id == "sapi" && *name == "SAPI 5")
        );
        assert_eq!(names.len(), engines::ALL.len());
    }
}
