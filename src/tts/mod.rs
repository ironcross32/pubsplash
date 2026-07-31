//! Text-to-speech engines behind a common interface.
//!
//! SAPI runs on a COM apartment thread and speaks locally as well as feeding
//! the mixer; every other engine is a blocking network call rendered on the
//! `tts-net` worker. [`speaker::Speaker`] routes between the two.

pub mod catalog;
pub mod clock;
pub mod decode;
pub mod engine;
pub mod engines;
pub mod net;
pub mod queue;
pub mod sapi;
pub mod speaker;
pub mod ssml;
pub mod usage;

use engine::Voice;

/// Engine ids and display names, in picker order.
pub fn engine_names() -> Vec<(&'static str, &'static str)> {
    engines::ALL.to_vec()
}

/// Cached voices filtered for a provider model. Engine-wide voices remain, and
/// an empty `model` filters nothing, which is how a caller asks for all of them.
pub fn cached_voices_for_model(engine: &str, model: &str) -> Option<Vec<Voice>> {
    catalog::voices(engine, model)
}

/// The cached display name for one voice id, for engines whose ids are opaque
/// keys — see [`engines::voice_ids_are_opaque`].
pub fn cached_voice_label(engine: &str, voice_id: &str) -> Option<String> {
    catalog::voice_label(engine, voice_id)
}

/// How many cached voices [`cached_voices_for_model`] would return, without
/// building the list — see [`catalog::voice_count_for_model`]. `None` means the
/// engine has never refreshed, which is not the same as no matching voice.
pub fn voice_count_for_model(engine: &str, model: &str) -> Option<usize> {
    catalog::voice_count_for_model(engine, model)
}

/// Loads the persistent catalog before the UI is constructed.
pub fn prewarm_voices() {
    catalog::load();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_catalog_has_no_voices() {
        catalog::install(catalog::TtsCatalog::default());
        assert_eq!(cached_voices_for_model(engines::OPENAI, ""), None);
        assert_eq!(voice_count_for_model(engines::OPENAI, ""), None);
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
