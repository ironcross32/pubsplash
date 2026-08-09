//! Decoding and rate/channel conversion into the mixer's sample format.
//!
//! Everything the engine plays — sound-pack cues, synthesized speech — arrives
//! as some other format and has to land as interleaved stereo f32 at
//! [`ENGINE_SAMPLE_RATE`]. That conversion lives here so the sound pack tools
//! and the TTS engines share one implementation.
//!
//! This module deliberately depends on nothing from this crate: `soundpack.rs`
//! is `#[path]`-included into the standalone `soundpack` and
//! `pubsplash-soundpack` binaries, which have no `crate::audio`, so it pulls
//! this file in the same way. Adding a `crate::` reference here breaks both of
//! those builds. External crates are fine, and this file owns all of them that
//! have to do with codecs — [`decode_audio`] is the single door every encoded
//! byte comes through, whether it arrived from a speech API or a sound pack.

#![allow(dead_code)]

use hound::{SampleFormat, WavReader};
use ogg::{PacketReader, PacketWriteEndInfo, PacketWriter};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const ENGINE_SAMPLE_RATE: u32 = 48_000;
pub const ENGINE_CHANNELS: usize = 2;

/// Opus always decodes at 48 kHz whatever the source was, which is already the
/// engine's rate — the one format that never needs resampling.
const OPUS_RATE: u32 = 48_000;
/// 20 ms per packet, the usual choice: long enough to be efficient, short
/// enough that the zero padding on the final packet is inaudible.
const OPUS_FRAME_FRAMES: usize = 960;
/// The longest frame Opus can carry (120 ms), which is what a decode buffer has
/// to be able to hold when the file came from somewhere else.
const OPUS_MAX_FRAME_FRAMES: usize = 5760;
/// Comfortably above the 1275-byte ceiling for a single Opus frame.
const OPUS_MAX_PACKET_BYTES: usize = 4000;
/// The serial number of the one logical stream we ever write. A real muxer
/// randomizes this so streams can be interleaved; a file with a single stream
/// in it has nothing to collide with, and a constant keeps encodes byte-stable.
const OPUS_STREAM_SERIAL: u32 = 0x7075_6273;

/// Decodes any audio file we understand into interleaved stereo f32 at
/// [`ENGINE_SAMPLE_RATE`].
///
/// The format is taken from the bytes, not from the name: a speech API's
/// `Content-Type` is often wrong and a sound pack's file may have been renamed.
/// `extension_hint` only steers symphonia's probe when the caller happens to
/// know (Star reports one per utterance); an empty string is fine and leaves
/// the format to be sniffed.
///
/// WAV and Opus are dispatched by magic bytes rather than left to symphonia:
/// WAV because `hound` is already here and reads bit depths symphonia's RIFF
/// reader does not, and Opus because *no* released symphonia decodes it, 0.6
/// included.
pub fn decode_audio(bytes: &[u8], extension_hint: &str) -> Result<Vec<f32>, String> {
    if bytes.starts_with(b"RIFF") {
        return decode_wav(bytes);
    }
    if is_ogg_opus(bytes) {
        return decode_ogg_opus(bytes);
    }
    decode_with_symphonia(bytes, extension_hint)
}

/// Whether this is an Ogg stream whose first packet is an `OpusHead`.
///
/// The check has to reach past the page header rather than just look for
/// `OggS`, because Vorbis and FLAC live in Ogg too and those go to symphonia.
/// A first page carries the segment count at offset 26, and the packet data
/// begins right after that many lacing bytes.
fn is_ogg_opus(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"OggS") {
        return false;
    }
    let Some(&segments) = bytes.get(26) else {
        return false;
    };
    bytes
        .get(27 + segments as usize..)
        .is_some_and(|packet| packet.starts_with(b"OpusHead"))
}

/// Decodes an Ogg Opus file. Always 48 kHz, so nothing is resampled.
///
/// Two trims are not optional here, and getting either wrong is silent: the
/// encoder's lookahead is emitted as real samples the decoder must throw away
/// (`pre_skip`), and the final page's granule position is how the encoder says
/// where the audio truly ended, since the last packet is padded to a whole
/// frame. Skipping the first leaves a lead-in of encoder ramp-up; skipping the
/// second appends up to 20 ms of silence to every sound.
pub fn decode_ogg_opus(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut reader = PacketReader::new(std::io::Cursor::new(bytes));
    let head = reader
        .read_packet()
        .map_err(|e| format!("reading the Opus header: {e}"))?
        .ok_or("this Opus file is empty")?;
    // Magic (8) + version + channels + pre-skip (2) + input rate (4) +
    // output gain (2) + mapping family.
    if head.data.len() < 19 || !head.data.starts_with(b"OpusHead") {
        return Err("this file does not start with an Opus header".into());
    }
    let channels = usize::from(head.data[9]);
    let pre_skip = usize::from(u16::from_le_bytes([head.data[10], head.data[11]]));
    let layout = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        other => {
            return Err(format!(
                "Opus files with {other} channels are not supported; use mono or stereo"
            ));
        }
    };

    let mut decoder =
        opus::Decoder::new(OPUS_RATE, layout).map_err(|e| format!("starting Opus decoding: {e}"))?;
    let mut samples: Vec<f32> = Vec::new();
    let mut frame = vec![0f32; OPUS_MAX_FRAME_FRAMES * channels];
    let mut end_granule = None;
    while let Some(packet) = reader
        .read_packet()
        .map_err(|e| format!("reading Opus audio: {e}"))?
    {
        // The comment header sits between the identification header and the
        // audio, and is not something the codec can decode.
        if packet.data.starts_with(b"OpusTags") {
            continue;
        }
        let decoded = decoder
            .decode_float(&packet.data, &mut frame, false)
            .map_err(|e| format!("decoding Opus audio: {e}"))?;
        samples.extend_from_slice(&frame[..decoded * channels]);
        if packet.last_in_stream() {
            end_granule = Some(packet.absgp_page());
        }
    }

    let decoded_frames = samples.len() / channels;
    let start = pre_skip.min(decoded_frames);
    // The granule position counts from before the pre-skip, so it is directly
    // comparable with `decoded_frames`. A file whose granule is nonsense (past
    // what we decoded, or before the pre-skip) keeps everything rather than
    // being truncated to nothing.
    let end = match end_granule {
        Some(granule) => (granule as usize).clamp(start, decoded_frames),
        None => decoded_frames,
    };
    convert_to_stereo(&samples[start * channels..end * channels], channels)
}

/// Encodes interleaved stereo f32 at [`ENGINE_SAMPLE_RATE`] as an Ogg Opus
/// file at roughly `bitrate_kbps`.
///
/// The last packet is zero-padded to a whole 20 ms frame — Opus has no shorter
/// unit — and the padding is undone by the granule position written on the
/// final page, which is the true sample count. That is the same mechanism
/// [`decode_ogg_opus`] honours on the way back in.
pub fn encode_ogg_opus(samples: &[f32], bitrate_kbps: u32) -> Result<Vec<u8>, String> {
    let total_frames = samples.len() / ENGINE_CHANNELS;
    if total_frames == 0 {
        return Err("there is no audio to encode".into());
    }

    let mut encoder = opus::Encoder::new(
        ENGINE_SAMPLE_RATE,
        opus::Channels::Stereo,
        opus::Application::Audio,
    )
    .map_err(|e| format!("starting Opus encoding: {e}"))?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(bitrate_kbps as i32 * 1000))
        .map_err(|e| format!("setting the Opus bitrate: {e}"))?;
    // The encoder's own delay, which the decoder is told to discard.
    let pre_skip = encoder
        .get_lookahead()
        .map_err(|e| format!("reading the Opus encoder delay: {e}"))?
        .max(0) as u64;

    let mut writer = PacketWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .write_packet(
            opus_head(pre_skip as u16),
            OPUS_STREAM_SERIAL,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(|e| format!("writing the Opus header: {e}"))?;
    writer
        .write_packet(
            opus_tags(),
            OPUS_STREAM_SERIAL,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(|e| format!("writing the Opus tags: {e}"))?;

    let frame_samples = OPUS_FRAME_FRAMES * ENGINE_CHANNELS;
    let body = &samples[..total_frames * ENGINE_CHANNELS];
    // Enough packets to cover the audio *plus* the encoder's own delay, then
    // rounded up to a whole packet. Without the `pre_skip` term the last
    // `pre_skip` samples never leave the encoder, and since the decoder throws
    // away exactly that many at the front, the file comes back short by the
    // lookahead — visible only when the audio happens to be a whole number of
    // packets long, because otherwise the padding of the final short packet
    // covers for it.
    let packets = (total_frames + pre_skip as usize).div_ceil(OPUS_FRAME_FRAMES);
    let mut frame = vec![0f32; frame_samples];
    for packet_index in 0..packets {
        let start = packet_index * frame_samples;
        let available = body.len().saturating_sub(start).min(frame_samples);
        frame[..available].copy_from_slice(&body[start..start + available]);
        frame[available..].fill(0.0);
        let packet = encoder
            .encode_vec_float(&frame, OPUS_MAX_PACKET_BYTES)
            .map_err(|e| format!("encoding Opus audio: {e}"))?;
        let last = packet_index + 1 == packets;
        let (end, absgp) = if last {
            // The real end, so the decoder drops the padding above.
            (PacketWriteEndInfo::EndStream, pre_skip + total_frames as u64)
        } else {
            (
                PacketWriteEndInfo::NormalPacket,
                pre_skip + ((packet_index + 1) * OPUS_FRAME_FRAMES) as u64,
            )
        };
        writer
            .write_packet(packet, OPUS_STREAM_SERIAL, end, absgp)
            .map_err(|e| format!("writing Opus audio: {e}"))?;
    }

    Ok(writer.into_inner().into_inner())
}

/// The Opus identification header (RFC 7845 §5.1), 19 bytes for the stereo
/// mapping family 0 we always write.
fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(ENGINE_CHANNELS as u8);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&ENGINE_SAMPLE_RATE.to_le_bytes()); // original rate, informational
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family
    head
}

/// The Opus comment header (RFC 7845 §5.2). Required to be present; we have
/// nothing to say in it beyond who wrote the file.
fn opus_tags() -> Vec<u8> {
    let vendor = concat!("Pubsplash ", env!("CARGO_PKG_VERSION")).as_bytes();
    let mut tags = Vec::with_capacity(20 + vendor.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // comment count
    tags
}

/// Everything else: MP3, FLAC, Ogg Vorbis, and whatever a probe recognizes.
fn decode_with_symphonia(bytes: &[u8], extension: &str) -> Result<Vec<f32>, String> {
    let stream = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        Default::default(),
    );
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
        .ok_or_else(|| "this file contains no audio track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut source_rate = track.codec_params.sample_rate.unwrap_or(ENGINE_SAMPLE_RATE);
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
    Ok(if source_rate == ENGINE_SAMPLE_RATE {
        stereo
    } else {
        resample_stereo(&stereo, source_rate, ENGINE_SAMPLE_RATE)
    })
}

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
    if source_channels == 0 || !samples.len().is_multiple_of(source_channels) {
        return Err("the audio data ended in the middle of a frame".into());
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

    /// A stereo tone at the engine rate, as the encoder wants it.
    fn engine_tone(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let phase =
                    frame as f32 / ENGINE_SAMPLE_RATE as f32 * 440.0 * std::f32::consts::TAU;
                let sample = phase.sin() * 0.5;
                [sample, sample]
            })
            .collect()
    }

    /// The round trip has to come back the same *length*, which is the whole
    /// point of the pre-skip and granule-position bookkeeping: a decoder that
    /// ignores the pre-skip returns extra lead-in, and one that ignores the
    /// final granule returns up to 20 ms of padding silence. Both are silent
    /// bugs that only show up as drift once cues are layered.
    #[test]
    fn opus_round_trip_preserves_length_and_waveform() {
        // Both cases, because they fail differently: a length that is not a
        // whole number of 20 ms packets exercises the padding and trimming of
        // the final packet, while one that *is* exposes a missing allowance for
        // the encoder's lookahead — the short-packet padding hides that.
        for frames in [ENGINE_SAMPLE_RATE as usize / 4 + 137, OPUS_FRAME_FRAMES * 5] {
            opus_round_trips_at_length(frames);
        }
        // A sound shorter than a single packet is still a sound.
        let tiny = encode_ogg_opus(&engine_tone(1), 96).unwrap();
        assert_eq!(decode_ogg_opus(&tiny).unwrap().len(), ENGINE_CHANNELS);
    }

    fn opus_round_trips_at_length(frames: usize) {
        let samples = engine_tone(frames);

        let encoded = encode_ogg_opus(&samples, 96).unwrap();
        assert!(encoded.starts_with(b"OggS"), "not an Ogg stream");
        assert!(is_ogg_opus(&encoded), "not recognized as Ogg Opus");

        let decoded = decode_ogg_opus(&encoded).unwrap();
        assert_eq!(
            decoded.len() / ENGINE_CHANNELS,
            frames,
            "round trip changed the length at {frames} frames"
        );

        // Opus is lossy, so compare energy rather than samples: a shifted or
        // truncated decode shows up as a collapsed correlation, while ordinary
        // coding noise does not.
        let peak = decoded.iter().fold(0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.3 && peak <= 1.0, "unexpected peak {peak}");
        let correlation: f32 = samples
            .iter()
            .zip(&decoded)
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / samples.iter().map(|s| s * s).sum::<f32>();
        assert!(
            correlation > 0.8,
            "decoded audio does not line up with the original ({correlation})"
        );
    }

    #[test]
    fn opus_encoding_rejects_empty_input() {
        assert!(encode_ogg_opus(&[], 96).is_err());
    }

    /// Opus at 96 kbps should be a fraction of the size of the 16-bit WAV it
    /// came from -- the reason packs may carry it at all.
    #[test]
    fn opus_is_much_smaller_than_the_equivalent_wav() {
        let frames = ENGINE_SAMPLE_RATE as usize; // one second
        let encoded = encode_ogg_opus(&engine_tone(frames), 96).unwrap();
        let wav_bytes = frames * ENGINE_CHANNELS * 2;
        assert!(
            encoded.len() * 4 < wav_bytes,
            "{} bytes is not much smaller than {wav_bytes}",
            encoded.len()
        );
    }

    #[test]
    fn decode_audio_dispatches_on_the_bytes_not_the_name() {
        // WAV, announced as something else entirely.
        let mut wav = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: ENGINE_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            };
            let mut writer =
                hound::WavWriter::new(std::io::Cursor::new(&mut wav), spec).unwrap();
            for _ in 0..100 {
                writer.write_sample(1000i16).unwrap();
            }
            writer.finalize().unwrap();
        }
        assert_eq!(
            decode_audio(&wav, "opus").unwrap().len(),
            100 * ENGINE_CHANNELS
        );

        // Opus, with no hint at all.
        let opus = encode_ogg_opus(&engine_tone(1000), 96).unwrap();
        assert_eq!(
            decode_audio(&opus, "").unwrap().len(),
            1000 * ENGINE_CHANNELS
        );

        // Garbage is an error, not a panic.
        assert!(decode_audio(&[0xde, 0xad, 0xbe, 0xef], "mp3").is_err());
        assert!(decode_audio(&[], "").is_err());
    }

    /// Ogg is not only Opus: a Vorbis or FLAC stream in the same container has
    /// to fall through to symphonia rather than be handed to the Opus decoder.
    #[test]
    fn ogg_that_is_not_opus_is_not_claimed_by_the_opus_path() {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.extend_from_slice(&[0; 22]);
        page.push(1); // one segment
        page.push(30); // its length
        page.push(1);
        page.extend_from_slice(b"vorbis");
        assert!(!is_ogg_opus(&page));
    }

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
