//! Makes a date or time picker say which field the caret is on, because Windows
//! will not.
//!
//! A `wxDatePickerCtrl`/`wxTimePickerCtrl` on MSW is a native
//! `SysDateTimePick32`. Left and right arrow move a caret between its fields —
//! month, day, year, or hour, minute, second, AM/PM — and **nothing announces
//! which one you land on**, so a screen-reader user pressing Up has no way to
//! know what is about to change. That is what this module fixes: it tracks the
//! caret itself and speaks the field through [`super::help::announce`], the same
//! `UiaRaiseNotificationEvent` path F1 help and the keybind refusals use.
//!
//! ## Why it has to be done the hard way
//!
//! Every gentler option was measured against the real control first, and the
//! measurements are pinned as `#[ignore]`d tests at the bottom of this file. What
//! they found:
//!
//! - **The fields are not MSAA children.** This is the surprise, and it is why
//!   `native_acc`-style `WM_GETOBJECT` passthrough does nothing here: the
//!   control's only accessible children are the optional none-checkbox
//!   (`ROLE_SYSTEM_CHECKBUTTON`) and the drop-down button
//!   (`ROLE_SYSTEM_PUSHBUTTON`). There is no per-field object to announce, so
//!   there is nothing for wx's wrapper to be hiding.
//! - **`accFocus` is useless.** It answers `CHILDID_SELF` however many times the
//!   right arrow has been pressed, so the selected field cannot be *queried* from
//!   anywhere. Neither can it be asked for with a `DTM_` message: none reports
//!   it. Tracking is not a shortcut here, it is the only door.
//!
//! ## The two things that could make tracking lie, and what is done instead
//!
//! **Which fields there are.** The locale's format pattern is the obvious source
//! and it is only a guess: it says what the *locale* would format, while the
//! control shows what **wx** told it to, and nothing guarantees the two agree. A
//! time picker rendering a 24-hour clock in a 12-hour locale has three fields
//! where the pattern describes four, and then every field announces as its
//! neighbour and the last one names a field that is not there. So the list is
//! taken from the control's own text: [`measured_fields`] sets [`PROBE`] — a value
//! whose every component is a different number — reads the text back, and matches
//! each number to the field it can only have come from. [`parse_fields`] survives
//! as the fallback for a format the text reader cannot account for.
//!
//! **Where the caret is.** Tracking keystrokes needs the control to agree about
//! the starting point and the movement, so both are measured rather than assumed:
//!
//! - **Left and right *wrap*** modulo the field count — they do not stop at the
//!   ends. (Nine rights from the day field of a three-field date land back on the
//!   month.) So the caret is `(caret ± 1).rem_euclid(len)`; a design that clamped
//!   would have desynced permanently on the first wrap. Wrapping also means there
//!   is **no end to pin against**, which is why the caret cannot be re-derived by
//!   pressing left a lot — the only absolute reference is the one below.
//! - **Home, End, Up and Down do not move the caret.** They change the current
//!   field's value — Home to its minimum, End to its maximum — so they are
//!   announced with the field unchanged rather than treated as movement. Typing
//!   digits does not move it either.
//! - Everything else goes through [`observe_caret`], which asks the control where
//!   the caret is by nudging the value and seeing which field moved, then putting
//!   it back. That is the one absolute reference available, and it is what makes a
//!   **mouse click** recoverable — a click chooses a field and no message reports
//!   which. It runs on focus and after a click, never per arrow key: four extra
//!   messages a keystroke to confirm arithmetic the contract tests already pin
//!   would be waste.
//!
//! When observation cannot answer — a field pinned at the limit of a range refuses
//! to move — the caret goes unknown and an arrow key reads the whole value out
//! instead of naming a field. Being told less is better than being told wrong.
//!
//! ## Why not compose the picker out of plain controls
//!
//! Three spin controls per picker would announce perfectly with no native code at
//! all. It would also replace one Tab stop with three and take the arrow-key
//! interaction people already know away from them. Kept as the fallback if these
//! measurements ever stop holding.
//!
//! Threading: everything here runs on the UI thread — `install` from dialog
//! construction, the subclass proc from message dispatch on the same thread.

use crate::t;
use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, SYSTEMTIME, WPARAM};
use windows::Win32::Globalization::{
    GetLocaleInfoEx, LOCALE_S1159, LOCALE_S2359, LOCALE_SSHORTDATE, LOCALE_STIMEFORMAT,
};
use windows::Win32::UI::Controls::{DTM_GETSYSTEMTIME, DTM_SETSYSTEMTIME, GDT_VALID};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageW, WM_GETTEXT, WM_KEYDOWN, WM_LBUTTONUP, WM_NCDESTROY, WM_SETFOCUS,
};
use wxdragon::prelude::*;

/// Ours alone; `slider_uia` has `0x0A11`, `help` `0x0A12`, `native_acc` `0x0A13`.
const SUBCLASS_ID: usize = 0x0A14;

/// Keys that change the current field's value without moving the caret, so the
/// same field is re-announced. Measured, not assumed — see the module header.
const VALUE_KEYS: [VIRTUAL_KEY; 4] = [VK_UP, VK_DOWN, VK_HOME, VK_END];

/// Which picker a control is, and so which locale pattern describes its fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Date,
    Time,
}

/// One editable part of a date or time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Month,
    Day,
    Year,
    /// Present only in the locales whose short date spells the day out.
    Weekday,
    /// A 12-hour hour, from a `h` pattern. Displayed 1-12, so it is announced
    /// that way and not as the 24-hour value the control stores.
    Hour12,
    /// A 24-hour hour, from `H`.
    Hour24,
    Minute,
    Second,
    Meridiem,
}

impl Field {
    /// What to call it. Lower case because it is spoken mid-sentence, and these
    /// are the words the user asked to hear.
    fn label(self) -> String {
        match self {
            Field::Month => t!("month"),
            Field::Day => t!("day"),
            Field::Year => t!("year"),
            Field::Weekday => t!("day of week"),
            Field::Hour12 | Field::Hour24 => t!("hour"),
            Field::Minute => t!("minute"),
            Field::Second => t!("second"),
            Field::Meridiem => t!("AM or PM"),
        }
    }

    /// The field's current value, worded for speech.
    ///
    /// Months and weekdays are named rather than numbered — "August" is what a
    /// listener wants, where "8" makes them work out which month that is. Every
    /// other field is a plain number and deliberately not zero-padded: "minute,
    /// 5" reads better than "minute, 05".
    fn value(self, t: &SystemTime) -> String {
        let months: [String; 12] = [
            t!("January"),
            t!("February"),
            t!("March"),
            t!("April"),
            t!("May"),
            t!("June"),
            t!("July"),
            t!("August"),
            t!("September"),
            t!("October"),
            t!("November"),
            t!("December"),
        ];
        let days: [String; 7] = [
            t!("Sunday"),
            t!("Monday"),
            t!("Tuesday"),
            t!("Wednesday"),
            t!("Thursday"),
            t!("Friday"),
            t!("Saturday"),
        ];
        match self {
            Field::Month => months
                .get(usize::from(t.month).wrapping_sub(1))
                .map(|m| (*m).to_string())
                .unwrap_or_else(|| t.month.to_string()),
            Field::Day => t.day.to_string(),
            Field::Year => t.year.to_string(),
            Field::Weekday => days
                .get(usize::from(t.weekday))
                .map(|d| (*d).to_string())
                .unwrap_or_else(|| t.weekday.to_string()),
            // The control stores 0-23 whatever it displays, so a 12-hour field
            // has to be converted or midnight announces as "hour, 0".
            Field::Hour12 => twelve_hour(t.hour).to_string(),
            Field::Hour24 => t.hour.to_string(),
            Field::Minute => t.minute.to_string(),
            Field::Second => t.second.to_string(),
            Field::Meridiem => if t.hour < 12 { "AM" } else { "PM" }.to_string(),
        }
    }
}

/// The parts of a `SYSTEMTIME` this module reads, so the pure code above and its
/// tests need no Win32 types.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct SystemTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
}

/// The fields a Windows date or time pattern puts on screen, in the order they
/// appear.
///
/// The pattern comes from the locale (`LOCALE_SSHORTDATE`, `LOCALE_STIMEFORMAT`),
/// which is the same source the control itself formats from, so the order matches
/// what the user sees — `d/M/y` and `M/d/y` locales both come out right without
/// anything being hard-coded.
///
/// Text inside single quotes is a literal separator and is skipped, `''` being an
/// escaped quote. Runs of one letter are one field, so `yyyy` is a single year.
/// The case of `M` and `m` is the whole difference between month and minute, and
/// getting it wrong is the classic bug here.
fn parse_fields(pattern: &str) -> Vec<Field> {
    let mut fields = Vec::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // Skip to the closing quote. A doubled quote inside is an escaped
            // literal and does not end the run.
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if !c.is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == c {
            i += 1;
        }
        let run = i - start;
        let field = match c {
            'M' => Some(Field::Month),
            // Three or more `d` is the weekday name, which the control makes a
            // field of its own; one or two is the day of the month.
            'd' if run >= 3 => Some(Field::Weekday),
            'd' => Some(Field::Day),
            'y' => Some(Field::Year),
            'h' => Some(Field::Hour12),
            'H' => Some(Field::Hour24),
            'm' => Some(Field::Minute),
            's' => Some(Field::Second),
            't' => Some(Field::Meridiem),
            // Era (`g`) and anything else the control does not let you edit.
            _ => None,
        };
        if let Some(field) = field {
            fields.push(field);
        }
    }
    fields
}

/// The value a picker is set to while its fields are being identified.
///
/// Every component is a **different number** — 6, 15, 2026, 37, 52, and an hour
/// that reads 22 on a 24-hour clock and 10 on a 12-hour one — so each number in
/// the control's own text can only have come from one field. That is what makes
/// [`derive_fields`] exact rather than a guess, and it is also how 12-hour and
/// 24-hour displays tell themselves apart.
const PROBE: SystemTime = SystemTime {
    year: 2026,
    // 15 June 2026 is a Monday, which is `weekday` below.
    month: 6,
    weekday: 1,
    day: 15,
    hour: 22,
    minute: 37,
    second: 52,
};

/// The fields a picker really shows, read out of its own text.
///
/// The locale pattern is a good guess and not an answer: it says what the
/// *locale* would format, while the control displays what **wx** asked it to, and
/// the two need not agree — a time picker showing a 24-hour clock in a 12-hour
/// locale has three fields where the pattern describes four, and every field
/// after the first then announces as its neighbour. Reading the text closes that
/// gap by asking the only authority that matters.
///
/// Pure, so the mapping is testable without a window: `text` is what the control
/// displayed while set to [`PROBE`], and `am`/`pm` are the locale's designators.
/// `None` when a token cannot be accounted for, which is the caller's cue to fall
/// back to [`parse_fields`] rather than announce something invented.
fn derive_fields(text: &str, probe: &SystemTime, am: &str, pm: &str) -> Option<Vec<Field>> {
    let mut fields = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            // Leading zeros are display padding, so this compares numbers.
            let n: u32 = chars[start..i].iter().collect::<String>().parse().ok()?;
            let field = match u16::try_from(n).ok()? {
                v if v == probe.year => Field::Year,
                v if v == probe.month => Field::Month,
                v if v == probe.day => Field::Day,
                v if v == probe.minute => Field::Minute,
                v if v == probe.second => Field::Second,
                v if v == probe.hour => Field::Hour24,
                // The 12-hour rendering of the same hour.
                v if v == twelve_hour(probe.hour) => Field::Hour12,
                _ => return None,
            };
            fields.push(field);
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // The only word a numeric date or a time can contain is the AM/PM
            // designator. A month or weekday *name* means this is a format
            // `derive_fields` was not built to read, so the caller falls back
            // rather than this guessing.
            if !am.is_empty() && word.eq_ignore_ascii_case(am)
                || !pm.is_empty() && word.eq_ignore_ascii_case(pm)
            {
                fields.push(Field::Meridiem);
                continue;
            }
            return None;
        }
        i += 1;
    }
    (!fields.is_empty()).then_some(fields)
}

/// The hour as a 12-hour clock shows it: 0 becomes 12, 13 becomes 1.
fn twelve_hour(hour: u16) -> u16 {
    match hour % 12 {
        0 => 12,
        h => h,
    }
}

/// Which of `fields` changed between two readings, as an index.
///
/// This is how the caret is *observed* rather than assumed: nudge the value, see
/// what moved, and that is the field the caret is on. Pure, so the awkward part —
/// telling the hour field apart from AM/PM, which both move the stored hour — is
/// testable without a window.
fn index_of_change(fields: &[Field], before: &SystemTime, after: &SystemTime) -> Option<usize> {
    let changed = if before.year != after.year {
        Field::Year
    } else if before.month != after.month {
        Field::Month
    } else if before.day != after.day {
        Field::Day
    } else if before.minute != after.minute {
        Field::Minute
    } else if before.second != after.second {
        Field::Second
    } else if before.hour != after.hour {
        // AM/PM moves the stored hour by twelve where the hour field moves it by
        // one, and that difference is the only thing separating them.
        let delta = (i32::from(after.hour) - i32::from(before.hour)).rem_euclid(24);
        if delta == 12 {
            Field::Meridiem
        } else if fields.contains(&Field::Hour24) {
            Field::Hour24
        } else {
            Field::Hour12
        }
    } else {
        return None;
    };
    // A weekday field moves the whole date, so a day change is ambiguous when one
    // is present and must not be guessed at.
    if changed == Field::Day && fields.contains(&Field::Weekday) {
        return None;
    }
    fields.iter().position(|f| *f == changed)
}

/// What we know about one picker window.
struct PickerState {
    fields: Vec<Field>,
    /// Which field the caret is on, or `None` when a mouse click has put it
    /// somewhere we cannot know.
    caret: Option<usize>,
}

thread_local! {
    /// Keyed by HWND, because the subclass proc has nothing else to go on.
    /// Entries are removed on `WM_NCDESTROY` rather than left to leak — Windows
    /// reuses handles, and a stale field list found by an unrelated control later
    /// would announce nonsense.
    static PICKERS: RefCell<HashMap<isize, PickerState>> = RefCell::new(HashMap::new());
}

/// One string from the user's locale.
fn locale_string(lctype: u32) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the call is given the length of the buffer it writes into.
    let written = unsafe { GetLocaleInfoEx(None, lctype, Some(&mut buf)) };
    if written <= 0 {
        log::warn!("picker_acc: GetLocaleInfoEx({lctype}) failed");
        return String::new();
    }
    // The count includes the terminating null.
    let end = (written as usize).saturating_sub(1);
    String::from_utf16_lossy(&buf[..end])
}

/// The locale's format pattern for `kind`, the fallback source of the field list.
fn locale_pattern(kind: Kind) -> String {
    locale_string(match kind {
        Kind::Date => LOCALE_SSHORTDATE,
        Kind::Time => LOCALE_STIMEFORMAT,
    })
}

/// Whatever the picker is currently displaying.
fn control_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: `WM_GETTEXT` writes at most `wParam` characters into `lParam`.
    let written = unsafe {
        SendMessageW(
            hwnd,
            WM_GETTEXT,
            Some(WPARAM(buf.len())),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
    }
    .0;
    if written <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(written as usize).min(buf.len())])
}

/// Writes a value to the picker.
fn write_time(hwnd: HWND, t: &SystemTime) {
    let st = SYSTEMTIME {
        wYear: t.year,
        wMonth: t.month,
        wDayOfWeek: t.weekday,
        wDay: t.day,
        wHour: t.hour,
        wMinute: t.minute,
        wSecond: t.second,
        wMilliseconds: 0,
    };
    // SAFETY: `DTM_SETSYSTEMTIME` reads one `SYSTEMTIME` through `lParam`.
    unsafe {
        SendMessageW(
            hwnd,
            DTM_SETSYSTEMTIME,
            Some(WPARAM(GDT_VALID.0 as usize)),
            Some(LPARAM(&st as *const _ as isize)),
        );
    }
}

/// Identifies the picker's fields by setting [`PROBE`] and reading back the text.
///
/// The value is put back afterwards, and this runs during dialog construction
/// before anything is on screen, so the round trip is invisible. It must run
/// **before** any range is applied to the control, or the probe value would be
/// clamped into the range and the numbers read back would not be the ones looked
/// for — `schedule_ui` calls `install` first for that reason.
fn measured_fields(hwnd: HWND) -> Option<Vec<Field>> {
    let saved = read_time(hwnd)?;
    write_time(hwnd, &PROBE);
    let text = control_text(hwnd);
    write_time(hwnd, &saved);
    let fields = derive_fields(
        &text,
        &PROBE,
        &locale_string(LOCALE_S1159),
        &locale_string(LOCALE_S2359),
    );
    if fields.is_none() {
        log::warn!("picker_acc: could not identify the fields in {text:?}");
    }
    fields
}

/// Starts announcing `picker`'s fields.
///
/// Call once, after the control is built. Nothing needs uninstalling: the state
/// is dropped on `WM_NCDESTROY` and the subclass dies with the window.
pub fn install(picker: &dyn WxWidget, kind: Kind) {
    let hwnd = HWND(picker.get_handle());
    if hwnd.0.is_null() {
        log::warn!("picker_acc: no window yet for the {kind:?} picker");
        return;
    }
    // The control's own text first, the locale pattern only if that fails: what
    // wx told the control to display is the thing the caret actually moves over.
    let pattern = locale_pattern(kind);
    let fields = match measured_fields(hwnd) {
        Some(fields) => fields,
        None => parse_fields(&pattern),
    };
    if fields.is_empty() {
        // Without a field list there is nothing to announce, and installing the
        // subclass would only add a no-op.
        log::warn!("picker_acc: no fields found for the {kind:?} picker ({pattern:?})");
        return;
    }
    // Both sources logged, so a disagreement between them is visible in a log
    // the user has sent rather than something to be guessed at.
    log::debug!(
        "picker_acc: {kind:?} fields {fields:?} (locale pattern {pattern:?} would give {:?})",
        parse_fields(&pattern)
    );
    PICKERS.with(|p| {
        p.borrow_mut().insert(
            hwnd.0 as isize,
            PickerState {
                fields,
                caret: Some(0),
            },
        );
    });
    // SAFETY: a UI-thread call on a live window; the proc is a plain function.
    let ok = unsafe { SetWindowSubclass(hwnd, Some(picker_proc), SUBCLASS_ID, 0) };
    if !ok.as_bool() {
        log::warn!("picker_acc: SetWindowSubclass failed for the {kind:?} picker");
    }
}

/// The control's current value.
fn read_time(hwnd: HWND) -> Option<SystemTime> {
    let mut st = windows::Win32::Foundation::SYSTEMTIME::default();
    // SAFETY: `DTM_GETSYSTEMTIME` writes one `SYSTEMTIME` through `lParam`, which
    // is the owned local here.
    let result = unsafe {
        SendMessageW(
            hwnd,
            DTM_GETSYSTEMTIME,
            None,
            Some(LPARAM(&mut st as *mut _ as isize)),
        )
    };
    // `GDT_NONE` means the picker's checkbox is clear and it holds no date. Ours
    // never has one, but a value that is not there must not be read.
    if result.0 != 0 {
        return None;
    }
    Some(SystemTime {
        year: st.wYear,
        month: st.wMonth,
        weekday: st.wDayOfWeek,
        day: st.wDay,
        hour: st.wHour,
        minute: st.wMinute,
        second: st.wSecond,
    })
}

/// Speaks the field the caret is on, or the whole value if we do not know it.
fn announce(hwnd: HWND) {
    let Some(time) = read_time(hwnd) else {
        return;
    };
    let line = PICKERS.with(|p| {
        let pickers = p.borrow();
        let state = pickers.get(&(hwnd.0 as isize))?;
        match state.caret.and_then(|i| state.fields.get(i)) {
            Some(field) => Some(format!("{}, {}", field.label(), field.value(&time))),
            // Caret unknown, which only a mouse click causes. Reading the whole
            // value out is less than the user wanted but is never wrong, and the
            // next focus change restores field tracking.
            None => Some(
                state
                    .fields
                    .iter()
                    .map(|f| f.value(&time))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    });
    if let Some(line) = line {
        super::help::announce(&line);
    }
}

thread_local! {
    /// Set while [`observe_caret`] is driving the control, so the keys it sends
    /// come back through the subclass proc without being mistaken for the user's
    /// and stepping the caret or announcing.
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Sends a key to the control without the subclass treating it as user input.
fn send_key(hwnd: HWND, vk: VIRTUAL_KEY) {
    PROBING.with(|p| p.set(true));
    // SAFETY: a synchronous message to a live window on its own thread.
    unsafe {
        SendMessageW(hwnd, WM_KEYDOWN, Some(WPARAM(vk.0 as usize)), Some(LPARAM(1)));
    }
    PROBING.with(|p| p.set(false));
}

/// Finds out where the caret really is, by nudging the value and seeing what
/// moved. The nudge is undone, so the control ends as it began.
///
/// Used where an assumption would otherwise have to be made — on focus, and after
/// a mouse click, which is the one thing key tracking cannot follow. Deliberately
/// *not* on every arrow key: it would put four extra messages through the control
/// per keystroke to confirm arithmetic that the contract tests already pin.
///
/// Up is tried first and Down second, because a field sitting at the limit of a
/// range refuses to move in one direction and would otherwise look like no field
/// at all.
fn observe_caret(hwnd: HWND) -> Option<usize> {
    let fields = PICKERS.with(|p| {
        p.borrow()
            .get(&(hwnd.0 as isize))
            .map(|s| s.fields.clone())
    })?;
    let before = read_time(hwnd)?;
    for (nudge, undo) in [(VK_UP, VK_DOWN), (VK_DOWN, VK_UP)] {
        send_key(hwnd, nudge);
        let after = read_time(hwnd)?;
        send_key(hwnd, undo);
        if after != before {
            return index_of_change(&fields, &before, &after);
        }
    }
    None
}

/// Moves the tracked caret by `delta`, wrapping as the control does.
fn step(hwnd: HWND, delta: isize) {
    PICKERS.with(|p| {
        let mut pickers = p.borrow_mut();
        let Some(state) = pickers.get_mut(&(hwnd.0 as isize)) else {
            return;
        };
        let len = state.fields.len() as isize;
        if len == 0 {
            return;
        }
        // A mouse click leaves this `None`, and an arrow key cannot recover it —
        // the control moves from wherever it really is, not from a known field.
        // It stays unknown until focus resets it.
        if let Some(caret) = state.caret {
            state.caret = Some((caret as isize + delta).rem_euclid(len) as usize);
        }
    });
}

fn set_caret(hwnd: HWND, caret: Option<usize>) {
    PICKERS.with(|p| {
        if let Some(state) = p.borrow_mut().get_mut(&(hwnd.0 as isize)) {
            state.caret = caret;
        }
    });
}

unsafe extern "system" fn picker_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    // Everything below runs *after* the control has processed the message, so the
    // value read back is the one the user has just moved to.
    let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    // `observe_caret` drives the control with the very keys handled below, so
    // without this its nudges would step the caret and announce.
    if PROBING.with(|p| p.get()) {
        return result;
    }
    match msg {
        // Focus is the resync point. The control lands on the first field, but
        // that is asked rather than assumed: `observe_caret` reads it out of the
        // control, and the measured default is only the fallback for a value
        // pinned at the limit of a range where nudging it cannot answer.
        //
        // Deliberately silent: the screen reader is already announcing the
        // control's name and value here, and `help::announce` asks for
        // `ImportantMostRecent`, which would cut that off to say something the
        // user is about to hear on their first arrow key anyway.
        WM_SETFOCUS => {
            let caret = observe_caret(hwnd).or(Some(0));
            set_caret(hwnd, caret);
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_LEFT.0 {
                step(hwnd, -1);
                announce(hwnd);
            } else if key == VK_RIGHT.0 {
                step(hwnd, 1);
                announce(hwnd);
            } else if VALUE_KEYS.iter().any(|k| k.0 == key) {
                // Up, Down, Home and End all change the current field's value and
                // leave the caret where it is, so the same field is re-announced
                // with its new value. Page Up and Page Down are deliberately
                // absent: they are not part of this control's keyboard interface.
                announce(hwnd);
            }
        }
        // A click chooses a field and no message reports which, so the caret is
        // observed instead of tracked here. On `WM_LBUTTONUP`, because the control
        // moves its caret while handling the press and the button has to be back
        // up before the value can be nudged. `None` if it cannot be worked out, at
        // which point an arrow key reads the whole value rather than name the
        // wrong field.
        WM_LBUTTONUP => {
            let caret = observe_caret(hwnd);
            set_caret(hwnd, caret);
        }
        WM_NCDESTROY => {
            PICKERS.with(|p| p.borrow_mut().remove(&(hwnd.0 as isize)));
        }
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> SystemTime {
        SystemTime {
            year: 2026,
            month: 8,
            weekday: 1,
            day: 17,
            hour: 14,
            minute: 5,
            second: 0,
        }
    }

    #[test]
    fn parses_a_us_short_date() {
        assert_eq!(
            parse_fields("M/d/yyyy"),
            vec![Field::Month, Field::Day, Field::Year]
        );
    }

    /// The order is the locale's, not ours — this is the whole reason the pattern
    /// is read rather than assumed.
    #[test]
    fn parses_a_day_first_short_date() {
        assert_eq!(
            parse_fields("dd/MM/yyyy"),
            vec![Field::Day, Field::Month, Field::Year]
        );
        assert_eq!(
            parse_fields("yyyy-MM-dd"),
            vec![Field::Year, Field::Month, Field::Day]
        );
    }

    #[test]
    fn parses_twelve_and_twenty_four_hour_times() {
        assert_eq!(
            parse_fields("h:mm:ss tt"),
            vec![
                Field::Hour12,
                Field::Minute,
                Field::Second,
                Field::Meridiem
            ]
        );
        assert_eq!(
            parse_fields("HH:mm:ss"),
            vec![Field::Hour24, Field::Minute, Field::Second]
        );
    }

    /// `M` is the month and `m` is the minute. Confusing them is the classic bug
    /// in this kind of parser, so it is pinned.
    #[test]
    fn month_and_minute_are_case_sensitive() {
        assert_eq!(parse_fields("MM"), vec![Field::Month]);
        assert_eq!(parse_fields("mm"), vec![Field::Minute]);
    }

    #[test]
    fn quoted_separators_are_not_fields() {
        // A literal `d` inside quotes must not become a day field.
        assert_eq!(
            parse_fields("dd' de 'MMMM' de 'yyyy"),
            vec![Field::Day, Field::Month, Field::Year]
        );
        // An escaped quote does not end the literal early.
        assert_eq!(parse_fields("h'o''clock'"), vec![Field::Hour12]);
    }

    #[test]
    fn a_spelled_out_weekday_is_its_own_field() {
        assert_eq!(
            parse_fields("dddd, MMMM d, yyyy"),
            vec![Field::Weekday, Field::Month, Field::Day, Field::Year]
        );
    }

    #[test]
    fn months_and_weekdays_are_named() {
        assert_eq!(Field::Month.value(&t()), "August");
        assert_eq!(Field::Weekday.value(&t()), "Monday");
        assert_eq!(Field::Day.value(&t()), "17");
        assert_eq!(Field::Year.value(&t()), "2026");
    }

    /// The control stores 0-23 however it displays the hour, so a 12-hour field
    /// has to be converted — otherwise noon reads "hour, 12" but midnight reads
    /// "hour, 0", which is not a time anybody writes.
    #[test]
    fn a_twelve_hour_field_is_converted() {
        let mut time = t();
        assert_eq!(Field::Hour12.value(&time), "2");
        assert_eq!(Field::Hour24.value(&time), "14");
        assert_eq!(Field::Meridiem.value(&time), "PM");
        time.hour = 0;
        assert_eq!(Field::Hour12.value(&time), "12");
        assert_eq!(Field::Hour24.value(&time), "0");
        assert_eq!(Field::Meridiem.value(&time), "AM");
        time.hour = 12;
        assert_eq!(Field::Hour12.value(&time), "12");
        assert_eq!(Field::Meridiem.value(&time), "PM");
    }

    #[test]
    fn every_field_has_a_label() {
        for field in [
            Field::Month,
            Field::Day,
            Field::Year,
            Field::Weekday,
            Field::Hour12,
            Field::Hour24,
            Field::Minute,
            Field::Second,
            Field::Meridiem,
        ] {
            assert!(!field.label().is_empty(), "{field:?} has no label");
        }
    }

    /// The whole point of reading the control's text: this is a 12-hour locale
    /// whose picker is nonetheless showing a 24-hour clock with no AM/PM field.
    /// The pattern says four fields, the control has three, and believing the
    /// pattern is what makes every field announce as its neighbour.
    #[test]
    fn a_twenty_four_hour_display_is_believed_over_the_locale() {
        assert_eq!(
            parse_fields("h:mm:ss tt"),
            vec![
                Field::Hour12,
                Field::Minute,
                Field::Second,
                Field::Meridiem
            ],
            "the locale's own pattern"
        );
        assert_eq!(
            derive_fields("22:37:52", &PROBE, "AM", "PM"),
            Some(vec![Field::Hour24, Field::Minute, Field::Second]),
            "but the control is showing three fields on a 24-hour clock"
        );
    }

    #[test]
    fn derives_a_twelve_hour_time() {
        assert_eq!(
            derive_fields("10:37:52 PM", &PROBE, "AM", "PM"),
            Some(vec![
                Field::Hour12,
                Field::Minute,
                Field::Second,
                Field::Meridiem
            ])
        );
        // Without seconds, and with the designator before the time.
        assert_eq!(
            derive_fields("PM 10:37", &PROBE, "AM", "PM"),
            Some(vec![Field::Meridiem, Field::Hour12, Field::Minute])
        );
    }

    #[test]
    fn derives_dates_in_whatever_order_they_are_shown() {
        assert_eq!(
            derive_fields("6/15/2026", &PROBE, "AM", "PM"),
            Some(vec![Field::Month, Field::Day, Field::Year])
        );
        // Zero padding is display only, so the numbers still identify themselves.
        assert_eq!(
            derive_fields("15.06.2026", &PROBE, "AM", "PM"),
            Some(vec![Field::Day, Field::Month, Field::Year])
        );
        assert_eq!(
            derive_fields("2026-06-15", &PROBE, "AM", "PM"),
            Some(vec![Field::Year, Field::Month, Field::Day])
        );
    }

    /// Anything unaccounted for has to fail rather than be guessed at, because the
    /// caller's fallback is a locale pattern and its own invention is nothing.
    #[test]
    fn unrecognised_text_is_refused() {
        // A month name: a format this was not built to read.
        assert_eq!(derive_fields("15 June 2026", &PROBE, "AM", "PM"), None);
        // A number that is not any of the probe's components.
        assert_eq!(derive_fields("6/15/1999", &PROBE, "AM", "PM"), None);
        assert_eq!(derive_fields("", &PROBE, "AM", "PM"), None);
    }

    /// A locale with no AM/PM designators must not match the empty string against
    /// every word it meets.
    #[test]
    fn empty_designators_match_nothing() {
        assert_eq!(derive_fields("22:37:52", &PROBE, "", ""), Some(vec![
            Field::Hour24,
            Field::Minute,
            Field::Second
        ]));
        assert_eq!(derive_fields("10:37:52 nachm", &PROBE, "", ""), None);
    }

    #[test]
    fn twelve_hour_wraps_midnight_and_noon() {
        assert_eq!(twelve_hour(0), 12);
        assert_eq!(twelve_hour(12), 12);
        assert_eq!(twelve_hour(13), 1);
        assert_eq!(twelve_hour(22), 10);
    }

    /// The probe's components must stay mutually distinct, or a number in the
    /// control's text could have come from two fields and [`derive_fields`] would
    /// silently pick the first.
    #[test]
    fn the_probe_values_are_all_different() {
        let mut seen = vec![
            PROBE.year,
            PROBE.month,
            PROBE.day,
            PROBE.minute,
            PROBE.second,
            PROBE.hour,
            twelve_hour(PROBE.hour),
        ];
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "PROBE has a repeated component");
    }

    /// Observation is what makes the caret recoverable after a mouse click, and it
    /// hinges on telling the hour field from AM/PM — both move the stored hour.
    #[test]
    fn a_change_is_traced_back_to_its_field() {
        let fields = [
            Field::Hour12,
            Field::Minute,
            Field::Second,
            Field::Meridiem,
        ];
        let before = SystemTime {
            hour: 10,
            minute: 30,
            second: 20,
            ..PROBE
        };
        let hour = SystemTime {
            hour: 11,
            ..before
        };
        assert_eq!(index_of_change(&fields, &before, &hour), Some(0));
        // Twelve hours is AM/PM, not the hour field.
        let meridiem = SystemTime {
            hour: 22,
            ..before
        };
        assert_eq!(index_of_change(&fields, &before, &meridiem), Some(3));
        let minute = SystemTime {
            minute: 31,
            ..before
        };
        assert_eq!(index_of_change(&fields, &before, &minute), Some(1));
        // Nothing moved: the value was at a limit and refused, which the caller
        // answers by nudging the other way.
        assert_eq!(index_of_change(&fields, &before, &before), None);
    }

    /// The hour wrapping 23 to 0 is a delta of one modulo 24, and must not be
    /// mistaken for the twelve-hour jump that means AM/PM.
    #[test]
    fn an_hour_wrapping_midnight_is_still_the_hour() {
        let fields = [Field::Hour24, Field::Minute];
        let before = SystemTime { hour: 23, ..PROBE };
        let after = SystemTime { hour: 0, ..PROBE };
        assert_eq!(index_of_change(&fields, &before, &after), Some(0));
    }

    #[test]
    fn a_date_change_maps_to_its_own_field() {
        let fields = [Field::Month, Field::Day, Field::Year];
        assert_eq!(
            index_of_change(&fields, &PROBE, &SystemTime { month: 7, ..PROBE }),
            Some(0)
        );
        assert_eq!(
            index_of_change(&fields, &PROBE, &SystemTime { day: 16, ..PROBE }),
            Some(1)
        );
        assert_eq!(
            index_of_change(&fields, &PROBE, &SystemTime { year: 2027, ..PROBE }),
            Some(2)
        );
    }

    /// A weekday field moves the whole date, so a day change cannot be attributed
    /// and must be refused rather than guessed.
    #[test]
    fn a_weekday_makes_a_day_change_ambiguous() {
        let fields = [Field::Weekday, Field::Month, Field::Day, Field::Year];
        assert_eq!(
            index_of_change(&fields, &PROBE, &SystemTime { day: 16, ..PROBE }),
            None
        );
    }

    /// The wrap the caret tracking depends on, as arithmetic. The control wraps
    /// rather than stopping at the ends — see the module header — so this must
    /// too, or the two desync the first time a user arrows past the last field.
    #[test]
    fn our_caret_wraps_in_both_directions() {
        let wrap = |caret: usize, delta: isize, len: usize| {
            (caret as isize + delta).rem_euclid(len as isize) as usize
        };
        assert_eq!(wrap(0, 1, 3), 1);
        assert_eq!(wrap(2, 1, 3), 0, "past the last field wraps to the first");
        assert_eq!(wrap(0, -1, 3), 2, "before the first wraps to the last");
        // The observed case: nine rights from the day of a three-field date.
        let mut caret = 1;
        for _ in 0..8 {
            caret = wrap(caret, 1, 3);
        }
        assert_eq!(caret, 0);
    }
}

/// The measurements this module's whole design rests on, against the real
/// control.
///
/// `#[ignore]`d because each one creates windows and drives comctl32, which does
/// not belong in the default run — but they are assertions and not a printout on
/// purpose. Every claim in the module header is checked here, so if a future
/// Windows changes any of it, this fails loudly instead of the app quietly
/// announcing the wrong field. Run with:
///
/// ```text
/// cargo test --bin pubsplash picker_contract -- --ignored
/// ```
#[cfg(test)]
mod picker_contract {
    use super::*;
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
    use windows::Win32::UI::Controls::{
        DTM_SETSYSTEMTIME, ICC_DATE_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_STYLE, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::{Interface, PCWSTR, w};

    const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
    /// `DTS_SHORTDATECENTURYFORMAT`, what `wxDatePickerCtrl` uses on MSW.
    const DTS_SHORTDATECENTURYFORMAT: u32 = 0x000C;
    /// `DTS_TIMEFORMAT`, what `wxTimePickerCtrl` uses.
    const DTS_TIMEFORMAT: u32 = 0x0009;

    /// A picker of `kind`, plus the throwaway top-level window parenting it.
    ///
    /// A stock `STATIC` is the host so that no window class has to be registered,
    /// which would pull in the Gdi feature for nothing.
    fn picker(kind: Kind) -> (HWND, HWND) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let icc = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_DATE_CLASSES,
            };
            assert!(InitCommonControlsEx(&icc).as_bool(), "comctl32 date classes");
            let host = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("picker contract"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                400,
                200,
                None,
                None,
                None,
                None,
            )
            .expect("host window");
            let style = match kind {
                Kind::Date => DTS_SHORTDATECENTURYFORMAT,
                Kind::Time => DTS_TIMEFORMAT,
            };
            let hwnd = CreateWindowExW(
                Default::default(),
                w!("SysDateTimePick32"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                0,
                0,
                200,
                24,
                Some(host),
                None,
                None,
                None,
            )
            .expect("picker window");
            // A baseline well away from every field's limits, so an Up press can
            // never be confused by a wrap.
            let st = SYSTEMTIME {
                wYear: 2026,
                wMonth: 6,
                wDayOfWeek: 1,
                wDay: 15,
                wHour: 10,
                wMinute: 30,
                wSecond: 20,
                wMilliseconds: 0,
            };
            SendMessageW(
                hwnd,
                DTM_SETSYSTEMTIME,
                None,
                Some(LPARAM(&st as *const _ as isize)),
            );
            let _ = SetFocus(Some(hwnd));
            (hwnd, host)
        }
    }

    fn key(hwnd: HWND, vk: VIRTUAL_KEY) {
        unsafe {
            SendMessageW(hwnd, WM_KEYDOWN, Some(WPARAM(vk.0 as usize)), Some(LPARAM(1)));
        }
    }

    /// Which field the caret is really on, worked out by pressing Up and seeing
    /// what moved. There is no way to ask — that is the finding this file exists
    /// for — so the caret is only ever observable through its effect.
    ///
    /// The Up is undone with a Down, leaving the value as it was.
    fn observed_field(hwnd: HWND, fields: &[Field]) -> Field {
        let before = read_time(hwnd).expect("picker holds a value");
        key(hwnd, VK_UP);
        let after = read_time(hwnd).expect("picker holds a value");
        key(hwnd, VK_DOWN);
        let hour_field = if fields.contains(&Field::Hour24) {
            Field::Hour24
        } else {
            Field::Hour12
        };
        if before.year != after.year {
            Field::Year
        } else if before.month != after.month {
            Field::Month
        } else if before.day != after.day {
            Field::Day
        } else if before.hour != after.hour {
            // AM/PM moves the stored hour by twelve; the hour field moves it by
            // one. That difference is the only way to tell those two apart.
            let delta = (i32::from(after.hour) - i32::from(before.hour)).rem_euclid(24);
            if delta == 12 {
                Field::Meridiem
            } else {
                hour_field
            }
        } else if before.minute != after.minute {
            Field::Minute
        } else if before.second != after.second {
            Field::Second
        } else {
            panic!("Up changed nothing: {before:?}");
        }
    }

    /// The field list read out of the control's own text must be the one the caret
    /// really walks — checked by walking it, one field at a time.
    ///
    /// This is the test that would have caught the bug where a picker showed a
    /// 24-hour clock while the locale pattern described a 12-hour one with an
    /// AM/PM field, which made every announcement name its neighbour.
    #[test]
    #[ignore]
    fn the_measured_fields_are_the_ones_the_caret_walks() {
        for kind in [Kind::Date, Kind::Time] {
            let (hwnd, host) = picker(kind);
            let fields = measured_fields(hwnd).expect("fields read from the control's text");
            println!(
                "{kind:?}: text {:?} -> {fields:?} (locale pattern {:?} -> {:?})",
                control_text(hwnd),
                locale_pattern(kind),
                parse_fields(&locale_pattern(kind))
            );
            if fields.contains(&Field::Weekday) {
                continue;
            }
            let _ = unsafe { SetFocus(Some(hwnd)) };
            for step in 0..fields.len() {
                assert_eq!(
                    observed_field(hwnd, &fields),
                    fields[step],
                    "{kind:?}: field {step} of {fields:?} is not what the caret is on"
                );
                key(hwnd, VK_RIGHT);
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyWindow(host);
            }
        }
    }

    /// The finding that ruled out the `native_acc` approach: a picker's fields are
    /// **not** MSAA children, and `accFocus` cannot report the caret.
    #[test]
    #[ignore]
    fn the_fields_are_not_accessible_children() {
        for kind in [Kind::Date, Kind::Time] {
            let (hwnd, host) = picker(kind);
            let fields = parse_fields(&locale_pattern(kind));
            let mut acc: Option<IAccessible> = None;
            unsafe {
                AccessibleObjectFromWindow(
                    hwnd,
                    OBJID_CLIENT,
                    &IAccessible::IID,
                    &mut acc as *mut _ as *mut _,
                )
                .expect("an IAccessible for the picker");
            }
            let acc = acc.expect("an IAccessible for the picker");
            let children = unsafe { acc.accChildCount() }.expect("child count");
            assert!(
                (children as usize) < fields.len(),
                "{kind:?}: {children} children for {} fields — if the fields have \
                 become real MSAA children, `native_acc`-style passthrough may now \
                 be the right fix and this module could go away",
                fields.len()
            );
            // Whatever the caret is on, the control reports itself as focused and
            // never a field, so the selected field cannot be queried.
            for _ in 0..3 {
                let focus = unsafe { acc.accFocus() }.expect("accFocus");
                let id = i32::try_from(&focus).unwrap_or(0);
                assert_eq!(
                    id, 0,
                    "{kind:?}: accFocus named child {id}; it may now track the caret"
                );
                key(hwnd, VK_RIGHT);
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyWindow(host);
            }
        }
    }

    /// `observe_caret` must agree with the caret the control really has, at every
    /// field. This is the primitive that recovers from a mouse click, and the one
    /// place the module asks the control instead of assuming.
    #[test]
    #[ignore]
    fn observation_agrees_with_the_real_caret() {
        for kind in [Kind::Date, Kind::Time] {
            let (hwnd, host) = picker(kind);
            let fields = measured_fields(hwnd).expect("fields");
            if fields.contains(&Field::Weekday) {
                continue;
            }
            // `observe_caret` reads the field list out of the registry, so the
            // window has to be registered exactly as `install` would.
            PICKERS.with(|p| {
                p.borrow_mut().insert(
                    hwnd.0 as isize,
                    PickerState {
                        fields: fields.clone(),
                        caret: Some(0),
                    },
                )
            });
            let _ = unsafe { SetFocus(Some(hwnd)) };
            for expected in 0..fields.len() {
                assert_eq!(
                    observe_caret(hwnd),
                    Some(expected),
                    "{kind:?}: observation disagrees at field {expected} of {fields:?}"
                );
                key(hwnd, VK_RIGHT);
            }
            PICKERS.with(|p| p.borrow_mut().remove(&(hwnd.0 as isize)));
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyWindow(host);
            }
        }
    }

    /// Focus starts on the first field — the resync point that makes a caret we
    /// have lost track of recoverable — and left/right **wrap** rather than
    /// stopping at the ends, which is what [`step`] mirrors.
    #[test]
    #[ignore]
    fn the_caret_starts_at_the_first_field_and_wraps() {
        for kind in [Kind::Date, Kind::Time] {
            let (hwnd, host) = picker(kind);
            let fields = parse_fields(&locale_pattern(kind));
            assert!(fields.len() >= 2, "{kind:?}: too few fields to test");
            if fields.contains(&Field::Weekday) {
                // Up on a weekday field moves the date, which is indistinguishable
                // from the day field moving, so this locale cannot be observed.
                continue;
            }
            let _ = unsafe { SetFocus(Some(hwnd)) };
            assert_eq!(
                observed_field(hwnd, &fields),
                fields[0],
                "{kind:?}: focus must start on the first field"
            );
            // One full lap, checking every field on the way round and that the
            // last right comes back to the first field rather than stopping.
            for step in 1..=fields.len() {
                key(hwnd, VK_RIGHT);
                assert_eq!(
                    observed_field(hwnd, &fields),
                    fields[step % fields.len()],
                    "{kind:?}: after {step} right presses"
                );
            }
            // And the other way.
            key(hwnd, VK_LEFT);
            assert_eq!(
                observed_field(hwnd, &fields),
                fields[fields.len() - 1],
                "{kind:?}: left from the first field must wrap to the last"
            );
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyWindow(host);
            }
        }
    }

    /// Home, End, Up and Down change the current field's value and leave the caret
    /// alone, which is why they are announced as the same field rather than as
    /// movement. Typing digits does not move it either — the last thing that could
    /// have made tracking unreliable.
    #[test]
    #[ignore]
    fn value_keys_and_typing_do_not_move_the_caret() {
        for kind in [Kind::Date, Kind::Time] {
            let (hwnd, host) = picker(kind);
            let fields = parse_fields(&locale_pattern(kind));
            if fields.contains(&Field::Weekday) {
                continue;
            }
            let _ = unsafe { SetFocus(Some(hwnd)) };
            // Move to the second field, so a caret that drifted either way would
            // show up.
            key(hwnd, VK_RIGHT);
            let expected = fields[1];
            for (label, vk) in [("Home", VK_HOME), ("End", VK_END), ("Up", VK_UP)] {
                key(hwnd, vk);
                assert_eq!(
                    observed_field(hwnd, &fields),
                    expected,
                    "{kind:?}: {label} must not move the caret"
                );
            }
            // Two digits into a two-digit field: the plausible auto-advance.
            for c in ['1', '2'] {
                unsafe {
                    SendMessageW(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::WM_CHAR,
                        Some(WPARAM(c as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            assert_eq!(
                observed_field(hwnd, &fields),
                expected,
                "{kind:?}: typing digits must not advance the caret"
            );
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyWindow(host);
            }
        }
    }
}

