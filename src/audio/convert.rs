//! Decoding and rate/channel conversion into the mixer's sample format.
//!
//! Everything the engine plays — sound-pack cues, synthesized speech — arrives
//! as some other format and has to land as interleaved stereo f32 at
//! [`ENGINE_SAMPLE_RATE`]. That conversion lives here so the sound pack tools
//! and the TTS engines share one implementation.
//!
//! This module deliberately depends on nothing but `hound`: `soundpack.rs` is
//! `#[path]`-included into the standalone `soundpack` and `pubsplash-soundpack`
//! binaries, which have no `crate::audio`, so it pulls this file in the same
//! way. Adding a `crate::` reference here breaks both of those builds.

#![allow(dead_code)]

use hound::{SampleFormat, WavReader};

pub const ENGINE_SAMPLE_RATE: u32 = 48_000;
pub const ENGINE_CHANNELS: usize = 2;

/// Decodes a RIFF WAV into interleaved stereo f32 at [`ENGINE_SAMPLE_RATE`].
pub fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut reader = WavReader::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let source_channels = usize::from(spec.channels);
    if source_channels == 0 {
        return Err("WAV files must have at least one channel".into());
    }
    if spec.sample_rate == 0 {
        return Err("WAV files must have a non-zero sample rate".into());
    }

    let samples = read_wav_samples(&mut reader, spec)?;
    let stereo = convert_to_stereo(&samples, source_channels)?;
    if spec.sample_rate == ENGINE_SAMPLE_RATE {
        Ok(stereo)
    } else {
        Ok(resample_stereo(
            &stereo,
            spec.sample_rate,
            ENGINE_SAMPLE_RATE,
        ))
    }
}

/// Converts raw little-endian 16-bit PCM to interleaved stereo f32 at
/// [`ENGINE_SAMPLE_RATE`].
///
/// Speech APIs mostly return headerless PCM whose rate and channel count come
/// from the request rather than the payload, so both are passed in. A trailing
/// odd byte is ignored rather than treated as an error — a truncated final
/// sample is not worth discarding an utterance over.
pub fn pcm16_to_engine(bytes: &[u8], source_rate: u32, source_channels: usize) -> Vec<f32> {
    if source_channels == 0 || source_rate == 0 {
        return Vec::new();
    }
    let samples: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect();
    // Drop a partial trailing frame so the stereo conversion below can't fail.
    let usable = samples.len() - samples.len() % source_channels;
    let Ok(stereo) = convert_to_stereo(&samples[..usable], source_channels) else {
        return Vec::new();
    };
    if source_rate == ENGINE_SAMPLE_RATE {
        stereo
    } else {
        resample_stereo(&stereo, source_rate, ENGINE_SAMPLE_RATE)
    }
}

pub fn read_wav_samples<R: std::io::Read>(
    reader: &mut WavReader<R>,
    spec: hound::WavSpec,
) -> Result<Vec<f32>, String> {
    if spec.sample_format == SampleFormat::Float {
        reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    } else if spec.bits_per_sample <= 16 {
        if spec.bits_per_sample == 0 {
            return Err("integer WAV files must have at least one bit per sample".into());
        }
        let max = (1_i32 << (spec.bits_per_sample - 1)) as f32;
        reader
            .samples::<i16>()
            .map(|x| x.map(|n| n as f32 / max))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    } else {
        let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
        reader
            .samples::<i32>()
            .map(|x| x.map(|n| n as f32 / max))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

pub fn convert_to_stereo(samples: &[f32], source_channels: usize) -> Result<Vec<f32>, String> {
    if source_channels == 0 || samples.len() % source_channels != 0 {
        return Err("WAV data ended in the middle of a frame".into());
    }

    let frames = samples.len() / source_channels;
    let mut stereo = Vec::with_capacity(frames * ENGINE_CHANNELS);
    for frame in samples.chunks_exact(source_channels) {
        stereo.push(frame[0]);
        stereo.push(if source_channels == 1 {
            frame[0]
        } else {
            frame[1]
        });
    }
    Ok(stereo)
}

/// Converts a *stream* of little-endian 16-bit PCM into interleaved stereo f32
/// at [`ENGINE_SAMPLE_RATE`], a chunk at a time.
///
/// [`pcm16_to_engine`] cannot just be called per chunk. A frame can straddle a
/// chunk boundary, and linear interpolation needs the frame *after* the one it
/// reads from, which may not have arrived yet — converting each chunk on its
/// own would drop the straddling bytes and clamp the last output frame of every
/// chunk against a neighbour it cannot see, which is a click per chunk. So this
/// holds back both the odd trailing bytes and the last complete source frame,
/// and carries the output position across calls. Feeding a buffer through in
/// any number of pieces gives the same samples as converting it in one go.
pub struct Pcm16Stream {
    source_rate: u32,
    source_channels: usize,
    /// Bytes from the last chunk that fell short of a whole frame.
    carry: Vec<u8>,
    /// Source frames from `base` onward, already widened to stereo.
    pending: Vec<f32>,
    /// Index of the source frame sitting in `pending[0]`.
    base: u64,
    /// Index of the next output frame to emit.
    next_target: u64,
}

impl Pcm16Stream {
    pub fn new(source_rate: u32, source_channels: usize) -> Self {
        Self {
            source_rate,
            source_channels,
            carry: Vec::new(),
            pending: Vec::new(),
            base: 0,
            next_target: 0,
        }
    }

    /// Whether this converter can do anything at all. Nonsense parameters
    /// yield no samples rather than an error, matching [`pcm16_to_engine`].
    fn usable(&self) -> bool {
        self.source_rate != 0 && self.source_channels != 0
    }

    /// Samples ready from `bytes` plus whatever was carried over.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<f32> {
        if !self.usable() {
            return Vec::new();
        }
        let frame_bytes = self.source_channels * 2;
        let mut source = std::mem::take(&mut self.carry);
        source.extend_from_slice(bytes);
        let whole = source.len() - source.len() % frame_bytes;
        for frame in source[..whole].chunks_exact(frame_bytes) {
            let sample = |index: usize| {
                i16::from_le_bytes([frame[index * 2], frame[index * 2 + 1]]) as f32 / 32768.0
            };
            let left = sample(0);
            self.pending.push(left);
            self.pending.push(if self.source_channels == 1 {
                left
            } else {
                sample(1)
            });
        }
        self.carry = source[whole..].to_vec();
        self.emit(false)
    }

    /// The tail, once the body has ended. A trailing partial frame is dropped,
    /// as it is in [`pcm16_to_engine`].
    pub fn finish(&mut self) -> Vec<f32> {
        self.carry.clear();
        if !self.usable() {
            return Vec::new();
        }
        let tail = self.emit(true);
        self.pending.clear();
        tail
    }

    /// Emits every output frame whose source neighbours have arrived. With
    /// `flush`, the right-hand neighbour is clamped to the last frame instead
    /// of waiting for one that is never coming.
    fn emit(&mut self, flush: bool) -> Vec<f32> {
        let total = self.base + (self.pending.len() / ENGINE_CHANNELS) as u64;
        let mut out = Vec::new();
        if total == 0 {
            return out;
        }
        let ratio = self.source_rate as f64 / ENGINE_SAMPLE_RATE as f64;
        loop {
            let position = self.next_target as f64 * ratio;
            let left = position.floor() as u64;
            if left >= total {
                break;
            }
            let (left, right) = if flush {
                (left.min(total - 1), (left + 1).min(total - 1))
            } else {
                if left + 1 >= total {
                    // The interpolation partner is in the next chunk.
                    break;
                }
                (left, left + 1)
            };
            let fraction = (position - left as f64) as f32;
            let left = ((left - self.base) as usize) * ENGINE_CHANNELS;
            let right = ((right - self.base) as usize) * ENGINE_CHANNELS;
            for channel in 0..ENGINE_CHANNELS {
                let a = self.pending[left + channel];
                let b = self.pending[right + channel];
                out.push(a + (b - a) * fraction);
            }
            self.next_target += 1;
        }
        // Everything below the next output frame's left-hand neighbour is done
        // with; without this the buffer would grow for the whole utterance.
        let keep_from = ((self.next_target as f64 * ratio).floor() as u64).min(total);
        if keep_from > self.base {
            let drop = ((keep_from - self.base) as usize) * ENGINE_CHANNELS;
            self.pending.drain(..drop);
            self.base = keep_from;
        }
        out
    }
}

pub fn resample_stereo(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let source_frames = samples.len() / ENGINE_CHANNELS;
    if source_frames <= 1 {
        return samples.to_vec();
    }

    let target_frames =
        ((source_frames as f64 * target_rate as f64 / source_rate as f64).round() as usize).max(1);
    let mut resampled = Vec::with_capacity(target_frames * ENGINE_CHANNELS);
    for target_frame in 0..target_frames {
        let source_pos = target_frame as f64 * source_rate as f64 / target_rate as f64;
        let left_frame = (source_pos.floor() as usize).min(source_frames - 1);
        let right_frame = (left_frame + 1).min(source_frames - 1);
        let fraction = (source_pos - left_frame as f64) as f32;

        for channel in 0..ENGINE_CHANNELS {
            let left = samples[left_frame * ENGINE_CHANNELS + channel];
            let right = samples[right_frame * ENGINE_CHANNELS + channel];
            resampled.push(left + (right - left) * fraction);
        }
    }
    resampled
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter second of mono 24 kHz should come back as a quarter second of
    /// stereo 48 kHz, with the waveform intact.
    #[test]
    fn pcm16_mono_24k_becomes_stereo_48k() {
        let source_rate = 24_000;
        let frames = source_rate as usize / 4;
        let mut bytes = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let phase = frame as f32 / source_rate as f32 * 440.0 * std::f32::consts::TAU;
            bytes.extend_from_slice(&((phase.sin() * 16384.0) as i16).to_le_bytes());
        }

        let samples = pcm16_to_engine(&bytes, source_rate, 1);

        let out_frames = samples.len() / ENGINE_CHANNELS;
        assert_eq!(out_frames, frames * 2, "expected a 2x upsample");
        // Mono is duplicated, not spread across the pair.
        for pair in samples.chunks_exact(ENGINE_CHANNELS) {
            assert_eq!(pair[0], pair[1]);
        }
        let peak = samples.iter().fold(0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.4 && peak <= 1.0, "unexpected peak {peak}");
    }

    #[test]
    fn pcm16_at_engine_rate_is_passed_through() {
        let bytes: Vec<u8> = (0..ENGINE_CHANNELS * 4)
            .flat_map(|n| (n as i16 * 1000).to_le_bytes())
            .collect();
        let samples = pcm16_to_engine(&bytes, ENGINE_SAMPLE_RATE, ENGINE_CHANNELS);
        assert_eq!(samples.len(), ENGINE_CHANNELS * 4);
    }

    /// A trailing odd byte must not cost us the whole utterance.
    #[test]
    fn pcm16_tolerates_a_truncated_trailing_frame() {
        let mut bytes: Vec<u8> = vec![0, 1, 0, 1, 0, 1, 0, 1];
        bytes.push(0);
        let samples = pcm16_to_engine(&bytes, ENGINE_SAMPLE_RATE, 2);
        assert_eq!(samples.len(), 4);
    }

    #[test]
    fn pcm16_rejects_nonsense_parameters() {
        assert!(pcm16_to_engine(&[0, 1, 0, 1], 0, 1).is_empty());
        assert!(pcm16_to_engine(&[0, 1, 0, 1], 24_000, 0).is_empty());
    }

    fn tone_bytes(frames: usize, channels: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(frames * channels * 2);
        for frame in 0..frames {
            for channel in 0..channels {
                let phase = (frame * channels + channel) as f32 * 0.07;
                bytes.extend_from_slice(&((phase.sin() * 16384.0) as i16).to_le_bytes());
            }
        }
        bytes
    }

    fn streamed(bytes: &[u8], rate: u32, channels: usize, chunk: usize) -> Vec<f32> {
        let mut stream = Pcm16Stream::new(rate, channels);
        let mut out = Vec::new();
        for piece in bytes.chunks(chunk) {
            out.extend(stream.push(piece));
        }
        out.extend(stream.finish());
        out
    }

    /// The whole point: a chunked conversion must be indistinguishable from
    /// converting the same bytes in one go, whatever the chunk sizes are.
    #[test]
    fn streaming_matches_a_single_conversion() {
        for (rate, channels) in [(24_000, 1), (24_000, 2), (48_000, 1), (16_000, 2)] {
            let bytes = tone_bytes(500, channels);
            let whole = pcm16_to_engine(&bytes, rate, channels);
            // Chunk sizes that split frames, samples, and nothing at all.
            for chunk in [1, 3, 7, 64, 333, bytes.len()] {
                let streamed = streamed(&bytes, rate, channels, chunk);
                assert_eq!(
                    streamed.len(),
                    whole.len(),
                    "{rate} Hz, {channels}ch, {chunk}-byte chunks"
                );
                for (index, (a, b)) in streamed.iter().zip(&whole).enumerate() {
                    assert!(
                        (a - b).abs() < 1e-6,
                        "sample {index} differs at {rate} Hz, {channels}ch, {chunk}-byte chunks: {a} vs {b}"
                    );
                }
            }
        }
    }

    /// A frame split across two chunks must be reassembled, not dropped — the
    /// failure this class of bug produces is a slow drift, not a crash.
    #[test]
    fn a_frame_straddling_a_chunk_boundary_survives() {
        let bytes = tone_bytes(8, 2);
        let mut stream = Pcm16Stream::new(48_000, 2);
        let mut out = stream.push(&bytes[..7]);
        out.extend(stream.push(&bytes[7..]));
        out.extend(stream.finish());
        assert_eq!(out, pcm16_to_engine(&bytes, 48_000, 2));
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_rather_than_kept() {
        let mut bytes = tone_bytes(4, 1);
        bytes.push(0);
        let mut stream = Pcm16Stream::new(48_000, 1);
        let mut out = stream.push(&bytes);
        out.extend(stream.finish());
        assert_eq!(out.len() / ENGINE_CHANNELS, 4);
    }

    #[test]
    fn a_stream_with_nonsense_parameters_yields_nothing() {
        let mut stream = Pcm16Stream::new(0, 1);
        assert!(stream.push(&[0, 1, 0, 1]).is_empty());
        assert!(stream.finish().is_empty());
        let mut stream = Pcm16Stream::new(24_000, 0);
        assert!(stream.push(&[0, 1, 0, 1]).is_empty());
        assert!(stream.finish().is_empty());
    }
}
