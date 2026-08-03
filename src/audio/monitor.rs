//! The monitoring output thread: plays whatever the mixer taps for monitoring
//! out of the default Windows playback device.
//!
//! This is the mirror image of [`crate::audio::capture`] — same supervisor
//! shape, same backoff, same cooperative stop flag — but running a WASAPI
//! *render* client instead of a capture one. It is the only place Pubsplash
//! opens an output device, and it is only opened while at least one mixer strip
//! is being monitored.
//!
//! The engine and the device are on independent clocks: the mixer is paced by
//! `sleep` on a 10 ms grid, the render client by WASAPI's event. The ring
//! buffer between them absorbs that difference. The engine drops samples when
//! the ring is full and this thread writes silence when it runs dry — neither
//! side ever blocks the other.

use crate::audio::device;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use rtrb::Consumer;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

/// How long to wait before the next attempt to open the playback device, after
/// `attempt` consecutive failures. Same shape as the capture backoff: quick at
/// first (a device that is a moment late to appear), then slow enough that a
/// device which is simply gone costs nothing to keep waiting for.
fn backoff(attempt: u32) -> Duration {
    const SCHEDULE_MS: [u64; 5] = [250, 500, 1_000, 2_000, 5_000];
    let index = (attempt as usize).min(SCHEDULE_MS.len() - 1);
    Duration::from_millis(SCHEDULE_MS[index])
}

/// Sleeps up to `total`, waking early once `stop` is set.
fn sleep_interruptibly(total: Duration, stop: &AtomicBool) {
    const SLICE: Duration = Duration::from_millis(50);
    let deadline = std::time::Instant::now() + total;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(SLICE));
    }
}

/// Spawns the monitoring output thread. It runs until `stop` is set, reopening
/// the device with a backoff whenever it fails, so unplugging the headphones
/// mid-session does not silently end monitoring.
///
/// Failures are logged only: monitoring is a convenience, and nothing about it
/// may disturb the stream.
pub fn spawn(consumer: Consumer<f32>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("audio-monitor".into())
        .spawn(move || {
            device::ensure_com_initialized();
            let mut consumer = consumer;
            let mut failures: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                match run(&mut consumer, &stop) {
                    Ok(()) => return,
                    Err(e) => {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        // Only the first failure of a run is news; the retries
                        // after it say the same thing every few seconds.
                        if failures == 0 {
                            log::error!("Monitoring output failed: {e}; retrying");
                        } else {
                            log::debug!("Monitoring output still failing: {e}");
                        }
                        sleep_interruptibly(backoff(failures), &stop);
                        failures = failures.saturating_add(1);
                    }
                }
            }
        })
        .expect("spawning monitor thread")
}

/// Opens the default playback device and feeds it until `stop` is set.
fn run(consumer: &mut Consumer<f32>, stop: &AtomicBool) -> Result<(), String> {
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
    log::debug!("Monitoring output is running");

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut guard = crate::audio::capture::StallGuard::default();
    while !stop.load(Ordering::Relaxed) {
        let frames = client
            .get_available_space_in_frames()
            .map_err(|e| format!("reading the available playback space: {e}"))?
            as usize;
        if frames > 0 {
            bytes.clear();
            bytes.reserve(frames * blockalign);
            fill(&mut bytes, consumer, frames * CHANNELS);
            render
                .write_to_device_from_deque(frames, &mut bytes, None)
                .map_err(|e| format!("writing to the playback device: {e}"))?;
        }
        // A timeout is normal when the device wants nothing yet, so the result
        // is not an error on its own. It used to be discarded outright, which
        // also discarded the case where the wait fails instantly and this loop
        // spins; `StallGuard` tells the two apart.
        guard.wait("playback", || {
            event.wait_for_event(crate::audio::capture::WAIT_MS).is_ok()
        })?;
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Appends `samples` samples' worth of little-endian f32 bytes to `bytes`,
/// taking what the ring has and padding the rest with silence. An underrun is
/// a gap in the monitor, never a stall in the mixer.
fn fill(bytes: &mut VecDeque<u8>, consumer: &mut Consumer<f32>, samples: usize) {
    // Read in one chunk rather than a `pop` per sample; see
    // `mixer::pull_block` for why that adds up.
    let take = consumer.slots().min(samples);
    let mut filled = 0;
    if take > 0
        && let Ok(chunk) = consumer.read_chunk(take)
    {
        let (first, second) = chunk.as_slices();
        for &sample in first.iter().chain(second.iter()) {
            bytes.extend(sample.to_le_bytes());
        }
        filled = first.len() + second.len();
        chunk.commit_all();
    }
    for _ in filled..samples {
        bytes.extend(0f32.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_ring_renders_silence() {
        let (_producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        let mut bytes = VecDeque::new();
        fill(&mut bytes, &mut consumer, 4);
        assert_eq!(bytes.len(), 16, "four f32 samples of silence");
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn a_partly_filled_ring_is_padded_with_silence() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        producer.push(1.0).unwrap();
        producer.push(-1.0).unwrap();
        let mut bytes = VecDeque::new();
        fill(&mut bytes, &mut consumer, 4);
        let out: Vec<u8> = bytes.into_iter().collect();
        let decoded: Vec<f32> = out
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(decoded, vec![1.0, -1.0, 0.0, 0.0]);
    }

    #[test]
    fn backoff_starts_quick_grows_and_settles() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        assert_eq!(backoff(99), Duration::from_secs(5));
    }
}
