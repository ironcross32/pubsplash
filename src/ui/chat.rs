//! Chat tab: message list, view popup, and outbound input box.

use super::{App, WXK_ESCAPE};
use crate::net::NetCommand;
use crate::state::relative_time;
use std::rc::Rc;
use wxdragon::prelude::*;

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, TextCtrl) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let list_label = StaticText::builder(panel).with_label("Messages").build();
    let chat_list = ListBox::builder(panel).build();
    super::set_accessible_name(&chat_list, "Messages");
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

    sizer.add(&list_label, 0, SizerFlag::All, 4);
    sizer.add(&chat_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&view_button, 0, SizerFlag::All, 4);
    sizer.add(&input_label, 0, SizerFlag::All, 4);
    sizer.add(&chat_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&send_button, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    // View popup (button or double-click).
    {
        let app = app.clone();
        let list = chat_list.clone();
        view_button.on_click(move |_| view_selected(&app, &list));
    }
    {
        let app = app.clone();
        let list = chat_list.clone();
        chat_list
            .clone()
            .on_item_double_clicked(move |_| view_selected(&app, &list));
    }

    // Sending: Enter in the box or the Send button.
    {
        let app = app.clone();
        let input = chat_input.clone();
        chat_input
            .clone()
            .on_text_enter(move |_| send_message(&app, &input));
    }
    {
        let app = app.clone();
        let input = chat_input.clone();
        send_button.on_click(move |_| send_message(&app, &input));
    }

    // Escape clears the input box.
    {
        let input = chat_input.clone();
        chat_input.clone().on_key_down(move |event| {
            if super::key_of(&event).map(|(code, _)| code) == Some(WXK_ESCAPE) {
                input.set_value("");
            } else {
                event.skip(true);
            }
        });
    }

    (chat_list, chat_input)
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
    let Some(index) = list.get_selection() else {
        return;
    };
    let Some((user, content)) = app
        .run
        .borrow()
        .chat
        .get(index as usize)
        .map(|entry| (entry.user.clone(), entry.content.clone()))
    else {
        return;
    };

    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
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
    let close = Button::builder(&panel).with_label("Close").build();
    {
        let dialog = dialog.clone();
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

fn format_entry(entry: &super::ChatEntry) -> String {
    let content = entry.content.replace(['\r', '\n'], " ");
    format!(
        "{}: {}: {}",
        entry.user,
        content,
        relative_time(entry.received.elapsed())
    )
}

/// Repopulates the whole list (new message arrived or stream restarted).
pub fn refresh_chat_list(app: &App) {
    app.widgets(|w| {
        let run = app.run.borrow();
        let selected = w.chat_list.get_selection();
        w.chat_list.clear();
        for entry in &run.chat {
            w.chat_list.append(&format_entry(entry));
        }
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

/// Updates only labels whose relative time has changed (runs every second).
pub fn refresh_chat_times(app: &App) {
    app.widgets(|w| {
        let run = app.run.borrow();
        for (index, entry) in run.chat.iter().enumerate() {
            let label = format_entry(entry);
            if w.chat_list.get_string(index as u32).as_deref() != Some(label.as_str()) {
                w.chat_list.set_string(index as u32, &label);
            }
        }
    });
}
