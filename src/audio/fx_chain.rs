//! Runs an ordered chain of VST2 plugins over a stereo interleaved block.
//!
//! The mixer works in interleaved stereo f32; VST2 `processReplacing` wants
//! separate per-channel buffers. Each chain owns preallocated deinterleave
//! and channel-pointer scratch so `process` never allocates on the audio
//! thread.

use crate::vst::host2::Vst2Plugin;
use mixer::{BLOCK_FRAMES, CHANNELS};
use std::sync::Arc;

use super::mixer;

pub struct FxUnit {
    pub plugin: Arc<Vst2Plugin>,
    pub bypass: bool,
}

pub struct FxChain {
    units: Vec<FxUnit>,
    /// Ping-pong per-channel buffers: `a` holds the current signal, `b`
    /// receives each plugin's output, then the two swap.
    a: [Vec<f32>; CHANNELS],
    b: [Vec<f32>; CHANNELS],
    /// Fed to plugin inputs beyond the two we have; never read back.
    silence: Vec<f32>,
    /// Absorbs plugin outputs beyond the two we keep.
    junk: Vec<f32>,
    /// Reused input/output pointer arrays, sized to the widest plugin.
    in_ptrs: Vec<*mut f32>,
    out_ptrs: Vec<*mut f32>,
}

impl FxChain {
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn new(units: Vec<FxUnit>) -> Self {
        let max_io = units
            .iter()
            .flat_map(|u| [u.plugin.num_inputs, u.plugin.num_outputs])
            .max()
            .unwrap_or(2)
            .max(2) as usize;
        FxChain {
            units,
            a: [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]],
            b: [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]],
            silence: vec![0.0; BLOCK_FRAMES],
            junk: vec![0.0; BLOCK_FRAMES],
            in_ptrs: vec![std::ptr::null_mut(); max_io],
            out_ptrs: vec![std::ptr::null_mut(); max_io],
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
        deinterleave(interleaved, &mut self.a);

        for unit in &self.units {
            if unit.bypass {
                continue;
            }
            let plugin = &unit.plugin;
            let n_in = plugin.num_inputs.max(0) as usize;
            let n_out = plugin.num_outputs.max(0) as usize;

            // Inputs: our two channels, then silence. Mono-in plugins get L
            // only, which is the conventional downmix source.
            for i in 0..n_in {
                self.in_ptrs[i] = match i {
                    0 => self.a[0].as_mut_ptr(),
                    1 => self.a[1].as_mut_ptr(),
                    _ => self.silence.as_mut_ptr(),
                };
            }
            // Outputs: first two into `b`, extras into junk.
            for o in 0..n_out {
                self.out_ptrs[o] = match o {
                    0 => self.b[0].as_mut_ptr(),
                    1 => self.b[1].as_mut_ptr(),
                    _ => self.junk.as_mut_ptr(),
                };
            }

            unsafe {
                plugin.process_replacing(
                    self.in_ptrs.as_ptr(),
                    self.out_ptrs.as_ptr(),
                    BLOCK_FRAMES as i32,
                );
            }

            // Mono-out plugin: mirror its single output to both channels.
            if n_out == 1 {
                let (left, right) = self.b.split_at_mut(1);
                right[0].copy_from_slice(&left[0]);
            }
            std::mem::swap(&mut self.a, &mut self.b);
        }

        interleave(&self.a, interleaved);
    }
}

// SAFETY: the raw pointers only ever point into this chain's own buffers and
// are rebuilt each block; the chain is used from the engine thread alone.
unsafe impl Send for FxChain {}

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
}
