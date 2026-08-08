//! The sound-pack preview dialog, opened from the Preferences "Sound packs"
//! tab.
//!
//! It lists every sound event Pubsplash supports and plays the active pack's
//! sound for whichever one is selected. The list is built from
//! `soundpack::SoundKind::ALL` — the same registry `decode_all`, `compile` and
//! `parse_filename` iterate — so a sound event added there appears here with no
//! further work.
//!
//! Playback goes through `audio::cue`, which is local-only: a preview never
//! reaches the mixer, the stream, or a recording.

use super::{WXK_SPACE, show_error};
use crate::audio::cue::CuePlayback;
use crate::soundpack::SoundKind;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wxdragon::prelude::*;

/// Never seen: `SoundKind::ALL` is not empty. Kept because `list::fill` wants a
/// placeholder, and because NVDA reads an empty `ListBox` as "Unknown".
const NO_SOUND_EVENTS: &str = "No sound events";

/// One row per event Pubsplash supports, in `SoundKind::ALL` order.
fn event_labels() -> Vec<String> {
    SoundKind::ALL
        .iter()
        .map(|kind| kind.label().to_string())
        .collect()
}

pub fn show(parent: &Dialog) {
    let dialog = Dialog::builder(parent, "Preview sounds")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(360, 320)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // `native_acc` takes the list's name from this label, so its text and the
    // name passed to `install` have to agree, and it has to stay in front of
    // the list.
    let label = StaticText::builder(&panel).with_label("Sound event").build();
    let list = ListBox::builder(&panel).build();
    super::native_acc::install(&list, "Sound event");
    super::help::tag(&list, "dialog.soundPreview.list", "Sound event list");
    let labels = event_labels();
    super::list::fill(&list, &labels, NO_SOUND_EVENTS);
    if !labels.is_empty() {
        // A freshly built dialog is the one place a list may select for
        // itself; every later selection move belongs to the user.
        list.set_selection(0, true);
    }

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    // This dialog is its own top-level window, so its mnemonics are its own:
    // ALT+P and ALT+S are free here even though the Preferences dialog behind
    // it has claimed both.
    let play = Button::builder(&panel).with_label("&Play").build();
    super::set_accessible_name(&play, "Play");
    super::help::tag(&play, "dialog.soundPreview.play", "Play or stop button");
    // Dismiss-only, so `dismiss_button` puts both Enter and Escape on it.
    let close = super::dismiss_button(&panel, "C&lose");
    buttons.add(&play, 0, SizerFlag::All, 4);
    buttons.add(&close, 0, SizerFlag::All, 4);

    sizer.add(&label, 0, SizerFlag::All, 4);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    /// The cue this dialog started, if one is still playing, and the event it
    /// belongs to.
    ///
    /// The event is what tells a press meaning "stop" from one meaning "play
    /// this other sound instead" — see the toggle below.
    ///
    /// Nothing may hold a borrow of this across a call that shows a window:
    /// a message box re-enters the pump, which runs the parked callback below,
    /// which borrows it again.
    type Current = Rc<RefCell<Option<(SoundKind, CuePlayback)>>>;
    let current: Current = Rc::new(RefCell::new(None));
    let alive = Rc::new(Cell::new(true));

    // Keeps the button's word for what it does true at all times: it reads
    // "Stop" only when pressing it would stop something, which is only when the
    // sound playing is the one the list is sitting on. Arrowing to another
    // event turns it back into "Play" even though a sound is still going,
    // because "play the selected event" is what a press would then do.
    //
    // Label and accessible name always move together — a screen reader reads
    // the name, so setting one without the other leaves it saying the old word.
    let refresh_button: Rc<dyn Fn()> = {
        let current = current.clone();
        Rc::new(move || {
            let selected = selected_kind(&list);
            let stops = current
                .borrow()
                .as_ref()
                .is_some_and(|(kind, _)| selected == Some(*kind));
            let (label, name) = if stops {
                ("&Stop", "Stop")
            } else {
                ("&Play", "Play")
            };
            play.set_label(label);
            super::set_accessible_name(&play, name);
        })
    };

    let toggle: Rc<dyn Fn()> = {
        let current = current.clone();
        let alive = alive.clone();
        let refresh_button = refresh_button.clone();
        Rc::new(move || {
            let selected = selected_kind(&list);

            // Whatever is playing stops here either way; what differs is what
            // happens next. A press on the event that is already playing means
            // "stop", and that is all it means. A press after arrowing to a
            // different event means "cut this off and play that one instead" —
            // having to wait out the first sound to hear the second would make
            // comparing two events in a pack needlessly slow.
            let playing = current.borrow_mut().take();
            if let Some((playing_kind, handle)) = playing {
                handle.stop();
                if selected == Some(playing_kind) {
                    refresh_button();
                    return;
                }
            }

            // Every path from here that does not start a sound has to refresh
            // the button first: it may still read "Stop" from the cue just cut
            // off above, and it must not say so with nothing playing. Refreshed
            // before the message boxes rather than after, because those
            // re-enter the pump.
            let Some(kind) = selected else {
                // Only the placeholder row, which `SoundKind::ALL` never leaves
                // showing.
                refresh_button();
                return;
            };

            // `active` falls back to the pack baked into the executable, so
            // `None` here means even that could not be read.
            let Some(pack) = crate::soundpack::active() else {
                refresh_button();
                show_error(
                    &dialog,
                    "Preview sound",
                    "The sound pack could not be loaded.",
                );
                return;
            };
            let Some(samples) = pack.random_decoded(kind) else {
                refresh_button();
                // Informational, and it answers a button the user just pressed
                // rather than arriving on its own, so a modal is right here.
                let notice = MessageDialog::builder(
                    &dialog,
                    "This pack has no sound for that event.",
                    "Preview sound",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                notice.show_modal();
                return;
            };

            let handle = crate::audio::cue::play_samples_handle(samples);
            *current.borrow_mut() = Some((kind, handle.clone()));
            refresh_button();

            // A cue that reaches its end has to put the button back itself.
            let current = current.clone();
            let alive = alive.clone();
            let refresh_button = refresh_button.clone();
            super::run_when_ready(move || {
                if !alive.get() {
                    return true;
                }
                if handle.is_playing() {
                    return false;
                }
                // Only if this playback is still the current one: the user may
                // have cut it off for another event since, and that one owns
                // the button now. Compared by identity, not by event — the same
                // event played twice is two different playbacks.
                let mine = current
                    .borrow()
                    .as_ref()
                    .is_some_and(|(_, live)| live.is_same(&handle));
                if mine {
                    current.borrow_mut().take();
                    refresh_button();
                }
                true
            });
        })
    };

    {
        let toggle = toggle.clone();
        play.on_click(move |_| toggle());
    }
    {
        let toggle = toggle.clone();
        // Space plays the selected event, or stops it if it is the one already
        // playing, so a user who never leaves the list can do the whole job
        // from here. Not skipping the key consumes the `WM_KEYDOWN` before
        // `TranslateMessage`, so no `WM_CHAR` is posted and the list box's own
        // type-ahead never sees it.
        list.clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_SPACE, false)) => toggle(),
                _ => event.skip(true),
            });
    }
    {
        // Arrowing off the playing event changes what the button would do, so
        // it has to change what it says. This touches the button only, never
        // the list, so nothing is announced over the row the user just landed
        // on.
        let refresh_button = refresh_button.clone();
        list.clone().on_selection_changed(move |_| refresh_button());
    }
    {
        close.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    dialog.show_modal();
    // The parked callback must not touch a destroyed button, and a cue still
    // playing has nothing left to play into.
    alive.set(false);
    let playing = current.borrow_mut().take();
    if let Some((_, handle)) = playing {
        handle.stop();
    }
    dialog.destroy();
}

/// The event the list is sitting on, or `None` on the placeholder row.
fn selected_kind(list: &ListBox) -> Option<SoundKind> {
    super::list::selection(list, SoundKind::ALL.len()).map(|index| SoundKind::ALL[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is the sound-event registry, not a copy of it: a new
    /// `SoundKind` must reach the dialog without anyone editing this file.
    #[test]
    fn every_sound_event_gets_a_row() {
        let labels = event_labels();

        assert_eq!(labels.len(), SoundKind::ALL.len());
        for kind in SoundKind::ALL {
            assert!(
                labels.iter().any(|label| label == kind.label()),
                "{kind:?} has no row"
            );
        }
    }

    /// Two events sharing a label would give the user two identical rows with
    /// no way to tell which is which.
    #[test]
    fn no_two_events_share_a_label() {
        let mut labels = event_labels();
        let total = labels.len();

        labels.sort();
        labels.dedup();

        assert_eq!(labels.len(), total);
    }
}
