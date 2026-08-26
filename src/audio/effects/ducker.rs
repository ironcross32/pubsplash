//! Turns one source down while another is making noise.
//!
//! The classic radio move — music that gets out of the way when the presenter
//! talks — with the controls named after what the broadcaster wants rather
//! than after a compressor's front panel. There is no ratio and no knee: the
//! level while ducked is stated outright, and the gate that decides when to
//! duck is a single trigger level.
//!
//! ## Where the key level comes from
//!
//! Nothing here reaches for the other source's audio. `mix_one_block` measures
//! every source's post-fader RMS in its first pass and hands the whole array to
//! the effects in its second, so a ducker only ever indexes into that. Two
//! things fall out of it: the reading is from the *same* block whether the key
//! source sits above or below this one in the scene, and the ducker cannot
//! touch — or be aliased against — a buffer it does not own.
//!
//! Post-fader is deliberate. Muting the source it listens to stops the
//! ducking, and pulling that source's fader down makes ducking less likely,
//! which is what someone expects from a mute button. It also means the ducker
//! reacts to what the stream is actually carrying.

use super::super::mixer::{self, BLOCK_FRAMES, CHANNELS, SAMPLE_RATE};

/// A ducker as the engine holds it: settings resolved to indices and raw
/// numbers, plus the state that has to survive between blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Ducker {
    spec: DuckerSpec,
    /// The gain currently applied, 1.0 being untouched.
    gain: f32,
    /// Frames left to stay ducked after the key source went quiet.
    hold_left: u32,
}

/// One ducker's settings, in the units the engine works in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DuckerSpec {
    /// Which source to listen to, as an index into the block's level array.
    ///
    /// `None` covers every way a ducker can have nothing to listen to — never
    /// configured, naming a source that has been deleted, or naming itself —
    /// and makes all of them the same harmless thing: an effect that does
    /// nothing and lets the gain return to unity.
    pub key: Option<usize>,
    /// Gain while ducked, 0..1.
    pub duck_to: f32,
    /// Key level at or above which ducking starts, 0..1.
    pub trigger: f32,
    /// Per-frame gain steps. Precomputed because they are constant for the
    /// life of the spec and this runs on the audio thread.
    pub down_step: f32,
    pub up_step: f32,
    /// How long to stay ducked after the key goes quiet.
    pub hold_frames: u32,
}

impl DuckerSpec {
    /// Builds a spec from what the user set, with `key` already resolved.
    ///
    /// A zero fade time means "instantly", not "never": the step is clamped to
    /// a whole block rather than dividing by zero and handing the mixer a NaN
    /// that would spread through the master bus and out to every listener.
    pub fn new(
        key: Option<usize>,
        duck_to_percent: u32,
        trigger_percent: u32,
        fade_down_ms: u32,
        fade_up_ms: u32,
        hold_ms: u32,
    ) -> Self {
        DuckerSpec {
            key,
            duck_to: (duck_to_percent.min(100) as f32) / 100.0,
            trigger: (trigger_percent.min(100) as f32) / 100.0,
            down_step: step_per_frame(fade_down_ms),
            up_step: step_per_frame(fade_up_ms),
            hold_frames: ms_to_frames(hold_ms),
        }
    }
}

/// How much of the 0..1 gain range one frame of a `ms`-long fade covers.
fn step_per_frame(ms: u32) -> f32 {
    let frames = ms_to_frames(ms);
    if frames == 0 {
        // One block is the finest granularity anything upstream can ask for,
        // so this is "as fast as possible" and still a finite number.
        return 1.0;
    }
    1.0 / frames as f32
}

fn ms_to_frames(ms: u32) -> u32 {
    ((ms as u64 * SAMPLE_RATE as u64) / 1000) as u32
}

impl Ducker {
    pub fn new(spec: DuckerSpec) -> Self {
        Ducker {
            spec,
            // Starts open. A ducker that began life clamped shut would silence
            // its source until the first block proved the key was quiet.
            gain: 1.0,
            hold_left: 0,
        }
    }

    /// Only the tests read this back; everything else tells a ducker what to
    /// be rather than asking it what it is.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn spec(&self) -> &DuckerSpec {
        &self.spec
    }

    /// Replaces the settings, keeping the gain and hold where they are.
    ///
    /// Every `SetRouting` rebuilds the source list from scratch, and those
    /// arrive for reasons that have nothing to do with this effect — the
    /// two-second application poll re-syncs whenever a captured program's pid
    /// moves. Rebuilding the ducker with it would snap the gain back to unity,
    /// so the music would jump back to full in the middle of a sentence.
    pub fn adopt(&mut self, spec: DuckerSpec) {
        self.spec = spec;
    }

    /// Applies this block's gain. Engine thread only.
    ///
    /// `levels` is every source's post-fader RMS for this block, indexed as the
    /// engine's source list is.
    pub fn process(&mut self, block: &mut [f32], levels: &[f32]) {
        // An unresolved key reads as silence, so the gain walks back to unity
        // and stays there rather than freezing wherever it happened to be.
        let key = self
            .spec
            .key
            .and_then(|i| levels.get(i))
            .copied()
            .unwrap_or(0.0);

        // A trigger of zero would otherwise be met by digital silence and duck
        // a source with nothing playing against it.
        let open = key > 0.0 && key >= self.spec.trigger;
        self.hold_left = if open {
            self.spec.hold_frames
        } else {
            self.hold_left.saturating_sub(BLOCK_FRAMES as u32)
        };
        let target = if open || self.hold_left > 0 {
            self.spec.duck_to
        } else {
            1.0
        };

        // Wide open and staying there: the overwhelmingly common case, and the
        // whole block is skipped for it.
        if self.gain >= 1.0 && target >= 1.0 {
            return;
        }

        let step = if target < self.gain {
            self.spec.down_step
        } else {
            self.spec.up_step
        };
        // Per frame rather than per block: a gain that moved in 10 ms steps
        // would be a staircase, and audible as one.
        let mut gain = self.gain;
        for frame in block.chunks_mut(CHANNELS) {
            gain = mixer::approach(gain, target, step);
            for sample in frame {
                *sample *= gain;
            }
        }
        self.gain = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer::BLOCK_SAMPLES;

    fn spec() -> DuckerSpec {
        // Duck to a quarter, trigger at 5%, 20 ms down, 300 ms up, 250 ms hold.
        DuckerSpec::new(Some(0), 25, 5, 20, 300, 250)
    }

    /// Runs `blocks` blocks at a constant key level, returning the last block.
    fn run(ducker: &mut Ducker, key: f32, blocks: usize) -> Vec<f32> {
        let mut last = Vec::new();
        for _ in 0..blocks {
            let mut block = vec![1.0f32; BLOCK_SAMPLES];
            ducker.process(&mut block, &[key]);
            last = block;
        }
        last
    }

    #[test]
    fn a_quiet_key_leaves_the_block_untouched() {
        let mut ducker = Ducker::new(spec());
        let block = run(&mut ducker, 0.0, 10);
        assert!(block.iter().all(|&s| s == 1.0), "no ducking at all");
        assert_eq!(ducker.gain, 1.0);
    }

    /// Below the trigger is not ducking territory, however close it gets.
    #[test]
    fn a_key_under_the_trigger_does_not_duck() {
        let mut ducker = Ducker::new(spec());
        let block = run(&mut ducker, 0.049, 10);
        assert!(block.iter().all(|&s| s == 1.0));
    }

    #[test]
    fn a_loud_key_ducks_to_the_configured_level() {
        let mut ducker = Ducker::new(spec());
        // 20 ms of fade is two blocks; a few more settles it.
        let block = run(&mut ducker, 0.5, 6);
        assert!(
            (block[BLOCK_SAMPLES - 1] - 0.25).abs() < 1e-4,
            "expected a quarter, got {}",
            block[BLOCK_SAMPLES - 1]
        );
    }

    /// The drop has to be smooth: a step discontinuity is a click, and the
    /// first sample of the fade must still be near full volume.
    #[test]
    fn the_drop_is_a_fade_not_a_step() {
        let mut ducker = Ducker::new(spec());
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        ducker.process(&mut block, &[0.5]);
        assert!(block[0] > 0.99, "first sample jumped to {}", block[0]);
        assert!(block[BLOCK_SAMPLES - 1] < 0.6, "and it is on its way down");
        assert!(ducker.gain > 0.25, "20 ms is two blocks, so not there yet");
    }

    /// The pauses between words are exactly what the hold is for.
    #[test]
    fn hold_keeps_it_ducked_after_the_key_goes_quiet() {
        let mut ducker = Ducker::new(spec());
        run(&mut ducker, 0.5, 6);
        // 250 ms of hold is 25 blocks; nothing may move for the first 20.
        let block = run(&mut ducker, 0.0, 20);
        assert!(
            (block[BLOCK_SAMPLES - 1] - 0.25).abs() < 1e-4,
            "still ducked through the hold, got {}",
            block[BLOCK_SAMPLES - 1]
        );
        // Well past the hold and the 300 ms recovery, it is back.
        let block = run(&mut ducker, 0.0, 60);
        assert_eq!(block[BLOCK_SAMPLES - 1], 1.0, "fully recovered");
    }

    #[test]
    fn recovery_takes_longer_than_the_drop() {
        let mut ducker = Ducker::new(spec());
        run(&mut ducker, 0.5, 6);
        // Clear the hold, then watch one block of the recovery.
        run(&mut ducker, 0.0, 26);
        let before = ducker.gain;
        run(&mut ducker, 0.0, 1);
        let risen = ducker.gain - before;
        assert!(risen > 0.0, "it is recovering");
        assert!(
            risen < 0.05,
            "300 ms of recovery is far slower than 20 ms of drop, moved {risen}"
        );
    }

    /// Every way of having nothing to listen to is the same harmless thing.
    #[test]
    fn a_ducker_with_no_key_is_a_no_op() {
        let mut ducker = Ducker::new(DuckerSpec::new(None, 25, 5, 20, 300, 250));
        let block = run(&mut ducker, 1.0, 10);
        assert!(block.iter().all(|&s| s == 1.0));
    }

    /// A key index left over from a source list that has since shrunk must not
    /// panic and must not duck.
    #[test]
    fn a_key_past_the_end_of_the_levels_is_a_no_op() {
        let mut ducker = Ducker::new(DuckerSpec::new(Some(7), 25, 5, 20, 300, 250));
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        ducker.process(&mut block, &[1.0]);
        assert!(block.iter().all(|&s| s == 1.0));
    }

    /// A trigger of zero must not be met by silence, or a source would duck
    /// against a key that is not playing at all.
    #[test]
    fn silence_never_triggers_even_at_a_zero_trigger() {
        let mut ducker = Ducker::new(DuckerSpec::new(Some(0), 25, 0, 20, 300, 250));
        let block = run(&mut ducker, 0.0, 10);
        assert!(block.iter().all(|&s| s == 1.0));
    }

    /// A zero fade time is "instantly", and above all is not a NaN: one would
    /// spread from here through master and out to every listener.
    #[test]
    fn zero_fade_times_are_instant_and_finite() {
        let mut ducker = Ducker::new(DuckerSpec::new(Some(0), 50, 5, 0, 0, 0));
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        ducker.process(&mut block, &[0.5]);
        assert!(
            block.iter().all(|s| s.is_finite()),
            "no NaN reached the mix"
        );
        assert!((ducker.gain - 0.5).abs() < 1e-6, "there within one block");
    }

    /// Settings change far more often than the source list really does, and a
    /// ducker that reset its gain on each one would lurch.
    #[test]
    fn adopting_new_settings_keeps_the_current_gain() {
        let mut ducker = Ducker::new(spec());
        run(&mut ducker, 0.5, 6);
        let ducked = ducker.gain;
        ducker.adopt(DuckerSpec::new(Some(0), 40, 5, 20, 300, 250));
        assert_eq!(ducker.gain, ducked, "the gain carried across");
        assert!(
            (ducker.spec.duck_to - 0.4).abs() < 1e-6,
            "the setting did not"
        );
    }

    #[test]
    fn percentages_and_times_convert_as_stated() {
        let spec = DuckerSpec::new(Some(1), 25, 5, 20, 300, 250);
        assert_eq!(spec.duck_to, 0.25);
        assert_eq!(spec.trigger, 0.05);
        assert_eq!(spec.hold_frames, 12_000, "250 ms at 48 kHz");
        // 20 ms is 960 frames, so each frame moves 1/960 of the range.
        assert!((spec.down_step - 1.0 / 960.0).abs() < 1e-9);
    }

    /// Out-of-range percentages come from a settings file, not from a slider,
    /// so they are clamped rather than trusted.
    #[test]
    fn percentages_over_a_hundred_are_clamped() {
        let spec = DuckerSpec::new(Some(0), 5_000, 900, 20, 300, 250);
        assert_eq!(spec.duck_to, 1.0);
        assert_eq!(spec.trigger, 1.0);
    }
}
