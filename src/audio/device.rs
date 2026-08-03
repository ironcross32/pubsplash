//! WASAPI device discovery for the source-edit UI and capture setup, plus the
//! process lookup that binds an Application source to a running executable.

use std::collections::{HashMap, HashSet};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use wasapi::{DeviceEnumerator, DeviceState, Direction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// Must be called once on every thread that touches WASAPI.
pub fn ensure_com_initialized() {
    // Ignore "already initialized" results.
    let _ = wasapi::initialize_mta();
}

/// Lists active capture devices (microphones).
pub fn capture_devices() -> Vec<DeviceInfo> {
    ensure_com_initialized();
    let mut devices = Vec::new();
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return devices;
    };
    let Ok(collection) = enumerator.get_device_collection(&Direction::Capture) else {
        return devices;
    };
    for device in &collection {
        let Ok(device) = device else { continue };
        if let (Ok(id), Ok(name)) = (device.get_id(), device.get_friendlyname()) {
            devices.push(DeviceInfo { id, name });
        }
    }
    devices
}

/// Resolves a configured device id to a WASAPI device, or the default capture
/// device when `id` is `None`.
///
/// A configured device is never silently swapped for a different one: going on
/// air through the laptop's built-in microphone because the good one was a
/// moment late to enumerate is worse than going on air silent. The caller
/// retries instead (see `capture::spawn`).
///
/// The state check is the point of this function. `IMMDeviceEnumerator::
/// GetDevice` happily returns endpoints in *any* state, including `Unplugged`
/// and `NotPresent`, and `Activate` on one of those fails with a bare
/// `ERROR_FILE_NOT_FOUND` that reads like a bug in us. USB interfaces sit in
/// exactly that state for a moment while Windows brings them up, which is a
/// window Pubsplash lands in because it opens its sources within a few hundred
/// milliseconds of launch. Rejecting a non-`Active` endpoint here turns that
/// into an accurate, retryable answer.
pub fn capture_device(id: Option<&str>) -> Result<wasapi::Device, String> {
    ensure_com_initialized();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let Some(id) = id else {
        return enumerator
            .get_default_device(&Direction::Capture)
            .map_err(|e| format!("opening the default microphone: {e}"));
    };
    let device = enumerator
        .get_device(id)
        .map_err(|e| format!("looking up the configured microphone: {e}"))?;
    match device.get_state() {
        Ok(DeviceState::Active) => Ok(device),
        Ok(state) => Err(format!("the configured microphone is {state:?}")),
        Err(e) => Err(format!("reading the configured microphone's state: {e}")),
    }
}

/// The default render device, used by the monitoring output.
pub fn default_render_device() -> Result<wasapi::Device, String> {
    ensure_com_initialized();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| e.to_string())
}

/// A running process matched to an Application source's configured name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProcess {
    pub pid: u32,
    /// The executable's file name as the OS reports it, e.g. `nvda.exe`.
    pub exe: String,
    /// The product name from the executable's version resource (e.g. `NVDA`),
    /// falling back to `exe` when the file carries no description.
    pub display_name: String,
}

/// Matches an Application source's configured name against a process name:
/// case-insensitive, with or without the `.exe` suffix.
fn matches(configured: &str, process_name: &str) -> bool {
    let wanted = configured.trim();
    !wanted.is_empty() && matches_key(wanted, process_name)
}

/// [`matches`] against an already-trimmed configured name.
///
/// Allocation-free on purpose: `resolve_apps` calls this once per running
/// process per configured Application name, on the UI thread, every couple of
/// seconds — it used to lowercase both strings and format a third every time.
fn matches_key(wanted: &str, process_name: &str) -> bool {
    if process_name.eq_ignore_ascii_case(wanted) {
        return true;
    }
    // The configured name may omit the `.exe` the OS reports. Splitting on the
    // last four bytes is safe once they are known to be the ASCII ".exe".
    let bytes = process_name.as_bytes();
    let Some(stem_len) = bytes.len().checked_sub(4) else {
        return false;
    };
    bytes[stem_len..].eq_ignore_ascii_case(b".exe")
        && process_name[..stem_len].eq_ignore_ascii_case(wanted)
}

/// A process as `choose_pid` needs to see it: `(pid, parent pid, exe name)`.
///
/// A flat slice rather than the `sysinfo` map so the selection rules can be
/// tested without a process table — the same reason `app_list::candidates`
/// takes pid sets instead of doing its own COM.
pub type ProcRow = (u32, Option<u32>, String);

/// How far `tree_root` will walk before giving up. A parent pid from the
/// toolhelp snapshot is only as good as the moment it was taken: a parent that
/// exited can have had its pid handed to a *new* process, which is how a
/// "parent" chain can point at a descendant and loop. The visited set below is
/// the real guard; this is the belt to its braces.
const MAX_PARENT_HOPS: usize = 16;

/// `pid -> (parent pid, exe name)`, so the parent walk is a lookup rather than
/// a scan. Built once per caller and shared by [`tree_root`] and [`roots_for`].
fn parent_index(procs: &[ProcRow]) -> HashMap<u32, (Option<u32>, &str)> {
    procs
        .iter()
        .map(|(pid, parent, name)| (*pid, (*parent, name.as_str())))
        .collect()
}

/// The distinct process trees carrying `key`, in first-seen order.
fn roots_for(index: &HashMap<u32, (Option<u32>, &str)>, procs: &[ProcRow], key: &str) -> Vec<u32> {
    let mut roots: Vec<u32> = Vec::new();
    for (pid, _, name) in procs {
        if !matches_key(key, name) {
            continue;
        }
        let root = tree_root(index, key, *pid);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// Walks `pid` up to the topmost process that still carries the same executable
/// name, which for a browser or an Electron app is the one process the user
/// thinks of as the application.
///
/// The walk stops at the first parent that is absent from the snapshot or named
/// something else, so an app launched from `explorer.exe` roots at itself.
fn tree_root(index: &HashMap<u32, (Option<u32>, &str)>, key: &str, pid: u32) -> u32 {
    let mut current = pid;
    let mut seen = vec![pid];
    for _ in 0..MAX_PARENT_HOPS {
        let Some((parent, _)) = index.get(&current) else {
            break;
        };
        let Some(parent) = *parent else { break };
        // A pid that is already on the path means the snapshot's parent links
        // are stale (pid reuse). Whatever we have is as good as it gets.
        if seen.contains(&parent) {
            break;
        }
        match index.get(&parent) {
            Some((_, name)) if matches_key(key, name) => {
                seen.push(parent);
                current = parent;
            }
            _ => break,
        }
    }
    current
}

/// Picks the one pid an Application source should capture for `key`.
///
/// This is the whole reason Chromium and Electron apps work at all. Capture
/// opens the pid with `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`
/// (`capture::run`), which covers the target **and its descendants** — so the
/// pid has to be the *root* of the app's process tree. Chromium plays every
/// sound from a `--type=utility` child, and rooting the tree at a renderer
/// instead of the browser captures nothing at all, silently: the loopback
/// client opens perfectly happily and simply never delivers a frame. This used
/// to take the first match in `sysinfo`'s `HashMap` order, which for a browser
/// with a dozen processes is a coin flip re-tossed on every poll.
///
/// The rules, in order:
///
/// 1. **Keep what we chose last time**, as long as it is still alive, still
///    named `key`, and still a root. The pid a source captures must not move
///    on its own: `ui::App::absorb_apps` reports a changed pid as
///    `capture_changed`, and that respawns *every* capture thread in the app.
/// 2. **Root every match** and dedupe. One root — the overwhelmingly common
///    case, including every browser — and there is nothing to decide.
/// 3. **Several roots** means several independent instances (two Brave windows
///    launched separately, two copies of an Electron app). Prefer the tree that
///    holds a render audio session, then the one with a visible window, then
///    the lowest pid so the answer is at least deterministic.
fn choose_pid(
    procs: &[ProcRow],
    key: &str,
    audio: &HashSet<u32>,
    windowed: &HashSet<u32>,
    previous: Option<u32>,
) -> Option<u32> {
    let index = parent_index(procs);
    let mut roots = roots_for(&index, procs, key);
    match roots.len() {
        0 => return None,
        1 => return Some(roots[0]),
        _ => {}
    }

    // Rule 1 only matters once there is a choice to be made: with a single root
    // the answer is already stable.
    if let Some(previous) = previous
        && roots.contains(&previous)
    {
        return Some(previous);
    }

    // Which pids belong to which root, so a session held by a child counts for
    // the tree it is in.
    let owner = |pid: u32| -> Option<u32> {
        let mut current = pid;
        for _ in 0..MAX_PARENT_HOPS {
            if roots.contains(&current) {
                return Some(current);
            }
            let (parent, _) = index.get(&current)?;
            current = (*parent)?;
        }
        None
    };
    let sounding: HashSet<u32> = audio.iter().filter_map(|pid| owner(*pid)).collect();
    let showing: HashSet<u32> = windowed.iter().filter_map(|pid| owner(*pid)).collect();

    roots.sort_unstable();
    roots
        .iter()
        .find(|root| sounding.contains(root))
        .or_else(|| roots.iter().find(|root| showing.contains(root)))
        .or_else(|| roots.first())
        .copied()
}

/// Resolves every configured Application name in one process enumeration.
/// The returned map is keyed by the configured name, lowercased and trimmed;
/// names that are blank or not currently running are simply absent.
///
/// Which pid a name resolves to is [`choose_pid`]'s decision; everything here
/// is about paying for it once. The two pid sets it may need are COM and
/// `EnumWindows`, so they are enumerated lazily and only if some name really is
/// ambiguous — for a single-process app this costs exactly what it used to.
pub fn resolve_apps(names: &[String]) -> HashMap<String, AppProcess> {
    let mut found = HashMap::new();
    let wanted: Vec<&String> = names.iter().filter(|n| !n.trim().is_empty()).collect();
    if wanted.is_empty() {
        return found;
    }
    // The `System` is kept between calls: this runs off the UI pump every
    // couple of seconds, and a fresh one would re-open every process to read
    // its exe path. Reusing it means `OnlyIfNotSet` really does skip processes
    // already seen, leaving only the enumeration itself.
    let mut system = lock_system();
    // Only exe paths are needed, so skip the (much costlier) default refresh.
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    // `parent()` comes straight out of the toolhelp snapshot, so it is
    // populated whatever the refresh kind asks for — no extra cost here.
    let procs: Vec<ProcRow> = system
        .processes()
        .iter()
        .map(|(pid, process)| {
            (
                pid.as_u32(),
                process.parent().map(|p| p.as_u32()),
                process.name().to_string_lossy().to_string(),
            )
        })
        .collect();

    // The lookup keys depend only on the configured names, so they are built
    // once rather than once per (process x name).
    let mut keys: Vec<String> = wanted
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    keys.sort_unstable();
    keys.dedup();

    // The tiebreakers are COM and `EnumWindows`; `choose_pid` only consults
    // them when a name has more than one root, so enumerate them once, and
    // only if some name actually does. For a single-process app — and for a
    // browser, which is one tree however many processes it has — both stay
    // empty and this poll costs exactly what it used to.
    let index = parent_index(&procs);
    let (audio, windowed) = if keys
        .iter()
        .any(|key| roots_for(&index, &procs, key).len() > 1)
    {
        (
            super::app_list::session_pids(),
            super::app_list::windowed_pids(),
        )
    } else {
        (HashSet::new(), HashSet::new())
    };

    let mut pins = lock_pins();
    for key in &keys {
        let Some(pid) = choose_pid(&procs, key, &audio, &windowed, pins.get(key).copied()) else {
            pins.remove(key);
            continue;
        };
        let Some((_, _, process_name)) = procs.iter().find(|(p, _, _)| *p == pid) else {
            continue;
        };
        if pins.insert(key.clone(), pid) != Some(pid) {
            let total = procs.iter().filter(|(_, _, n)| matches_key(key, n)).count();
            log::info!("Application {key:?} resolved to pid {pid} (of {total} matching processes)");
        }
        let display_name = system
            .process(sysinfo::Pid::from_u32(pid))
            .and_then(|p| p.exe())
            .and_then(friendly_name)
            .unwrap_or_else(|| process_name.clone());
        found.insert(
            key.clone(),
            AppProcess {
                pid,
                exe: process_name.clone(),
                display_name,
            },
        );
    }
    found
}

/// Matches an Application source's configured name against a process name,
/// for callers outside this module (the app picker's preselect).
pub fn name_matches(configured: &str, process_name: &str) -> bool {
    matches(configured, process_name)
}

/// Every running process as `(pid, exe file name, exe path)`, for the app
/// picker. Unlike `resolve_apps` this returns the whole table, so it is the
/// caller's job to filter it down to things a user would recognize.
///
/// Shares `SYSTEM` (and therefore the once-per-process exe path read) with
/// `resolve_apps`, so opening the picker costs no more than a pump refresh.
pub fn list_processes() -> Vec<(u32, String, Option<PathBuf>)> {
    let mut system = lock_system();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| {
            (
                pid.as_u32(),
                process.name().to_string_lossy().to_string(),
                process.exe().map(Path::to_path_buf),
            )
        })
        .collect()
}

/// Finds the PID of a running process by executable name (case-insensitive,
/// with or without `.exe`).
pub fn find_process(name: &str) -> Option<u32> {
    let key = name.trim().to_ascii_lowercase();
    resolve_apps(std::slice::from_ref(&name.to_string()))
        .get(&key)
        .map(|app| app.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, parent: u32, name: &str) -> ProcRow {
        (pid, (parent != 0).then_some(parent), name.to_string())
    }

    fn pids(list: &[u32]) -> HashSet<u32> {
        list.iter().copied().collect()
    }

    /// The Brave process table from the machine this bug was found on, minus a
    /// few interchangeable renderers: one browser process launched from the
    /// shell, everything else a child of it. Chromium's audio really does come
    /// out of a `--type=utility` child, and the browser is the only one with a
    /// window — so both tiebreakers would point at different processes if the
    /// tree walk did not settle it first.
    fn brave() -> Vec<ProcRow> {
        vec![
            row(26312, 0, "explorer.exe"),
            row(33884, 26312, "brave.exe"), // the browser process
            row(32656, 33884, "brave.exe"), // crashpad-handler
            row(30416, 33884, "brave.exe"), // gpu-process
            row(14544, 33884, "brave.exe"), // utility: the audio service
            row(39684, 33884, "brave.exe"), // renderer
            row(27076, 33884, "brave.exe"), // renderer
        ]
    }

    /// The whole bug: `include_tree` covers descendants, so anything but the
    /// browser process captures silence.
    #[test]
    fn a_browser_resolves_to_the_root_of_its_process_tree() {
        let chosen = choose_pid(
            &brave(),
            "brave.exe",
            &pids(&[14544]),
            &pids(&[33884]),
            None,
        );
        assert_eq!(chosen, Some(33884));
    }

    /// The configured name may omit the `.exe`, and the tree walk has to use
    /// the same comparison the match did or every process is its own root.
    #[test]
    fn the_root_walk_accepts_a_name_without_its_extension() {
        assert_eq!(
            choose_pid(&brave(), "brave", &HashSet::new(), &HashSet::new(), None),
            Some(33884)
        );
    }

    /// One process, no children: the answer it always gave, still.
    #[test]
    fn a_single_process_app_resolves_to_itself() {
        let procs = vec![row(26312, 0, "explorer.exe"), row(900, 26312, "mpv.exe")];
        assert_eq!(
            choose_pid(&procs, "mpv.exe", &HashSet::new(), &HashSet::new(), None),
            Some(900)
        );
    }

    #[test]
    fn a_name_that_is_not_running_resolves_to_nothing() {
        assert_eq!(
            choose_pid(
                &brave(),
                "firefox.exe",
                &HashSet::new(),
                &HashSet::new(),
                None
            ),
            None
        );
    }

    /// Two copies of the same app started separately are two roots, and the
    /// user means the one making a sound — even when the session belongs to a
    /// child rather than the root itself.
    #[test]
    fn between_two_instances_the_sounding_tree_wins() {
        let mut procs = brave();
        procs.push(row(41002, 26312, "brave.exe")); // a second, silent browser
        procs.push(row(41100, 41002, "brave.exe"));
        // Lowest pid would pick the silent one; the session is on 14544, a
        // child of 33884.
        let chosen = choose_pid(&procs, "brave.exe", &pids(&[14544]), &HashSet::new(), None);
        assert_eq!(chosen, Some(33884));
    }

    /// With nothing sounding, a visible window is the next best evidence of
    /// which instance the user means.
    #[test]
    fn between_two_silent_instances_the_windowed_tree_wins() {
        let mut procs = brave();
        procs.push(row(11000, 26312, "brave.exe"));
        let chosen = choose_pid(&procs, "brave.exe", &HashSet::new(), &pids(&[33884]), None);
        assert_eq!(chosen, Some(33884));
    }

    /// Neither tiebreaker says anything: the answer still must not depend on
    /// process-table iteration order, or the pid flaps and every capture
    /// thread in the app is respawned.
    #[test]
    fn an_undecidable_tie_is_broken_deterministically() {
        let mut procs = brave();
        procs.push(row(11000, 26312, "brave.exe"));
        let forwards = choose_pid(&procs, "brave.exe", &HashSet::new(), &HashSet::new(), None);
        procs.reverse();
        let backwards = choose_pid(&procs, "brave.exe", &HashSet::new(), &HashSet::new(), None);
        assert_eq!(forwards, Some(11000));
        assert_eq!(forwards, backwards);
    }

    /// Rule 1: a live choice is kept even once something else starts sounding,
    /// because moving the pid tears down and respawns every capture thread.
    #[test]
    fn a_previous_choice_is_kept_while_it_is_still_a_root() {
        let mut procs = brave();
        procs.push(row(41002, 26312, "brave.exe"));
        let chosen = choose_pid(
            &procs,
            "brave.exe",
            &pids(&[14544]), // sounding under 33884
            &HashSet::new(),
            Some(41002),
        );
        assert_eq!(chosen, Some(41002));
    }

    /// ...but only while it is still there. A pin for an instance that has
    /// exited must not survive it.
    #[test]
    fn a_previous_choice_that_has_exited_is_re_picked() {
        let mut procs = brave();
        procs.push(row(41002, 26312, "brave.exe"));
        let chosen = choose_pid(
            &procs,
            "brave.exe",
            &pids(&[14544]),
            &HashSet::new(),
            Some(50000),
        );
        assert_eq!(chosen, Some(33884));
    }

    /// A pin naming a process that is now a *child* (its own parent restarted
    /// into a recycled pid) is not a root, so it is dropped rather than
    /// capturing a subtree of the app.
    #[test]
    fn a_previous_choice_that_is_no_longer_a_root_is_dropped() {
        let mut procs = brave();
        procs.push(row(41002, 26312, "brave.exe"));
        let chosen = choose_pid(
            &procs,
            "brave.exe",
            &HashSet::new(),
            &pids(&[33884]),
            Some(39684), // a renderer under 33884
        );
        assert_eq!(chosen, Some(33884));
    }

    /// Parent pids come from a snapshot and are recycled, so a chain really can
    /// close on itself. Terminating matters more than the answer.
    #[test]
    fn a_cyclic_parent_chain_terminates() {
        let procs = vec![
            row(100, 200, "loop.exe"),
            row(200, 300, "loop.exe"),
            row(300, 100, "loop.exe"),
        ];
        assert!(choose_pid(&procs, "loop.exe", &HashSet::new(), &HashSet::new(), None).is_some());
    }

    #[test]
    fn a_self_parented_process_terminates() {
        let procs = vec![row(100, 100, "odd.exe")];
        assert_eq!(
            choose_pid(&procs, "odd.exe", &HashSet::new(), &HashSet::new(), None),
            Some(100)
        );
    }

    /// Prints what a name really resolves to on this machine, against the whole
    /// process table — the only way to check the tree walk on a live browser
    /// without launching the app. Ignored: the answer depends on what is
    /// running. Run as
    /// `cargo test prints_the_real_resolution -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn prints_the_real_resolution() {
        for name in [
            "brave", "chrome", "msedge", "spotify", "discord", "explorer",
        ] {
            let key = name.to_ascii_lowercase();
            let apps = resolve_apps(&[name.to_string()]);
            let Some(app) = apps.get(&key) else {
                continue;
            };
            let mut system = lock_system();
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                sysinfo::ProcessRefreshKind::nothing(),
            );
            let matching: Vec<(u32, Option<u32>)> = system
                .processes()
                .iter()
                .filter(|(_, p)| matches_key(&key, &p.name().to_string_lossy()))
                .map(|(pid, p)| (pid.as_u32(), p.parent().map(|p| p.as_u32())))
                .collect();
            let parent = system
                .process(sysinfo::Pid::from_u32(app.pid))
                .and_then(|p| p.parent())
                .map(|p| p.as_u32());
            println!(
                "{name}: {} processes -> pid {} (parent {parent:?}, {})",
                matching.len(),
                app.pid,
                app.display_name
            );
            // The whole point: nothing else with this name may be its parent.
            assert!(
                !matching.iter().any(|(pid, _)| Some(*pid) == parent),
                "{name} resolved to {} whose parent {parent:?} is also a {name}",
                app.pid
            );
        }
    }

    /// The real table, for the shape rather than the value: whatever is picked
    /// for a running multi-process app must be a root, and must be the same
    /// answer twice in a row.
    #[test]
    fn the_real_resolution_is_stable_and_roots_its_tree() {
        let names = vec!["explorer".to_string()];
        let Some(first) = resolve_apps(&names).get("explorer").map(|a| a.pid) else {
            return; // a bare service session with no shell
        };
        assert_eq!(
            resolve_apps(&names).get("explorer").map(|a| a.pid),
            Some(first)
        );
    }

    /// Explorer is always running on a desktop Windows session and always
    /// carries a version resource, so it exercises the whole path: name match
    /// without the `.exe` the user may not have typed, plus the friendly name.
    #[test]
    fn resolves_a_running_process_to_its_friendly_name() {
        let apps = resolve_apps(&["explorer".to_string()]);
        let Some(app) = apps.get("explorer") else {
            // No desktop shell (a bare service session); nothing to assert.
            return;
        };
        assert_eq!(app.exe.to_ascii_lowercase(), "explorer.exe");
        assert!(app.pid != 0);
        assert_eq!(app.display_name, "Windows Explorer");
    }

    /// This runs on the UI thread every couple of seconds, so a repeat poll
    /// must not be something a screen-reader user would feel.
    #[test]
    fn repeat_polls_are_cheap() {
        let names = vec!["explorer".to_string()];
        resolve_apps(&names);
        let start = std::time::Instant::now();
        for _ in 0..5 {
            resolve_apps(&names);
        }
        let each = start.elapsed() / 5;
        assert!(
            each < std::time::Duration::from_millis(50),
            "{each:?} per poll"
        );
    }

    /// Spotify's version resource really does read `Spotify\0` followed by
    /// stray bytes. Letting those through hands wx a string containing a NUL,
    /// which panics `ListBox::append` inside an event handler — where wxdragon
    /// swallows the panic, so the list simply stops halfway.
    #[test]
    fn a_padded_version_string_stops_at_its_first_nul() {
        assert_eq!(
            first_string("Spotify\0\u{38}\u{16}\u{1}FileV"),
            Some("Spotify".to_string())
        );
        assert_eq!(
            first_string("Windows Explorer"),
            Some("Windows Explorer".into())
        );
        assert_eq!(first_string("  NVDA \0"), Some("NVDA".to_string()));
        assert_eq!(first_string("\0anything"), None);
        assert_eq!(first_string("   "), None);
    }

    /// No name that reaches a wx control may contain a NUL, whatever the
    /// executable's resource looks like.
    #[test]
    fn resolved_names_are_safe_to_hand_to_wx() {
        for app in super::super::app_list::list_apps() {
            assert!(!app.display_name.contains('\0'), "{:?}", app.display_name);
            assert!(!app.exe.contains('\0'), "{:?}", app.exe);
        }
    }

    #[test]
    fn a_process_that_is_not_running_is_absent() {
        assert!(resolve_apps(&["definitely-not-a-real-program".to_string()]).is_empty());
    }
}

/// The process table, reused across refreshes so exe paths are read once per
/// process rather than once per poll.
static SYSTEM: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();

/// Locks a `static` cache, recovering from poisoning.
///
/// Bailing out on a poisoned lock would be the worst possible answer here: every
/// later call would report that nothing is running, so the app picker would come
/// up empty and every Application source would relabel itself "(not running)"
/// for the rest of the session — with nothing said about why. The data behind
/// these locks is a cache, so carrying on with it is safe; losing the ability to
/// see any process is not.
pub fn lock_recovering<'a, T>(mutex: &'a Mutex<T>, what: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("{what} lock was poisoned by an earlier panic; recovering");
            mutex.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn lock_system() -> std::sync::MutexGuard<'static, sysinfo::System> {
    lock_recovering(
        SYSTEM.get_or_init(|| Mutex::new(sysinfo::System::new())),
        "the process table",
    )
}

/// The pid last chosen for each configured Application name, keyed exactly as
/// [`resolve_apps`] keys its result. See rule 1 of [`choose_pid`]: a source's
/// pid moving on its own respawns every capture thread in the app, so once a
/// tree has been picked it is kept until it exits.
static LAST_CHOICE: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();

fn lock_pins() -> std::sync::MutexGuard<'static, HashMap<String, u32>> {
    lock_recovering(
        LAST_CHOICE.get_or_init(|| Mutex::new(HashMap::new())),
        "the resolved-process cache",
    )
}

/// Cache of version-resource lookups, keyed by executable path. Parsing the
/// resource is comparatively expensive and the answer never changes for a
/// given file, but this runs on the UI pump every couple of seconds.
static DESCRIPTIONS: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();

/// The name users recognize for an executable, from its version resource.
/// `None` when the file has no version resource, which is common for console
/// tools and portable builds.
///
/// Neither of the two candidate fields wins outright: `nvda.exe` describes
/// itself as "NVDA application (has UIAccess)" but has product "NVDA", while
/// `explorer.exe` is the other way round ("Windows Explorer" against the
/// product "Microsoft® Windows® Operating System"). This name is spoken on
/// every visit to the strip, so take whichever is shorter.
pub(crate) fn friendly_name(path: &Path) -> Option<String> {
    let cache = DESCRIPTIONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = lock_recovering(cache, "the executable description cache").get(path) {
        return cached.clone();
    }
    let name = read_version_strings(path, &["FileDescription", "ProductName"])
        .into_iter()
        .flatten()
        .min_by_key(|s| s.chars().count());
    lock_recovering(cache, "the executable description cache")
        .insert(path.to_path_buf(), name.clone());
    name
}

/// The first NUL-terminated string in a raw version-resource value, trimmed;
/// `None` when it is blank.
///
/// The length `VerQueryValueW` reports is the size of the *value*, not of the
/// string inside it, and some executables pad theirs with whatever follows in
/// the resource: Spotify's `ProductName` comes back as `Spotify\0` plus a few
/// stray bytes. Keeping those bytes produces a `String` with an interior NUL,
/// which is not merely ugly — every name here reaches a wx control, and
/// `ListBox::append` panics on a string it cannot turn into a `CString`. That
/// panic is then swallowed by wxdragon's event-handler `catch_unwind`, so the
/// only visible effect is a list that stops halfway and a dialog that saves
/// nothing. Cut at the first NUL.
fn first_string(raw: &str) -> Option<String> {
    let text = raw.split('\0').next().unwrap_or("").trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Reads the named `\StringFileInfo\` entries from a file's version resource,
/// in the order requested. Entries that are missing or blank come back `None`.
fn read_version_strings(path: &Path, fields: &[&str]) -> Vec<Option<String>> {
    read_version_strings_inner(path, fields).unwrap_or_else(|| vec![None; fields.len()])
}

fn read_version_strings_inner(path: &Path, fields: &[&str]) -> Option<Vec<Option<String>>> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows_core::PCWSTR;

    /// The `\VarFileInfo\Translation` entry: which language/codepage the
    /// string table under `\StringFileInfo\` is filed under.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct LangCodepage {
        language: u16,
        codepage: u16,
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives every call below,
    // and `block` is sized by the API itself before it is filled.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size,
            block.as_mut_ptr() as *mut std::ffi::c_void,
        )
        .ok()?;

        let mut translations: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let key: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
        if !VerQueryValueW(
            block.as_ptr() as *const std::ffi::c_void,
            PCWSTR(key.as_ptr()),
            &mut translations,
            &mut len,
        )
        .as_bool()
            || len < std::mem::size_of::<LangCodepage>() as u32
        {
            return None;
        }
        let translation = *(translations as *const LangCodepage);

        let read = |field: &str| -> Option<String> {
            let sub_block: Vec<u16> = format!(
                "\\StringFileInfo\\{:04x}{:04x}\\{field}\0",
                translation.language, translation.codepage
            )
            .encode_utf16()
            .collect();
            let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut chars: u32 = 0;
            if !VerQueryValueW(
                block.as_ptr() as *const std::ffi::c_void,
                PCWSTR(sub_block.as_ptr()),
                &mut value,
                &mut chars,
            )
            .as_bool()
                || chars == 0
            {
                return None;
            }
            let text = std::slice::from_raw_parts(value as *const u16, chars as usize);
            first_string(&String::from_utf16_lossy(text))
        };
        Some(fields.iter().map(|field| read(field)).collect())
    }
}
