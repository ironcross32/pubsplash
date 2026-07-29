//! Runtime half of user-configurable shortcuts: catching the keys and running
//! the actions. The data model is [`crate::keybind`], the settings UI is
//! [`super::keybinds_ui`].
//!
//! ## Why the low-level hook
//!
//! There is no wx accelerator table in this app, and adding one would not help:
//! an accelerator only fires while our window has focus, and a control that
//! wants raw keys (the chat input, a mixer slider) eats them first. Worse, a
//! **global** binding is not something wx can express at all.
//!
//! So both capture and dispatch ride the app-wide `WH_KEYBOARD_LL` hook that
//! [`super::help`] already installs for F1 — that file is explicit that it is
//! "the one hook installed for the whole life of the app", and it already hosts
//! the F6 arm for [`super::panes`]. `help::keyboard_hook` calls into here in a
//! fixed order: capture first (while the Add-binding dialog's shortcut box has
//! focus, keys belong to it and nothing else), then F1, then F6, then dispatch.
//! F1 and F6 are consequently refused as bindings by the dialog rather than
//! being accepted and then silently shadowed.
//!
//! Riding the hook is also what makes the capture box work at all: ESC, ENTER,
//! TAB, the arrows and CTRL+letter are all swallowed by `::IsDialogMessage`
//! during wx's message *pre-processing*, before any handler on the control can
//! see them (`ui/slider_uia.rs` documents this at length). The hook runs ahead
//! of all of that.
//!
//! ## Threading
//!
//! The hook proc must touch nothing but atomics and its own mutexes — it runs in
//! the OS hook context and any `RefCell` borrow of `App` live elsewhere at that
//! moment would panic. So the hook matches against a flat [`SNAPSHOT`] refreshed
//! by [`reload`] on every keybinds edit, parks the matched action in [`PENDING`],
//! rings the idle doorbell, and returns; [`pump`] does the real work on the UI
//! thread from the 100 ms pump. Same shape as `panes.rs`.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetClassNameW, GetGUIThreadInfo};

use super::App;
use super::home::{self, StripTarget};
use crate::config::Config;
use crate::keybind::{BindAction, Chord};

/// Every binding, flattened for the hook. Rebuilt by [`reload`].
static SNAPSHOT: OnceLock<Mutex<Vec<(Chord, bool, BindAction)>>> = OnceLock::new();
/// Actions whose key has been pressed but which have not run yet.
static PENDING: OnceLock<Mutex<Vec<BindAction>>> = OnceLock::new();

fn snapshot() -> &'static Mutex<Vec<(Chord, bool, BindAction)>> {
    SNAPSHOT.get_or_init(|| Mutex::new(Vec::new()))
}

fn pending() -> &'static Mutex<Vec<BindAction>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// Re-reads the bindings the hook matches against. Call after any edit to
/// `config.keybinds`, and once at startup.
pub fn reload(config: &Config) {
    let binds = config
        .keybinds
        .binds
        .iter()
        .map(|b| (b.key, b.global, b.action.clone()))
        .collect();
    // Poisoning here would mean a panic inside the hook; keep running with what
    // we have rather than taking the app down over a shortcut table.
    *snapshot().lock().unwrap_or_else(|e| e.into_inner()) = binds;
}

// --- capture ---------------------------------------------------------------

/// Set while the Add-binding dialog's shortcut box has keyboard focus.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// The chord the user last pressed while capturing, packed by [`pack`].
static CAPTURED: AtomicU32 = AtomicU32::new(0);
/// Whether [`CAPTURED`] holds something [`pump`] has not shown yet.
static CAPTURED_NEW: AtomicBool = AtomicBool::new(false);

fn pack(chord: Chord) -> u32 {
    chord.vk & 0xFF
        | (chord.ctrl as u32) << 8
        | (chord.alt as u32) << 9
        | (chord.shift as u32) << 10
}

fn unpack(bits: u32) -> Chord {
    Chord {
        vk: bits & 0xFF,
        ctrl: bits & 0x100 != 0,
        alt: bits & 0x200 != 0,
        shift: bits & 0x400 != 0,
    }
}

pub fn begin_capture() {
    CAPTURED_NEW.store(false, Ordering::Relaxed);
    CAPTURING.store(true, Ordering::Relaxed);
}

pub fn end_capture() {
    CAPTURING.store(false, Ordering::Relaxed);
}

/// The chord captured since the last call, if any. Drains the flag.
pub fn take_captured() -> Option<Chord> {
    CAPTURED_NEW
        .swap(false, Ordering::Relaxed)
        .then(|| unpack(CAPTURED.load(Ordering::Relaxed)))
}

/// Physical modifier state. `GetKeyState` would report this thread's *queued*
/// state, which is not what the user is holding right now — `help.rs` makes the
/// same point about its SHIFT read.
fn modifiers() -> (bool, bool, bool) {
    let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
    (down(VK_CONTROL.0), down(VK_MENU.0), down(VK_SHIFT.0))
}

const VK_TAB: u32 = 0x09;
const VK_ESCAPE: u32 = 0x1B;
const VK_DELETE: u32 = 0x2E;

fn is_modifier_key(vk: u32) -> bool {
    matches!(
        vk as u16,
        x if x == VK_SHIFT.0 || x == VK_CONTROL.0 || x == VK_MENU.0
            || x == VK_LWIN.0 || x == VK_RWIN.0
    ) || matches!(vk, 0xA0..=0xA5)
}

/// Called from the hook before anything else. Returns `true` to swallow the key.
///
/// TAB is deliberately passed through: the shortcut box must stay escapable with
/// TAB and SHIFT+TAB, which is the whole reason a screen-reader user can leave
/// it at all.
pub fn capture_key(vk: u32) -> bool {
    if !CAPTURING.load(Ordering::Relaxed) {
        return false;
    }
    // Alt-tabbing away does not necessarily fire the box's kill-focus, and
    // swallowing every keystroke in whatever the user switched to would be the
    // worst bug this feature could have.
    if !super::help::foreground_is_ours() {
        return false;
    }
    if vk == VK_TAB {
        return false;
    }
    if is_modifier_key(vk) {
        // A modifier on its own is not a chord — but it must be *passed through*,
        // not swallowed.
        //
        // Returning 1 from a `WH_KEYBOARD_LL` hook stops the key being processed
        // any further, and that includes the async key-state table. Swallowing
        // the CTRL keydown here therefore meant `GetAsyncKeyState(VK_CONTROL)`
        // still read "up" when the letter arrived a moment later, and every
        // chord captured as a bare character with no modifiers on it.
        return false;
    }
    let chord = if vk == VK_ESCAPE || vk == VK_DELETE {
        // Both clear the binding. ESC in particular must not reach the dialog,
        // or it would close it instead.
        Chord::default()
    } else {
        let (ctrl, alt, shift) = modifiers();
        Chord::new(vk, ctrl, alt, shift)
    };
    CAPTURED.store(pack(chord), Ordering::Relaxed);
    CAPTURED_NEW.store(true, Ordering::Relaxed);
    true
}

// --- dispatch --------------------------------------------------------------

/// Whether the focused control is a text entry.
///
/// A binding on a bare character key (`K`, `7`) would otherwise make that key
/// untypable in the chat box — the hook swallows it before the control sees it.
/// Anything with a modifier, and every non-character key, still fires.
fn focus_is_text_entry() -> bool {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let focus = unsafe {
        if GetGUIThreadInfo(0, &mut info).is_ok() {
            info.hwndFocus
        } else {
            HWND::default()
        }
    };
    if focus.0.is_null() {
        return false;
    }
    let mut buffer = [0u16; 64];
    let written = unsafe { GetClassNameW(focus, &mut buffer) };
    if written <= 0 {
        return false;
    }
    let class = String::from_utf16_lossy(&buffer[..written as usize]);
    let class = class.to_ascii_lowercase();
    class == "edit" || class.starts_with("richedit") || class == "combobox"
}

/// Called from the hook. Returns `true` when a binding claimed the key, in which
/// case the caller swallows it and wakes the idle pump.
pub fn hook_key(vk: u32) -> bool {
    if is_modifier_key(vk) {
        return false;
    }
    let (ctrl, alt, shift) = modifiers();
    let pressed = Chord::new(vk, ctrl, alt, shift);

    let action = {
        let binds = snapshot().lock().unwrap_or_else(|e| e.into_inner());
        let Some((_, global, action)) = binds.iter().find(|(chord, _, _)| *chord == pressed) else {
            return false;
        };
        // Gating. A non-global binding fires only while the *main frame itself*
        // is foreground, which excludes our own modal dialogs and the plugin
        // editor frames — the pump timer keeps firing inside a modal loop, and
        // some actions open dialogs of their own. A global binding is allowed
        // the same case plus "we are not in the foreground at all"; under our
        // own modal it stays silent for exactly the same reason.
        let ours = super::help::foreground_is_ours();
        let main = super::help::foreground_is_main_frame();
        let allowed = if *global { main || !ours } else { main };
        if !allowed {
            return false;
        }
        if pressed.is_plain_character() && focus_is_text_entry() {
            return false;
        }
        action.clone()
    };

    pending()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(action);
    true
}

/// Called each pump tick. One uncontended lock unless a binding actually fired.
pub fn pump(app: &Rc<App>) {
    let actions: Vec<BindAction> = {
        let mut queue = pending().lock().unwrap_or_else(|e| e.into_inner());
        if queue.is_empty() {
            return;
        }
        std::mem::take(&mut *queue)
    };
    if app.shutting_down.get() {
        // Widgets are being torn down; touching them would be a use-after-free.
        return;
    }
    for action in actions {
        run(app, &action);
    }
}

// --- the actions -----------------------------------------------------------

/// Resolves a source identity name to its index in the **active** scene.
///
/// Bindings store the identity name rather than an index because the index means
/// nothing outside one scene. A name that is not in the current scene is not an
/// error — the user is simply somewhere else — so it announces and stops.
fn source_target(app: &Rc<App>, name: &str) -> Option<StripTarget> {
    let index = app
        .config
        .borrow()
        .scenes
        .active_scene()
        .and_then(|scene| scene.sources.iter().position(|s| s.name == name));
    match index {
        Some(index) => Some(StripTarget::Source(index)),
        None => {
            super::help::announce(&format!("{name} is not in the current scene"));
            None
        }
    }
}

fn bus_target(app: &Rc<App>, name: &str) -> Option<StripTarget> {
    let index = app
        .config
        .borrow()
        .buses
        .buses
        .iter()
        .position(|b| b.name == name);
    match index {
        Some(index) => Some(StripTarget::Bus(index)),
        None => {
            super::help::announce(&format!("There is no bus called {name}"));
            None
        }
    }
}

fn run(app: &Rc<App>, action: &BindAction) {
    match action {
        BindAction::ToggleStream => {
            if app.is_streaming_or_starting() {
                app.stop_streaming();
            } else if app.run.borrow().recording {
                // Same rule the disabled stream button expresses: streaming and a
                // standalone recording are mutually exclusive.
                super::help::announce("Cannot stream while a recording is running");
            } else {
                super::start_streaming(app);
            }
        }
        BindAction::ToggleRecording => {
            let recording = app.run.borrow().recording;
            if recording {
                app.stop_recording();
            } else if app.is_streaming_or_starting() {
                super::help::announce("Cannot start a recording while streaming");
            } else {
                app.start_recording();
            }
        }
        BindAction::NextScene => home::cycle_scene(app, true),
        BindAction::PreviousScene => home::cycle_scene(app, false),
        BindAction::SwitchScene { scene } => home::switch_to_scene_named(app, scene),
        BindAction::ToggleMonitorMaster => home::toggle_monitor_target(app, StripTarget::Master),
        BindAction::ToggleMonitorSource { source } => {
            if let Some(target) = source_target(app, source) {
                home::toggle_monitor_target(app, target);
            }
        }
        BindAction::ToggleMonitorBus { bus } => {
            if let Some(target) = bus_target(app, bus) {
                home::toggle_monitor_target(app, target);
            }
        }
        BindAction::ToggleMuteMaster => home::toggle_mute_target(app, StripTarget::Master),
        BindAction::ToggleMuteSource { source } => {
            if let Some(target) = source_target(app, source) {
                home::toggle_mute_target(app, target);
            }
        }
        BindAction::ToggleMuteBus { bus } => {
            if let Some(target) = bus_target(app, bus) {
                home::toggle_mute_target(app, target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_survive_the_atomic_packing() {
        for chord in [
            Chord::default(),
            Chord::plain(0x78),
            Chord::new(0xFF, true, true, true),
            Chord::new(b'M' as u32, true, false, true),
            Chord::new(b'K' as u32, false, true, false),
        ] {
            assert_eq!(unpack(pack(chord)), chord);
        }
    }

    #[test]
    fn the_modifier_keys_are_never_chords_of_their_own() {
        assert!(is_modifier_key(0xA0)); // left shift
        assert!(is_modifier_key(0xA5)); // right alt
        assert!(is_modifier_key(VK_CONTROL.0 as u32));
        assert!(is_modifier_key(VK_LWIN.0 as u32));
        assert!(!is_modifier_key(0x78)); // F9
        assert!(!is_modifier_key(b'K' as u32));
    }
}
