//! The engine registry.
//!
//! Engines are identified by a stable string stored in the config. That id is
//! deliberately *not* derived from the display name: renaming "Google
//! Translate" in the UI must not orphan every saved source that selected it.

use crate::config::SpeechConfig;
use crate::tts::engine::SpeechEngine;

pub mod azure;
pub mod edge;
pub mod elevenlabs;
pub mod google;
pub mod gtts;
pub mod openai;
pub mod polly;
pub mod star;

/// Every engine, in the order the picker shows them: the two that need no
/// setup first, then the rest alphabetically.
pub const ALL: &[(&str, &str)] = &[
    (SAPI, "SAPI 5"),
    (EDGE, "Microsoft Edge"),
    (AWS, "AWS Polly"),
    (AZURE, "Azure"),
    (ELEVENLABS, "ElevenLabs"),
    (GOOGLE, "Google Cloud"),
    (GTTS, "Google Translate"),
    (OPENAI, "OpenAI"),
    (STAR, "Star"),
];

pub const SAPI: &str = "sapi";
pub const EDGE: &str = "edge";
pub const AWS: &str = "polly";
pub const AZURE: &str = "azure";
pub const ELEVENLABS: &str = "elevenlabs";
pub const GOOGLE: &str = "google";
pub const GTTS: &str = "gtts";
pub const OPENAI: &str = "openai";
pub const STAR: &str = "star";

/// The id to use for an `engine` string read from the config.
///
/// An unrecognised value falls back to SAPI rather than to silence: a config
/// written by a newer build, or hand-edited, should still talk.
pub fn resolve_id(engine: &str) -> &'static str {
    match ALL.iter().find(|(id, _)| *id == engine) {
        Some((id, _)) => id,
        None => {
            log::warn!("Unknown TTS engine {engine:?}; falling back to SAPI");
            SAPI
        }
    }
}

/// The name shown in the picker and in mixer strip labels.
pub fn display_name(engine: &str) -> &'static str {
    ALL.iter()
        .find(|(id, _)| *id == engine)
        .map(|(_, name)| *name)
        .unwrap_or("SAPI 5")
}

/// Whether an engine runs over the network, and so belongs on the `tts-net`
/// worker rather than the COM apartment thread.
pub fn is_network(engine: &str) -> bool {
    resolve_id(engine) != SAPI
}

/// Builds a network engine from the current credentials.
///
/// Returns `None` for SAPI, which is not a [`SpeechEngine`] on this path — it
/// owns an apartment thread and speaks locally as well as synthesizing, so it
/// is driven directly by [`crate::tts::sapi`].
pub fn build(engine: &str, config: &SpeechConfig) -> Option<Box<dyn SpeechEngine>> {
    match resolve_id(engine) {
        EDGE => Some(Box::new(edge::Edge::new(config))),
        AWS => Some(Box::new(polly::Polly::new(config))),
        AZURE => Some(Box::new(azure::Azure::new(config))),
        ELEVENLABS => Some(Box::new(elevenlabs::ElevenLabs::new(config))),
        GOOGLE => Some(Box::new(google::GoogleCloud::new(config))),
        GTTS => Some(Box::new(gtts::GoogleTranslate::new())),
        OPENAI => Some(Box::new(openai::OpenAi::new(config))),
        STAR => Some(Box::new(star::Star::new(config))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_is_unique_and_resolvable() {
        let mut seen = std::collections::HashSet::new();
        for (id, name) in ALL {
            assert!(seen.insert(*id), "duplicate engine id {id}");
            assert_eq!(resolve_id(id), *id);
            assert_eq!(display_name(id), *name);
        }
    }

    #[test]
    fn unknown_engines_fall_back_to_sapi_rather_than_silence() {
        assert_eq!(resolve_id("piper"), SAPI);
        assert_eq!(resolve_id(""), SAPI);
        assert!(!is_network("piper"));
    }

    #[test]
    fn sapi_is_the_only_non_network_engine() {
        let config = SpeechConfig::default();
        for (id, _) in ALL {
            assert_eq!(
                is_network(id),
                *id != SAPI,
                "{id} is on the wrong side of the network split"
            );
            assert_eq!(
                build(id, &config).is_some(),
                *id != SAPI,
                "{id} should only be buildable when it is a network engine"
            );
        }
    }

    /// The id a source stores must survive a round trip through the picker.
    #[test]
    fn built_engines_report_the_id_they_were_asked_for() {
        let config = SpeechConfig::default();
        for (id, name) in ALL.iter().filter(|(id, _)| *id != SAPI) {
            let engine = build(id, &config).expect("network engine");
            assert_eq!(engine.id(), *id);
            assert_eq!(engine.display_name(), *name);
        }
    }
}
