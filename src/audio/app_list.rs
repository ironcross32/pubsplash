//! The list of applications a user can pick as an Application source.
//!
//! Typing an executable name from memory is the one thing a screen-reader user
//! cannot verify, so the picker offers the running apps instead. "Running" is
//! deliberately narrower than "in the process table": what matters is whether
//! Windows has ever seen the process open an audio stream, which is exactly
//! what the WASAPI session list records — sessions stay enumerable while they
//! are `Inactive` or `Expired`, so an app that played a sound a minute ago is
//! still offered. Everything else the user might want (an app that has not made
//! a sound yet) is reachable through the wider view, keyed off having a visible
//! window, and anything still missing can be typed by hand.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::device;

/// A process the user could pick as an Application source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCandidate {
    /// The executable's file name, e.g. `firefox.exe`. This is what gets
    /// stored in `SourceKindConfig::Application`.
    pub exe: String,
    /// The name users recognize, from the version resource; falls back to `exe`.
    pub display_name: String,
    /// Whether this executable holds (or held) a render session — i.e. it has
    /// produced sound at least once since it started.
    pub has_audio: bool,
}

/// Processes worth showing in the picker, sorted by display name.
///
/// Both views come back in one list: the caller filters on `has_audio` for the
/// default "only apps that have played sound" view. Enumerating twice for the
/// checkbox would make the two views disagree whenever an app started playing
/// in between.
pub fn list_apps() -> Vec<AppCandidate> {
    let audio = session_pids();
    let windowed = windowed_pids();
    let procs = device::list_processes();
    candidates(&procs, &audio, &windowed, std::process::id())
}

/// Executables that are part of Windows itself rather than something a user
/// would think of as an application. Several of them do hold audio sessions
/// (`audiodg` is the audio engine; `svchost` hosts the shell's own sounds), so
/// the session list alone is not a sufficient filter. Lowercase, no `.exe`.
const SYSTEM_PROCESSES: &[&str] = &[
    "applicationframehost",
    "audiodg",
    "backgroundtaskhost",
    "backgroundtransferhost",
    "conhost",
    "csrss",
    "ctfmon",
    "dllhost",
    "dwm",
    "fontdrvhost",
    "lockapp",
    "lsass",
    "memory compression",
    "registry",
    "runtimebroker",
    "searchapp",
    "searchhost",
    "searchindexer",
    "services",
    "shellexperiencehost",
    "sihost",
    "smss",
    "spoolsv",
    "startmenuexperiencehost",
    "svchost",
    "system",
    "systemsettings",
    "taskhostw",
    "textinputhost",
    "wininit",
    "winlogon",
    "wmiprvse",
];

fn is_system_process(exe: &str) -> bool {
    let key = exe
        .trim()
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_string();
    SYSTEM_PROCESSES.contains(&key.as_str())
}

/// Filters and merges the raw process table into pickable candidates.
///
/// Kept free of COM so the rules are testable: everything Windows-specific is
/// already reduced to the two pid sets by the time this runs.
///
/// Multiple pids share one executable name — browsers in particular play audio
/// from a child utility process rather than the window they belong to — and the
/// config stores a name, not a pid. So rows are deduped by name and `has_audio`
/// is OR-ed across the group: `chrome.exe` counts as sounding when any of its
/// processes does. Capture then opens the resolved parent with `include_tree`,
/// which picks the children back up (see `capture::run`).
fn candidates(
    procs: &[(u32, String, Option<PathBuf>)],
    audio: &HashSet<u32>,
    windowed: &HashSet<u32>,
    own_pid: u32,
) -> Vec<AppCandidate> {
    // Keyed by lowercased exe name; the value keeps the first-seen spelling
    // plus a path to read a friendly name from once the row has survived.
    let mut merged: HashMap<String, (String, Option<PathBuf>, bool)> = HashMap::new();
    for (pid, exe, path) in procs {
        if *pid == own_pid || exe.trim().is_empty() || is_system_process(exe) {
            continue;
        }
        let has_audio = audio.contains(pid);
        if !has_audio && !windowed.contains(pid) {
            continue;
        }
        let entry = merged
            .entry(exe.to_ascii_lowercase())
            .or_insert_with(|| (exe.clone(), path.clone(), false));
        entry.2 |= has_audio;
        if entry.1.is_none() {
            entry.1 = path.clone();
        }
    }

    // Friendly names are read from the file's version resource, so resolve them
    // only for the handful of rows that survived rather than the whole table.
    let mut apps: Vec<AppCandidate> = merged
        .into_values()
        .map(|(exe, path, has_audio)| {
            let display_name = path
                .as_deref()
                .and_then(device::friendly_name)
                .unwrap_or_else(|| exe.clone());
            AppCandidate {
                exe,
                display_name,
                has_audio,
            }
        })
        .collect();
    apps.sort_by(|a, b| {
        a.display_name
            .to_ascii_lowercase()
            .cmp(&b.display_name.to_ascii_lowercase())
            .then_with(|| a.exe.to_ascii_lowercase().cmp(&b.exe.to_ascii_lowercase()))
    });
    apps
}

/// Pids with a render audio session on any active output device.
///
/// Every output is enumerated, not just the default one: a user with headphones
/// and speakers has apps on both, and an app missing from the picker because it
/// happens to be playing to the other endpoint would look like a bug.
fn session_pids() -> HashSet<u32> {
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        MMDeviceEnumerator, eRender,
    };
    use windows::Win32::Foundation::S_OK;
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows_core::Interface;

    let mut pids = HashSet::new();
    device::ensure_com_initialized();
    // SAFETY: plain COM calls; every interface pointer comes from a checked
    // `Result` and is dropped by windows-rs at the end of its scope.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("app list: creating the device enumerator: {e}");
                    return pids;
                }
            };
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return pids;
        };
        let count = devices.GetCount().unwrap_or(0);
        for i in 0..count {
            let Ok(device) = devices.Item(i) else { continue };
            let manager: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
                Ok(m) => m,
                Err(e) => {
                    log::debug!("app list: no session manager on output {i}: {e}");
                    continue;
                }
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else {
                continue;
            };
            let session_count = sessions.GetCount().unwrap_or(0);
            for s in 0..session_count {
                let Ok(control) = sessions.GetSession(s) else {
                    continue;
                };
                let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // The system-sounds session reports the pid of whatever service
                // is hosting it, which is not an app anyone wants to capture.
                // Note the exact comparison: this method answers "no" with
                // `S_FALSE`, which `is_ok` would also accept, and skipping on
                // that would silently drop every session on the machine.
                if control.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                if let Ok(pid) = control.GetProcessId()
                    && pid != 0
                {
                    pids.insert(pid);
                }
            }
        }
    }
    pids
}

/// Pids owning at least one visible, titled top-level window — the same signal
/// Task Manager uses to tell "Apps" from "Background processes", and what makes
/// the wider view short enough to arrow through.
fn windowed_pids() -> HashSet<u32> {
    use windows::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_core::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
    };

    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        // SAFETY: `param` is the `&mut HashSet` handed to `EnumWindows` below,
        // which outlives the enumeration.
        unsafe {
            let pids = &mut *(param.0 as *mut HashSet<u32>);
            if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid != 0 {
                    pids.insert(pid);
                }
            }
        }
        TRUE
    }

    let mut pids: HashSet<u32> = HashSet::new();
    // SAFETY: `visit` matches WNDENUMPROC and only touches `pids`, which is
    // borrowed for the duration of the (synchronous) call.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut pids as *mut HashSet<u32> as isize));
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, exe: &str) -> (u32, String, Option<PathBuf>) {
        (pid, exe.to_string(), None)
    }

    fn pids(list: &[u32]) -> HashSet<u32> {
        list.iter().copied().collect()
    }

    fn names(apps: &[AppCandidate]) -> Vec<&str> {
        apps.iter().map(|a| a.exe.as_str()).collect()
    }

    /// `svchost` hosts the shell's own sounds and `audiodg` *is* the audio
    /// engine, so both turn up in the session list. Neither is an application.
    #[test]
    fn system_processes_are_dropped_even_when_they_hold_a_session() {
        let procs = [
            proc(10, "svchost.exe"),
            proc(11, "audiodg.exe"),
            proc(12, "firefox.exe"),
        ];
        let apps = candidates(&procs, &pids(&[10, 11, 12]), &pids(&[]), 1);
        assert_eq!(names(&apps), ["firefox.exe"]);
    }

    /// Capturing ourselves would feed local TTS and cues back into the stream.
    #[test]
    fn pubsplash_itself_is_never_offered() {
        let procs = [proc(7, "pubsplash.exe"), proc(8, "spotify.exe")];
        let apps = candidates(&procs, &pids(&[7, 8]), &pids(&[7, 8]), 7);
        assert_eq!(names(&apps), ["spotify.exe"]);
    }

    /// An app that has not made a sound yet is only useful in the wider view,
    /// which the dialog reaches by ignoring `has_audio`.
    #[test]
    fn a_windowed_app_appears_but_is_not_marked_as_sounding() {
        let procs = [proc(20, "notepad.exe")];
        let apps = candidates(&procs, &pids(&[]), &pids(&[20]), 1);
        assert_eq!(names(&apps), ["notepad.exe"]);
        assert!(!apps[0].has_audio);
    }

    /// A screen reader or console player has no window at all, and is exactly
    /// the sort of thing users route into a stream.
    #[test]
    fn a_sounding_app_with_no_window_is_kept() {
        let procs = [proc(30, "nvda.exe")];
        let apps = candidates(&procs, &pids(&[30]), &pids(&[]), 1);
        assert_eq!(names(&apps), ["nvda.exe"]);
        assert!(apps[0].has_audio);
    }

    #[test]
    fn a_process_with_neither_sound_nor_a_window_is_dropped() {
        let procs = [proc(40, "some-daemon.exe")];
        assert!(candidates(&procs, &pids(&[]), &pids(&[]), 1).is_empty());
    }

    /// Browsers play audio from a child utility process that shares the parent's
    /// executable name. Since the config stores a name, the one row for that
    /// name has to inherit the child's sound.
    #[test]
    fn one_row_per_executable_inherits_sound_from_any_of_its_processes() {
        let procs = [
            proc(50, "chrome.exe"),
            proc(51, "chrome.exe"),
            proc(52, "chrome.exe"),
        ];
        // Only the utility process (51) has the session; only the browser
        // window (50) has a window.
        let apps = candidates(&procs, &pids(&[51]), &pids(&[50]), 1);
        assert_eq!(names(&apps), ["chrome.exe"]);
        assert!(apps[0].has_audio);
    }

    /// Rows are arrowed through by a screen-reader user, so the order must not
    /// depend on process-table iteration order.
    #[test]
    fn rows_are_sorted_by_display_name() {
        let procs = [
            proc(60, "zoom.exe"),
            proc(61, "Audacity.exe"),
            proc(62, "mpv.exe"),
        ];
        let apps = candidates(&procs, &pids(&[60, 61, 62]), &pids(&[]), 1);
        // No version resources in the fixtures, so display name == exe.
        assert_eq!(names(&apps), ["Audacity.exe", "mpv.exe", "zoom.exe"]);
    }

    /// Prints what this machine would offer, for eyeballing the filter against
    /// a real desktop. Ignored: the answer depends on what is running.
    #[test]
    #[ignore]
    fn print_the_real_list() {
        for app in list_apps() {
            let mark = if app.has_audio { "sound" } else { "     " };
            println!("[{mark}] {} ({})", app.display_name, app.exe);
        }
    }

    /// The real enumeration must not panic or hand back junk rows. It runs on
    /// the UI thread when the picker opens, so it also has to be quick.
    #[test]
    fn the_real_list_is_well_formed() {
        let start = std::time::Instant::now();
        let apps = list_apps();
        let elapsed = start.elapsed();
        if apps.is_empty() {
            // A bare session with no user apps at all; nothing to assert.
            return;
        }
        for app in &apps {
            assert!(!app.exe.trim().is_empty());
            assert!(!app.display_name.trim().is_empty());
            assert!(!is_system_process(&app.exe), "{:?} is a system process", app.exe);
        }
        assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    }
}
