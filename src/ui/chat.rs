//! Chat tab: message list, view popup, and outbound input box.

use super::{App, WXK_ESCAPE};
use crate::net::NetCommand;
use crate::state::relative_time;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when there are no messages. See [`super::list`].
const NO_CHATS: &str = "No chats";

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, TextCtrl, Button) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let list_label = StaticText::builder(panel).with_label("Messages").build();
    let chat_list = ListBox::builder(panel).build();
    // Nothing refreshes this list until a stream starts, so seed the
    // placeholder here rather than leaving it empty (and unannounceable).
    super::list::fill(&chat_list, &[], NO_CHATS);
    super::native_acc::install(&chat_list, "Messages");
    super::help::tag(&chat_list, "tab.chat.messageList", "Chat message list");
    let view_button = Button::builder(panel).with_label("&View message").build();
    super::help::tag(
        &view_button,
        "tab.chat.viewButton",
        "View selected message button",
    );

    let input_label = StaticText::builder(panel)
        .with_label("Send a message")
        .build();
    let chat_input = TextCtrl::builder(panel)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    super::set_accessible_name(&chat_input, "Send a message");
    super::help::tag(&chat_input, "tab.chat.input", "Chat message input box");
    let send_button = Button::builder(panel).with_label("Se&nd").build();
    super::help::tag(
        &send_button,
        "tab.chat.sendButton",
        "Send chat message button",
    );

    // Created last so the common path (list, view, input, send) keeps the tab
    // order it had. ALT+O, because mnemonics are searched frame-wide and the
    // obvious letters are all spoken for: C is "Stop re&cording" and R is
    // "Start &recording" on the Home tab, H and T and F are the menu bar.
    let reconnect_button = Button::builder(panel).with_label("Rec&onnect chat").build();
    super::help::tag(
        &reconnect_button,
        "tab.chat.reconnectButton",
        "Reconnect chat feed button",
    );

    sizer.add(&list_label, 0, SizerFlag::All, 4);
    sizer.add(&chat_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&view_button, 0, SizerFlag::All, 4);
    sizer.add(&input_label, 0, SizerFlag::All, 4);
    sizer.add(&chat_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&send_button, 0, SizerFlag::All, 4);
    sizer.add(&reconnect_button, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    // The message rows are never rewritten while selected (see
    // `refresh_times`), so they are brought up to date at the two moments the
    // user is about to hear one instead.
    //
    // Arrowing within the list: the row just vacated is no longer selected, so
    // it updates at once and stepping off a message and back reads its age as
    // it is now. The row being moved *onto* is still skipped, so this cannot
    // talk over it.
    {
        let app = app.clone();
        chat_list
            .clone()
            .on_selection_changed(move |_| refresh_chat_times(&app));
    }
    // Focus arriving on the list: whatever row it lands on is about to be read
    // out, so this is the one refresh allowed to write the selected row.
    {
        let app = app.clone();
        chat_list.clone().on_set_focus(move |event| {
            refresh_times(&app, Selected::Write);
            event.skip(true);
        });
    }

    // View popup (button or double-click).
    {
        let app = app.clone();
        let list = chat_list;
        view_button.on_click(move |_| view_selected(&app, &list));
    }
    {
        let app = app.clone();
        let list = chat_list;
        chat_list
            .clone()
            .on_item_double_clicked(move |_| view_selected(&app, &list));
    }

    // Sending: Enter in the box or the Send button.
    {
        let app = app.clone();
        let input = chat_input;
        chat_input.clone().on_text_enter(move |event| {
            send_message(&app, &input);
            // A single-line EDIT beeps when a `\r` WM_CHAR reaches its window
            // proc, and `wxTextCtrl::OnChar` only withholds that char when the
            // wxEVT_TEXT_ENTER handler reports the event as processed. wxdragon
            // pre-sets Skip(true) before every closure, so an ignored event
            // counts as unhandled — not skipping here is what keeps the send
            // silent. (Unrelated to the `slider_uia` DLGC_WANTCHARS beep: the
            // ProcessEnter style already keeps `IsDialogMessage` out of this.)
            event.skip(false);
        });
    }
    {
        let app = app.clone();
        let input = chat_input;
        send_button.on_click(move |_| send_message(&app, &input));
    }

    // Escape clears the input box.
    {
        let input = chat_input;
        chat_input.clone().on_key_down(move |event| {
            if super::key_of(&event).map(|(code, _)| code) == Some(WXK_ESCAPE) {
                input.set_value("");
            } else {
                event.skip(true);
            }
        });
    }

    {
        let app = app.clone();
        let button = reconnect_button;
        reconnect_button
            .clone()
            .on_click(move |_| reconnect_chat(&app, &button));
    }

    (chat_list, chat_input, reconnect_button)
}

/// Drops the live-events connection and opens a new one. The stream itself is
/// untouched: the chat feed and the Icecast connection are separate tasks, so
/// this costs the broadcast nothing.
fn reconnect_chat(app: &Rc<App>, button: &Button) {
    if !matches!(app.run.borrow().stream, super::StreamState::Live { .. }) {
        super::show_error(
            button,
            "Chat",
            "You can only reconnect the chat feed while streaming.",
        );
        return;
    }
    app.net.send(NetCommand::ReconnectChat);
}

fn send_message(app: &Rc<App>, input: &TextCtrl) {
    let content = input.get_value();
    let content = content.trim();
    if content.is_empty() {
        return;
    }
    if !matches!(app.run.borrow().stream, super::StreamState::Live { .. }) {
        super::show_error(
            input,
            "Chat",
            "You can only send chat messages while streaming.",
        );
        return;
    }
    app.net.send(NetCommand::SendChat(content.to_string()));
    input.set_value("");
}

fn view_selected(app: &Rc<App>, list: &ListBox) {
    let Some(index) = super::list::selection(list, app.run.borrow().chat.len()) else {
        return;
    };
    let Some((user, content)) = app
        .run
        .borrow()
        .chat
        .get(index)
        .map(|entry| (entry.user.clone(), entry.content.clone()))
    else {
        return;
    };

    let Some(frame) = app.widgets(|w| w.frame) else {
        return;
    };
    let dialog = Dialog::builder(&frame, &format!("Message from {user}"))
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(500, 300)
        .build();
    let panel = Panel::builder(&dialog).build();
    // Read-only but selectable/copyable.
    let text = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .with_value(&format!("{user}: {content}"))
        .build();
    super::help::tag(&text, "dialog.chatView.text", "Full chat message text");
    // Dismiss-only, so `dismiss_button` puts both Escape and Enter on it. The
    // message body above is `MultiLine`, which wx exempts from the default-item
    // handling, so Enter there still moves the caret rather than closing.
    let close = super::dismiss_button(&panel, "Close");
    {
        close.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
    let panel_sizer = BoxSizer::builder(Orientation::Vertical).build();
    panel_sizer.add(&text, 1, SizerFlag::Expand | SizerFlag::All, 8);
    panel_sizer.add(&close, 0, SizerFlag::All, 8);
    panel.set_sizer(panel_sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);
    dialog.show_modal();
    dialog.destroy();
}

/// The list label for an entry showing `age` as its relative time. The
/// unchanging half is `entry.prefix`, precomputed when the message arrived.
fn label_for(entry: &super::ChatEntry, age: &str) -> String {
    format!("{}: {}", entry.prefix, age)
}

/// Repopulates the whole list. Only for when the history itself changed out
/// from under the list (it is cleared at the start of a stream) — an arriving
/// message goes through [`append_new_messages`] instead.
pub fn refresh_chat_list(app: &App) {
    app.widgets(|w| {
        let mut run = app.run.borrow_mut();
        let selected = w.chat_list.get_selection();
        let labels: Vec<String> = run
            .chat
            .iter_mut()
            .map(|entry| {
                entry.shown_age = relative_time(entry.received.elapsed());
                label_for(entry, &entry.shown_age)
            })
            .collect();
        super::list::fill(&w.chat_list, &labels, NO_CHATS);
        if let Some(index) = selected {
            if index < w.chat_list.get_count() {
                w.chat_list.set_selection(index, true);
            }
        } else if !run.chat.is_empty() {
            // Keep the newest message visible.
            w.chat_list.ensure_visible(run.chat.len() as i32 - 1);
        }
    });
}

/// Appends the `count` newest entries to the list.
///
/// Rebuilding the list per arriving message made a session cost O(n²) in
/// formats and FFI appends, and clearing a list a screen-reader user may be
/// sitting in is worse for them than leaving it alone — so messages are only
/// ever added.
pub fn append_new_messages(app: &App, count: usize) {
    app.widgets(|w| {
        let mut run = app.run.borrow_mut();
        let total = run.chat.len();
        let start = total.saturating_sub(count);
        if start == 0 {
            // These are the first messages, so the list is still holding the
            // placeholder row — that one has to go before anything is added.
            w.chat_list.clear();
        }
        for entry in run.chat[start..].iter_mut() {
            entry.shown_age = relative_time(entry.received.elapsed());
            w.chat_list.append(&label_for(entry, &entry.shown_age));
        }
        // Keep the newest message visible, as the full rebuild does.
        if w.chat_list.get_selection().is_none() && total > 0 {
            w.chat_list.ensure_visible(total as i32 - 1);
        }
    });
}

/// Whether a row the user has selected may be rewritten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    /// Leave it alone: this refresh was not prompted by the user.
    Skip,
    /// Write it: the row is about to be read out anyway.
    Write,
}

/// Updates only labels whose relative time has changed (runs every second).
///
/// `relative_time` moves in buckets — seconds, then minutes, then hours — so
/// after the first minute almost every entry is unchanged on any given tick.
/// Comparing against the stored `shown_age` skips the label formatting and the
/// `get_string` FFI round-trip for all of them, which matters because this runs
/// over the entire history once a second for as long as the app is open.
pub fn refresh_chat_times(app: &App) {
    refresh_times(app, Selected::Skip);
}

/// The body of [`refresh_chat_times`], with the rule that keeps it silent.
///
/// `set_string` on the *selected* row is announced whether or not the list has
/// focus; on any other row it is silent. `home::refresh` documents that finding
/// at length — the consequence here is that a message's age changes every
/// second for its first minute, so a user parked on a recent message heard it
/// read out once a second. The tick therefore never writes the selected row,
/// whichever row it is, and nothing in this list can speak on its own.
///
/// The skipped row's `shown_age` is deliberately left stale as well, so the row
/// is retried the moment it stops being the selected one.
fn refresh_times(app: &App, selected_rows: Selected) {
    // Widget writes happen with nothing borrowed: `on_selection_changed` and
    // `on_set_focus` both land here, so a `run` borrow held across a
    // `set_string` would be a panic waiting on whichever wx build decides to
    // raise an event from one.
    let mut pending: Vec<(u32, String)> = Vec::new();
    let selected = app.widgets(|w| w.chat_list.get_selection()).flatten();

    {
        let mut run = app.run.borrow_mut();
        // Indexing rows by position is safe here: the placeholder row exists
        // only while `run.chat` is empty, and then this loop does not run.
        for (index, entry) in run.chat.iter_mut().enumerate() {
            let age = relative_time(entry.received.elapsed());
            if age == entry.shown_age {
                continue;
            }
            if selected_rows == Selected::Skip && selected == Some(index as u32) {
                continue;
            }
            pending.push((index as u32, label_for(entry, &age)));
            entry.shown_age = age;
        }
    }

    if pending.is_empty() {
        return;
    }
    app.widgets(|w| {
        for (index, label) in &pending {
            w.chat_list.set_string(*index, label);
        }
    });
}
