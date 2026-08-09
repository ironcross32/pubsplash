//! Local cue playback for sound-pack sounds.
//!
//! These cues are local feedback only. They go straight to the default Windows
//! render device and never enter the mixer, stream encoder, or recorder - which
//! is what the interface startup/shutdown sounds want, and also how a Sound
//! Events source gets heard by the broadcaster, whether or not the same cue is
//! also being fed to the stream.
//!
//! The device and the render loop itself live in [`crate::audio::render`],
//! which the standalone Sound Pack Manager shares by path; what stays here is
//! everything that needs the rest of the crate — looking a cue up in the active
//! pack, and the threads a UI starts one on.

use crate::audio::render::play_samples_until;
use crate::soundpack::SoundKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    // is not a burst of decodes and resamples.
    let samples = pack
        .random_decoded(kind)
        .ok_or_else(|| format!("the active sound pack has no {} cue", kind.label()))?;
    play_samples(&samples)
}

fn play_samples(samples: &[f32]) -> Result<(), String> {
    crate::audio::render::play_samples(samples)
}
