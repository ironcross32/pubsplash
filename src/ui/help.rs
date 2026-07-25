//! Context-sensitive help: pressing **F1** on any control speaks a
//! hand-authored help string through the user's running screen reader.
//!
//! Three moving parts:
//!
//! 1. [`tag`] is called next to each control as it is built, stamping a stable
//!    help-id onto the control's window via `SetPropW`. The property dies with
//!    the window automatically, so rebuilt panels (the mixer) never leave stale
//!    ids behind.
//! 2. The authored messages live in `help.toml` at the repo root, embedded with
//!    `include_str!` and parsed once into an id -> message map. The dev binary
//!    `gen-help` round-trips that file from the `tag` call sites (see
//!    `src/bin/gen_help.rs`).
//! 3. A global low-level keyboard hook (mirroring the F6 hook in `fx_editor`)
//!    catches F1 app-wide, swallows it, and flags the pump. [`pump`] resolves the
//!    focused window to its id and announces the message via a UIA notification
//!    raised on the main frame's host provider — so NVDA/Narrator speak it,
//!    interrupting whatever they were saying.
//!
//! Threading: `tag`, `install_*`, `uninstall_*`, and `pump` all run on the UI
//! thread (thread-locals below live there); only the hook proc runs in the OS
//! hook context, and it touches nothing but two atomics.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    IRawElementProviderSimple, IRawElementProviderSimple_Impl, NotificationKind_Other,
    NotificationProcessing_ImportantMostRecent, ProviderOptions, ProviderOptions_ServerSideProvider,
    UIA_ControlTypePropertyId, UIA_NamePropertyId, UIA_PATTERN_ID, UIA_PROPERTY_ID,
    UIA_WindowControlTypeId, UiaClientsAreListening, UiaDisconnectProvider,
    UiaHostProviderFromHwnd, UiaRaiseNotificationEvent, UiaReturnRawElementProvider,
    UiaRootObjectId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_F1;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetPropW,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, SetPropW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_GETOBJECT, WM_KEYDOWN, WM_SYSKEYDOWN,
};
use windows::core::{BSTR, Error, IUnknown, PCWSTR, Result, implement, w};
use wxdragon::prelude::WxWidget;

/// Distinguishes our frame subclass from any other on the same window.
const FRAME_SUBCLASS_ID: usize = 0x0A12;

/// Window-property name carrying a control's interned help-id (index + 1).
const HELP_PROP: PCWSTR = w!("PubsplashHelpId");

/// Spoken when a control has no id, or an id with no authored message.
const GENERIC: &str = "No help available for this control.";

thread_local! {
    /// Interns help-ids to small indices so each can be stored as a single
    /// `SetPropW` handle value. Ids repeat (the mixer re-tags its strips on every
    /// rebuild), so the map dedupes and the vec stays tiny.
    static INTERN: RefCell<(Vec<String>, HashMap<String, usize>)> =
        RefCell::new((Vec::new(), HashMap::new()));

    /// The main frame (hwnd) and the server-side UIA provider installed on it,
    /// used to raise notification events. Set by [`install_announcer`].
    static ANNOUNCER: RefCell<Option<(isize, IRawElementProviderSimple)>> =
        const { RefCell::new(None) };
}

/// The installed low-level keyboard hook (as isize; 0 = not installed).
static HELP_HOOK: AtomicIsize = AtomicIsize::new(0);
/// Set by the hook when F1 is pressed in our app; read and cleared by [`pump`].
static HELP_REQUESTED: AtomicBool = AtomicBool::new(false);

fn intern(id: &str) -> usize {
    INTERN.with(|c| {
        let (vec, map) = &mut *c.borrow_mut();
        if let Some(&i) = map.get(id) {
            return i;
        }
        let i = vec.len();
        vec.push(id.to_string());
        map.insert(id.to_string(), i);
        i
    })
}

fn id_for(idx: usize) -> Option<String> {
    INTERN.with(|c| c.borrow().0.get(idx).cloned())
}

/// Gives `widget` a context-help id. Call beside `set_accessible_name` at each
/// control site. `label` is documentation for the `gen-help` scanner only and is
/// unused at runtime.
pub fn tag(widget: &dyn WxWidget, id: &str, _label: &str) {
    let hwnd = HWND(widget.get_handle());
    if hwnd.0.is_null() {
        return;
    }
    let handle = HANDLE((intern(id) as isize + 1) as *mut _);
    if let Err(e) = unsafe { SetPropW(hwnd, HELP_PROP, Some(handle)) } {
        log::warn!("help::tag: SetPropW failed for {id:?}: {e}");
    }
}

// --- authored content ------------------------------------------------------

const HELP_TOML: &str = include_str!("../../help.toml");

#[derive(serde::Deserialize, Default)]
struct HelpFile {
    #[serde(default)]
    control: Vec<HelpEntry>,
}

#[derive(serde::Deserialize)]
struct HelpEntry {
    id: String,
    #[serde(default)]
    message: String,
}

/// The id -> message map, parsed once. Entries with a blank message are dropped
/// so those controls fall back to [`GENERIC`].
fn messages() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| match toml::from_str::<HelpFile>(HELP_TOML) {
        Ok(file) => file
            .control
            .into_iter()
            .filter(|e| !e.message.trim().is_empty())
            .map(|e| (e.id, e.message))
            .collect(),
        Err(e) => {
            log::error!("help.toml parse error: {e}");
            HashMap::new()
        }
    })
}

fn help_for(hwnd: HWND) -> String {
    let raw = unsafe { GetPropW(hwnd, HELP_PROP) }.0 as isize;
    if raw <= 0 {
        return GENERIC.to_string();
    }
    id_for((raw - 1) as usize)
        .and_then(|id| messages().get(&id).cloned())
        .unwrap_or_else(|| GENERIC.to_string())
}

// --- UIA announcer ---------------------------------------------------------
//
// A screen reader only processes a UIA notification event whose provider is
// connected to its automation tree. A bare `UiaHostProviderFromHwnd` provider
// that was never returned through `WM_GETOBJECT` is not connected, so the
// notification is dropped. We therefore install a real server-side provider on
// the frame — reachable via the frame's `WM_GETOBJECT`/`UiaRootObjectId` — and
// raise the notification on it. `HostRawElementProvider` delegates structure to
// the default window provider, so the frame's children still enumerate normally.

thread_local! {
    /// The frame provider, keyed by frame hwnd, for the subclass proc (which
    /// only has the hwnd) to return on the UIA root query.
    static FRAME_PROVIDER: RefCell<HashMap<isize, IRawElementProviderSimple>> =
        RefCell::new(HashMap::new());
}

#[implement(IRawElementProviderSimple)]
struct FrameProvider {
    hwnd: isize,
}

impl IRawElementProviderSimple_Impl for FrameProvider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _patternid: UIA_PATTERN_ID) -> Result<IUnknown> {
        // No control patterns; this element exists only to source notifications.
        Err(Error::empty())
    }

    fn GetPropertyValue(&self, propertyid: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(if propertyid == UIA_ControlTypePropertyId {
            VARIANT::from(UIA_WindowControlTypeId.0)
        } else if propertyid == UIA_NamePropertyId {
            VARIANT::from("Pubsplash")
        } else {
            VARIANT::default()
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // Delegate structure/navigation to the default window provider so the
        // frame's children remain visible to the screen reader.
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut _)) }
    }
}

unsafe extern "system" fn frame_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    if msg == WM_GETOBJECT && lparam.0 as i32 == UiaRootObjectId {
        if let Some(provider) =
            FRAME_PROVIDER.with(|r| r.borrow().get(&(hwnd.0 as isize)).cloned())
        {
            return unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider) };
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Installs a UIA provider on the main frame so [`pump`] can raise notification
/// events that the screen reader will speak, regardless of which control has
/// focus.
pub fn install_announcer(frame: &dyn WxWidget) {
    let hwnd = HWND(frame.get_handle());
    let key = hwnd.0 as isize;
    let provider: IRawElementProviderSimple = FrameProvider { hwnd: key }.into();
    FRAME_PROVIDER.with(|r| {
        r.borrow_mut().insert(key, provider.clone());
    });
    let ok = unsafe { SetWindowSubclass(hwnd, Some(frame_subclass), FRAME_SUBCLASS_ID, 0) };
    if !ok.as_bool() {
        log::warn!("help: SetWindowSubclass failed for frame hwnd={key:#x}");
    }
    ANNOUNCER.with(|a| *a.borrow_mut() = Some((key, provider)));
}

pub fn uninstall_announcer() {
    let entry = ANNOUNCER.with(|a| a.borrow_mut().take());
    if let Some((key, provider)) = entry {
        unsafe {
            let _ = RemoveWindowSubclass(HWND(key as *mut _), Some(frame_subclass), FRAME_SUBCLASS_ID);
            if let Err(e) = UiaDisconnectProvider(&provider) {
                log::warn!("help: UiaDisconnectProvider failed: {e}");
            }
        }
        FRAME_PROVIDER.with(|r| {
            r.borrow_mut().remove(&key);
        });
    }
}

fn announce(text: &str) {
    ANNOUNCER.with(|a| {
        let Some((_, provider)) = a.borrow().clone() else {
            return;
        };
        unsafe {
            if !UiaClientsAreListening().as_bool() {
                return;
            }
            // ImportantMostRecent asks the screen reader to speak this now and
            // discard any older queued help, so repeated F1 presses interrupt
            // rather than pile up.
            if let Err(e) = UiaRaiseNotificationEvent(
                &provider,
                NotificationKind_Other,
                NotificationProcessing_ImportantMostRecent,
                &BSTR::from(text),
                &BSTR::new(),
            ) {
                log::warn!("help: UiaRaiseNotificationEvent failed: {e}");
            }
        }
    });
}

// --- F1 hook ---------------------------------------------------------------

fn foreground_is_ours() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN)
    {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kb.vkCode == VK_F1.0 as u32 && foreground_is_ours() {
            HELP_REQUESTED.store(true, Ordering::Relaxed);
            // Swallow F1 so no underlying control pops its own help.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Installs the app-wide F1 hook. Idempotent.
pub fn install_hook() {
    if HELP_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        if let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) {
            HELP_HOOK.store(hook.0 as isize, Ordering::Relaxed);
        }
    }
}

/// Removes the F1 hook. Idempotent.
pub fn uninstall_hook() {
    let raw = HELP_HOOK.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
        }
    }
}

/// Called each pump tick. If F1 was pressed, resolves the focused control to its
/// help message and announces it.
pub fn pump() {
    if !HELP_REQUESTED.swap(false, Ordering::Relaxed) {
        return;
    }
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
    announce(&help_for(focus));
}
