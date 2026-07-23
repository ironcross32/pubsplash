//! MP3 encoding via LAME.

use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use mp3lame_encoder::{Bitrate, Builder, Encoder, FlushNoGap, InterleavedPcm};

pub struct Mp3Encoder {
    encoder: Encoder,
    out: Vec<u8>,
}

fn bitrate_from_kbps(kbps: u32) -> Bitrate {
    match kbps {
        0..=48 => Bitrate::Kbps48,
        49..=64 => Bitrate::Kbps64,
        65..=96 => Bitrate::Kbps96,
        97..=128 => Bitrate::Kbps128,
        129..=160 => Bitrate::Kbps160,
        161..=192 => Bitrate::Kbps192,
        193..=256 => Bitrate::Kbps256,
        _ => Bitrate::Kbps320,
    }
}

impl Mp3Encoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, String> {
        let mut builder = Builder::new().ok_or("failed to allocate LAME encoder")?;
        builder
            .set_num_channels(CHANNELS as u8)
            .map_err(|e| e.to_string())?;
        builder
            .set_sample_rate(SAMPLE_RATE)
            .map_err(|e| e.to_string())?;
        builder
            .set_brate(bitrate_from_kbps(bitrate_kbps))
            .map_err(|e| e.to_string())?;
        builder
            .set_quality(mp3lame_encoder::Quality::Good)
            .map_err(|e| e.to_string())?;
        let encoder = builder.build().map_err(|e| e.to_string())?;
        Ok(Self {
            encoder,
            out: Vec::new(),
        })
    }

    /// Encodes one interleaved i16 block; returns the MP3 bytes produced
    /// (possibly empty while LAME buffers).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<&[u8], String> {
        self.out.clear();
        self.out
            .reserve(mp3lame_encoder::max_required_buffer_size(pcm.len() / CHANNELS));
        let written = self
            .encoder
            .encode(InterleavedPcm(pcm), self.out.spare_capacity_mut())
            .map_err(|e| e.to_string())?;
        // SAFETY: `encode` initialized `written` bytes of the spare capacity.
        unsafe { self.out.set_len(written) };
        Ok(&self.out)
    }

    /// Flushes LAME's internal buffer at end of stream.
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        self.out.clear();
        self.out.reserve(16 * 1024);
        let written = self
            .encoder
            .flush::<FlushNoGap>(self.out.spare_capacity_mut())
            .map_err(|e| e.to_string())?;
        // SAFETY: `flush` initialized `written` bytes of the spare capacity.
        unsafe { self.out.set_len(written) };
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer::BLOCK_SAMPLES;

    #[test]
    fn encodes_sine_to_valid_mp3_frames() {
        let mut enc = Mp3Encoder::new(128).unwrap();
        // 1 second of 440 Hz.
        let mut produced = Vec::new();
        let mut t = 0f32;
        for _ in 0..100 {
            let block: Vec<i16> = (0..BLOCK_SAMPLES)
                .map(|i| {
                    if i % 2 == 0 {
                        t += 1.0 / SAMPLE_RATE as f32;
                    }
                    ((t * 440.0 * std::f32::consts::TAU).sin() * 16000.0) as i16
                })
                .collect();
            produced.extend_from_slice(enc.encode(&block).unwrap());
        }
        produced.extend(enc.finish().unwrap());
        assert!(produced.len() > 4000, "should produce a meaningful bitstream");
        // MP3 frame sync: 11 set bits.
        let sync = produced
            .windows(2)
            .position(|w| w[0] == 0xFF && (w[1] & 0xE0) == 0xE0);
        assert!(sync.is_some(), "no MP3 frame sync found");
    }
}
