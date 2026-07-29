//! Empty-state placeholder rows for list boxes.
//!
//! NVDA announces a `ListBox` holding zero items as just "Unknown" — not the
//! control type, not its accessible name. So every list that can legitimately
//! empty keeps one placeholder row instead ("No chats", "No sources"), which
//! goes away as soon as there is something real to show.
//!
//! That row is not data, and the whole point of this module is that no call
//! site can mistake it for data: widget indices reach a config collection only
//! through [`selection`], which takes the collection's length and so rejects
//! the placeholder for free. Several of the call sites it guards would
//! otherwise panic (`Vec::remove` past the end), not merely act on the wrong
//! item.
//!
//! Nothing here ever moves a selection. A screen reader reads a row out when it
//! becomes selected, so a list that reselects rows as it refreshes talks over
//! whatever the user is actually doing — and these lists refresh constantly.
//! Selection belongs to the user and to the code handling a deliberate edit.

use wxdragon::prelude::*;

/// The selected row as an index into the backing data, or `None` when the row
/// isn't one — either nothing is selected, or the list is showing its
/// placeholder.
///
/// A placeholder only exists when `len` is 0, and then no index is in range,
/// so the bounds check is the placeholder check.
fn data_index(selected: Option<u32>, len: usize) -> Option<usize> {
    let index = selected? as usize;
    (index < len).then_some(index)
}

/// The list's selected row as an index into the backing collection of length
/// `len`, or `None` when the placeholder row is showing.
pub fn selection(list: &ListBox, len: usize) -> Option<usize> {
    data_index(list.get_selection(), len)
}

/// Clears and refills `list`, substituting a single `empty` row when there is
/// nothing to show.
///
/// Content only: it must not touch the selection. The placeholder does not
/// need to be selected to be announced — NVDA reads it because the row is
/// there, which is the entire reason for putting it there. Selecting it would
/// instead raise focus and selection events on a list the user may not be in,
/// and these lists are refilled constantly (the Sources list on every arrow key
/// in the Scenes list, and again every two seconds by the application poll). It
/// also leaked: callers read `get_selection` before refilling and restore it
/// afterwards, so a selected placeholder made every later refresh reselect row
/// 0 and speak that scene's first source over its name.
pub fn fill(list: &ListBox, labels: &[String], empty: &str) {
    list.clear();
    if labels.is_empty() {
        list.append(empty);
        return;
    }
    for label in labels {
        list.append(label);
    }
}

/// Whether [`sync`] had to rebuild the list, which drops its selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Synced {
    /// The rows were already right, or only their text needed rewriting. The
    /// selection is untouched and the caller must leave it alone.
    Kept,
    /// Rows were added or removed, so the list was refilled and has no
    /// selection. Only now may the caller restore one.
    Rebuilt,
}

/// Brings `list` up to date with `labels`, doing the least work that will do
/// it, and reports whether the selection survived.
///
/// A clear-and-refill is loud: the list loses its selection, the caller sets it
/// again, and a screen reader announces the control and the row afresh. That is
/// right when the list really changed and wrong every other time — and most
/// refreshes change nothing. `after_scene_edit` refreshes the Scenes list on
/// every *source* edit, `on_sources_changed` refreshes the Home scene list, and
/// `add_plugin` refreshes the bus list; deleting one source used to rebuild all
/// three, so the user heard the Scenes list read itself out after every delete.
/// The two-second application poll is the same story on the Sources list.
///
/// `empty` is used only when `labels` is empty; see [`fill`].
pub fn sync(list: &ListBox, labels: &[String], empty: &str) -> Synced {
    if labels.is_empty() {
        // Already empty and already saying so.
        if showing_placeholder(list, empty) {
            return Synced::Kept;
        }
    } else if list.get_count() as usize == labels.len() {
        // Same rows, possibly renamed: touch only what differs. `set_string`
        // still raises a name change for the item it touches, so the diff is
        // what does the work.
        for (index, label) in labels.iter().enumerate() {
            let index = index as u32;
            if list.get_string(index).as_deref() != Some(label.as_str()) {
                list.set_string(index, label);
            }
        }
        return Synced::Kept;
    }
    fill(list, labels, empty);
    Synced::Rebuilt
}

/// True when `list` holds only its placeholder row — for refresh paths that
/// would otherwise rebuild an unchanged empty list.
pub fn showing_placeholder(list: &ListBox, empty: &str) -> bool {
    list.get_count() == 1 && list.get_string(0).as_deref() == Some(empty)
}

#[cfg(test)]
mod tests {
    use super::data_index;

    #[test]
    fn placeholder_row_is_not_data() {
        // The empty list shows one row; selecting it must not yield index 0.
        assert_eq!(data_index(Some(0), 0), None);
    }

    #[test]
    fn nothing_selected_is_none() {
        assert_eq!(data_index(None, 3), None);
    }

    #[test]
    fn real_rows_map_straight_through() {
        assert_eq!(data_index(Some(0), 3), Some(0));
        assert_eq!(data_index(Some(2), 3), Some(2));
    }

    #[test]
    fn stale_selection_past_the_end_is_rejected() {
        assert_eq!(data_index(Some(3), 3), None);
    }
}
