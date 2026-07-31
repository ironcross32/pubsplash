//! F6 / SHIFT+F6 pane cycling in the main window.
//!
//! Each tab is one long Tab-order chain, so reaching the Scenes list from the
//! mixer — or the tab bar from anywhere — costs a lot of Tab presses. F6 makes
//! the lists (and the tab bar) the coarse stops that every other accessible
//! Windows app offers: **F6** moves to the next list on the current tab and
//! wraps past the last one onto the tab bar, **SHIFT+F6** goes the other way.
//!
//! The target is computed from where focus *actually is*, not from a remembered
//! ring position: standing on a mixer slider that sits between two lists, F6
//! goes forward to the list after it rather than back to the first one. That is
//! why [`page_order`] walks the real Win32 Tab order of the page rather than
//! consulting a table — a control we know nothing about still has a position
//! relative to the lists.
//!
//! The key itself is caught by the app-wide hook in [`super::help`] (one hook
//! for the whole process) and handed here through [`request`]; [`pump`] does the
//! work on the UI thread, from the idle pump.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GW_CHILD, GW_HWNDNEXT, GetGUIThreadInfo, GetWindow,
};
use wxdragon::prelude::*;

use super::{App, Widgets};

/// Set by the hook when F6 is pressed over the main frame; read and cleared by
/// [`pump`].
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Direction of the pending request. Only meaningful while `REQUESTED` is set.
static BACKWARD: AtomicBool = AtomicBool::new(false);

/// Called from the keyboard hook. Touches nothing but the two atomics.
pub fn request(shift: bool) {
    BACKWARD.store(shift, Ordering::Relaxed);
    REQUESTED.store(true, Ordering::Relaxed);
}

/// Where F6 should land.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Target {
    /// The list at this position in the page's Tab order.
    List(usize),
    /// The notebook's tab strip.
    TabBar,
}

/// The ring rule, kept pure so it can be tested without a window.
///
/// `lists` are the Tab-order positions of the page's lists, ascending. `focus`
/// is the position of the focused control in that same order, or `None` when
/// focus is on the tab bar (or somewhere we could not place).
fn target(lists: &[usize], focus: Option<usize>, backward: bool) -> Option<Target> {
    let Some(&first) = lists.first() else {
        // No lists on this page: F6 goes to the tab bar, and does nothing at all
        // once it is already there.
        return focus.map(|_| Target::TabBar);
    };
    let last = *lists.last().expect("non-empty");
    let Some(at) = focus else {
        // On the tab bar: forward enters at the top of the page, back at the end.
        return Some(Target::List(if backward { last } else { first }));
    };
    if backward {
        // The last list strictly before us; the tab bar if we are at or before
        // the first one.
        lists
            .iter()
            .rev()
            .find(|&&i| i < at)
            .map_or(Some(Target::TabBar), |&i| Some(Target::List(i)))
    } else {
        lists
            .iter()
            .find(|&&i| i > at)
            .map_or(Some(Target::TabBar), |&i| Some(Target::List(i)))
    }
}

fn hwnd_of(widget: &dyn WxWidget) -> HWND {
    HWND(widget.get_handle())
}

/// The page's controls in Tab order.
///
/// On MSW a sibling's Tab order *is* its Z order, so this is the same walk
/// `GetNextDlgTabItem` does: children of a window in Z order, depth first so a
/// control nested in a container (a mixer slider inside the mixer panel) lands
/// between the containers's neighbours rather than after all of them.
fn page_order(page: HWND) -> Vec<HWND> {
    fn walk(parent: HWND, out: &mut Vec<HWND>) {
        let mut child = unsafe { GetWindow(parent, GW_CHILD) };
        while let Ok(hwnd) = child {
            if hwnd.0.is_null() {
                break;
            }
            out.push(hwnd);
            walk(hwnd, out);
            child = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
        }
    }
    let mut out = Vec::new();
    walk(page, &mut out);
    out
}

/// Every list in the main window. Only the ones on the current page survive the
/// filter in [`pump`], so this needs no per-tab table: a new list on a tab needs
/// only a `Widgets` field and a line here.
fn all_lists(w: &Widgets) -> [&ListBox; 8] {
    [
        &w.overview,
        &w.home_scene_list,
        &w.chat_list,
        &w.scenes_list,
        &w.sources_list,
        &w.bus_list,
        &w.fx_list,
        &w.usage_list,
    ]
}

/// The window with keyboard focus, however deep — the same route `help::pump`
/// takes, since wxdragon exposes no `FindFocus`.
fn focused() -> HWND {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetGUIThreadInfo(0, &mut info).is_ok() {
            info.hwndFocus
        } else {
            HWND::default()
        }
    }
}

/// Called each pump tick. One relaxed load unless F6 was actually pressed.
pub fn pump(app: &Rc<App>) {
    if !REQUESTED.swap(false, Ordering::Relaxed) {
        return;
    }
    if app.shutting_down.get() {
        // Widgets are being torn down; touching them would be a use-after-free.
        return;
    }
    let backward = BACKWARD.load(Ordering::Relaxed);
    app.widgets(|w| {
        let selected = w.notebook.selection();
        let Some(page) = usize::try_from(selected)
            .ok()
            .and_then(|i| w.notebook.get_page(i))
        else {
            return;
        };
        let order = page_order(hwnd_of(&page));
        let focus = focused();
        let position = |hwnd: HWND| order.iter().position(|&h| h == hwnd);

        // Focus on the tab bar (or on nothing we can place) reads as `None`,
        // which the ring treats as "outside the page".
        let at = position(focus);
        let mut lists: Vec<usize> = all_lists(w)
            .iter()
            .filter_map(|list| position(hwnd_of(*list)))
            .collect();
        lists.sort_unstable();

        match target(&lists, at, backward) {
            // Focusing a list must never move its selection — see `ui::list`.
            // Arriving is enough for the screen reader to read the current row.
            Some(Target::List(i)) => {
                if let Some(&hwnd) = order.get(i)
                    && let Some(list) = all_lists(w).iter().find(|l| hwnd_of(**l) == hwnd)
                {
                    list.set_focus();
                }
            }
            Some(Target::TabBar) => w.notebook.set_focus(),
            None => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Target, target};

    // Two lists at Tab-order positions 1 and 9, with other controls around and
    // between them — the Home tab in miniature.
    const LISTS: &[usize] = &[1, 9];

    #[test]
    fn from_the_tab_bar_forward_enters_the_first_list() {
        assert_eq!(target(LISTS, None, false), Some(Target::List(1)));
    }

    #[test]
    fn from_the_tab_bar_backward_enters_the_last_list() {
        assert_eq!(target(LISTS, None, true), Some(Target::List(9)));
    }

    #[test]
    fn a_list_steps_to_the_next_list() {
        assert_eq!(target(LISTS, Some(1), false), Some(Target::List(9)));
        assert_eq!(target(LISTS, Some(9), true), Some(Target::List(1)));
    }

    #[test]
    fn the_last_list_wraps_onto_the_tab_bar() {
        assert_eq!(target(LISTS, Some(9), false), Some(Target::TabBar));
        assert_eq!(target(LISTS, Some(1), true), Some(Target::TabBar));
    }

    // The point of computing from focus: a control between the lists goes
    // forward to the one after it, not back to the one before.
    #[test]
    fn a_control_between_lists_moves_in_the_pressed_direction() {
        assert_eq!(target(LISTS, Some(5), false), Some(Target::List(9)));
        assert_eq!(target(LISTS, Some(5), true), Some(Target::List(1)));
    }

    #[test]
    fn a_control_past_every_list_goes_on_to_the_tab_bar() {
        assert_eq!(target(LISTS, Some(12), false), Some(Target::TabBar));
        assert_eq!(target(LISTS, Some(12), true), Some(Target::List(9)));
    }

    #[test]
    fn one_list_alternates_with_the_tab_bar() {
        assert_eq!(target(&[3], Some(3), false), Some(Target::TabBar));
        assert_eq!(target(&[3], Some(3), true), Some(Target::TabBar));
        assert_eq!(target(&[3], None, false), Some(Target::List(3)));
        assert_eq!(target(&[3], None, true), Some(Target::List(3)));
    }

    #[test]
    fn a_page_without_lists_only_reaches_the_tab_bar() {
        assert_eq!(target(&[], Some(2), false), Some(Target::TabBar));
        assert_eq!(target(&[], Some(2), true), Some(Target::TabBar));
        // Already there: nothing to do.
        assert_eq!(target(&[], None, false), None);
        assert_eq!(target(&[], None, true), None);
    }
}
