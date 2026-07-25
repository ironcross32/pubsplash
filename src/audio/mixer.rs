//! Pure mixing DSP: gain strips with click-free fades, and block mixing.
//!
//! All audio is interleaved stereo f32 at [`SAMPLE_RATE`] Hz.

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Frames per mix block (10 ms at 48 kHz).
pub const BLOCK_FRAMES: usize = 480;
/// Samples (not frames) per mix block.
pub const BLOCK_SAMPLES: usize = BLOCK_FRAMES * CHANNELS;

/// Duration of the mute/unmute fade, in seconds.
const FADE_SECONDS: f32 = 0.05;

/// Absolute ceiling for a strip's volume. 100 is unity gain; anything above it
/// is make-up gain for a source that is simply too quiet at the OS level, and
/// is only reachable when the strip's "volume boost" is enabled in the UI (the
/// UI holds un-boosted strips at 100). The strip itself only enforces the
/// absolute ceiling.
pub const MAX_VOLUME: u32 = 500;

/// A volume/mute stage for one source (or the master bus). Gain changes are
/// ramped over [`FADE_SECONDS`] so mutes fade out and unmutes fade in.
#[derive(Debug, Clone)]
pub struct ChannelStrip {
    /// Slider volume 0-[`MAX_VOLUME`] (100 = unity), remembered across mute.
    volume: u32,
    muted: bool,
    /// The gain currently being applied (moves toward `target_gain`).
    current_gain: f32,
}

impl ChannelStrip {
    pub fn new(volume: u32, muted: bool) -> Self {
        let mut strip = Self {
            volume,
            muted,
            current_gain: 0.0,
        };
        strip.current_gain = strip.target_gain();
        strip
    }

    fn target_gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            (self.volume.min(MAX_VOLUME) as f32) / 100.0
        }
    }

    pub fn set_volume(&mut self, volume: u32) {
        self.volume = volume.min(MAX_VOLUME);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn volume(&self) -> u32 {
        self.volume
    }

    /// Mute keeps the volume so unmute restores the previous level.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    #[allow(dead_code)]
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Applies the strip's gain to `block` in place, ramping toward the
    /// target gain across the block.
    pub fn process(&mut self, block: &mut [f32]) {
        let target = self.target_gain();
        if (self.current_gain - target).abs() < f32::EPSILON {
            if target == 0.0 {
                block.fill(0.0);
            } else if (target - 1.0).abs() > f32::EPSILON {
                for s in block.iter_mut() {
                    *s *= target;
                }
            }
            return;
        }
        // Per-frame step so the fade takes FADE_SECONDS regardless of block size.
        let step = 1.0 / (FADE_SECONDS * SAMPLE_RATE as f32);
        let mut gain = self.current_gain;
        for frame in block.chunks_mut(CHANNELS) {
            if gain < target {
                gain = (gain + step).min(target);
            } else {
                gain = (gain - step).max(target);
            }
            for s in frame {
                *s *= gain;
            }
        }
        self.current_gain = gain;
    }
}

/// Adds `src` into `dst` (same length), saturating is not needed for f32;
/// the master stage clamps before conversion to integer PCM.
pub fn mix_into(dst: &mut [f32], src: &[f32]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += s;
    }
}

/// Converts a mixed f32 block to interleaved i16 with clamping, for the
/// MP3 encoder.
pub fn to_i16(block: &[f32], out: &mut Vec<i16>) {
    out.clear();
    out.extend(
        block
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_gain_passes_through() {
        let mut strip = ChannelStrip::new(100, false);
        let mut block = vec![0.5f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(block.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn muted_strip_silences_after_fade() {
        let mut strip = ChannelStrip::new(100, false);
        strip.set_muted(true);
        let mut block = vec![0.5f32; BLOCK_SAMPLES];
        strip.process(&mut block); // 10ms of a 50ms fade: not yet silent
        assert!(block[0] > 0.4, "fade should start near previous gain");
        assert!(block[BLOCK_SAMPLES - 1] < 0.5, "fade should be descending");
        for _ in 0..6 {
            let mut b = vec![0.5f32; BLOCK_SAMPLES];
            strip.process(&mut b);
            block = b;
        }
        assert!(
            block.iter().all(|&s| s == 0.0),
            "silent after fade completes"
        );
    }

    #[test]
    fn unmute_restores_previous_volume() {
        let mut strip = ChannelStrip::new(60, false);
        strip.set_muted(true);
        for _ in 0..10 {
            strip.process(&mut vec![0.5f32; BLOCK_SAMPLES]);
        }
        strip.set_muted(false);
        assert_eq!(strip.volume(), 60);
        for _ in 0..10 {
            strip.process(&mut vec![0.5f32; BLOCK_SAMPLES]);
        }
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.6).abs() < 1e-3),
            "gain should settle at volume 60 => 0.6"
        );
    }

    /// Runs enough blocks for the gain ramp to reach its target.
    fn settle(strip: &mut ChannelStrip) {
        for _ in 0..60 {
            strip.process(&mut vec![0.0f32; BLOCK_SAMPLES]);
        }
    }

    #[test]
    fn boosted_volume_amplifies() {
        let mut strip = ChannelStrip::new(200, false);
        settle(&mut strip);
        let mut block = vec![0.25f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.5).abs() < 1e-3),
            "volume 200 should double the signal"
        );
    }

    #[test]
    fn volume_is_capped_at_max() {
        let mut strip = ChannelStrip::new(100, false);
        strip.set_volume(9_999);
        assert_eq!(strip.volume(), MAX_VOLUME);
        settle(&mut strip);
        let mut block = vec![0.1f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.5).abs() < 1e-3),
            "gain should cap at MAX_VOLUME / 100 = 5.0"
        );
    }

    #[test]
    fn mix_and_convert() {
        let mut dst = vec![0.25f32; 4];
        mix_into(&mut dst, &[0.25, 0.25, 1.0, -2.0]);
        let mut out = Vec::new();
        to_i16(&dst, &mut out);
        assert_eq!(out[0], (0.5 * i16::MAX as f32) as i16);
        assert_eq!(out[2], i16::MAX); // clamped from 1.25
        assert_eq!(out[3], -i16::MAX); // clamped from -1.75
    }
}
