//! Derived, user-visible names for audio sources.
//!
//! `SourceConfig.name` is an identity key, not a display string: it routes
//! external feeds in the engine and keys TTS speech requests, and it is always
//! just the kind's display name plus a counter (there is no rename UI). So
//! every label the user sees or hears is derived here instead, from the kind
//! and whatever it points at — the capture device, the running application, the
//! chosen voice. One module so the Scenes list, the mixer strips, the mute
//! checkboxes and the sends dialog can never drift apart.
//!
//! Two forms per source: [`list_labels`] is the verbose one for the Scenes &
//! Sources list (it also reports configuration problems, such as an application
//! that is not running), and [`strip_labels`] is the concise one that a screen
//! reader reads out on every mixer strip.

use crate::audio::device::{AppProcess, DeviceInfo};
use crate::config::{SourceConfig, SourceKindConfig};
use std::collections::{HashMap, HashSet};

/// What the labels need to know about the machine's current state.
#[derive(Debug, Clone, Default)]
pub struct NameContext {
    /// Capture devices, for resolving a microphone's `device_id`.
    pub devices: Vec<DeviceInfo>,
    /// Running applications, keyed by the configured process name, lowercased
    /// and trimmed. Absent means "not running right now".
    pub apps: HashMap<String, AppProcess>,
    /// Identity names of sources whose capture thread is failing and retrying.
    pub failing: HashSet<String>,
    /// Display names for TTS voices whose configured id is an opaque key,
    /// keyed by engine id and then voice id. Only engines that need it are
    /// looked up, and an id the catalog has never seen is simply absent.
    pub voice_labels: HashMap<String, HashMap<String, String>>,
}

impl NameContext {
    /// Gathers what the given sources need. Capture devices are enumerated
    /// only when some source actually names one, since that costs a WASAPI
    /// round trip and most scenes are on the default device. Voice names are
    /// looked up here, and not in the label functions, so those stay pure — the
    /// point of this type.
    pub fn build(
        sources: &[SourceConfig],
        apps: HashMap<String, AppProcess>,
        failing: HashSet<String>,
    ) -> Self {
        let needs_devices = sources
            .iter()
            .any(|s| matches!(s.kind, SourceKindConfig::Microphone { device_id: Some(_) }));
        Self {
            devices: if needs_devices {
                crate::audio::device::capture_devices()
            } else {
                Vec::new()
            },
            apps,
            failing,
            voice_labels: voice_labels_for(sources),
        }
    }

    fn device_name(&self, id: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.as_str())
    }

    fn app(&self, process_name: &str) -> Option<&AppProcess> {
        self.apps.get(&process_name.trim().to_ascii_lowercase())
    }

    /// The voice's display name, for the engines that need resolving. `None`
    /// covers both "this engine's ids read fine already" and "the catalog has
    /// not got this voice", which the callers distinguish.
    fn voice_label(&self, engine: &str, voice: &str) -> Option<&str> {
        self.voice_labels
            .get(engine)?
            .get(voice)
            .map(String::as_str)
    }
}

/// Resolves the voice names the given sources need from the TTS catalog.
///
/// Only sources on an engine with opaque voice ids are looked up, so the common
/// configuration touches the catalog not at all. Names the catalog does not hold
/// are left out rather than defaulted: the caller must be able to tell an
/// unresolved voice from a resolved one, since it prints neither the same way.
fn voice_labels_for(sources: &[SourceConfig]) -> HashMap<String, HashMap<String, String>> {
    let mut labels: HashMap<String, HashMap<String, String>> = HashMap::new();
    for source in sources {
        let SourceKindConfig::Tts(tts) = &source.kind else {
            continue;
        };
        if tts.voice.is_empty() || !crate::tts::engines::voice_ids_are_opaque(&tts.engine) {
            continue;
        }
        let engine = labels.entry(tts.engine.clone()).or_default();
        if engine.contains_key(&tts.voice) {
            continue;
        }
        if let Some(label) = crate::tts::cached_voice_label(&tts.engine, &tts.voice) {
            engine.insert(tts.voice.clone(), label);
        }
    }
    labels
}

/// How a TTS source's voice should be named, if at all.
enum VoiceDisplay<'a> {
    /// No voice is configured: the engine's default.
    Default,
    /// A name to say — either the catalog's, or an id that reads as one.
    Named(&'a str),
    /// A voice is configured but its name is not known yet, and its id is an
    /// opaque key. Saying the key would be worse than saying nothing.
    Unnamed,
}

fn voice_display<'a>(
    tts: &'a crate::config::TtsSourceConfig,
    ctx: &'a NameContext,
) -> VoiceDisplay<'a> {
    if tts.voice.is_empty() {
        return VoiceDisplay::Default;
    }
    match ctx.voice_label(&tts.engine, &tts.voice) {
        Some(label) => VoiceDisplay::Named(label),
        None if crate::tts::engines::voice_ids_are_opaque(&tts.engine) => VoiceDisplay::Unnamed,
        None => VoiceDisplay::Named(&tts.voice),
    }
}

/// Marks a label whose source cannot currently reach its device. The capture
/// thread keeps retrying, so this is a state the source comes back from — the
/// word has to say that, or a user hearing it would go looking for a setting to
/// fix instead of waiting a couple of seconds.
fn with_state(label: String, source: &SourceConfig, ctx: &NameContext) -> String {
    if ctx.failing.contains(&source.name) {
        format!("{label} (reconnecting)")
    } else {
        label
    }
}

/// Verbose labels for the Scenes & Sources list, deduplicated.
pub fn list_labels(sources: &[SourceConfig], ctx: &NameContext) -> Vec<String> {
    dedup(sources.iter().map(|s| list_label(s, ctx)).collect())
}

/// Concise base names for mixer strips, mute checkboxes and the sends dialog,
/// deduplicated. These get " volume" or "Mute " wrapped around them, so they
/// stay as short as they can be while still telling two sources apart.
pub fn strip_labels(sources: &[SourceConfig], ctx: &NameContext) -> Vec<String> {
    dedup(sources.iter().map(|s| strip_label(s, ctx)).collect())
}

/// The single-source form, for callers that have one source in hand and no
/// sibling list to disambiguate against.
pub fn strip_label(source: &SourceConfig, ctx: &NameContext) -> String {
    with_state(base_strip_label(source, ctx), source, ctx)
}

fn base_strip_label(source: &SourceConfig, ctx: &NameContext) -> String {
    match &source.kind {
        SourceKindConfig::Microphone { device_id: None } => "Microphone".to_string(),
        SourceKindConfig::Microphone {
            device_id: Some(id),
        } => match ctx.device_name(id) {
            Some(name) => name.to_string(),
            None => "Microphone (unavailable)".to_string(),
        },
        SourceKindConfig::DesktopAudio => "Desktop Audio".to_string(),
        SourceKindConfig::Application { process_name } => {
            if process_name.trim().is_empty() {
                "Application".to_string()
            } else {
                match ctx.app(process_name) {
                    Some(app) => app.display_name.clone(),
                    None => process_name.trim().to_string(),
                }
            }
        }
        SourceKindConfig::Tts(tts) => match voice_display(tts, ctx) {
            VoiceDisplay::Named(voice) => format!("Text-to-Speech ({voice})"),
            // An unresolved opaque id reads exactly like an unset voice here.
            // The strip form is the short one and has no room to explain the
            // difference; the list form below does.
            VoiceDisplay::Default | VoiceDisplay::Unnamed => "Text-to-Speech".to_string(),
        },
        SourceKindConfig::SoundEvents(_) => "Sound Events".to_string(),
    }
}

fn list_label(source: &SourceConfig, ctx: &NameContext) -> String {
    with_state(base_list_label(source, ctx), source, ctx)
}

fn base_list_label(source: &SourceConfig, ctx: &NameContext) -> String {
    match &source.kind {
        SourceKindConfig::Microphone { device_id: None } => {
            "Microphone (default device)".to_string()
        }
        SourceKindConfig::Microphone { device_id: Some(_) } => base_strip_label(source, ctx),
        SourceKindConfig::DesktopAudio => "Desktop Audio".to_string(),
        SourceKindConfig::Application { process_name } => {
            if process_name.trim().is_empty() {
                "Application: not set".to_string()
            } else {
                match ctx.app(process_name) {
                    Some(app) => format!("Application: {} ({})", app.display_name, app.exe),
                    None => format!("Application: {} (not running)", process_name.trim()),
                }
            }
        }
        SourceKindConfig::Tts(tts) => {
            let engine = crate::tts::engines::display_name(&tts.engine);
            match voice_display(tts, ctx) {
                VoiceDisplay::Default => format!("Text-to-Speech: {engine}, default voice"),
                VoiceDisplay::Named(voice) => format!("Text-to-Speech: {engine}, {voice}"),
                // A voice *is* configured, so this must not say "default
                // voice" — the engine name alone, until the catalog resolves it.
                VoiceDisplay::Unnamed => format!("Text-to-Speech: {engine}"),
            }
        }
        SourceKindConfig::SoundEvents(_) => "Sound Events".to_string(),
    }
}

/// Appends " 2", " 3"... to repeated labels so two sources that resolve to the
/// same thing (two default-device microphones, say) stay distinguishable.
fn dedup(mut labels: Vec<String>) -> Vec<String> {
    // The *suffixed* label goes into `seen` too, not just the base. Recording
    // only the base meant that a source whose own name already ended in a
    // number could collide with a suffix generated for a different one — two
    // default microphones alongside something already called "Microphone 2"
    // produced two identical labels, in the module whose entire job is telling
    // sources apart.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for label in &mut labels {
        let base = label.clone();
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            let mut n = *count;
            // Step past any suffix already taken by a label of its own.
            let mut candidate = format!("{base} {n}");
            while seen.contains_key(&candidate) {
                n += 1;
                candidate = format!("{base} {n}");
            }
            *seen.get_mut(&base).unwrap() = n;
            *label = candidate;
        }
        seen.entry(label.clone()).or_insert(1);
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TtsSourceConfig;

    #[test]
    fn a_generated_suffix_never_collides_with_a_real_label() {
        // "Microphone" twice would generate "Microphone 2" for the second —
        // which is already the name of a different device here.
        let labels = dedup(vec![
            "Microphone".to_string(),
            "Microphone 2".to_string(),
            "Microphone".to_string(),
        ]);
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "every label must be distinct");
        assert_eq!(labels[0], "Microphone");
        assert_eq!(labels[1], "Microphone 2");
        assert_eq!(labels[2], "Microphone 3");
    }

    #[test]
    fn repeated_labels_still_count_up_normally() {
        assert_eq!(
            dedup(vec![
                "Mic".to_string(),
                "Mic".to_string(),
                "Mic".to_string()
            ]),
            vec!["Mic", "Mic 2", "Mic 3"]
        );
    }

    fn ctx() -> NameContext {
        NameContext {
            devices: vec![DeviceInfo {
                id: "zoom-id".to_string(),
                name: "Microphone (ZOOM H1essential)".to_string(),
            }],
            // Keyed by what the user typed (here, without the extension) —
            // that is the contract `resolve_apps` returns.
            apps: HashMap::from([(
                "nvda".to_string(),
                AppProcess {
                    pid: 42,
                    exe: "nvda.exe".to_string(),
                    display_name: "NVDA".to_string(),
                },
            )]),
            failing: HashSet::new(),
            voice_labels: HashMap::from([(
                crate::tts::engines::ELEVENLABS.to_string(),
                HashMap::from([("21m00Tcm4TlvDq8ikWAM".to_string(), "Rachel".to_string())]),
            )]),
        }
    }

    fn elevenlabs(voice: &str) -> SourceConfig {
        source(SourceKindConfig::Tts(TtsSourceConfig {
            engine: crate::tts::engines::ELEVENLABS.into(),
            voice: voice.into(),
            ..Default::default()
        }))
    }

    fn source(kind: SourceKindConfig) -> SourceConfig {
        SourceConfig {
            name: kind.type_display_name().to_string(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn microphone_uses_the_device_name() {
        let src = source(SourceKindConfig::Microphone {
            device_id: Some("zoom-id".to_string()),
        });
        assert_eq!(strip_label(&src, &ctx()), "Microphone (ZOOM H1essential)");
        assert_eq!(list_label(&src, &ctx()), "Microphone (ZOOM H1essential)");
    }

    #[test]
    fn default_microphone_stays_short_on_the_strip() {
        let src = source(SourceKindConfig::Microphone { device_id: None });
        assert_eq!(strip_label(&src, &ctx()), "Microphone");
        assert_eq!(list_label(&src, &ctx()), "Microphone (default device)");
    }

    #[test]
    fn missing_device_is_called_out() {
        let src = source(SourceKindConfig::Microphone {
            device_id: Some("unplugged".to_string()),
        });
        assert_eq!(strip_label(&src, &ctx()), "Microphone (unavailable)");
    }

    /// A source whose device dropped out is retrying, not broken, and both
    /// forms have to say so — the mixer strip is the only place a screen-reader
    /// user finds out their microphone is not on air.
    #[test]
    fn a_reconnecting_source_says_so_in_both_forms() {
        let src = source(SourceKindConfig::Microphone {
            device_id: Some("zoom-id".to_string()),
        });
        let mut ctx = ctx();
        ctx.failing.insert(src.name.clone());
        assert_eq!(
            strip_label(&src, &ctx),
            "Microphone (ZOOM H1essential) (reconnecting)"
        );
        assert_eq!(
            list_label(&src, &ctx),
            "Microphone (ZOOM H1essential) (reconnecting)"
        );
    }

    /// The suffix is applied once, at the outer label, even though the verbose
    /// form of a device-specific microphone is built from the concise one.
    #[test]
    fn reconnecting_is_not_said_twice() {
        let src = source(SourceKindConfig::Microphone {
            device_id: Some("zoom-id".to_string()),
        });
        let mut ctx = ctx();
        ctx.failing.insert(src.name.clone());
        assert_eq!(list_label(&src, &ctx).matches("reconnecting").count(), 1);
    }

    /// Only the source that is actually failing is marked, and the dedup
    /// counter still lands on the labels that really do collide.
    #[test]
    fn only_the_failing_source_is_marked() {
        let mut a = source(SourceKindConfig::Microphone { device_id: None });
        a.name = "Microphone".to_string();
        let mut b = source(SourceKindConfig::Microphone { device_id: None });
        b.name = "Microphone 2".to_string();
        let mut ctx = ctx();
        ctx.failing.insert("Microphone 2".to_string());
        assert_eq!(
            strip_labels(&[a, b], &ctx),
            vec!["Microphone", "Microphone (reconnecting)"]
        );
    }

    #[test]
    fn desktop_and_sound_events_are_not_repeated() {
        assert_eq!(
            list_label(&source(SourceKindConfig::DesktopAudio), &ctx()),
            "Desktop Audio"
        );
        assert_eq!(
            list_label(
                &source(SourceKindConfig::SoundEvents(
                    crate::config::SoundEventsSourceConfig::default()
                )),
                &ctx()
            ),
            "Sound Events"
        );
    }

    #[test]
    fn running_application_uses_its_product_name() {
        let src = source(SourceKindConfig::Application {
            process_name: "nvda".to_string(),
        });
        assert_eq!(strip_label(&src, &ctx()), "NVDA");
        assert_eq!(list_label(&src, &ctx()), "Application: NVDA (nvda.exe)");
    }

    #[test]
    fn stopped_application_falls_back_to_what_the_user_typed() {
        let src = source(SourceKindConfig::Application {
            process_name: "obs64.exe".to_string(),
        });
        assert_eq!(strip_label(&src, &ctx()), "obs64.exe");
        assert_eq!(
            list_label(&src, &ctx()),
            "Application: obs64.exe (not running)"
        );
    }

    #[test]
    fn unset_application_says_so() {
        let src = source(SourceKindConfig::Application {
            process_name: String::new(),
        });
        assert_eq!(strip_label(&src, &ctx()), "Application");
        assert_eq!(list_label(&src, &ctx()), "Application: not set");
    }

    #[test]
    fn tts_carries_the_voice() {
        let src = source(SourceKindConfig::Tts(TtsSourceConfig {
            voice: "Blastbay Libby - English (United States)".to_string(),
            ..Default::default()
        }));
        assert_eq!(
            strip_label(&src, &ctx()),
            "Text-to-Speech (Blastbay Libby - English (United States))"
        );
        assert_eq!(
            list_label(&src, &ctx()),
            "Text-to-Speech: SAPI 5, Blastbay Libby - English (United States)"
        );
        let default_voice = source(SourceKindConfig::Tts(TtsSourceConfig::default()));
        assert_eq!(strip_label(&default_voice, &ctx()), "Text-to-Speech");
        assert_eq!(
            list_label(&default_voice, &ctx()),
            "Text-to-Speech: SAPI 5, default voice"
        );
    }

    /// The list label names the engine, so a user with several TTS sources can
    /// tell which is which without opening each one.
    #[test]
    fn tts_names_the_engine_it_is_using() {
        let edge = source(SourceKindConfig::Tts(TtsSourceConfig {
            engine: "edge".into(),
            voice: "en-US-AriaNeural".into(),
            ..Default::default()
        }));
        assert_eq!(
            list_label(&edge, &ctx()),
            "Text-to-Speech: Microsoft Edge, en-US-AriaNeural"
        );

        // An engine id from a newer build must not print raw into the UI.
        let unknown = source(SourceKindConfig::Tts(TtsSourceConfig {
            engine: "not-an-engine".into(),
            ..Default::default()
        }));
        assert_eq!(
            list_label(&unknown, &ctx()),
            "Text-to-Speech: SAPI 5, default voice"
        );
    }

    /// An ElevenLabs voice id is a 20-character key. Both forms say the voice's
    /// name instead, which is the only part a listener can act on.
    #[test]
    fn an_elevenlabs_voice_is_named_not_keyed() {
        let src = elevenlabs("21m00Tcm4TlvDq8ikWAM");
        assert_eq!(strip_label(&src, &ctx()), "Text-to-Speech (Rachel)");
        assert_eq!(
            list_label(&src, &ctx()),
            "Text-to-Speech: ElevenLabs, Rachel"
        );
    }

    /// Before the catalog has been fetched — no API key, a first run, a voice
    /// since deleted from the account — the id must not be read out. The list
    /// form still has to distinguish this from a source with no voice set,
    /// since here one *is* set and the label simply cannot name it yet.
    #[test]
    fn an_unresolved_elevenlabs_voice_says_nothing_rather_than_its_id() {
        let src = elevenlabs("XrExE9yKIg1WjnnlVkGX");
        let mut ctx = ctx();
        ctx.voice_labels.clear();
        assert_eq!(strip_label(&src, &ctx), "Text-to-Speech");
        assert_eq!(list_label(&src, &ctx), "Text-to-Speech: ElevenLabs");
        for label in [strip_label(&src, &ctx), list_label(&src, &ctx)] {
            assert!(
                !label.contains("XrExE9yKIg1WjnnlVkGX"),
                "the raw voice id leaked into {label:?}"
            );
        }
    }

    /// And an ElevenLabs source with no voice chosen is still the default one.
    #[test]
    fn an_elevenlabs_source_with_no_voice_is_still_the_default() {
        let src = elevenlabs("");
        assert_eq!(strip_label(&src, &ctx()), "Text-to-Speech");
        assert_eq!(
            list_label(&src, &ctx()),
            "Text-to-Speech: ElevenLabs, default voice"
        );
    }

    #[test]
    fn identical_sources_get_a_counter() {
        let sources = vec![
            source(SourceKindConfig::Microphone { device_id: None }),
            source(SourceKindConfig::Microphone { device_id: None }),
            source(SourceKindConfig::DesktopAudio),
        ];
        assert_eq!(
            strip_labels(&sources, &ctx()),
            vec!["Microphone", "Microphone 2", "Desktop Audio"]
        );
        assert_eq!(
            list_labels(&sources, &ctx()),
            vec![
                "Microphone (default device)",
                "Microphone (default device) 2",
                "Desktop Audio"
            ]
        );
    }
}
