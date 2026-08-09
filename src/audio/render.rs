//! Playing a buffer of engine-format samples out of the default Windows render
//! device, with nothing else attached.
//!
//! This is the bottom half of `cue.rs`, split out for the same reason
//! `convert.rs` was: the standalone Sound Pack Manager has to preview the file
//! an author just picked, and its preview used to be a PowerShell
//! `System.Media.SoundPlayer` shell-out, which plays WAV and nothing else — no
//! use at all once a pack may hold Opus. So this file is `#[path]`-included
//! there, and like `convert.rs` it must name nothing from this crate: no
//! `crate::`, no `super::`. That is why the two constants below are spelled out
//! rather than imported from `convert`, and why the device is opened here
//! instead of through `audio::device`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wasapi::{Direction, DeviceEnumerator, SampleType, StreamMode, WaveFormat};

/// Must match `convert::ENGINE_SAMPLE_RATE`; every buffer reaching here has
/// already been converted to it.
const SAMPLE_RATE: u32 = 48_000;
/// Must match `convert::ENGINE_CHANNELS`.
const CHANNELS: usize = 2;

/// How long to keep the device open after the last sample was handed over, so
/// the tail of a cue is not cut off by the stream closing under it.
const DRAIN_AFTER_CUE: Duration = Duration::from_millis(100);

/// The default render device. Kept here rather than shared with
/// `audio::device` because this file cannot name the crate it lives in.
fn default_render_device() -> Result<wasapi::Device, String> {
    let _ = wasapi::initialize_mta();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| format!("finding the default playback device: {e}"))
}

/// Plays `samples` to the end, blocking until it has drained.
pub fn play_samples(samples: &[f32]) -> Result<(), String> {
    play_samples_until(samples, &AtomicBool::new(false))
}

/// The same, but stoppable: `stop` is read at the top of every render block.
///
/// A stop deliberately skips the [`DRAIN_AFTER_CUE`] wait. That wait exists so
/// a cue that reached its end is not cut off by the device closing under it,
/// and a stop is the user asking for exactly that cut-off.
pub fn play_samples_until(samples: &[f32], stop: &AtomicBool) -> Result<(), String> {
    // Checked before the device is opened, not only in the loop: a playback
    // stopped this early should cost nothing, which is also what makes the
    // behaviour testable without an audio device.
    if samples.is_empty() || stop.load(Ordering::Relaxed) {
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

    let mut client = default_render_device()?
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
        if stop.load(Ordering::Relaxed) {
            break;
        }
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

    /// An already-stopped playback must return before it touches WASAPI, which
    /// is both the point of the early check and the only reason this test can
    /// run on a machine with no audio device.
    #[test]
    fn an_already_stopped_playback_opens_no_device() {
        let stop = AtomicBool::new(true);

        assert!(play_samples_until(&[0.5, 0.5, -0.5, -0.5], &stop).is_ok());
    }
}
