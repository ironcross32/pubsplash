//! The contract every speech engine implements.
//!
//! An engine turns text into interleaved stereo f32 at the mixer's sample
//! rate, and that is the whole of it. Where the samples then go — the outgoing
//! stream, a bus, the local monitor — is the mixer's business, not the
//! engine's.
//!
//! [`synth`](SpeechEngine::synth) is called on a worker thread and is allowed
//! to block on the network. It must never be called from the audio thread.

/// A single utterance to render.
///
/// `rate`, `volume`, and `pitch` are carried in the ranges the *UI* uses —
/// SAPI's -10..=10 for rate, 0..=100 for volume, -50..=50 for pitch — and each
/// engine maps them onto whatever its API wants. Keeping one range in the
/// config means switching engines doesn't silently reinterpret a saved value
/// as something ten times louder or faster.
#[derive(Debug, Clone)]
pub struct SynthRequest {
    pub text: String,
    /// Engine-specific voice identifier; empty selects the engine default.
    pub voice: String,
    /// -10..=10, 0 being the voice's normal speed.
    pub rate: i32,
    /// 0..=100.
    pub volume: u32,
    /// -50..=50, 0 being the voice's normal pitch.
    pub pitch: i32,
}

impl SynthRequest {
    /// `rate` as a percentage offset, the unit Edge and Azure SSML want.
    /// -10..=10 maps onto -50%..=+100%, so the extremes stay intelligible.
    pub fn rate_percent(&self) -> i32 {
        let rate = self.rate.clamp(-10, 10);
        if rate < 0 { rate * 5 } else { rate * 10 }
    }

    /// `rate` as a speed multiplier, the unit OpenAI and ElevenLabs want.
    pub fn rate_multiplier(&self) -> f32 {
        1.0 + self.rate_percent() as f32 / 100.0
    }

    /// `volume` as a decibel gain relative to unity, for engines that take dB.
    /// 0 is silence rather than -inf dB so callers can clamp sensibly.
    pub fn volume_percent(&self) -> u32 {
        self.volume.clamp(0, 100)
    }

    /// Applies `volume` to samples an engine returned at full scale. Engines
    /// whose API has no volume control call this so the slider still works.
    pub fn apply_volume(&self, samples: &mut [f32]) {
        let gain = self.volume_percent() as f32 / 100.0;
        if (gain - 1.0).abs() < f32::EPSILON {
            return;
        }
        for sample in samples {
            *sample *= gain;
        }
    }

    /// Truncates the text to `max_chars` characters, on a character boundary.
    ///
    /// Chat can carry a wall of text and the paid engines bill per character,
    /// so a long message is cut short rather than refused — a clipped reading
    /// is more useful than silence.
    pub fn truncated(&self, max_chars: usize) -> Self {
        let mut copy = self.clone();
        if copy.text.chars().count() > max_chars {
            copy.text = copy.text.chars().take(max_chars).collect::<String>() + "…";
        }
        copy
    }
}

/// A selectable voice, as shown in the source dialog's picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// Stored in `TtsSourceConfig::voice`.
    pub id: String,
    /// Shown to the user; may carry a language or gender hint.
    pub label: String,
}

impl Voice {
    /// A voice whose identifier is also its label — the common case.
    pub fn plain(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
        }
    }

    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    /// A credential or endpoint the engine needs is blank. The message names
    /// the missing field so the UI can point at Preferences.
    #[error("{0}")]
    NotConfigured(String),
    #[error("network error: {0}")]
    Network(String),
    /// The service answered, and said no. Carries whatever detail it gave —
    /// this is what tells a user their API key is wrong rather than their
    /// microphone being muted.
    #[error("{service} returned {status}: {detail}")]
    Service {
        service: &'static str,
        status: u16,
        detail: String,
    },
    #[error("could not decode the audio {0} returned: {1}")]
    Decode(&'static str, String),
    #[error("{0}")]
    Other(String),
}

/// A text-to-speech engine.
///
/// Implementors are constructed per request batch from the current
/// [`crate::config::SpeechConfig`], so they hold configuration by value and
/// need no interior mutability.
pub trait SpeechEngine: Send {
    /// Stable identifier stored in `TtsSourceConfig::engine`.
    fn id(&self) -> &'static str;

    /// Name shown in the engine picker and in mixer strip labels.
    fn display_name(&self) -> &'static str;

    /// Renders `request` to interleaved stereo f32 at
    /// [`crate::audio::mixer::SAMPLE_RATE`].
    fn synth(&self, request: &SynthRequest) -> Result<Vec<f32>, TtsError>;

    /// Voices for the picker.
    ///
    /// Engines with a fixed list answer immediately; the rest go to the
    /// network, which is why the UI puts this behind a button and a worker
    /// thread rather than calling it while building the dialog.
    fn voices(&self) -> Result<Vec<Voice>, TtsError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_maps_onto_a_symmetric_but_wider_positive_range() {
        let at = |rate| SynthRequest {
            text: String::new(),
            voice: String::new(),
            rate,
            volume: 100,
            pitch: 0,
        };
        assert_eq!(at(0).rate_percent(), 0);
        assert_eq!(at(10).rate_percent(), 100);
        assert_eq!(at(-10).rate_percent(), -50);
        // Out-of-range values from a hand-edited config are clamped, not wrapped.
        assert_eq!(at(99).rate_percent(), 100);
        assert_eq!(at(-99).rate_percent(), -50);
        assert!((at(0).rate_multiplier() - 1.0).abs() < f32::EPSILON);
        assert!((at(-10).rate_multiplier() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn volume_scales_samples() {
        let request = SynthRequest {
            text: String::new(),
            voice: String::new(),
            rate: 0,
            volume: 50,
            pitch: 0,
        };
        let mut samples = vec![1.0, -1.0, 0.5];
        request.apply_volume(&mut samples);
        assert_eq!(samples, vec![0.5, -0.5, 0.25]);
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        let request = SynthRequest {
            text: "héllo wörld".into(),
            voice: String::new(),
            rate: 0,
            volume: 100,
            pitch: 0,
        };
        assert_eq!(request.truncated(5).text, "héllo…");
        // Short enough already: left alone, no ellipsis.
        assert_eq!(request.truncated(50).text, "héllo wörld");
    }
}
