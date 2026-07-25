//! Capture threads: each audio source that reads from the OS runs one of
//! these, producing interleaved stereo f32 at 48 kHz into a ring buffer the
//! mixer drains.

use crate::audio::device;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use rtrb::Producer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureKind {
    Microphone { device_id: Option<String> },
    DesktopAudio,
    Application { pid: u32 },
}

/// What a capture thread reports about itself, whenever the answer changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureState {
    /// The device opened and audio is flowing.
    Running,
    /// The device could not be opened, or stopped working. The thread is
    /// retrying; the string says which step failed.
    Failed(String),
}

/// One report from a capture thread. `epoch` is the source-set generation the
/// thread was spawned for: threads outlive their `SetSources` (they retire
/// asynchronously, and a retrying one may be sleeping), so the engine uses this
/// to drop reports from threads that no longer own their source's name.
#[derive(Debug, Clone)]
pub struct CaptureReport {
    pub name: String,
    pub epoch: u64,
    pub state: CaptureState,
}

/// How long to wait before the next attempt to open a device, after `attempt`
/// consecutive failures. Quick at first, since the common case is an interface
/// that is a few hundred milliseconds late to enumerate at launch, then slow
/// enough that a device which is simply gone costs nothing to keep waiting for.
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

/// Spawns a capture thread. It runs until `stop` is set, reopening the device
/// with a backoff whenever it fails, so a source that was not ready at launch
/// (or that is unplugged mid-session) recovers on its own. Returns the join
/// handle; state changes are logged and reported through `on_state`.
pub fn spawn(
    name: String,
    epoch: u64,
    kind: CaptureKind,
    mut producer: Producer<f32>,
    stop: Arc<AtomicBool>,
    on_state: crossbeam_channel::Sender<CaptureReport>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("capture-{name}"))
        .spawn(move || {
            device::ensure_com_initialized();
            let report = |state: CaptureState| {
                let _ = on_state.send(CaptureReport {
                    name: name.clone(),
                    epoch,
                    state,
                });
            };
            // Counts consecutive failures, so a device that flaps does not
            // flood the log and a device that recovers gets a fast retry again.
            let mut failures: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                let opened = std::cell::Cell::new(false);
                // Coming back from a failure is worth a line; opening normally
                // at launch is not.
                let recovering = failures > 0;
                let started = || {
                    opened.set(true);
                    if recovering {
                        log::info!("Capture source {name:?} is running again");
                    } else {
                        log::debug!("Capture source {name:?} is running");
                    }
                    report(CaptureState::Running);
                };
                let outcome = run(&kind, &mut producer, &stop, started);
                // A run that got as far as producing audio starts the backoff
                // over, so a device lost mid-session is retried as promptly as
                // one that was late to appear at launch — and its next failure
                // is news again.
                if opened.get() {
                    failures = 0;
                }
                match outcome {
                    // The stop flag was set: this source is being retired.
                    Ok(()) => return,
                    Err(e) => {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        // Only the first failure of a run is news; the retries
                        // after it say the same thing every few seconds.
                        if failures == 0 {
                            log::error!("Capture source {name:?} failed: {e}; retrying");
                            report(CaptureState::Failed(e));
                        } else {
                            log::debug!("Capture source {name:?} still failing: {e}");
                        }
                        sleep_interruptibly(backoff(failures), &stop);
                        failures = failures.saturating_add(1);
                    }
                }
            }
        })
        .expect("spawning capture thread")
}

/// Opens the device and pumps it until `stop` is set. `started` is called once
/// audio is actually flowing, which is also what resets the retry backoff.
fn run(
    kind: &CaptureKind,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
    started: impl FnOnce(),
) -> Result<(), String> {
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );

    let mut client = match kind {
        CaptureKind::Microphone { device_id } => device::capture_device(device_id.as_deref())?
            .get_iaudioclient()
            .map_err(|e| format!("activating the microphone's audio client: {e}"))?,
        // Desktop Audio is process-exclusion loopback with our own process
        // as the excluded tree: all system audio except Pubsplash itself.
        // This keeps locally played TTS and sound cues out of the capture,
        // so they can never feed back into the stream.
        CaptureKind::DesktopAudio => {
            AudioClient::new_application_loopback_client(std::process::id(), false)
                .map_err(|e| format!("opening the desktop audio loopback client: {e}"))?
        }
        CaptureKind::Application { pid } => {
            AudioClient::new_application_loopback_client(*pid, true)
                .map_err(|e| format!("opening the loopback client for process {pid}: {e}"))?
        }
    };

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|e| format!("initializing the capture stream: {e}"))?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("setting up the capture event: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("getting the capture client: {e}"))?;
    let blockalign = format.get_blockalign() as usize;

    client
        .start_stream()
        .map_err(|e| format!("starting the capture stream: {e}"))?;
    started();

    let mut byte_queue: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        let new_frames = capture
            .get_next_packet_size()
            .map_err(|e| format!("reading the next packet size: {e}"))?
            .unwrap_or(0);
        if new_frames > 0 {
            byte_queue.reserve(new_frames as usize * blockalign);
            capture
                .read_from_device_to_deque(&mut byte_queue)
                .map_err(|e| format!("reading from the device: {e}"))?;
            push_f32(&mut byte_queue, producer);
        }
        // Short enough that a retiring thread releases the endpoint promptly:
        // `stop` is only checked at the top of this loop, and a replacement
        // thread is spawned for the same device the instant `SetSources`
        // lands, so a long wait here means the two overlap on the device.
        if event.wait_for_event(200).is_err() {
            // Timeouts are normal for loopback with no audio playing; only
            // treat it as fatal if the client stopped.
            continue;
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Moves whole f32 samples from the byte queue into the ring, dropping
/// samples when the ring is full (mixer stalled or source unattached).
fn push_f32(bytes: &mut std::collections::VecDeque<u8>, producer: &mut Producer<f32>) {
    while bytes.len() >= 4 {
        let sample = f32::from_le_bytes([
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
        ]);
        // Full ring: discard. Capture must never block.
        let _ = producer.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first retry has to be quick — the bug this exists for is a USB
    /// interface a few hundred milliseconds late to enumerate at launch — and
    /// the last one has to be slow enough to wait out a device that is simply
    /// gone without costing anything.
    #[test]
    fn backoff_starts_quick_grows_and_settles() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        let delays: Vec<Duration> = (0..8).map(backoff).collect();
        for pair in delays.windows(2) {
            assert!(pair[1] >= pair[0], "backoff went backwards: {delays:?}");
        }
        assert_eq!(*delays.last().unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn a_stopped_source_does_not_wait_out_its_backoff() {
        let stop = AtomicBool::new(true);
        let start = std::time::Instant::now();
        sleep_interruptibly(Duration::from_secs(5), &stop);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    /// The whole point of the supervisor: a device that will not open must not
    /// kill the source. It reports the failure once, keeps retrying in the
    /// background, and still shuts down promptly when the source is retired.
    /// A device id that cannot exist stands in for the real case (a USB
    /// interface Windows has not finished bringing up).
    #[test]
    fn a_device_that_will_not_open_is_reported_once_and_retried() {
        let (producer, _consumer) = rtrb::RingBuffer::<f32>::new(64);
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn(
            "Microphone".to_string(),
            7,
            CaptureKind::Microphone {
                device_id: Some("no-such-endpoint".to_string()),
            },
            producer,
            stop.clone(),
            tx,
        );

        let report = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a failure report");
        assert_eq!(report.name, "Microphone");
        assert_eq!(report.epoch, 7, "the spawn epoch is carried through");
        let CaptureState::Failed(message) = report.state else {
            panic!("expected a failure, got {:?}", report.state);
        };
        assert!(
            message.contains("looking up the configured microphone"),
            "the message should name the step that failed: {message}"
        );

        // Repeats are the caller's to filter, but the thread must still be
        // alive and trying rather than gone.
        assert!(!handle.is_finished());
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !handle.is_finished(),
            "the source gave up instead of retrying"
        );

        stop.store(true, Ordering::Relaxed);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(handle.is_finished(), "retiring the source did not stop it");
        handle.join().expect("capture thread panicked");
    }

    #[test]
    fn an_uninterrupted_backoff_sleeps_its_full_span() {
        let stop = AtomicBool::new(false);
        let start = std::time::Instant::now();
        sleep_interruptibly(Duration::from_millis(150), &stop);
        assert!(start.elapsed() >= Duration::from_millis(150));
    }
}
