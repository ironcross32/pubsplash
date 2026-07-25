//! A UI Automation provider that makes a native wxSlider announce a custom
//! string value to screen readers on every change.
//!
//! A wxSlider is a native `msctls_trackbar32`. Its built-in UIA provider exposes
//! a numeric 0..1000 position, which NVDA (preferring UIA on Windows 10/11)
//! reads as a bare percentage — not the plugin's formatted value ("-3.0 dB").
//! MSAA customisation can supply the right value on focus but cannot drive a
//! re-read on a keyboard step that doesn't move focus.
//!
//! So we install our own server-side UIA provider on the slider's window: it
//! answers `WM_GETOBJECT`/`UiaRootObjectId` (replacing the native trackbar
//! provider), exposes our text via the Value pattern, and raises a Value
//! property-changed event on each step so NVDA speaks it immediately.
//!
//! Threading contract: `install`/`update`/`uninstall` and the subclass proc run
//! on the UI thread. UIA may call the provider's `IRawElementProviderSimple` /
//! `IValueProvider` methods from another thread, so the shared state is a
//! `Mutex` behind an `Arc` and the provider object is `Send + Sync`. The mutex
//! is only ever held for a cheap read/clone, never across a UIA call.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    IRawElementProviderSimple, IRawElementProviderSimple_Impl, IValueProvider, IValueProvider_Impl,
    ProviderOptions, ProviderOptions_ServerSideProvider, UIA_ControlTypePropertyId,
    UIA_HasKeyboardFocusPropertyId, UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId,
    UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_SliderControlTypeId, UIA_ValuePatternId,
    UIA_ValueValuePropertyId, UiaClientsAreListening, UiaDisconnectProvider,
    UiaHostProviderFromHwnd, UiaRaiseAutomationPropertyChangedEvent, UiaReturnRawElementProvider,
    UiaRootObjectId,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetGUIThreadInfo, WM_GETOBJECT};
use windows::core::{BOOL, BSTR, Error, IUnknown, PCWSTR, Result, implement};
use windows_core::IUnknownImpl;
use wxdragon::prelude::WxWidget;

/// Distinguishes our subclass from any other on the same window.
const SUBCLASS_ID: usize = 0x0A11;

thread_local! {
    /// Live providers keyed by slider HWND, so the subclass proc (which only has
    /// the HWND) can hand the right provider to `UiaReturnRawElementProvider`.
    static REGISTRY: RefCell<HashMap<isize, IRawElementProviderSimple>> =
        RefCell::new(HashMap::new());
}

/// Locks `m`, tolerating a poisoned mutex (a panic must never abort the process
/// by unwinding out of a COM vtable thunk).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the window with handle `hwnd` currently holds the keyboard focus.
///
/// Uses `GetGUIThreadInfo(0, ..)` (the foreground thread's focus) rather than
/// `GetFocus`, so it is correct even when UIA calls the provider from a thread
/// other than the one that owns the window. NVDA queries `HasKeyboardFocus`
/// before announcing a value change; answering it authoritatively is what keeps
/// announcements flowing.
fn is_focused(hwnd: isize) -> bool {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(0, &mut info).is_ok() && info.hwndFocus.0 as isize == hwnd }
}

/// The announced name and formatted value, shared between the COM object (read
/// by UIA) and [`SliderAnnouncer::update`] (written on the UI thread).
struct ProviderState {
    hwnd: isize,
    text: Mutex<(String, String)>, // (name, formatted value)
}

impl ProviderState {
    fn name(&self) -> String {
        lock(&self.text).0.clone()
    }
    fn value(&self) -> String {
        lock(&self.text).1.clone()
    }
}

#[implement(IRawElementProviderSimple, IValueProvider)]
struct SliderProvider {
    inner: Arc<ProviderState>,
}

impl IRawElementProviderSimple_Impl for SliderProvider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, patternid: UIA_PATTERN_ID) -> Result<IUnknown> {
        if patternid == UIA_ValuePatternId {
            Ok(self.to_interface::<IValueProvider>().into())
        } else {
            // Error::empty() carries HRESULT(0) (S_OK); the vtable thunk leaves
            // the caller's null out-pointer untouched → "pattern not supported".
            Err(Error::empty())
        }
    }

    fn GetPropertyValue(&self, propertyid: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(if propertyid == UIA_ControlTypePropertyId {
            VARIANT::from(UIA_SliderControlTypeId.0)
        } else if propertyid == UIA_NamePropertyId {
            VARIANT::from(self.inner.name().as_str())
        } else if propertyid == UIA_ValueValuePropertyId {
            VARIANT::from(self.inner.value().as_str())
        } else if propertyid == UIA_HasKeyboardFocusPropertyId {
            // NVDA checks this before announcing a value change; if we don't
            // answer it, focus can't be confirmed and the announcement is
            // dropped.
            VARIANT::from(is_focused(self.inner.hwnd))
        } else if propertyid == UIA_IsKeyboardFocusablePropertyId {
            VARIANT::from(true)
        } else {
            VARIANT::default() // VT_EMPTY = property not supplied
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // The generic window host gives UIA navigation, focus, and bounds; it
        // does not expose the native trackbar's RangeValue, so no percentage.
        unsafe { UiaHostProviderFromHwnd(HWND(self.inner.hwnd as *mut _)) }
    }
}

impl IValueProvider_Impl for SliderProvider_Impl {
    fn SetValue(&self, _val: &PCWSTR) -> Result<()> {
        // The dialog drives the value from the keyboard; UIA clients don't set it.
        Ok(())
    }

    fn Value(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.inner.value().as_str()))
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(false.into())
    }
}

/// Intercepts the UIA root-object query so our provider replaces the native
/// trackbar's; all other messages (including MSAA `OBJID_CLIENT`) pass through
/// to wxWidgets.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    if msg == WM_GETOBJECT && lparam.0 as i32 == UiaRootObjectId {
        if let Some(provider) = REGISTRY.with(|r| r.borrow().get(&(hwnd.0 as isize)).cloned()) {
            return unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider) };
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// A UIA provider installed on a slider. Drop (or [`uninstall`](Self::uninstall))
/// removes it; keep it alive for as long as the slider exists.
pub struct SliderAnnouncer {
    hwnd: isize,
    provider: IRawElementProviderSimple,
    inner: Arc<ProviderState>,
    done: Cell<bool>,
}

/// Installs a UIA provider on `slider` so [`SliderAnnouncer::update`] can make
/// screen readers announce a custom formatted value.
pub fn install(slider: &dyn WxWidget) -> SliderAnnouncer {
    let hwnd = HWND(slider.get_handle());
    let key = hwnd.0 as isize;
    let inner = Arc::new(ProviderState {
        hwnd: key,
        text: Mutex::new((String::new(), String::new())),
    });
    let provider: IRawElementProviderSimple = SliderProvider {
        inner: inner.clone(),
    }
    .into();
    REGISTRY.with(|r| {
        r.borrow_mut().insert(key, provider.clone());
    });
    let ok = unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) };
    if !ok.as_bool() {
        log::warn!("slider_uia: SetWindowSubclass failed for hwnd={key:#x}");
    }
    SliderAnnouncer {
        hwnd: key,
        provider,
        inner,
        done: Cell::new(false),
    }
}

impl SliderAnnouncer {
    /// Sets the announced name and formatted value without raising an event, so
    /// the new text is what a screen reader reads the next time the slider is
    /// focused or queried. Use this when something else is already speaking the
    /// change, so it isn't followed by a redundant value re-read.
    pub fn set_text(&self, name: &str, value: &str) {
        let mut guard = lock(&self.inner.text);
        *guard = (name.to_string(), value.to_string());
    }

    /// Sets the announced name and formatted value and, if a screen reader is
    /// listening, raises a UIA Value property-changed event so it speaks the new
    /// value on a keyboard step (no focus change required).
    pub fn update(&self, name: &str, value: &str) {
        self.set_text(name, value);
        unsafe {
            if !UiaClientsAreListening().as_bool() {
                return;
            }
            // NVDA reads the new value; the old value is unused.
            let new_value = VARIANT::from(value);
            match UiaRaiseAutomationPropertyChangedEvent(
                &self.provider,
                UIA_ValueValuePropertyId,
                &VARIANT::default(),
                &new_value,
            ) {
                Ok(()) => {}
                Err(e) => log::warn!("slider_uia: property-changed event failed: {e}"),
            }
        }
    }

    /// Removes the subclass and provider. Idempotent; also runs on drop.
    pub fn uninstall(&self) {
        if self.done.replace(true) {
            return;
        }
        unsafe {
            let _ =
                RemoveWindowSubclass(HWND(self.hwnd as *mut _), Some(subclass_proc), SUBCLASS_ID);
            if let Err(e) = UiaDisconnectProvider(&self.provider) {
                log::warn!("slider_uia: UiaDisconnectProvider failed: {e}");
            }
        }
        REGISTRY.with(|r| {
            r.borrow_mut().remove(&self.hwnd);
        });
    }
}

impl Drop for SliderAnnouncer {
    fn drop(&mut self) {
        self.uninstall();
    }
}
