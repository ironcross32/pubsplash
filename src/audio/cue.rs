//! Local cue playback for sound-pack sounds.
//!
//! These cues are local feedback only. They go straight to the default Windows
//! render device and never enter the mixer, stream encoder, or recorder - which
//! is what the interface startup/shutdown sounds want, and also how a Sound
//! Events source gets heard by the broadcaster, whether or not the same cue is
//! also being fed to the stream.

use crate::audio::device;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use crate::soundpack::SoundKind;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

const DRAIN_AFTER_CUE: Duration = Duration::from_millis(100);

/// Plays an already-decoded buffer. Callers that also feed the same samples to
/// the mixer use this rather than [`play_sound_kind_blocking`] so that both
/// copies are the same randomly chosen variant.
pub fn play_samples_async(samples: std::sync::Arc<Vec<f32>>) {
    std::thread::Builder::new()
        .name("ui-sound-cue".into())
        .spawn(move || {
            if let Err(e) = play_samples(&samples) {
                log::warn!("Could not play sound cue: {e}");
            }
        })
        .ok();
}

/// A cue playing on its own thread that the caller can stop and poll.
///
/// Cloning it is cloning the reference: every clone refers to the same
/// playback. The sound-pack preview dialog needs both halves — a Play/Stop
/// button has to interrupt a cue on demand, and it has to know when one ended
/// on its own so it can go back to reading "Play".
#[derive(Clone)]
pub struct CuePlayback {
    stop: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
}

impl CuePlayback {
    /// Asks the playback thread to stop. It exits at the top of its next
    /// block, so this returns long before the device is closed.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Whether both handles refer to the same playback. Identity, not equality:
    /// a caller that started a second cue needs to know whether the one that
    /// just ended is still the one it is tracking.
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.playing, &other.playing)
    }
}

/// Like [`play_samples_async`], but hands back a handle to the playback.
///
/// `playing` is cleared on every exit from the thread, the error path
/// included, so a device that will not open still returns a Play/Stop button
/// to "Play" rather than leaving it stuck on "Stop".
pub fn play_samples_handle(samples: Arc<Vec<f32>>) -> CuePlayback {
    let handle = CuePlayback {
        stop: Arc::new(AtomicBool::new(false)),
        playing: Arc::new(AtomicBool::new(true)),
    };
    let thread_handle = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("ui-sound-cue".into())
        .spawn(move || {
            if let Err(e) = play_samples_until(&samples, &thread_handle.stop) {
                log::warn!("Could not play sound cue: {e}");
            }
            thread_handle.playing.store(false, Ordering::Relaxed);
        });
    if spawned.is_err() {
        // Nothing will ever clear the flag, so clear it here; the caller polls
        // `is_playing` and would otherwise wait forever.
        handle.playing.store(false, Ordering::Relaxed);
    }
    handle
}

pub fn play_sound_kind_blocking(kind: SoundKind) -> Result<(), String> {
    let pack = crate::soundpack::active()
        .ok_or_else(|| "the built-in sound pack could not be loaded".to_string())?;
    // Decoded once per variant and remembered on the pack, so a burst of cues
    // is not a burst of WAV parses and resamples.
    let samples = pack
        .random_decoded(kind)
        .ok_or_else(|| format!("the active sound pack has no {} cue", kind.label()))?;
    play_samples(&samples)
}

fn play_samples(samples: &[f32]) -> Result<(), String> {
    play_samples_until(samples, &AtomicBool::new(false))
}

/// The same, but stoppable: `stop` is read at the top of every render block.
///
/// A stop deliberately skips the `DRAIN_AFTER_CUE` wait. That wait exists so a
/// cue that reached its end is not cut off by the device closing under it, and
/// a stop is the user asking for exactly that cut-off.
fn play_samples_until(samples: &[f32], stop: &AtomicBool) -> Result<(), String> {
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
