//! Capture health accounting: what each source's ring is doing, and what the
//! device told us on the way in.
//!
//! This exists because the whole source→mixer path used to be silent. A source
//! whose ring filled up discarded samples in [`super::capture::push_f32`]
//! without a word, a starved ring was zero-padded in [`super::mixer::pull_block`]
//! without a word, and the `BufferInfo` WASAPI hands back from every read — which
//! carries the OS's own "I dropped your audio" flag — was thrown away unread. So
//! when a user reported their microphone crackling half an hour into a stream,
//! there was nothing in their log to look at, and no way to tell the difference
//! between the three things it could have been.
//!
//! The three, and how one line separates them:
//!
//! - **The ring ratcheting up.** The engine mixes on a wall clock and pads
//!   whatever has not arrived; padding inserts samples without consuming any, so
//!   a late packet adds latency that never drains. `ring` climbing across a
//!   session toward the one-second capacity, then flattening as `samples dropped`
//!   starts moving, is this one.
//! - **The OS losing packets.** `discontinuities` and `gap frames` are Windows
//!   telling on itself; if they move, the audio was gone before we saw it.
//! - **The device clock being off.** `device N Hz (M ppm)` is measured against
//!   the monotonic clock, not asserted from the format we asked for.
//!
//! Nothing in here is on the mixing hot path except integer counter arithmetic.
//! The formatting happens once every thirty seconds, from the engine loop, which
//! already logs and allocates on that thread.

use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long the measured device rate needs before it means anything. Under this
/// the reading is packet jitter rather than a clock difference, and a wrong
/// number in the log is worse than no number.
const RATE_SETTLE: Duration = Duration::from_secs(10);

/// The running totals a capture thread keeps about itself, read by the engine
/// thread when it writes the periodic report.
///
/// Every field is a monotonic total since the device was last opened, not a
/// windowed rate: the reader takes deltas. `Relaxed` throughout, because these
/// are counters and nothing is synchronized through them — a report that
/// straddles an increment by one packet is not wrong in any way that matters.
#[derive(Debug)]
pub struct CaptureStats {
    frames: AtomicU64,
    dropped_samples: AtomicU64,
    discontinuities: AtomicU64,
    silent_packets: AtomicU64,
    gap_frames: AtomicU64,
    /// Measured device rate in millihertz. Zero until there is enough of a
    /// window to mean anything; see [`RATE_SETTLE`].
    rate_millihz: AtomicU64,
    /// False once the driver has shown it does not report a device position.
    index_usable: AtomicBool,
    /// Whether a capture thread writes here at all. External feeds (speech,
    /// sound events) have no device and no thread, so their counters would
    /// otherwise read as a device that has delivered nothing.
    external: bool,
}

impl CaptureStats {
    pub fn new() -> Self {
        Self {
            frames: AtomicU64::new(0),
            dropped_samples: AtomicU64::new(0),
            discontinuities: AtomicU64::new(0),
            silent_packets: AtomicU64::new(0),
            gap_frames: AtomicU64::new(0),
            rate_millihz: AtomicU64::new(0),
            index_usable: AtomicBool::new(true),
            external: false,
        }
    }

    /// Stats for a source that is fed by another part of the app rather than by
    /// a device. Never reported on.
    pub fn external() -> Self {
        Self {
            external: true,
            ..Self::new()
        }
    }

    pub fn is_external(&self) -> bool {
        self.external
    }

    /// Starts the totals over. Called on each successful open, because a reopen
    /// is a new device session and carrying the old session's drops into it
    /// would blame the new one for them.
    pub fn reset(&self) {
        self.frames.store(0, Ordering::Relaxed);
        self.dropped_samples.store(0, Ordering::Relaxed);
        self.discontinuities.store(0, Ordering::Relaxed);
        self.silent_packets.store(0, Ordering::Relaxed);
        self.gap_frames.store(0, Ordering::Relaxed);
        self.rate_millihz.store(0, Ordering::Relaxed);
        self.index_usable.store(true, Ordering::Relaxed);
    }

    pub fn add_frames(&self, frames: u64) {
        self.frames.fetch_add(frames, Ordering::Relaxed);
    }

    pub fn add_dropped_samples(&self, samples: u64) {
        self.dropped_samples.fetch_add(samples, Ordering::Relaxed);
    }

    pub fn add_gap_frames(&self, frames: u64) {
        self.gap_frames.fetch_add(frames, Ordering::Relaxed);
    }

    pub fn note_discontinuity(&self) {
        self.discontinuities.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_silent_packet(&self) {
        self.silent_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_rate_millihz(&self, rate: u64) {
        self.rate_millihz.store(rate, Ordering::Relaxed);
    }

    pub fn set_index_usable(&self, usable: bool) {
        self.index_usable.store(usable, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Counters {
        Counters {
            frames: self.frames.load(Ordering::Relaxed),
            dropped_samples: self.dropped_samples.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            silent_packets: self.silent_packets.load(Ordering::Relaxed),
            gap_frames: self.gap_frames.load(Ordering::Relaxed),
            rate_millihz: self.rate_millihz.load(Ordering::Relaxed),
            index_usable: self.index_usable.load(Ordering::Relaxed),
        }
    }
}

impl Default for CaptureStats {
    fn default() -> Self {
        Self::new()
    }
}

/// One read of a [`CaptureStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub frames: u64,
    pub dropped_samples: u64,
    pub discontinuities: u64,
    pub silent_packets: u64,
    pub gap_frames: u64,
    pub rate_millihz: u64,
    pub index_usable: bool,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            frames: 0,
            dropped_samples: 0,
            discontinuities: 0,
            silent_packets: 0,
            gap_frames: 0,
            rate_millihz: 0,
            index_usable: true,
        }
    }
}

impl Counters {
    /// What happened between an earlier snapshot and this one. The rate and the
    /// index verdict are states rather than totals, so they are taken from the
    /// later reading as-is.
    pub fn since(&self, earlier: &Counters) -> Counters {
        Counters {
            frames: self.frames.saturating_sub(earlier.frames),
            dropped_samples: self.dropped_samples.saturating_sub(earlier.dropped_samples),
            discontinuities: self.discontinuities.saturating_sub(earlier.discontinuities),
            silent_packets: self.silent_packets.saturating_sub(earlier.silent_packets),
            gap_frames: self.gap_frames.saturating_sub(earlier.gap_frames),
            rate_millihz: self.rate_millihz,
            index_usable: self.index_usable,
        }
    }
}

/// Tracks the device's own idea of where it is, from the `index` and frame count
/// of each captured packet.
///
/// Split out of the capture loop so it can be tested without a sound card, and
/// because both of the things it works out are easy to get subtly wrong: the
/// first packet has nothing to be compared against, and a driver that does not
/// report a position at all reports zero, which naively differenced makes the
/// whole stream look like one enormous gap.
#[derive(Debug)]
pub struct DeviceTimeline {
    /// When the first packet arrived. Both the frame count and the elapsed time
    /// used for the rate are measured from here.
    started: Option<Instant>,
    last: Option<Instant>,
    /// Frames delivered *after* the first packet. The first one is excluded
    /// because there is no interval to divide it by.
    frames: u64,
    next_index: Option<u64>,
    index_usable: bool,
    packets: u64,
}

impl DeviceTimeline {
    pub fn new() -> Self {
        Self {
            started: None,
            last: None,
            frames: 0,
            next_index: None,
            index_usable: true,
            packets: 0,
        }
    }

    /// Records one captured packet, returning how many frames the device skipped
    /// before it — which is the OS having dropped that audio on the floor.
    pub fn observe(&mut self, index: u64, frames: u64, now: Instant) -> u64 {
        if frames == 0 {
            return 0;
        }
        self.packets += 1;
        if self.started.is_none() {
            // The first packet anchors both clocks and counts toward neither:
            // there is no earlier index to compare it against and no interval to
            // measure its frames over.
            self.started = Some(now);
            self.last = Some(now);
            self.next_index = Some(index.wrapping_add(frames));
            return 0;
        }
        self.frames += frames;
        self.last = Some(now);
        // A driver that leaves the position pinned at zero is not reporting one.
        // Believing it would turn every packet into a gap the size of the whole
        // stream so far, so the continuity half is switched off instead.
        if index == 0 && self.packets > 1 {
            self.index_usable = false;
        }
        if !self.index_usable {
            return 0;
        }
        let expected = self.next_index.unwrap_or(index);
        self.next_index = Some(index.wrapping_add(frames));
        // A position that went *backwards* is not a gap; it is a driver doing
        // something we do not model, and guessing would invent frames.
        index.saturating_sub(expected)
    }

    pub fn index_usable(&self) -> bool {
        self.index_usable
    }

    /// The device's measured rate in millihertz, once the window is long enough
    /// to be worth reporting. Independent of `index` — this is frames delivered
    /// against the monotonic clock, which every driver can be held to.
    pub fn rate_millihz(&self) -> Option<u64> {
        let (started, last) = (self.started?, self.last?);
        let elapsed = last.duration_since(started);
        if elapsed < RATE_SETTLE || self.frames == 0 {
            return None;
        }
        Some((self.frames as f64 * 1000.0 / elapsed.as_secs_f64()).round() as u64)
    }
}

impl Default for DeviceTimeline {
    fn default() -> Self {
        Self::new()
    }
}

/// What the engine observed about one source's ring over a reporting window.
#[derive(Debug, Default, Clone, Copy)]
pub struct Window {
    /// Ring occupancy in samples at the start and end of the window, and the
    /// highest it reached. The difference between the first two is the whole
    /// point of this file.
    pub fill_start: usize,
    pub fill_now: usize,
    pub peak: usize,
    pub blocks: u64,
    /// Blocks where the ring could not supply a full block.
    pub starved_blocks: u64,
}

impl Window {
    /// Begins the next window from where this one ended. The peak and the
    /// counters start over; the fill does not, because it is a level rather than
    /// a count and the next window's trend is measured from here.
    pub fn roll(&mut self) {
        self.fill_start = self.fill_now;
        self.peak = self.fill_now;
        self.blocks = 0;
        self.starved_blocks = 0;
    }
}

/// Everything one log line is built from.
#[derive(Debug)]
pub struct Report<'a> {
    pub name: &'a str,
    pub capacity_samples: usize,
    pub window: Window,
    pub elapsed: Duration,
    /// Counter movement over the window.
    pub delta: Counters,
}

/// Samples of interleaved stereo, as milliseconds of audio.
fn samples_to_ms(samples: usize) -> u64 {
    (samples as u64 * 1000) / (SAMPLE_RATE as u64 * CHANNELS as u64)
}

impl Report<'_> {
    /// Whether this source did nothing at all this window. A loopback source
    /// with nothing playing delivers no packets and holds an empty ring for as
    /// long as it exists, and a line a minute saying so in every user's log is
    /// noise that would bury the one line that matters.
    pub fn idle(&self) -> bool {
        self.delta.frames == 0 && self.window.fill_now == 0 && self.window.peak == 0
    }

    pub fn line(&self) -> String {
        let capacity_ms = samples_to_ms(self.capacity_samples);
        let fill_ms = samples_to_ms(self.window.fill_now);
        let peak_ms = samples_to_ms(self.window.peak);
        let trend = self.trend_ms_per_min();
        let gaps = if self.delta.index_usable {
            format!("{} gap frames", self.delta.gap_frames)
        } else {
            "gap frames unavailable".to_string()
        };
        let mut line = format!(
            "Capture {:?}: ring {fill_ms}/{capacity_ms} ms ({trend:+} ms/min, peak {peak_ms}), \
             {}/{} starved blocks, {} discontinuities, {gaps}, {} samples dropped, \
             {} silent packets",
            self.name,
            self.window.starved_blocks,
            self.window.blocks,
            self.delta.discontinuities,
            self.delta.dropped_samples,
            self.delta.silent_packets,
        );
        if self.delta.rate_millihz > 0 {
            let hz = self.delta.rate_millihz as f64 / 1000.0;
            let ppm = (hz - SAMPLE_RATE as f64) / SAMPLE_RATE as f64 * 1e6;
            line.push_str(&format!(", device {hz:.0} Hz ({ppm:+.0} ppm)"));
        }
        line
    }

    /// How fast the ring is filling, in milliseconds of added latency per minute.
    /// A healthy source sits at zero; the failure this file exists for is a
    /// number that stays positive for half an hour.
    fn trend_ms_per_min(&self) -> i64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0;
        }
        let start = samples_to_ms(self.window.fill_start) as i64;
        let now = samples_to_ms(self.window.fill_now) as i64;
        (((now - start) as f64) * 60.0 / seconds).round() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn the_first_packet_is_never_a_gap() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        // A device that has been running since before we opened it hands over a
        // large index on the very first packet. That is not audio we lost.
        assert_eq!(timeline.observe(4_800_000, 480, base), 0);
    }

    #[test]
    fn contiguous_packets_report_no_gap() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        let mut index = 1_000u64;
        for turn in 0..50 {
            let gap = timeline.observe(index, 480, at(base, turn * 10));
            assert_eq!(gap, 0, "turn {turn} invented a gap");
            index += 480;
        }
        assert!(timeline.index_usable());
    }

    #[test]
    fn a_skipped_index_is_reported_as_the_frames_lost() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        timeline.observe(1_000, 480, base);
        timeline.observe(1_480, 480, at(base, 10));
        // The device jumped 960 frames past where the next packet should start.
        let gap = timeline.observe(1_960 + 960, 480, at(base, 20));
        assert_eq!(gap, 960);
        // And the next contiguous packet is measured from the new position, so
        // one gap is not counted forever.
        assert_eq!(timeline.observe(1_960 + 960 + 480, 480, at(base, 30)), 0);
    }

    /// The case that would otherwise report the entire stream as lost audio.
    #[test]
    fn a_driver_that_never_reports_a_position_is_not_a_gap_machine() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        for turn in 0..20 {
            let gap = timeline.observe(0, 480, at(base, turn * 10));
            assert_eq!(gap, 0, "turn {turn} treated a missing position as a gap");
        }
        assert!(!timeline.index_usable());
    }

    /// A position that moves backwards is not modelled, and must not be turned
    /// into an invented gap by an underflow.
    #[test]
    fn a_position_that_goes_backwards_reports_nothing() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        timeline.observe(10_000, 480, base);
        assert_eq!(timeline.observe(5_000, 480, at(base, 10)), 0);
    }

    #[test]
    fn the_rate_is_not_reported_until_the_window_is_worth_believing() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        timeline.observe(0, 480, base);
        timeline.observe(480, 480, at(base, 10));
        assert_eq!(timeline.rate_millihz(), None, "two packets is not a rate");
    }

    #[test]
    fn a_slow_device_clock_is_measured() {
        let base = Instant::now();
        let mut timeline = DeviceTimeline::new();
        // 30 seconds of wall clock carrying 47,990 frames a second.
        let mut index = 0u64;
        timeline.observe(index, 480, base);
        index += 480;
        for turn in 1..=3_000u64 {
            // 479.9 frames per 10 ms, accumulated so the rounding does not drift.
            let frames = (turn * 4_799 / 10) - ((turn - 1) * 4_799 / 10);
            timeline.observe(index, frames, at(base, turn * 10));
            index += frames;
        }
        let rate = timeline.rate_millihz().expect("a rate after 30 seconds");
        let hz = rate as f64 / 1000.0;
        assert!(
            (hz - 47_990.0).abs() < 5.0,
            "expected about 47990 Hz, got {hz}"
        );
    }

    #[test]
    fn external_sources_are_marked_and_never_counted() {
        let stats = CaptureStats::external();
        assert!(stats.is_external());
        assert!(!CaptureStats::new().is_external());
    }

    #[test]
    fn reopening_a_device_starts_the_totals_over() {
        let stats = CaptureStats::new();
        stats.add_dropped_samples(500);
        stats.note_discontinuity();
        stats.set_index_usable(false);
        stats.reset();
        let counters = stats.snapshot();
        assert_eq!(counters.dropped_samples, 0);
        assert_eq!(counters.discontinuities, 0);
        assert!(
            counters.index_usable,
            "a new device session deserves a fresh verdict on its driver"
        );
    }

    #[test]
    fn deltas_subtract_totals_but_carry_states_forward() {
        let before = Counters {
            frames: 100,
            dropped_samples: 5,
            ..Counters::default()
        };
        let after = Counters {
            frames: 250,
            dropped_samples: 9,
            rate_millihz: 47_999_000,
            index_usable: false,
            ..Counters::default()
        };
        let delta = after.since(&before);
        assert_eq!(delta.frames, 150);
        assert_eq!(delta.dropped_samples, 4);
        assert_eq!(delta.rate_millihz, 47_999_000, "a rate is not a total");
        assert!(!delta.index_usable, "nor is the driver verdict");
    }

    fn report(window: Window, delta: Counters) -> String {
        Report {
            name: "Microphone",
            capacity_samples: SAMPLE_RATE as usize * CHANNELS,
            window,
            elapsed: Duration::from_secs(30),
            delta,
        }
        .line()
    }

    #[test]
    fn a_healthy_source_reads_as_flat_and_empty() {
        let window = Window {
            fill_start: 480,
            fill_now: 480,
            peak: 960,
            blocks: 3_000,
            starved_blocks: 0,
        };
        let delta = Counters {
            frames: 1_440_000,
            rate_millihz: 48_000_000,
            ..Counters::default()
        };
        let line = report(window, delta);
        assert!(line.contains("ring 5/1000 ms"), "{line}");
        assert!(line.contains("+0 ms/min"), "{line}");
        assert!(line.contains("0/3000 starved blocks"), "{line}");
        assert!(line.contains("device 48000 Hz (+0 ppm)"), "{line}");
    }

    /// The signature we are shipping this to catch: the ring climbing while
    /// samples start going over the side.
    #[test]
    fn the_ratchet_is_legible_in_the_line() {
        let window = Window {
            // Half a second of backlog, up nine milliseconds this window.
            fill_start: 47_136,
            fill_now: 48_000,
            peak: 48_000,
            blocks: 3_000,
            starved_blocks: 2,
        };
        let delta = Counters {
            frames: 1_440_000,
            dropped_samples: 2_048,
            rate_millihz: 47_996_000,
            ..Counters::default()
        };
        let line = report(window, delta);
        assert!(line.contains("ring 500/1000 ms"), "{line}");
        assert!(line.contains("+18 ms/min"), "{line}");
        assert!(line.contains("2048 samples dropped"), "{line}");
        assert!(line.contains("ppm"), "{line}");
    }

    #[test]
    fn an_unusable_position_says_so_rather_than_reporting_zero() {
        let delta = Counters {
            frames: 1_000,
            index_usable: false,
            ..Counters::default()
        };
        let line = report(Window::default(), delta);
        assert!(line.contains("gap frames unavailable"), "{line}");
        assert!(!line.contains("0 gap frames"), "{line}");
    }

    #[test]
    fn a_rate_that_is_not_known_yet_is_left_out_entirely() {
        let delta = Counters {
            frames: 1_000,
            ..Counters::default()
        };
        let line = report(Window::default(), delta);
        assert!(!line.contains("device"), "{line}");
    }

    #[test]
    fn a_silent_loopback_source_is_idle_and_a_live_one_is_not() {
        let idle = Report {
            name: "Desktop Audio",
            capacity_samples: SAMPLE_RATE as usize * CHANNELS,
            window: Window {
                blocks: 3_000,
                starved_blocks: 3_000,
                ..Window::default()
            },
            elapsed: Duration::from_secs(30),
            delta: Counters::default(),
        };
        assert!(idle.idle(), "a loopback source with nothing playing");

        let live = Report {
            delta: Counters {
                frames: 1,
                ..Counters::default()
            },
            ..idle
        };
        assert!(!live.idle(), "one frame is enough to be worth reporting");
    }

    /// A window that has only just started must not divide by zero, nor report a
    /// trend from a single reading.
    #[test]
    fn a_zero_length_window_has_no_trend() {
        let line = Report {
            name: "Microphone",
            capacity_samples: SAMPLE_RATE as usize * CHANNELS,
            window: Window {
                fill_start: 0,
                fill_now: 48_000,
                ..Window::default()
            },
            elapsed: Duration::ZERO,
            delta: Counters::default(),
        }
        .line();
        assert!(line.contains("+0 ms/min"), "{line}");
    }

    #[test]
    fn rolling_a_window_keeps_the_level_and_clears_the_counts() {
        let mut window = Window {
            fill_start: 100,
            fill_now: 900,
            peak: 1_200,
            blocks: 3_000,
            starved_blocks: 7,
        };
        window.roll();
        assert_eq!(window.fill_start, 900, "the next trend starts from here");
        assert_eq!(window.peak, 900, "the peak is per window, not per session");
        assert_eq!(window.blocks, 0);
        assert_eq!(window.starved_blocks, 0);
    }
}
