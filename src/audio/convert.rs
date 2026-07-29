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
}
