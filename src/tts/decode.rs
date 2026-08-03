//! Decoding for the two engines that don't offer PCM.
//!
//! Every other speech API is asked for raw PCM or WAV at a rate we choose, so
//! this path is deliberately narrow: Google Translate serves MP3 and nothing
//! else, and a Star coagulator returns whatever its voice produced.

use crate::audio::convert::{convert_to_stereo, decode_wav, resample_stereo};
use crate::audio::mixer::SAMPLE_RATE;
use crate::tts::engine::TtsError;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decodes compressed audio to interleaved stereo f32 at [`SAMPLE_RATE`].
///
/// `extension` steers the probe when the server told us what it sent; an empty
/// string leaves the format to be sniffed from the bytes.
pub fn decode_compressed(
    service: &'static str,
    bytes: Vec<u8>,
    extension: &str,
) -> Result<Vec<f32>, TtsError> {
    // A RIFF header needs no codec at all, and `hound` is already here.
    if bytes.starts_with(b"RIFF") {
        return decode_wav(&bytes).map_err(|e| TtsError::Decode(service, e));
    }
    decode_with_symphonia(bytes, extension).map_err(|e| TtsError::Decode(service, e))
}

fn decode_with_symphonia(bytes: Vec<u8>, extension: &str) -> Result<Vec<f32>, String> {
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| "the response contains no audio track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut source_rate = track.codec_params.sample_rate.unwrap_or(SAMPLE_RATE);
    let mut source_channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);
    let mut buffer: Option<SampleBuffer<f32>> = None;

    // Any error from `next_packet` is end-of-stream in practice: symphonia
    // reports a clean end as an IO error, and a torn tail is still worth
    // playing, so the loop ends rather than failing.
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else {
            continue;
        };
        let spec = *decoded.spec();
        source_rate = spec.rate;
        source_channels = spec.channels.count().max(1);
        let buffer =
            buffer.get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        buffer.copy_interleaved_ref(decoded);
        interleaved.extend_from_slice(buffer.samples());
    }

    if interleaved.is_empty() {
        return Err("no audio was decoded".into());
    }

    // A torn final frame would fail the stereo fold; drop it.
    let usable = interleaved.len() - interleaved.len() % source_channels;
    let stereo = convert_to_stereo(&interleaved[..usable], source_channels)?;
    Ok(if source_rate == SAMPLE_RATE {
        stereo
    } else {
        resample_stereo(&stereo, source_rate, SAMPLE_RATE)
    })
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
