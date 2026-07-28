//! Runs an ordered chain of VST plugins over a stereo interleaved block.
//!
//! The mixer works in interleaved stereo f32; plugins want separate
//! per-channel buffers. Each chain owns preallocated deinterleave and
//! channel-pointer scratch so `process` never allocates on the audio thread.
//!
//! ## Fading, not switching
//!
//! A plugin can drop out of circuit mid-stream — a VST3 instance is
//! unavailable while its editor is being built or its state saved, and any
//! slot can be bypassed from the UI. Hard-switching to dry in the middle of a
//! block puts a step discontinuity in the stream and leaves the plugin's
//! internal state (reverb tails, lookahead) un-advanced so it clicks again on
//! the way back. Instead every unit carries a wet/dry gain ramped over
//! [`FADE_SECONDS`]. A plugin that knows it is about to become unavailable
//! reports [`Processed::Retiring`] *while still producing audio*, so the fade
//! happens over real output rather than over a gap.

use crate::vst::{MixMode, PluginInstance, Processed, PtrScratch};
use mixer::{BLOCK_FRAMES, CHANNELS, FADE_SECONDS, SAMPLE_RATE};
use std::sync::Arc;

use super::mixer;

pub struct FxUnit {
    pub plugin: Arc<PluginInstance>,
    pub bypass: bool,
    /// How much of this unit's output is currently in circuit: 1.0 fully
    /// processed, 0.0 fully dry. Engine thread only.
    wet: f32,
    /// The last block this unit actually produced. An unannounced dropout has
    /// no fresh audio to fade out of, so it fades out of this instead of
    /// stepping straight to dry.
    last_wet: [Vec<f32>; CHANNELS],
}

impl FxUnit {
    pub fn new(plugin: Arc<PluginInstance>, bypass: bool) -> Self {
        FxUnit {
            plugin,
            // A newly built chain starts wherever it belongs; the fade exists
            // for changes during playback, not for the first block after a
            // chain edit (which is already a discontinuity).
            wet: if bypass { 0.0 } else { 1.0 },
            bypass,
            last_wet: [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]],
        }
    }
}

pub struct FxChain {
    units: Vec<FxUnit>,
    /// The running signal, updated in place by each unit in turn.
    signal: [Vec<f32>; CHANNELS],
    /// Receives each plugin's output before it is mixed into `signal`.
    produced: [Vec<f32>; CHANNELS],
    /// Pointer plumbing for the VST2 units, sized to the widest plugin.
    scratch: PtrScratch,
}

impl FxChain {
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn new(units: Vec<FxUnit>) -> Self {
        let max_io = units
            .iter()
            .flat_map(|u| [u.plugin.num_inputs(), u.plugin.num_outputs()])
            .max()
            .unwrap_or(0)
            .max(0) as usize;
        FxChain {
            units,
            signal: [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]],
            produced: [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]],
            scratch: PtrScratch::new(max_io, BLOCK_FRAMES),
        }
    }

    pub fn set_bypass(&mut self, slot: usize, bypass: bool) {
        if let Some(unit) = self.units.get_mut(slot) {
            unit.bypass = bypass;
        }
    }

    /// Processes the interleaved stereo block in place through every
    /// non-bypassed plugin, in order. Engine thread only.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if self.units.is_empty() {
            return;
        }
        let FxChain {
            units,
            signal,
            produced,
            scratch,
        } = self;
        deinterleave(interleaved, signal);

        for unit in units.iter_mut() {
            // A bypassed unit keeps processing until its fade has finished, so
            // the bypass itself is click-free.
            let result = if unit.bypass && unit.wet <= 0.0 {
                Processed::Passthrough
            } else {
                match unit.plugin.process(signal, produced, scratch) {
                    Processed::Passthrough => Processed::Passthrough,
                    // Fading out either way, and the source is still fresh.
                    _ if unit.bypass => Processed::Retiring,
                    other => other,
                }
            };
            mix_in(
                &mut unit.wet,
                &mut unit.last_wet,
                unit.plugin.mix_mode(),
                result,
                signal,
                produced,
            );
        }

        interleave(signal, interleaved);
    }
}

// SAFETY: the raw pointers in `scratch` only ever point into this chain's own
// buffers and are rebuilt each block; the chain is used from the engine thread
// alone.
unsafe impl Send for FxChain {}

/// Mixes one unit's contribution into `signal`, ramping `wet` toward the gain
/// `result` asks for and remembering the last real output so a later dropout
/// has something to fade out of.
fn mix_in(
    wet: &mut f32,
    last_wet: &mut [Vec<f32>; CHANNELS],
    mode: MixMode,
    result: Processed,
    signal: &mut [Vec<f32>; CHANNELS],
    produced: &[Vec<f32>; CHANNELS],
) {
    let target = if result == Processed::Wet { 1.0 } else { 0.0 };
    let fresh = result != Processed::Passthrough;

    // Out of circuit and staying there: the signal passes untouched.
    if *wet <= 0.0 && target <= 0.0 {
        return;
    }

    {
        let source: &[Vec<f32>; CHANNELS] = if fresh { produced } else { last_wet };
        let frames = signal[0].len().min(source[0].len());

        if *wet >= 1.0 && target >= 1.0 {
            // Fully in circuit and staying there: no per-sample ramp.
            match mode {
                MixMode::Replace => {
                    for (channel, src) in signal.iter_mut().zip(source) {
                        channel[..frames].copy_from_slice(&src[..frames]);
                    }
                }
                MixMode::Add => {
                    for (channel, src) in signal.iter_mut().zip(source) {
                        for (sample, value) in channel[..frames].iter_mut().zip(&src[..frames]) {
                            *sample += value;
                        }
                    }
                }
            }
        } else {
            let step = 1.0 / (FADE_SECONDS * SAMPLE_RATE as f32);
            let mut gain = *wet;
            match mode {
                MixMode::Replace => {
                    for frame in 0..frames {
                        gain = approach(gain, target, step);
                        for channel in 0..CHANNELS {
                            signal[channel][frame] = gain * source[channel][frame]
                                + (1.0 - gain) * signal[channel][frame];
                        }
                    }
                }
                MixMode::Add => {
                    for frame in 0..frames {
                        gain = approach(gain, target, step);
                        for channel in 0..CHANNELS {
                            signal[channel][frame] += gain * source[channel][frame];
                        }
                    }
                }
            }
            *wet = gain;
        }
    }

    if fresh {
        for (stored, src) in last_wet.iter_mut().zip(produced) {
            let n = stored.len().min(src.len());
            stored[..n].copy_from_slice(&src[..n]);
        }
    }
}

fn approach(current: f32, target: f32, step: f32) -> f32 {
    if current < target {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

fn deinterleave(interleaved: &[f32], channels: &mut [Vec<f32>; CHANNELS]) {
    for (frame, sample) in interleaved.chunks_exact(CHANNELS).enumerate() {
        channels[0][frame] = sample[0];
        channels[1][frame] = sample[1];
    }
}

fn interleave(channels: &[Vec<f32>; CHANNELS], interleaved: &mut [f32]) {
    for (frame, sample) in interleaved.chunks_exact_mut(CHANNELS).enumerate() {
        sample[0] = channels[0][frame];
        sample[1] = channels[1][frame];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffers(left: f32, right: f32) -> [Vec<f32>; CHANNELS] {
        [vec![left; BLOCK_FRAMES], vec![right; BLOCK_FRAMES]]
    }

    /// Runs `blocks` blocks through `mix_in`, returning the last block's final
    /// sample on the left channel and the resulting wet gain.
    fn run(
        result: Processed,
        mode: MixMode,
        start_wet: f32,
        blocks: usize,
    ) -> (f32, f32, Vec<f32>) {
        let mut wet = start_wet;
        let mut last_wet = buffers(0.0, 0.0);
        let mut tail = Vec::new();
        let mut final_sample = 0.0;
        for _ in 0..blocks {
            let mut signal = buffers(1.0, 1.0);
            let produced = buffers(-1.0, -1.0);
            mix_in(
                &mut wet,
                &mut last_wet,
                mode,
                result,
                &mut signal,
                &produced,
            );
            final_sample = signal[0][BLOCK_FRAMES - 1];
            tail = signal[0].clone();
        }
        (final_sample, wet, tail)
    }

    #[test]
    fn deinterleave_interleave_roundtrip() {
        let mut interleaved: Vec<f32> = (0..BLOCK_FRAMES * CHANNELS).map(|i| i as f32).collect();
        let original = interleaved.clone();
        let mut channels = [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]];
        deinterleave(&interleaved, &mut channels);
        // Left channel is even indices, right is odd.
        assert_eq!(channels[0][0], 0.0);
        assert_eq!(channels[1][0], 1.0);
        assert_eq!(channels[0][1], 2.0);
        interleaved.fill(0.0);
        interleave(&channels, &mut interleaved);
        assert_eq!(interleaved, original);
    }

    #[test]
    fn empty_chain_is_a_noop() {
        let mut chain = FxChain::empty();
        let mut block: Vec<f32> = (0..BLOCK_FRAMES * CHANNELS).map(|i| i as f32).collect();
        let original = block.clone();
        chain.process(&mut block);
        assert_eq!(block, original);
    }

    #[test]
    fn a_wet_unit_in_circuit_replaces_the_signal() {
        let (sample, wet, _) = run(Processed::Wet, MixMode::Replace, 1.0, 1);
        assert_eq!(sample, -1.0);
        assert_eq!(wet, 1.0);
    }

    #[test]
    fn a_generator_sums_rather_than_replacing() {
        // Dry is 1.0, the generator produces -1.0, so a sum lands at 0.0 —
        // whereas a replace would land at -1.0.
        let (sample, _, _) = run(Processed::Wet, MixMode::Add, 1.0, 1);
        assert_eq!(sample, 0.0);
    }

    #[test]
    fn a_retiring_unit_fades_out_instead_of_switching() {
        // One 10 ms block cannot complete a 50 ms fade, and the first sample
        // must still be essentially wet — no step discontinuity.
        let (sample, wet, tail) = run(Processed::Retiring, MixMode::Replace, 1.0, 1);
        assert!(wet > 0.0 && wet < 1.0, "mid-fade after one block, got {wet}");
        assert!(
            (tail[0] - -1.0).abs() < 0.01,
            "first sample of the fade must still be wet, got {}",
            tail[0]
        );
        assert!(sample > -1.0, "the block must have moved toward dry");
    }

    #[test]
    fn a_fade_out_completes_within_the_fade_time() {
        // FADE_SECONDS at 48 kHz is 2400 frames — five 480-frame blocks.
        let (sample, wet, _) = run(Processed::Retiring, MixMode::Replace, 1.0, 6);
        assert_eq!(wet, 0.0);
        assert_eq!(sample, 1.0, "fully dry once the fade has finished");
    }

    #[test]
    fn a_unit_already_out_of_circuit_costs_nothing() {
        let mut wet = 0.0;
        let mut last_wet = buffers(9.0, 9.0);
        let mut signal = buffers(1.0, 1.0);
        let produced = buffers(-1.0, -1.0);
        mix_in(
            &mut wet,
            &mut last_wet,
            MixMode::Replace,
            Processed::Passthrough,
            &mut signal,
            &produced,
        );
        assert_eq!(signal[0][0], 1.0);
        assert_eq!(wet, 0.0);
        // The stale history is left alone rather than overwritten with a block
        // the plugin never produced.
        assert_eq!(last_wet[0][0], 9.0);
    }

    #[test]
    fn an_unannounced_dropout_fades_out_of_the_last_real_block() {
        let mut wet = 1.0;
        let mut last_wet = buffers(0.0, 0.0);
        // One good block, so there is something to fade out of.
        let mut signal = buffers(1.0, 1.0);
        let produced = buffers(-1.0, -1.0);
        mix_in(
            &mut wet,
            &mut last_wet,
            MixMode::Replace,
            Processed::Wet,
            &mut signal,
            &produced,
        );
        assert_eq!(last_wet[0][0], -1.0);

        // Now the plugin misses a block: the first sample must still be close
        // to the wet value rather than jumping to dry.
        let mut signal = buffers(1.0, 1.0);
        mix_in(
            &mut wet,
            &mut last_wet,
            MixMode::Replace,
            Processed::Passthrough,
            &mut signal,
            &produced,
        );
        assert!(
            (signal[0][0] - -1.0).abs() < 0.01,
            "expected a fade from the last real block, got {}",
            signal[0][0]
        );
    }

    #[test]
    fn a_unit_fades_back_in_when_it_recovers() {
        let (_, wet, _) = run(Processed::Wet, MixMode::Replace, 0.0, 1);
        assert!(wet > 0.0 && wet < 1.0, "mid-fade-in, got {wet}");
        let (sample, wet, _) = run(Processed::Wet, MixMode::Replace, 0.0, 6);
        assert_eq!(wet, 1.0);
        assert_eq!(sample, -1.0);
    }
}
