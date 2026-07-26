//! Local cue playback for sound-pack sounds.
//!
//! These cues are local feedback only. They go straight to the default Windows
//! render device and never enter the mixer, stream encoder, or recorder - which
//! is what the interface startup/shutdown sounds want, and also what a Sound
//! Events source wants when it is not sending its cues to the stream.

use crate::audio::device;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use crate::soundpack::SoundKind;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

const DRAIN_AFTER_CUE: Duration = Duration::from_millis(100);

pub fn play_sound_kind_async(kind: SoundKind) {
    std::thread::Builder::new()
        .name("ui-sound-cue".into())
        .spawn(move || {
            if let Err(e) = play_sound_kind_blocking(kind) {
                log::warn!("Could not play {:?} sound cue: {e}", kind);
            }
        })
        .ok();
}

pub fn play_sound_kind_blocking(kind: SoundKind) -> Result<(), String> {
    let pack = crate::soundpack::embedded_default()
        .ok_or_else(|| "the built-in sound pack could not be loaded".to_string())?;
    // Decoded once per variant and remembered on the pack, so a burst of cues
    // is not a burst of WAV parses and resamples.
    let samples = pack
        .random_decoded(kind)
        .ok_or_else(|| format!("embedded default sound pack has no {} cue", kind.label()))?;
    play_samples(&samples)
}

fn play_samples(samples: &[f32]) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }

    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );

    let mut client = device::default_render_device()?
        .get_iaudioclient()
        .map_err(|e| format!("activating the playback device's audio client: {e}"))?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(&format, &Direction::Render, &mode)
        .map_err(|e| format!("initializing the playback stream: {e}"))?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("setting up the playback event: {e}"))?;
    let render = client
        .get_audiorenderclient()
        .map_err(|e| format!("getting the render client: {e}"))?;
    let blockalign = format.get_blockalign() as usize;

    client
        .start_stream()
        .map_err(|e| format!("starting the playback stream: {e}"))?;

    let mut offset = 0;
    let mut bytes = VecDeque::new();
    let mut finished_writing_at: Option<Instant> = None;

    loop {
        if finished_writing_at.is_some_and(|finished| finished.elapsed() >= DRAIN_AFTER_CUE) {
            break;
        }

        let frames = client
            .get_available_space_in_frames()
            .map_err(|e| format!("reading the available playback space: {e}"))?
            as usize;
        if frames > 0 {
            bytes.clear();
            bytes.reserve(frames * blockalign);
            append_frames(&mut bytes, samples, &mut offset, frames);
            render
                .write_to_device_from_deque(frames, &mut bytes, None)
                .map_err(|e| format!("writing to the playback device: {e}"))?;
            if offset >= samples.len() && finished_writing_at.is_none() {
                finished_writing_at = Some(Instant::now());
            }
        }
        let _ = event.wait_for_event(200);
    }

    let _ = client.stop_stream();
    Ok(())
}

fn append_frames(bytes: &mut VecDeque<u8>, samples: &[f32], offset: &mut usize, frames: usize) {
    for _ in 0..frames * CHANNELS {
        let sample = samples
            .get(*offset)
            .copied()
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0);
        if *offset < samples.len() {
            *offset += 1;
        }
        bytes.extend(sample.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_frames_clamps_and_pads() {
        let mut bytes = VecDeque::new();
        let mut offset = 0;

        append_frames(&mut bytes, &[2.0, -2.0], &mut offset, 2);

        let out: Vec<f32> = bytes
            .into_iter()
            .collect::<Vec<_>>()
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(out, vec![1.0, -1.0, 0.0, 0.0]);
        assert_eq!(offset, 2);
    }
}
