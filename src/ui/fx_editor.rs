//! Hosts a plugin's own editor window in a Pubsplash frame, without letting
//! keyboard focus get trapped inside the plugin.
//!
//! A plugin editor is a raw child window that swallows Tab and most keys, so
//! a screen-reader user who tabs into it can get stuck. The escape hatch is
//! **F6**: while any editor frame is open, a low-level keyboard hook watches
//! for F6 in one of our editor frames and refocuses that frame's toolbar
//! (Close button). Everything else the plugin receives normally, and the
//! toolbar's own buttons are ordinary Tab-reachable wx controls.

use super::App;
use super::fx::{self, ChainTarget};
use crate::vst::host2::Vst2Plugin;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use wxdragon::prelude::*;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_F6};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GA_ROOT, GetAncestor, GetForegroundWindow, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// The installed low-level keyboard hook (as isize; 0 = not installed).
static HOOK: AtomicIsize = AtomicIsize::new(0);
/// Root HWNDs of open editor frames (as usize).
static EDITOR_HWNDS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// A frame HWND the pump should refocus after F6 (0 = nothing pending).
static ESCAPE_TO: AtomicUsize = AtomicUsize::new(0);

/// A live editor window.
pub struct EditorWindow {
    frame: Frame,
    plugin: std::sync::Arc<Vst2Plugin>,
    close_button: Button,
    effect_id: usize,
    target: ChainTarget,
    slot: usize,
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN)
    {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kb.vkCode == VK_F6.0 as u32 {
            unsafe {
                let root = GetAncestor(GetForegroundWindow(), GA_ROOT);
                if let Ok(hwnds) = EDITOR_HWNDS.lock() {
                    if hwnds.contains(&(root.0 as usize)) {
                        ESCAPE_TO.store(root.0 as usize, Ordering::Relaxed);
                        // Swallow F6 so the plugin never sees it.
                        return LRESULT(1);
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn install_hook() {
    if HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        if let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) {
            HOOK.store(hook.0 as isize, Ordering::Relaxed);
        }
    }
}

fn uninstall_hook_if_idle() {
    let empty = EDITOR_HWNDS.lock().map(|h| h.is_empty()).unwrap_or(true);
    if empty {
        let raw = HOOK.swap(0, Ordering::Relaxed);
        if raw != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(raw as *mut _));
            }
        }
    }
}

fn hwnd_of(widget: &dyn WxWidget) -> HWND {
    HWND(widget.get_handle())
}

/// Opens (or re-focuses) the native editor for a plugin slot.
pub fn open_editor(
    app: &Rc<App>,
    target: ChainTarget,
    slot: usize,
    plugin: std::sync::Arc<Vst2Plugin>,
) {
    let Some(parent) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let effect_id = plugin.effect_id();

    // Already open for this instance? Just raise it.
    let already = app
        .open_editors
        .borrow()
        .iter()
        .any(|e| e.effect_id == effect_id);
    if already {
        for e in app.open_editors.borrow().iter() {
            if e.effect_id == effect_id {
                e.frame.show(true);
                unsafe {
                    let _ = SetFocus(Some(hwnd_of(&e.close_button)));
                }
            }
        }
        return;
    }

    if !plugin.has_editor {
        super::show_info(
            &parent,
            "Plugin interface",
            "This plugin has no interface of its own. Use \"Edit parameters\" instead.",
        );
        return;
    }

    let (w, h) = plugin.editor_rect().unwrap_or((600, 400));
    let name = plugin.info.name.clone();
    let frame = Frame::builder()
        .with_parent(&parent)
        .with_title(&format!("{name} interface"))
        .with_size(Size::new(w.max(300) + 20, h.max(200) + 70))
        .build();
    let panel = Panel::builder(&frame).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Toolbar (Tab-reachable, always an escape from the plugin's own UI).
    let toolbar = BoxSizer::builder(Orientation::Horizontal).build();
    let params = Button::builder(&panel).with_label("&Parameters...").build();
    let bypass = CheckBox::builder(&panel).with_label("&Bypass").build();
    bypass.set_value(
        fx::slots_for(app, target)
            .get(slot)
            .map(|s| s.bypass)
            .unwrap_or(false),
    );
    super::set_accessible_name(&bypass, "Bypass this plugin");
    let focus_plugin = Button::builder(&panel)
        .with_label("Plugin &interface")
        .build();
    let close = Button::builder(&panel).with_label("&Close").build();
    super::help::tag(
        &params,
        "dialog.fxEditor.parameters",
        "Open accessible parameters dialog button",
    );
    super::help::tag(
        &bypass,
        "dialog.fxEditor.bypass",
        "Bypass this plugin checkbox",
    );
    super::help::tag(
        &focus_plugin,
        "dialog.fxEditor.focusPlugin",
        "Move focus into the plugin's own interface button",
    );
    super::help::tag(
        &close,
        "dialog.fxEditor.close",
        "Close plugin interface button",
    );
    toolbar.add(&params, 0, SizerFlag::All, 4);
    toolbar.add(
        &bypass,
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::All,
        4,
    );
    toolbar.add(&focus_plugin, 0, SizerFlag::All, 4);
    toolbar.add(&close, 0, SizerFlag::All, 4);

    // The plugin draws into this panel.
    let host = Panel::builder(&panel)
        .with_size(Size::new(w.max(300), h.max(200)))
        .build();

    sizer.add_sizer(&toolbar, 0, SizerFlag::Expand, 0);
    sizer.add(&host, 1, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    frame.show(true);
    plugin.editor_open(host.get_handle());

    // Register for the F6 hook.
    if let Ok(mut hwnds) = EDITOR_HWNDS.lock() {
        hwnds.push(hwnd_of(&frame).0 as usize);
    }
    install_hook();

    // Toolbar wiring.
    {
        let app = app.clone();
        let plugin = plugin.clone();
        params.on_click(move |_| {
            super::fx_params::edit_parameters(&app, target, slot, plugin.clone());
        });
    }
    {
        let app = app.clone();
        let bypass = bypass.clone();
        bypass.clone().on_toggled(move |_| {
            fx::set_bypass(&app, target, slot, bypass.get_value());
            super::buses::refresh_fx_list(&app);
        });
    }
    {
        let host = host.clone();
        focus_plugin.on_click(move |_| unsafe {
            let _ = SetFocus(Some(HWND(host.get_handle())));
        });
    }
    {
        let frame = frame.clone();
        close.on_click(move |_| frame.close(false));
    }

    // Close: tear down the editor and persist state.
    {
        let app = app.clone();
        frame.on_close(move |event| {
            close_editor(&app, effect_id);
            event.skip(true);
        });
    }

    app.open_editors.borrow_mut().push(EditorWindow {
        frame,
        plugin,
        close_button: close,
        effect_id,
        target,
        slot,
    });
}

/// Closes and destroys one editor by effect id, snapshotting its state.
fn close_editor(app: &Rc<App>, effect_id: usize) {
    let editor = {
        let mut editors = app.open_editors.borrow_mut();
        let pos = editors.iter().position(|e| e.effect_id == effect_id);
        pos.map(|p| editors.remove(p))
    };
    let Some(editor) = editor else {
        return;
    };
    editor.plugin.editor_close();
    fx::snapshot_slot(app, editor.target, editor.slot);
    if let Ok(mut hwnds) = EDITOR_HWNDS.lock() {
        hwnds.retain(|&h| h != hwnd_of(&editor.frame).0 as usize);
    }
    uninstall_hook_if_idle();
    editor.frame.destroy();
}

/// Closes every open editor (chain structural edit, or app exit).
pub fn close_all(app: &Rc<App>) {
    let ids: Vec<usize> = app
        .open_editors
        .borrow()
        .iter()
        .map(|e| e.effect_id)
        .collect();
    for id in ids {
        close_editor(app, id);
    }
}

/// Per-tick maintenance: drives editor idle, applies plugin resize requests,
/// and performs a pending F6 focus escape. Called from the UI pump.
pub fn pump(app: &Rc<App>) {
    {
        let editors = app.open_editors.borrow();
        if editors.is_empty() {
            return;
        }
        for editor in editors.iter() {
            editor.plugin.editor_idle();
        }
        // Apply any audioMasterSizeWindow requests to matching frames.
        for (effect_id, w, h) in crate::vst::host2::take_size_requests() {
            if let Some(editor) = editors.iter().find(|e| e.effect_id == effect_id) {
                editor
                    .frame
                    .set_size(Size::new(w.max(300) + 20, h.max(200) + 70));
            }
        }
    }

    // F6 escape: focus the matching frame's Close button.
    let escape = ESCAPE_TO.swap(0, Ordering::Relaxed);
    if escape != 0 {
        let editors = app.open_editors.borrow();
        if let Some(editor) = editors
            .iter()
            .find(|e| hwnd_of(&e.frame).0 as usize == escape)
        {
            unsafe {
                let _ = SetFocus(Some(hwnd_of(&editor.close_button)));
            }
        }
    }
}
