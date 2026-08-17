//! Arming a stream to go live at a wall-clock time.
//!
//! Two shapes, both described in `scheduling.md`. **Simple** connects at the
//! chosen moment on whatever scene is active. **Advanced** connects early on a
//! "pre-stream" scene and switches to a "start of stream" scene when the real
//! content begins — which exists because Audio Pub is slow to accept a source
//! (it ffprobes 33 KB of *real-time* audio and kills the source if the check
//! fails), so being connected and healthy several minutes before the first
//! listener arrives is worth a great deal.
//!
//! Three things here are deliberate.
//!
//! **The clock is the wall clock, in Unix seconds — not `Instant`.** The user
//! picked "20:00 on Tuesday", not "in six hours". `Instant` is a monotonic tick
//! count (QPC on Windows) whose relationship to the wall clock is not preserved
//! across sleep, hibernation, or a clock correction, and a schedule that
//! silently slips by however long the machine was suspended is worse than no
//! schedule at all. (`Runtime::next_announcement` uses `Instant` and is right
//! to: its deadline really is "an hour from the last one".)
//!
//! **The state machine is pure.** [`Schedule::stage`] takes the time and answers
//! what to do; it touches no widgets, no `Runtime`, and no clock of its own. All
//! the awkward cases — a deadline missed because the machine slept, a switch
//! time landing before the connect time, a connect that has already been fired
//! once — are decided here, where they can be tested, rather than in the UI tick.
//!
//! **A fired connect is remembered, not inferred.** [`Schedule::connect_fired`]
//! exists because the obvious inference — "we have not connected, so it must
//! still be time to connect" — retries forever if the stream fails to start:
//! `StreamState` returns to `Idle` on a failed connect, the deadline is still in
//! the past, and the next tick would try again, and the tick after that. A
//! schedule fires once.

use std::time::Duration;

/// How late a deadline may be and still fire.
///
/// The reason this constant has to exist is the same reason the clock is the
/// wall clock: the machine can sleep through a schedule. Waking a laptop two
/// hours after a broadcast was due and having Pubsplash immediately open a live
/// stream — unattended, on whatever scene, to an audience that has long since
/// gone — is a bad enough outcome to be worth a threshold. Inside the grace the
/// schedule is merely late (a suspended machine that woke promptly, a tick
/// missed under a long-running dialog) and fires; outside it the schedule is
/// stale and is cancelled with a spoken and logged reason.
pub const STALE_GRACE: u64 = 120;

/// An armed schedule. Session-only: this is never persisted, because launching
/// Pubsplash must never start a broadcast on the strength of a choice made
/// before the last restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Unix seconds at which Pubsplash connects.
    pub connect_at: u64,
    /// `None` in simple mode.
    pub advanced: Option<Advanced>,
    /// Whether the connect leg has already been fired. See the module header:
    /// without this a stream that fails to start is retried on every tick.
    pub connect_fired: bool,
}

/// The extra legs of an advanced schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advanced {
    /// Unix seconds at which the scene switches. Always after
    /// [`Schedule::connect_at`] — see [`resolve_times`], which is what
    /// guarantees it.
    pub switch_at: u64,
    /// The scene to connect on: music, a holding loop, whatever the user wants
    /// early listeners to hear.
    pub pre_scene: String,
    /// The scene the real content runs on.
    pub start_scene: String,
}

/// What the schedule wants done at a given moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Not yet time to connect. `remaining` is what the Home tab counts down.
    WaitingToConnect { remaining: Duration },
    /// Connect now, first switching to `scene` if there is one.
    ///
    /// `scene` is `None` in simple mode (the active scene is whatever the user
    /// left selected, which is the whole point of simple mode) and also when the
    /// pre-stream window has already elapsed — see [`Schedule::stage`].
    Connect { scene: Option<String> },
    /// Connected, and an advanced schedule still owes a scene switch.
    WaitingToSwitch { remaining: Duration },
    /// Switch to `scene` now.
    Switch { scene: String },
    /// A deadline was missed by more than [`STALE_GRACE`]; abandon the schedule
    /// and say how late it was.
    Stale { late_by: Duration },
    /// Nothing left to do; the caller drops the schedule.
    Done,
}

impl Schedule {
    /// What to do at `now`.
    ///
    /// Reads [`Self::connect_fired`] rather than the stream's state, so it stays
    /// free of `ui::StreamState` and testable without a UI — and so a failed
    /// connect is not mistaken for a connect still owed.
    pub fn stage(&self, now: u64) -> Stage {
        if !self.connect_fired {
            if let Some(remaining) = remaining(self.connect_at, now) {
                return Stage::WaitingToConnect { remaining };
            }
            // Too late to be worth starting a broadcast nobody asked for now.
            // Measured from the connect deadline and not the switch one: the
            // connect is the leg that has been missed.
            if let Some(late_by) = stale_by(self.connect_at, now) {
                return Stage::Stale { late_by };
            }
            // In advanced mode the pre-stream scene is skipped when its whole
            // window has already gone by — the machine slept through it, or a
            // tick was missed — because playing the music scene *after* the
            // content was due to start gets the running order exactly backwards.
            // Connecting straight onto the start scene is the closest thing to
            // what was asked for.
            let scene = self
                .advanced
                .as_ref()
                .filter(|a| a.switch_at > now)
                .map(|a| a.pre_scene.clone());
            return Stage::Connect { scene };
        }
        let Some(advanced) = &self.advanced else {
            // Simple mode has nothing left once the stream is up. The caller
            // drops the schedule on `Connect` anyway, so this is belt and braces.
            return Stage::Done;
        };
        match remaining(advanced.switch_at, now) {
            Some(remaining) => Stage::WaitingToSwitch { remaining },
            // No staleness check here on purpose. The stream is already up and
            // the pre-stream scene is on air; switching late is exactly what the
            // user wants in that situation, however late it is, because the
            // alternative is leaving the holding music playing forever.
            None => Stage::Switch {
                scene: advanced.start_scene.clone(),
            },
        }
    }
}

/// How long until `deadline`, or `None` once it has arrived.
///
/// Saturating rather than signed: a deadline in the past is not "negative time
/// remaining", it is a deadline that has arrived, and every caller wants to know
/// which of those two it holds rather than a number to compare against zero.
fn remaining(deadline: u64, now: u64) -> Option<Duration> {
    (deadline > now).then(|| Duration::from_secs(deadline - now))
}

/// How far past `deadline` we are, but only once that is more than
/// [`STALE_GRACE`].
fn stale_by(deadline: u64, now: u64) -> Option<Duration> {
    let late = now.saturating_sub(deadline);
    (late > STALE_GRACE).then(|| Duration::from_secs(late))
}

/// A day in seconds, for the midnight roll in [`resolve_times`].
const ONE_DAY: u64 = 24 * 60 * 60;

/// Refuses a connect time that is not in the future.
///
/// The whole of simple mode's validation, and the first half of advanced mode's.
pub fn check_future(connect_at: u64, now: u64) -> Result<(), String> {
    if connect_at <= now {
        return Err("That time has already passed. Choose a time in the future.".into());
    }
    Ok(())
}

/// Checks and normalises the two times an advanced schedule is built from,
/// returning `(connect_at, switch_at)` or the message to show the user.
///
/// The dialog offers one date and two times, so a show whose pre-stream leg
/// starts at 23:55 and whose content starts at 00:05 gives a switch time
/// *earlier* than the connect time. That is not a mistake to refuse — it is a
/// broadcast crossing midnight, and adding a day is what the user meant. Only a
/// gap of a full day or more is genuinely unusable.
///
/// Pure, and separate from the dialog, because these three rules are the whole
/// of what "a valid schedule" means and they are worth testing without a window.
pub fn resolve_times(connect_at: u64, switch_at: u64, now: u64) -> Result<(u64, u64), String> {
    check_future(connect_at, now)?;
    if switch_at > connect_at {
        return Ok((connect_at, switch_at));
    }
    // Crossing midnight: the content starts on the day after the pre-stream leg.
    let rolled = switch_at + ONE_DAY;
    if rolled > connect_at {
        return Ok((connect_at, rolled));
    }
    Err("The start of stream time must be after the pre-stream time.".into())
}

/// A countdown, worded to be read aloud.
///
/// Deliberately not `ui::format_duration`, which gives `"00:04:32"`. That is
/// right for a stopwatch — it is what the Duration row shows — but a screen
/// reader renders it as three two-digit numbers, and this is a time *until*
/// something, which people say in words. Rounded down to whole units and never
/// more than two of them, so it stays short enough to hear.
pub fn format_countdown(secs: u64) -> String {
    fn unit(n: u64, name: &str) -> String {
        if n == 1 {
            format!("{n} {name}")
        } else {
            format!("{n} {name}s")
        }
    }
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        // Seconds are dropped once there is an hour to say: nobody needs
        // "1 hour 5 minutes 3 seconds", and the row is re-read often.
        if minutes > 0 {
            return format!("{} {}", unit(hours, "hour"), unit(minutes, "minute"));
        }
        return unit(hours, "hour");
    }
    if minutes > 0 {
        if seconds > 0 {
            return format!("{} {}", unit(minutes, "minute"), unit(seconds, "second"));
        }
        return unit(minutes, "minute");
    }
    unit(seconds, "second")
}

/// A local date and time as Unix seconds, or `None` if no such moment exists.
///
/// Win32 rather than a date crate for the same reason `mastodon::local_hour`
/// gives: the tree has neither `chrono` nor `time`, and pulling one in for a
/// single conversion is a lot of dependency. `month` is **1-based**, matching
/// `wxdragon::DateTime::month()` (which converts wx's 0-based value for us at
/// both ends — see the crate's `datetime.rs`), so components read straight off a
/// `DatePickerCtrl` pass through unchanged.
///
/// `TzSpecificLocalTimeToSystemTime` is the load-bearing call: it applies the
/// machine's time zone *and its DST rules for that particular date*, so a
/// schedule set either side of a transition lands on the right instant. It is
/// also what rejects a time that does not exist — the hour a spring-forward
/// skips — and an impossible date such as 31 February. Requires the
/// `Win32_System_Time` feature on the `windows` crate.
pub fn local_to_unix(
    year: i32,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> Option<u64> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};

    let local = SYSTEMTIME {
        wYear: u16::try_from(year).ok()?,
        wMonth: month,
        // Ignored by the conversion, which derives it from the date.
        wDayOfWeek: 0,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: second,
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME::default();
    // SAFETY: both calls only read the `SYSTEMTIME` they are given and write
    // their out-parameter, and all three are owned locals here.
    unsafe {
        TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).ok()?;
    }
    let mut file = FILETIME::default();
    unsafe {
        SystemTimeToFileTime(&utc, &mut file).ok()?;
    }
    let ticks = (u64::from(file.dwHighDateTime) << 32) | u64::from(file.dwLowDateTime);
    // FILETIME counts 100 ns intervals from 1601-01-01; this is the offset to
    // the Unix epoch. A time before 1970 is not something the pickers can
    // produce, but answering `None` beats wrapping.
    const EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    ticks.checked_sub(EPOCH_TICKS).map(|t| t / 10_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advanced(connect_at: u64, switch_at: u64) -> Schedule {
        Schedule {
            connect_at,
            advanced: Some(Advanced {
                switch_at,
                pre_scene: "Music".into(),
                start_scene: "Show".into(),
            }),
            connect_fired: false,
        }
    }

    fn simple(connect_at: u64) -> Schedule {
        Schedule {
            connect_at,
            advanced: None,
            connect_fired: false,
        }
    }

    #[test]
    fn simple_waits_then_connects() {
        let s = simple(1_000);
        assert_eq!(
            s.stage(940),
            Stage::WaitingToConnect {
                remaining: Duration::from_secs(60)
            }
        );
        // The deadline itself counts as arrived, not as one last second of wait.
        assert_eq!(s.stage(1_000), Stage::Connect { scene: None });
    }

    #[test]
    fn simple_is_done_once_fired() {
        let mut s = simple(1_000);
        s.connect_fired = true;
        assert_eq!(s.stage(1_000), Stage::Done);
    }

    /// The retry guard. A stream that fails to start leaves `StreamState::Idle`
    /// behind with the deadline still in the past, so anything inferring "not
    /// connected" from the stream would try again on every tick, forever.
    #[test]
    fn a_fired_connect_never_fires_again() {
        let mut s = advanced(1_000, 1_300);
        s.connect_fired = true;
        assert!(!matches!(s.stage(1_000), Stage::Connect { .. }));
        assert!(!matches!(s.stage(5_000), Stage::Connect { .. }));
    }

    #[test]
    fn advanced_walks_all_four_legs() {
        let mut s = advanced(1_000, 1_300);
        assert_eq!(
            s.stage(900),
            Stage::WaitingToConnect {
                remaining: Duration::from_secs(100)
            }
        );
        assert_eq!(
            s.stage(1_000),
            Stage::Connect {
                scene: Some("Music".into())
            }
        );
        s.connect_fired = true;
        assert_eq!(
            s.stage(1_100),
            Stage::WaitingToSwitch {
                remaining: Duration::from_secs(200)
            }
        );
        assert_eq!(
            s.stage(1_300),
            Stage::Switch {
                scene: "Show".into()
            }
        );
    }

    #[test]
    fn a_missed_pre_stream_window_connects_on_the_start_scene() {
        // Inside the grace, so it still fires — but the music leg is over, and
        // playing it now would put the running order backwards.
        assert_eq!(
            advanced(1_000, 1_060).stage(1_060),
            Stage::Connect { scene: None }
        );
    }

    #[test]
    fn a_late_connect_inside_the_grace_still_fires() {
        assert!(matches!(
            simple(1_000).stage(1_000 + STALE_GRACE),
            Stage::Connect { .. }
        ));
    }

    #[test]
    fn a_connect_missed_by_more_than_the_grace_goes_stale() {
        // The laptop-lid case: waking hours later must not open a live stream.
        assert_eq!(
            simple(1_000).stage(1_000 + STALE_GRACE + 1),
            Stage::Stale {
                late_by: Duration::from_secs(STALE_GRACE + 1)
            }
        );
        assert!(matches!(
            advanced(1_000, 1_300).stage(20_000),
            Stage::Stale { .. }
        ));
    }

    /// A switch is never stale. The stream is up and the holding scene is on
    /// air, so switching late beats never switching.
    #[test]
    fn a_late_switch_still_happens() {
        let mut s = advanced(1_000, 1_300);
        s.connect_fired = true;
        assert_eq!(
            s.stage(1_300 + STALE_GRACE * 10),
            Stage::Switch {
                scene: "Show".into()
            }
        );
    }

    #[test]
    fn resolve_times_refuses_the_past() {
        assert!(resolve_times(500, 900, 1_000).is_err());
        assert!(resolve_times(1_000, 1_200, 1_000).is_err());
    }

    #[test]
    fn resolve_times_crosses_midnight() {
        // 23:55 -> 00:05 the next day.
        assert_eq!(resolve_times(1_000, 900, 500), Ok((1_000, 900 + ONE_DAY)));
        // Two equal times are not a switch at all, so they roll too.
        assert_eq!(
            resolve_times(1_000, 1_000, 500),
            Ok((1_000, 1_000 + ONE_DAY))
        );
        // Already in order: left alone.
        assert_eq!(resolve_times(1_000, 1_200, 500), Ok((1_000, 1_200)));
        // A gap of a full day is unusable whichever way it is read.
        assert!(resolve_times(ONE_DAY + 2_000, 1_000, 500).is_err());
    }

    #[test]
    fn countdowns_are_worded_for_speech() {
        assert_eq!(format_countdown(0), "0 seconds");
        assert_eq!(format_countdown(1), "1 second");
        assert_eq!(format_countdown(45), "45 seconds");
        assert_eq!(format_countdown(60), "1 minute");
        assert_eq!(format_countdown(61), "1 minute 1 second");
        assert_eq!(format_countdown(272), "4 minutes 32 seconds");
        assert_eq!(format_countdown(3_599), "59 minutes 59 seconds");
        assert_eq!(format_countdown(3_600), "1 hour");
        assert_eq!(format_countdown(3_900), "1 hour 5 minutes");
        assert_eq!(format_countdown(7_260), "2 hours 1 minute");
        // No bare clock-speak anywhere.
        assert!(!format_countdown(272).contains(':'));
    }

    /// `local_to_unix` against the clock it is meant to agree with.
    ///
    /// A tolerance rather than equality: `GetLocalTime` and `now_unix` are two
    /// separate readings and the second can tick between them.
    #[test]
    fn local_to_unix_agrees_with_now() {
        // SAFETY: `GetLocalTime` only writes the `SYSTEMTIME` it returns.
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let converted = local_to_unix(
            i32::from(now.wYear),
            now.wMonth,
            now.wDay,
            now.wHour,
            now.wMinute,
            now.wSecond,
        )
        .expect("the current local time must be convertible");
        let unix = crate::mastodon::now_unix();
        assert!(
            converted.abs_diff(unix) <= 2,
            "converted {converted} vs now_unix {unix}"
        );
    }

    #[test]
    fn local_to_unix_rejects_what_does_not_exist() {
        assert_eq!(local_to_unix(2026, 2, 31, 12, 0, 0), None, "31 February");
        assert_eq!(local_to_unix(2026, 1, 1, 25, 0, 0), None, "hour 25");
        assert_eq!(local_to_unix(2026, 13, 1, 12, 0, 0), None, "month 13");
    }

    /// The assertion that proves the time zone is actually being applied: the
    /// same clock reading in January and in July is a different number of
    /// seconds from the epoch on any machine that observes DST.
    ///
    /// Written so it also passes where there is no DST (many time zones, and CI
    /// containers set to UTC): it requires the two offsets to be either
    /// identical or an hour apart, never something else.
    #[test]
    fn local_to_unix_applies_the_zone_for_the_date() {
        let winter = local_to_unix(2026, 1, 15, 12, 0, 0).expect("15 January noon");
        let summer = local_to_unix(2026, 7, 15, 12, 0, 0).expect("15 July noon");
        // 181 whole days separate the two dates; whatever is left over either way
        // is the change in UTC offset. Compared against the exact span rather
        // than divided into days, because a negative shift would otherwise land
        // a day short.
        let span = summer - winter;
        let drift = span.abs_diff(181 * ONE_DAY);
        assert!(
            drift == 0 || drift == 3_600,
            "expected 181 days give or take an hour of DST, got {span} seconds"
        );
    }
}
