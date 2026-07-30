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

use engine::Voice;

/// Engine ids and display names, in picker order.
pub fn engine_names() -> Vec<(&'static str, &'static str)> {
    engines::ALL.to_vec()
}

/// Cached catalog voices for an engine, or `None` if it has never refreshed.
pub fn cached_voices(engine: &str) -> Option<Vec<Voice>> {
    catalog::voices(engine, "")
}

/// Cached voices filtered for a provider model. Engine-wide voices remain.
pub fn cached_voices_for_model(engine: &str, model: &str) -> Option<Vec<Voice>> {
    catalog::voices(engine, model)
}

/// How many engine-wide voices are cached; `None` means never refreshed.
pub fn voice_count(engine: &str) -> Option<usize> {
    cached_voices(engine).map(|voices| voices.len())
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
        assert_eq!(cached_voices(engines::OPENAI), None);
        assert_eq!(voice_count(engines::OPENAI), None);
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
