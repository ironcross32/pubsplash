//! Capture threads: each audio source that reads from the OS runs one of
//! these, producing interleaved stereo f32 at 48 kHz into a ring buffer the
//! mixer drains.

use crate::audio::device;
use crate::audio::health::{CaptureStats, DeviceTimeline};
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use rtrb::Producer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
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

/// How long the device event is waited on per loop turn, in milliseconds. Short
/// enough that a retiring thread releases the endpoint promptly — `stop` is only
/// checked at the top of the loop, and a replacement thread is spawned for the
/// same device the instant `SetSources` lands, so a long wait here means the two
/// overlap on the device.
pub const WAIT_MS: u32 = 200;

/// A failed wait that returned this quickly did not wait at all.
const IMMEDIATE: Duration = Duration::from_millis(50);

/// How many immediate failures in a row before the wait is declared broken.
/// Twenty is about a second of spinning — long enough that a burst of scheduler
/// noise cannot reach it, short enough that the reopen happens while the user is
/// still wondering why the source went quiet.
const STALL_LIMIT: u32 = 20;

/// Tracks a wait that keeps failing without waiting.
///
/// `wasapi 0.23`'s `Handle::wait_for_event` maps *every* non-signalled return to
/// the same `WasapiError::EventTimeout` — `WAIT_TIMEOUT` and `WAIT_FAILED`
/// alike — and does not expose the raw handle, so there is no way to ask which
/// one happened. But the two do not look alike from outside: a real timeout
/// takes the full budget, while a dead handle fails instantly. A run of instant
/// failures is therefore a wait that will never work again, and the loop around
/// it is spinning a core rather than waiting.
///
/// Counting them turns that into an error the caller's existing reopen-with-
/// backoff loop already knows how to handle.
#[derive(Default)]
pub struct StallGuard {
    immediate_failures: u32,
}

impl StallGuard {
    /// Runs one wait. `wait` reports whether the event was signalled; the
    /// elapsed time decides how a `false` is read.
    ///
    /// Returns `Err` once the wait has failed instantly [`STALL_LIMIT`] times
    /// running, naming `what` in the message.
    pub fn wait(&mut self, what: &str, wait: impl FnOnce() -> bool) -> Result<(), String> {
        let started = std::time::Instant::now();
        if wait() {
            self.immediate_failures = 0;
            return Ok(());
        }
        if started.elapsed() >= IMMEDIATE {
            // A genuine timeout, which is normal: loopback with nothing playing
            // times out every turn forever.
            self.immediate_failures = 0;
            return Ok(());
        }
        self.immediate_failures += 1;
        if self.immediate_failures >= STALL_LIMIT {
            self.immediate_failures = 0;
            return Err(format!("waiting on the {what} event stopped working"));
        }
        Ok(())
    }
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
    stats: Arc<CaptureStats>,
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
                let outcome = run(&kind, &mut producer, &stop, &stats, started);
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
    stats: &CaptureStats,
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
    // What the device itself runs at, before `autoconvert` puts a resampler in
    // the way to give us the 48 kHz stereo float we asked for. Worth a line
    // because that resampler is a suspect whenever a source sounds wrong and
    // nothing else in the log moves — and it is invisible from anywhere else.
    match client.get_mixformat() {
        Ok(mix) => {
            let (rate, channels) = (mix.get_samplespersec(), mix.get_nchannels());
            if rate == SAMPLE_RATE && channels as usize == CHANNELS {
                log::debug!("Capture device is {rate} Hz, {channels} channels (no conversion)");
            } else {
                log::info!(
                    "Capture device is {rate} Hz, {channels} channels; Windows is converting it \
                     to {SAMPLE_RATE} Hz, {CHANNELS} channels"
                );
            }
        }
        // Not knowing the device format costs us a diagnostic, not the capture.
        Err(e) => log::debug!("Could not read the capture device's format: {e}"),
    }
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
    // A reopen is a new device session, and carrying the previous one's drops
    // into it would blame this device for the last one's trouble.
    stats.reset();

    let mut byte_queue: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut guard = StallGuard::default();
    let mut timeline = DeviceTimeline::new();
    // The first packet after a start carries `DATA_DISCONTINUITY` as a matter of
    // course — there is a gap between the device starting and us reading it, and
    // Windows says so. Counting it would put a 1 in every log line.
    let mut first_packet = true;
    while !stop.load(Ordering::Relaxed) {
        let new_frames = capture
            .get_next_packet_size()
            .map_err(|e| format!("reading the next packet size: {e}"))?
            .unwrap_or(0);
        if new_frames > 0 {
            byte_queue.reserve(new_frames as usize * blockalign);
            let before = byte_queue.len();
            let info = capture
                .read_from_device_to_deque(&mut byte_queue)
                .map_err(|e| format!("reading from the device: {e}"))?;
            // `AUDCLNT_BUFFERFLAGS_SILENT` means the contents of that buffer are
            // undefined, not that they are zeros — the crate copies them out
            // regardless, so without this whatever was in that memory would be
            // mixed and broadcast as audio.
            if info.flags.silent {
                stats.note_silent_packet();
                silence_appended(&mut byte_queue, before);
            }
            if info.flags.data_discontinuity && !first_packet {
                stats.note_discontinuity();
            }
            first_packet = false;
            let frames = ((byte_queue.len() - before) / blockalign) as u64;
            stats.add_frames(frames);
            let gap = timeline.observe(info.index, frames, Instant::now());
            if gap > 0 {
                stats.add_gap_frames(gap);
            }
            stats.set_index_usable(timeline.index_usable());
            if let Some(rate) = timeline.rate_millihz() {
                stats.set_rate_millihz(rate);
            }
            stats.add_dropped_samples(push_f32(&mut byte_queue, producer) as u64);
        }
        // Timeouts are normal here — loopback with nothing playing times out
        // every turn — so a failed wait is not itself an error. `StallGuard`
        // separates those from a wait that has stopped waiting at all, which
        // would otherwise spin this loop on a core forever; see its docs.
        guard.wait("capture", || event.wait_for_event(WAIT_MS).is_ok())?;
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Replaces everything appended to `bytes` from `before` onward with zeros.
///
/// Split out so the [`wasapi::BufferFlags::silent`] handling can be tested
/// without a sound card: the crate copies the device buffer into the deque
/// before we ever see the flag, so the fix is to overwrite what it just wrote
/// rather than to skip the read.
fn silence_appended(bytes: &mut std::collections::VecDeque<u8>, before: usize) {
    for byte in bytes.iter_mut().skip(before) {
        *byte = 0;
    }
}

/// Moves whole f32 samples from the byte queue into the ring, dropping
/// samples when the ring is full (mixer stalled or source unattached).
///
/// Returns how many samples were discarded. That number is the difference
/// between a source that is working and one that is crackling, so it is counted
/// rather than thrown away: a full ring means the mixer is not draining this
/// source as fast as the device fills it, and every sample dropped here is a
/// step discontinuity in the middle of a waveform.
fn push_f32(bytes: &mut std::collections::VecDeque<u8>, producer: &mut Producer<f32>) -> usize {
    let available = bytes.len() / 4;
    if available == 0 {
        return 0;
    }
    // Written in one chunk rather than a `push` per sample: at 48 kHz stereo
    // that was 96,000 atomic index stores a second per source, plus four
    // `pop_front`s each.
    let take = producer.slots().min(available);
    if take > 0
        && let Ok(mut chunk) = producer.write_chunk_uninit(take)
    {
        let (first, second) = chunk.as_mut_slices();
        for slot in first.iter_mut().chain(second.iter_mut()) {
            // `available` was computed from the queue length, so each of
            // these four bytes is there.
            slot.write(f32::from_le_bytes([
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
            ]));
        }
        // SAFETY: every slot in both slices was just written.
        unsafe { chunk.commit_all() };
    }
    // Full ring: discard the remainder. Capture must never block.
    let dropped = available - take;
    bytes.drain(..dropped * 4);
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
    fn a_wait_that_never_waits_is_eventually_an_error() {
        let mut guard = StallGuard::default();
        for turn in 0..STALL_LIMIT - 1 {
            assert!(
                guard.wait("test", || false).is_ok(),
                "gave up after only {turn} instant failures"
            );
        }
        let last = guard.wait("test", || false);
        assert_eq!(
            last,
            Err("waiting on the test event stopped working".to_string())
        );
    }

    /// The normal case, and the one that must never be mistaken for the case
    /// above: loopback with nothing playing times out on every single turn, for
    /// as long as the source exists.
    #[test]
    fn real_timeouts_are_not_a_stall() {
        let mut guard = StallGuard::default();
        for _ in 0..STALL_LIMIT + 1 {
            let slow_timeout = || {
                std::thread::sleep(IMMEDIATE + Duration::from_millis(2));
                false
            };
            assert!(guard.wait("test", slow_timeout).is_ok());
        }
    }

    /// A signalled wait clears the count, so instant failures scattered among
    /// working turns never add up to a stall.
    #[test]
    fn a_successful_wait_resets_the_count() {
        let mut guard = StallGuard::default();
        for _ in 0..STALL_LIMIT * 3 {
            for _ in 0..STALL_LIMIT - 1 {
                assert!(guard.wait("test", || false).is_ok());
            }
            assert!(guard.wait("test", || true).is_ok());
        }
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
            Arc::new(CaptureStats::new()),
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

    /// A full ring must report what it threw away. Silence here is what made a
    /// crackling microphone impossible to diagnose from a log file.
    #[test]
    fn a_full_ring_reports_the_samples_it_discarded() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(4);
        let mut bytes: std::collections::VecDeque<u8> =
            (0..10).flat_map(|n| (n as f32).to_le_bytes()).collect();

        // Four fit; the other six go over the side and are counted.
        assert_eq!(push_f32(&mut bytes, &mut producer), 6);
        assert!(bytes.is_empty(), "the queue is drained either way");
        assert_eq!(consumer.slots(), 4);

        // A ring with room for everything drops nothing.
        while consumer.pop().is_ok() {}
        let mut bytes: std::collections::VecDeque<u8> =
            (0..3).flat_map(|n| (n as f32).to_le_bytes()).collect();
        assert_eq!(push_f32(&mut bytes, &mut producer), 0);
    }

    #[test]
    fn an_empty_queue_drops_nothing() {
        let (mut producer, _consumer) = rtrb::RingBuffer::<f32>::new(4);
        let mut bytes = std::collections::VecDeque::new();
        assert_eq!(push_f32(&mut bytes, &mut producer), 0);
    }

    /// WASAPI's SILENT flag means the buffer contents are *undefined*, not that
    /// they are zeros, and the crate copies them into the queue before we get to
    /// see the flag. Without this the garbage would be mixed and broadcast.
    #[test]
    fn a_silent_packet_is_zeroed_without_disturbing_what_came_before() {
        let mut bytes: std::collections::VecDeque<u8> = VecDeque::from(vec![1, 2, 3, 4]);
        let before = bytes.len();
        bytes.extend([0xDE, 0xAD, 0xBE, 0xEF]);
        silence_appended(&mut bytes, before);
        assert_eq!(
            bytes.iter().copied().collect::<Vec<u8>>(),
            vec![1, 2, 3, 4, 0, 0, 0, 0],
            "only the new bytes are silenced"
        );
    }

    /// The deque wraps once it has been pushed and popped enough, so the
    /// silencing must go through the iterator rather than a contiguous slice.
    #[test]
    fn silencing_works_on_a_wrapped_deque() {
        let mut bytes: VecDeque<u8> = VecDeque::with_capacity(8);
        for byte in 0..8u8 {
            bytes.push_back(byte);
        }
        for _ in 0..6 {
            bytes.pop_front();
        }
        let before = bytes.len();
        bytes.extend([9, 9, 9, 9, 9, 9]);
        silence_appended(&mut bytes, before);
        assert_eq!(
            bytes.iter().copied().collect::<Vec<u8>>(),
            vec![6, 7, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn silencing_an_empty_append_changes_nothing() {
        let mut bytes: VecDeque<u8> = VecDeque::from(vec![1, 2, 3]);
        silence_appended(&mut bytes, 3);
        assert_eq!(bytes.iter().copied().collect::<Vec<u8>>(), vec![1, 2, 3]);
    }

    #[test]
    fn an_uninterrupted_backoff_sleeps_its_full_span() {
        let stop = AtomicBool::new(false);
        let start = std::time::Instant::now();
        sleep_interruptibly(Duration::from_millis(150), &stop);
        assert!(start.elapsed() >= Duration::from_millis(150));
    }
}
