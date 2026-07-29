//! Keeps wxWidgets' MSAA layer out of the way of a native list box, so a screen
//! reader sees the plain Win32 control instead of a hybrid of two objects.
//!
//! wx wraps *every* MSW control: `wxWindowMSW`'s `WM_GETOBJECT` handler calls
//! `GetOrCreateAccessible()`, which creates a `wxWindowAccessible` when the
//! window has none, and hands OLEACC that instead of the control's own object.
//! What that object answers is final; what it declines (`wxACC_NOT_IMPLEMENTED`)
//! is forwarded to a **second, unrelated** object built by
//! `CreateStdAccessibleObject` — the real `LISTBOX` proxy, whose child ids index
//! list *items*, where wx's index child *windows*.
//!
//! For a control with no children that hybrid is harmless, which is why sliders,
//! checkboxes and buttons are fine. For a list it is not: the focus event the
//! reader receives comes from the native control and names a row in the std
//! object's id space, while `AccessibleObjectFromEvent` hands back wx's. The
//! reader reconciles the row across both graphs and announces it twice — once on
//! every tab or F6 into a list, though not when arrowing within one.
//!
//! Two more symptoms of the same wrapping, for the record: wx answers the list's
//! name with `wxWindow::GetName()`, which is the literal string `"listBox"`, and
//! its child count with the number of child windows, which is zero. The first is
//! what stops the native "name comes from the label in front of it" behaviour
//! from ever running, and so is the reason [`super::set_accessible_name`] had to
//! exist for lists at all.
//!
//! The fix is to answer `WM_GETOBJECT` before wx can. A subclass proc runs ahead
//! of the window's own proc, so returning `DefWindowProc` for `OBJID_CLIENT`
//! gives OLEACC the native object and wx never sees the message. The list is
//! then an ordinary Win32 list box in every respect, including taking its
//! accessible name from the static text created just before it.
//!
//! Radio boxes have the same disease, one control down. `wxRadioBox` is a
//! `wxStaticBox` (a `BUTTON` with `BS_GROUPBOX`) whose items are real
//! `wxRadioButton` **windows**, so wx wraps every one of them too, and
//! `wxWindowAccessible::GetName` answers with `wxWindow::GetName()` for
//! anything that is not a `wxButton` — the literal `"radioButton"` for each
//! item and `"radioBox"` for the group. It is non-empty, so it is returned as
//! the name and the real one (the button's window text) is never reached; role
//! and checked state still come from the std object, which is why the result is
//! a nameless radio button rather than a silent one. [`install_radio_box`] is
//! the fix, and [`super::set_accessible_name`] cannot be: it would replace the
//! accessible of the group window only, and the items are separate child
//! windows that wxdragon hands out no handles for.
//!
//! Threading: `install` and the subclass procs all run on the UI thread. They
//! hold no state, so nothing needs uninstalling — the subclass dies with the
//! window, which matters for the lists that live in dialogs.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::Shell::{DefSubclassProc, SUBCLASSPROC, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, EVENT_OBJECT_FOCUS, GW_CHILD, GW_HWNDNEXT, GetClassNameW, GetWindow,
    LB_GETCARETINDEX, LB_GETCOUNT, SendMessageW, WM_GETOBJECT, WM_SETFOCUS,
};
use wxdragon::prelude::*;

/// Distinguishes our subclass from any other on the same window.
const SUBCLASS_ID: usize = 0x0A13;

/// `OBJID_CLIENT`, the object id wx intercepts. Windows declares it in the
/// accessibility headers as a negative constant; `WM_GETOBJECT` delivers it in
/// `lParam` sign-extended.
const OBJID_CLIENT: i32 = -4;

/// `LB_ERR`, what a list box answers when it has no caret row.
const LB_ERR: isize = -1;

/// Hands `WM_GETOBJECT` to `DefWindowProcW` and changes nothing else.
///
/// Deliberately *not* `DefSubclassProc` for that message: that would reach wx's
/// handler, which is the whole problem. `DefWindowProcW` lets OLEACC build the
/// control's own accessible object.
unsafe extern "system" fn passthrough_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _refdata: usize,
) -> LRESULT {
    if msg == WM_GETOBJECT && lparam.0 as i32 == OBJID_CLIENT {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// [`passthrough_proc`] plus the focus fix that only list boxes need.
unsafe extern "system" fn list_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    refdata: usize,
) -> LRESULT {
    if msg == WM_SETFOCUS {
        announce_caret_row(hwnd);
    }
    unsafe { passthrough_proc(hwnd, msg, wparam, lparam, id, refdata) }
}

/// Raises the focus event for the row the caret is on, right now.
///
/// Windows has already raised `EVENT_OBJECT_FOCUS` for the list itself by the
/// time `WM_SETFOCUS` arrives, and the list box raises a second one for the
/// caret row — but only after the window proc returns, which in wx means after
/// `HandleSetFocus` has dispatched a `wxChildFocusEvent` up the whole parent
/// chain and a `wxEVT_SET_FOCUS` on top. That work is the gap.
///
/// NVDA queues focus events and keeps only the last one per window when it
/// flushes, so the pair is harmless as long as both are waiting at the same
/// flush — and ruinous otherwise: the list event alone makes it speak the
/// control, its name, *and* the current row, and then the row event makes it
/// speak the row again. Half a millisecond decides which happens, so the same
/// keystroke on the same list would double or not at random.
///
/// Raising the row event here, before any of the wx dispatch, closes the gap to
/// nothing. The list box's own event still follows and is an exact duplicate,
/// which the same queue discards. Narrator was never affected — it does not
/// report a list-level and a row-level focus as two separate things.
fn announce_caret_row(hwnd: HWND) {
    let count = unsafe { SendMessageW(hwnd, LB_GETCOUNT, None, None) }.0;
    let caret = unsafe { SendMessageW(hwnd, LB_GETCARETINDEX, None, None) }.0;
    if caret == LB_ERR || caret < 0 || caret >= count {
        return;
    }
    // MSAA numbers a control's children from 1; the list box's own event uses
    // the same convention, so this must match it exactly to be a duplicate.
    unsafe { NotifyWinEvent(EVENT_OBJECT_FOCUS, hwnd, OBJID_CLIENT, caret as i32 + 1) };
}

/// Makes `list` present itself to screen readers as the native Win32 control it
/// is, rather than as wx's hybrid.
///
/// Use this on list boxes in place of [`super::set_accessible_name`], which is
/// still right for controls that have no MSAA children of their own. `name` is
/// what the list should announce as; the native object takes it from the static
/// text in front of the list, so every call site must keep one there saying
/// exactly this.
pub fn install(list: &ListBox, name: &str) {
    install_hwnd(HWND(list.get_handle()), Some(list_subclass_proc), name);
}

/// Makes `radio` and each of its items present themselves as the native Win32
/// controls they are, so the items announce their own labels instead of wx's
/// `"radioButton"` and the group announces its title instead of `"radioBox"`.
///
/// `name` is only used for logging; the names a reader speaks come from the
/// controls' own window text, which is the label passed to the builder and the
/// item strings. Every item is a child `BUTTON` window of the group box.
pub fn install_radio_box(radio: &RadioBox, name: &str) {
    let root = HWND(radio.get_handle());
    if root.0.is_null() {
        log::warn!("native_acc: no window yet for the {name:?} radio box");
        return;
    }
    install_hwnd(root, Some(passthrough_proc), name);
    let items = descendants_of_class(root, "Button");
    if items.is_empty() {
        log::warn!("native_acc: no item buttons found in the {name:?} radio box");
    }
    for hwnd in items {
        install_hwnd(hwnd, Some(passthrough_proc), name);
    }
}

fn install_hwnd(hwnd: HWND, proc: SUBCLASSPROC, name: &str) {
    if hwnd.0.is_null() {
        log::warn!("native_acc: no window yet for the {name:?} control");
        return;
    }
    let ok = unsafe { SetWindowSubclass(hwnd, proc, SUBCLASS_ID, 0) };
    if !ok.as_bool() {
        log::warn!("native_acc: SetWindowSubclass failed for the {name:?} control");
    }
}

/// Does the same for a list box inside a dialog wxWidgets built for us, which
/// hands out no handle to it.
///
/// `SingleChoiceDialog` is the case that matters: its list doubles exactly like
/// the ones we build, and there is no widget to pass to [`install`]. Call this
/// on the dialog before `show_modal`, while its windows exist but nothing has
/// asked them for an accessible object yet. Every `LISTBOX` in the dialog is
/// treated, since these dialogs only ever hold the one.
pub fn install_in_dialog(dialog: &dyn WxWidget, name: &str) {
    let root = HWND(dialog.get_handle());
    if root.0.is_null() {
        log::warn!("native_acc: no window yet for the {name:?} dialog");
        return;
    }
    let lists = descendants_of_class(root, "ListBox");
    if lists.is_empty() {
        log::warn!("native_acc: no list box found in the {name:?} dialog");
    }
    for hwnd in lists {
        install_hwnd(hwnd, Some(list_subclass_proc), name);
    }
}

/// Every descendant of `root` whose window class is `class`.
fn descendants_of_class(root: HWND, class: &str) -> Vec<HWND> {
    fn walk(parent: HWND, class: &str, out: &mut Vec<HWND>) {
        let mut child = unsafe { GetWindow(parent, GW_CHILD) };
        while let Ok(hwnd) = child {
            if hwnd.0.is_null() {
                break;
            }
            if class_name_is(hwnd, class) {
                out.push(hwnd);
            }
            walk(hwnd, class, out);
            child = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
        }
    }
    let mut out = Vec::new();
    walk(root, class, &mut out);
    out
}

fn class_name_is(hwnd: HWND, class: &str) -> bool {
    let mut buf = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return false;
    }
    String::from_utf16_lossy(&buf[..len as usize]).eq_ignore_ascii_case(class)
}
