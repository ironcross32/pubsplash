//! The per-source Sends dialog: choose which buses a source feeds, at what
//! level, and whether the source also goes directly to master.

use super::{App, WXK_DELETE, show_error};
use crate::config::SendConfig;
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Opens the sends dialog for one source of one scene.
pub fn edit_sends(app: &Rc<App>, scene_index: usize, source_index: usize) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let (source_name, to_master, sends, bus_names) = {
        let config = app.config.borrow();
        let Some(source) = config
            .scenes
            .scenes
            .get(scene_index)
            .and_then(|s| s.sources.get(source_index))
        else {
            return;
        };
        (
            crate::source_name::strip_label(
                source,
                &app.name_context(std::slice::from_ref(source)),
            ),
            source.to_master,
            source.sends.clone(),
            config
                .buses
                .buses
                .iter()
                .map(|b| b.name.clone())
                .collect::<Vec<_>>(),
        )
    };
    if bus_names.is_empty() {
        show_error(
            &frame,
            "Sends",
            "There are no buses yet. Create one on the Buses tab first.",
        );
        return;
    }

    let dialog = Dialog::builder(&frame, &format!("Sends for {source_name}"))
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(420, 380)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let master_check = CheckBox::builder(&panel)
        .with_label("Send directly to &master")
        .build();
    super::set_accessible_name(&master_check, "Send directly to master");
    super::help::tag(
        &master_check,
        "dialog.sends.toMaster",
        "Send this source directly to master checkbox",
    );
    master_check.set_value(to_master);

    let list_label = StaticText::builder(&panel).with_label("Sends").build();
    let send_list = ListBox::builder(&panel).build();
    super::set_accessible_name(&send_list, "Sends");
    super::help::tag(
        &send_list,
        "dialog.sends.sendList",
        "Bus sends for this source list",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add = Button::builder(&panel).with_label("&Add send").build();
    let remove = Button::builder(&panel).with_label("&Remove send").build();
    super::help::tag(&add, "dialog.sends.add", "Add a send to a bus button");
    super::help::tag(
        &remove,
        "dialog.sends.remove",
        "Remove selected send button",
    );
    buttons.add(&add, 0, SizerFlag::All, 4);
    buttons.add(&remove, 0, SizerFlag::All, 4);

    let level_label = StaticText::builder(&panel).with_label("Send level").build();
    let level_slider = Slider::builder(&panel)
        .with_value(100)
        .with_min_value(0)
        .with_max_value(100)
        .build();
    super::set_accessible_name(&level_slider, "Send level");
    super::help::tag(
        &level_slider,
        "dialog.sends.level",
        "Send level slider for the selected bus",
    );

    let ok_cancel = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = Button::builder(&panel).with_label("OK").build();
    let cancel = Button::builder(&panel).with_label("Cancel").build();
    ok_cancel.add(&ok, 0, SizerFlag::All, 4);
    ok_cancel.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&master_check, 0, SizerFlag::All, 8);
    sizer.add(&list_label, 0, SizerFlag::All, 4);
    sizer.add(&send_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    sizer.add(&level_label, 0, SizerFlag::All, 4);
    sizer.add(&level_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&ok_cancel, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // Working copy shared by the handlers; written back only on OK.
    let working: Rc<RefCell<Vec<SendConfig>>> = Rc::new(RefCell::new(sends));

    let refresh_list = {
        let working = working.clone();
        let send_list = send_list.clone();
        move |select: Option<usize>| {
            let previous = send_list.get_selection();
            send_list.clear();
            for send in working.borrow().iter() {
                send_list.append(&format!("{}, level {}", send.bus, send.level));
            }
            let target = select
                .map(|i| i as u32)
                .or(previous)
                .filter(|&i| i < send_list.get_count());
            if let Some(index) = target {
                send_list.set_selection(index, true);
            }
        }
    };
    refresh_list(Some(0));

    // Selecting a send loads its level into the slider (and renames the
    // slider so the screen reader says which send it now controls).
    let load_selected = {
        let working = working.clone();
        let send_list = send_list.clone();
        let level_slider = level_slider.clone();
        move || {
            let Some(index) = send_list.get_selection() else {
                return;
            };
            if let Some(send) = working.borrow().get(index as usize) {
                level_slider.set_value(send.level as i32);
                super::set_accessible_name(&level_slider, &format!("Send level for {}", send.bus));
            }
        }
    };
    load_selected();
    {
        let load_selected = load_selected.clone();
        send_list
            .clone()
            .on_selection_changed(move |_| load_selected());
    }

    // Slider edits the selected send's level.
    {
        let working = working.clone();
        let send_list = send_list.clone();
        let level_slider = level_slider.clone();
        let refresh_list = refresh_list.clone();
        level_slider.clone().on_slider(move |_| {
            let Some(index) = send_list.get_selection() else {
                return;
            };
            let value = level_slider.value().clamp(0, 100) as u32;
            if let Some(send) = working.borrow_mut().get_mut(index as usize) {
                send.level = value;
            }
            refresh_list(Some(index as usize));
        });
    }

    // Add send: pick from buses this source doesn't send to yet.
    {
        let working = working.clone();
        let refresh_list = refresh_list.clone();
        let load_selected = load_selected.clone();
        let dialog = dialog.clone();
        let bus_names = bus_names.clone();
        add.on_click(move |_| {
            let available: Vec<String> = {
                let working = working.borrow();
                bus_names
                    .iter()
                    .filter(|name| !working.iter().any(|s| &s.bus == *name))
                    .cloned()
                    .collect()
            };
            if available.is_empty() {
                show_error(
                    &dialog,
                    "Add send",
                    "This source already sends to every bus.",
                );
                return;
            }
            let refs: Vec<&str> = available.iter().map(|s| s.as_str()).collect();
            let chooser =
                SingleChoiceDialog::builder(&dialog, "Send to which bus?", "Add send", &refs)
                    .build();
            if chooser.show_modal() == ID_OK {
                let index = chooser.get_selection();
                if let Some(name) = available.get(index as usize) {
                    working.borrow_mut().push(SendConfig {
                        bus: name.clone(),
                        level: 100,
                    });
                    let new_index = working.borrow().len() - 1;
                    refresh_list(Some(new_index));
                    load_selected();
                }
            }
        });
    }

    // Remove send (button or Delete key on the list).
    let remove_selected = {
        let working = working.clone();
        let send_list = send_list.clone();
        let refresh_list = refresh_list.clone();
        let load_selected = load_selected.clone();
        move || {
            let Some(index) = send_list.get_selection() else {
                return;
            };
            let index = index as usize;
            {
                let mut working = working.borrow_mut();
                if index >= working.len() {
                    return;
                }
                working.remove(index);
            }
            let remaining = working.borrow().len();
            refresh_list(Some(index.min(remaining.saturating_sub(1))));
            load_selected();
        }
    };
    {
        let remove_selected = remove_selected.clone();
        remove.on_click(move |_| remove_selected());
    }
    {
        send_list
            .clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_DELETE, _)) => remove_selected(),
                _ => event.skip(true),
            });
    }

    {
        let dialog = dialog.clone();
        ok.on_click(move |_| dialog.end_modal(ID_OK));
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    if dialog.show_modal() == ID_OK {
        {
            let mut config = app.config.borrow_mut();
            if let Some(source) = config
                .scenes
                .scenes
                .get_mut(scene_index)
                .and_then(|s| s.sources.get_mut(source_index))
            {
                source.to_master = master_check.get_value();
                source.sends = working.borrow().clone();
            }
        }
        app.save_config();
        app.sync_engine_sources();
    }
    dialog.destroy();
}
