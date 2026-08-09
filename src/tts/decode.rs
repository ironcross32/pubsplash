//! Decoding for the two engines that don't offer PCM.
//!
//! Every other speech API is asked for raw PCM or WAV at a rate we choose, so
//! only two callers reach this far: Google Translate serves MP3 and nothing
//! else, and a Star coagulator returns whatever its voice produced, naming the
//! format in the frame rather than sticking to one.
//!
//! The decoding itself is `audio::convert::decode_audio`, shared with the
//! sound-pack side. All this layer adds is the error type, so a failure says
//! which service produced the bytes.

use crate::audio::convert::decode_audio;
use crate::tts::engine::TtsError;

/// Decodes compressed audio to interleaved stereo f32 at the engine's rate.
///
/// `extension` steers the probe when the server told us what it sent; an empty
/// string leaves the format to be sniffed from the bytes.
pub fn decode_compressed(
    service: &'static str,
    bytes: Vec<u8>,
    extension: &str,
) -> Result<Vec<f32>, TtsError> {
    decode_audio(&bytes, extension).map_err(|e| TtsError::Decode(service, e))
}

/// Frames of engine-format audio in `samples`.
#[cfg(test)]
fn frames(samples: &[f32]) -> usize {
    samples.len() / crate::audio::convert::ENGINE_CHANNELS
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{SampleFormat, WavSpec, WavWriter};

    fn tone_wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
        let spec = WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut bytes = Vec::new();
        {
            let mut writer = WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
            for frame in 0..frames {
                let phase = frame as f32 / rate as f32 * 440.0 * std::f32::consts::TAU;
                for _ in 0..channels {
                    writer.write_sample((phase.sin() * 16384.0) as i16).unwrap();
                }
            }
            writer.finalize().unwrap();
        }
        bytes
    }

    /// WAV must short-circuit past symphonia entirely.
    #[test]
    fn riff_payloads_go_through_hound_and_land_at_the_engine_rate() {
        let bytes = tone_wav(24_000, 1, 24_000 / 4);
        let samples = decode_compressed("test", bytes, "wav").unwrap();
        assert_eq!(frames(&samples), 48_000 / 4);
    }

    #[test]
    fn undecodable_payloads_name_the_service() {
        let error = decode_compressed("Star", vec![0xde, 0xad, 0xbe, 0xef], "mp3").unwrap_err();
        assert!(matches!(error, TtsError::Decode("Star", _)), "{error}");
    }
}
