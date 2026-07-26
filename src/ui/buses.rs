//! Buses tab: manage the global mixing buses that sources send to, and edit
//! the FX chain on the selected bus (or on the master output).
//!
//! The bus list has a pinned first row, "Master output", which stands for the
//! master FX chain; the remaining rows are the real buses. So list row 0 maps
//! to `ChainTarget::Master` and row `n` to `ChainTarget::Bus(n - 1)`. Volume,
//! mute, rename, remove, and reorder apply to real buses only (in the Home
//! mixer for volume/mute); row 0 only offers an FX chain.

use super::fx::{self, ChainTarget};
use super::{App, WXK_DELETE, WXK_DOWN, WXK_UP, show_error, show_info};
use crate::state::{ListEdit, move_down, move_up};
use std::rc::Rc;
use wxdragon::prelude::*;

/// Rows before a bus in the list (the pinned "Master output" row).
const MASTER_ROWS: u32 = 1;

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, ListBox, CheckBox) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // --- Buses ---
    let label = StaticText::builder(panel).with_label("Buses").build();
    let bus_list = ListBox::builder(panel).build();
    super::set_accessible_name(&bus_list, "Buses");
    super::help::tag(
        &bus_list,
        "tab.buses.busList",
        "Buses list (row 0 is the master output)",
    );
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add = Button::builder(panel).with_label("&Add bus").build();
    let rename = Button::builder(panel).with_label("&Rename bus").build();
    let remove = Button::builder(panel).with_label("Re&move bus").build();
    let up = Button::builder(panel).with_label("Move &up").build();
    let down = Button::builder(panel).with_label("Move &down").build();
    super::help::tag(&add, "tab.buses.busAdd", "Add bus button");
    super::help::tag(&rename, "tab.buses.busRename", "Rename bus button");
    super::help::tag(&remove, "tab.buses.busRemove", "Remove bus button");
    super::help::tag(&up, "tab.buses.busUp", "Move bus up button");
    super::help::tag(&down, "tab.buses.busDown", "Move bus down button");
    for b in [&add, &rename, &remove, &up, &down] {
        buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&label, 0, SizerFlag::All, 4);
    sizer.add(&bus_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);

    // --- FX chain for the selected bus ---
    let fx_label = StaticText::builder(panel)
        .with_label("Effects on selected bus")
        .build();
    let fx_list = ListBox::builder(panel).build();
    super::set_accessible_name(&fx_list, "Effects on selected bus");
    super::help::tag(
        &fx_list,
        "tab.buses.fxList",
        "Effects chain on the selected bus",
    );
    let fx_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let fx_add = Button::builder(panel).with_label("Add &plugin").build();
    let fx_remove = Button::builder(panel).with_label("Remove plugi&n").build();
    let fx_up = Button::builder(panel).with_label("Move plugin u&p").build();
    let fx_down = Button::builder(panel)
        .with_label("Move plugin do&wn")
        .build();
    let fx_bypass = CheckBox::builder(panel).with_label("&Bypass").build();
    super::set_accessible_name(&fx_bypass, "Bypass selected plugin");
    let fx_edit = Button::builder(panel)
        .with_label("&Edit parameters")
        .build();
    let fx_open = Button::builder(panel).with_label("Open &interface").build();
    super::help::tag(&fx_add, "tab.buses.fxAdd", "Add plugin to chain button");
    super::help::tag(
        &fx_remove,
        "tab.buses.fxRemove",
        "Remove plugin from chain button",
    );
    super::help::tag(&fx_up, "tab.buses.fxUp", "Move plugin up in chain button");
    super::help::tag(
        &fx_down,
        "tab.buses.fxDown",
        "Move plugin down in chain button",
    );
    super::help::tag(
        &fx_bypass,
        "tab.buses.fxBypass",
        "Bypass selected plugin checkbox",
    );
    super::help::tag(
        &fx_edit,
        "tab.buses.fxEditParams",
        "Edit plugin parameters button",
    );
    super::help::tag(
        &fx_open,
        "tab.buses.fxOpenInterface",
        "Open plugin interface button",
    );
    for b in [&fx_add, &fx_remove, &fx_up, &fx_down] {
        fx_buttons.add(b, 0, SizerFlag::All, 4);
    }
    fx_buttons.add(
        &fx_bypass,
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::All,
        4,
    );
    for b in [&fx_edit, &fx_open] {
        fx_buttons.add(b, 0, SizerFlag::All, 4);
    }

    // Chain library row.
    let lib_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let save_chain = Button::builder(panel).with_label("&Save chain...").build();
    let load_chain = Button::builder(panel).with_label("&Load chain...").build();
    let import_chain = Button::builder(panel)
        .with_label("&Import chain...")
        .build();
    let export_chain = Button::builder(panel)
        .with_label("E&xport chain...")
        .build();
    super::help::tag(
        &save_chain,
        "tab.buses.chainSave",
        "Save FX chain to library button",
    );
    super::help::tag(
        &load_chain,
        "tab.buses.chainLoad",
        "Load FX chain from library button",
    );
    super::help::tag(
        &import_chain,
        "tab.buses.chainImport",
        "Import FX chain file button",
    );
    super::help::tag(
        &export_chain,
        "tab.buses.chainExport",
        "Export FX chain file button",
    );
    for b in [&save_chain, &load_chain, &import_chain, &export_chain] {
        lib_buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&fx_label, 0, SizerFlag::All, 4);
    sizer.add(&fx_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&fx_buttons, 0, SizerFlag::Expand, 0);
    sizer.add_sizer(&lib_buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    // Selecting a bus refreshes the FX list.
    {
        let app = app.clone();
        bus_list
            .clone()
            .on_selection_changed(move |_| refresh_fx_list(&app));
    }

    // Bus buttons.
    {
        let app = app.clone();
        add.on_click(move |_| add_bus(&app));
    }
    {
        let app = app.clone();
        let list = bus_list.clone();
        rename.on_click(move |_| rename_bus(&app, &list));
    }
    {
        let app = app.clone();
        let list = bus_list.clone();
        remove.on_click(move |_| remove_bus(&app, &list));
    }
    {
        let app = app.clone();
        let list = bus_list.clone();
        up.on_click(move |_| move_bus(&app, &list, true));
    }
    {
        let app = app.clone();
        let list = bus_list.clone();
        down.on_click(move |_| move_bus(&app, &list, false));
    }
    {
        let app = app.clone();
        let list = bus_list.clone();
        bus_list
            .clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_UP, true)) => move_bus(&app, &list, true),
                Some((WXK_DOWN, true)) => move_bus(&app, &list, false),
                Some((WXK_DELETE, _)) => remove_bus(&app, &list),
                _ => event.skip(true),
            });
    }

    // FX chain buttons.
    {
        let app = app.clone();
        fx_add.on_click(move |_| add_plugin(&app));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_remove.on_click(move |_| remove_plugin(&app, &list));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_up.on_click(move |_| move_plugin(&app, &list, true));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_down.on_click(move |_| move_plugin(&app, &list, false));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        let fx_bypass = fx_bypass.clone();
        fx_bypass
            .clone()
            .on_toggled(move |_| set_bypass(&app, &list, fx_bypass.get_value()));
    }
    // Keep the Bypass checkbox showing the newly selected plugin's state.
    {
        let app = app.clone();
        fx_list
            .clone()
            .on_selection_changed(move |_| sync_bypass_check(&app));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_edit.on_click(move |_| edit_parameters(&app, &list));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_open.on_click(move |_| open_interface(&app, &list));
    }

    // Chain library buttons.
    {
        let app = app.clone();
        save_chain.on_click(move |_| save_chain_to_library(&app));
    }
    {
        let app = app.clone();
        load_chain.on_click(move |_| load_chain_from_library(&app));
    }
    {
        let app = app.clone();
        import_chain.on_click(move |_| import_chain_file(&app));
    }
    {
        let app = app.clone();
        export_chain.on_click(move |_| export_chain_file(&app));
    }
    {
        let app = app.clone();
        let list = fx_list.clone();
        fx_list
            .clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_UP, true)) => move_plugin(&app, &list, true),
                Some((WXK_DOWN, true)) => move_plugin(&app, &list, false),
                Some((WXK_DELETE, _)) => remove_plugin(&app, &list),
                _ => event.skip(true),
            });
    }

    (bus_list, fx_list, fx_bypass)
}

/// The chain the given bus-list row targets.
fn target_for(row: u32) -> ChainTarget {
    if row < MASTER_ROWS {
        ChainTarget::Master
    } else {
        ChainTarget::Bus((row - MASTER_ROWS) as usize)
    }
}

/// The bus-list row currently selected (defaults to Master).
fn selected_target(app: &Rc<App>) -> ChainTarget {
    app.widgets(|w| target_for(w.bus_list.get_selection().unwrap_or(0)))
        .unwrap_or(ChainTarget::Master)
}

/// Save, refresh both lists, re-sync the engine. Sources go in the same
/// routing command as the buses because their send targets are bus indices,
/// which a bus edit can shift.
fn after_bus_edit(app: &Rc<App>) {
    app.save_config();
    refresh_bus_list(app);
    refresh_fx_list(app);
    app.sync_engine_routing();
    super::home::rebuild_mixer(app);
}

pub fn refresh_bus_list(app: &Rc<App>) {
    app.widgets(|w| {
        let config = app.config.borrow();
        let selected = w.bus_list.get_selection();
        w.bus_list.clear();
        w.bus_list.append("Master output");
        for bus in &config.buses.buses {
            w.bus_list.append(&bus.name);
        }
        let target = selected.filter(|&i| i < w.bus_list.get_count());
        if let Some(index) = target {
            w.bus_list.set_selection(index, true);
        } else {
            w.bus_list.set_selection(0, true);
        }
    });
}

/// Refreshes the FX list to show the selected bus's chain.
pub fn refresh_fx_list(app: &Rc<App>) {
    let target = selected_target(app);
    // Build the labels first, reading the chain in place rather than cloning
    // it: `FxSlotConfig` owns the plugin's serialized state, which can be
    // hundreds of kilobytes, and none of it is needed here.
    let count = fx::with_slots(app, target, |slots| slots.len()).unwrap_or(0);
    // Whether each instance actually loaded lives in `app.fx`, not the config,
    // and is gathered separately so neither borrow spans the other.
    let loaded: Vec<bool> = (0..count)
        .map(|i| fx::instance_at(app, target, i).is_some())
        .collect();
    let labels = fx::with_slots(app, target, |slots| {
        slots
            .iter()
            .enumerate()
            .map(|(i, slot)| fx::slot_label(i, slot, loaded.get(i).copied().unwrap_or(false)))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    app.widgets(|w| {
        let previous = w.fx_list.get_selection();
        w.fx_list.clear();
        for label in &labels {
            w.fx_list.append(label);
        }
        if let Some(index) = previous.filter(|&i| i < w.fx_list.get_count()) {
            w.fx_list.set_selection(index, true);
        }
    });
    sync_bypass_check(app);
}

fn add_bus(app: &Rc<App>) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let dialog = TextEntryDialog::builder(&frame, "Bus name:", "Add bus").build();
    if dialog.show_modal() == ID_OK {
        if let Some(name) = dialog.get_value() {
            let result = app.config.borrow_mut().add_bus(&name);
            match result {
                ListEdit::Changed => {
                    fx::on_bus_added(app);
                    after_bus_edit(app);
                    app.widgets(|w| {
                        let count = w.bus_list.get_count();
                        if count > 0 {
                            w.bus_list.set_selection(count - 1, true);
                        }
                    });
                    refresh_fx_list(app);
                }
                ListEdit::Unchanged if !name.trim().is_empty() => {
                    show_error(&frame, "Add bus", "A bus with that name already exists.");
                }
                ListEdit::Unchanged => {}
            }
        }
    }
    dialog.destroy();
}

fn rename_bus(app: &Rc<App>, list: &ListBox) {
    let ChainTarget::Bus(index) = target_for(list.get_selection().unwrap_or(0)) else {
        return;
    };
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let current = {
        let config = app.config.borrow();
        let Some(bus) = config.buses.buses.get(index) else {
            return;
        };
        bus.name.clone()
    };
    let dialog = TextEntryDialog::builder(&frame, "New bus name:", "Rename bus")
        .with_default_value(&current)
        .build();
    if dialog.show_modal() == ID_OK {
        if let Some(name) = dialog.get_value() {
            let result = app.config.borrow_mut().rename_bus(index, &name);
            match result {
                ListEdit::Changed => after_bus_edit(app),
                ListEdit::Unchanged if !name.trim().is_empty() && name.trim() != current => {
                    show_error(&frame, "Rename bus", "A bus with that name already exists.");
                }
                ListEdit::Unchanged => {}
            }
        }
    }
    dialog.destroy();
}

fn remove_bus(app: &Rc<App>, list: &ListBox) {
    let ChainTarget::Bus(index) = target_for(list.get_selection().unwrap_or(0)) else {
        return;
    };
    // Closing the editors snapshots each open plugin's state back into its
    // config slot, addressed by bus index — so it has to happen while the
    // config still agrees with `app.fx` about what lives at which index.
    // Deleting first would write the snapshot into whichever bus shifted down
    // into this slot. (`fx::remove_plugin` closes first for the same reason.)
    super::fx_editor::close_all(app);
    if app.config.borrow_mut().delete_bus(index) == ListEdit::Changed {
        fx::on_bus_removed(app, index);
        after_bus_edit(app);
    }
}

fn move_bus(app: &Rc<App>, list: &ListBox, towards_start: bool) {
    let ChainTarget::Bus(index) = target_for(list.get_selection().unwrap_or(0)) else {
        return;
    };
    // Snapshot open editors before the swap, for the same reason as
    // `remove_bus`: the snapshot is written back by bus index.
    super::fx_editor::close_all(app);
    let result = {
        let mut config = app.config.borrow_mut();
        if towards_start {
            move_up(&mut config.buses.buses, index)
        } else {
            move_down(&mut config.buses.buses, index)
        }
    };
    if result == ListEdit::Changed {
        let other = if towards_start { index - 1 } else { index + 1 };
        fx::on_bus_moved(app, index, other);
        after_bus_edit(app);
        app.widgets(|w| w.bus_list.set_selection((other as u32) + MASTER_ROWS, true));
        refresh_fx_list(app);
    }
}

fn add_plugin(app: &Rc<App>) {
    let target = selected_target(app);
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    // Only VST2 plugins can be hosted in this version.
    let choices: Vec<(String, crate::config::PluginRef)> = {
        let cache = app.plugins.borrow();
        cache
            .plugins
            .iter()
            .filter(|p| p.format == crate::vst::PluginFormat::Vst2)
            .map(|p| (p.name.clone(), crate::config::PluginRef::from_info(p)))
            .collect()
    };
    if choices.is_empty() {
        show_info(
            &frame,
            "Add plugin",
            "No VST2 plugins are available. Scan for plugins in Preferences first (VST3 hosting is coming in a later version).",
        );
        return;
    }
    let labels: Vec<&str> = choices.iter().map(|(name, _)| name.as_str()).collect();
    let dialog =
        SingleChoiceDialog::builder(&frame, "Add which plugin?", "Add plugin", &labels).build();
    if dialog.show_modal() == ID_OK {
        let index = dialog.get_selection() as usize;
        if let Some((name, plugin)) = choices.get(index) {
            let loaded = fx::add_plugin(app, target, plugin.clone());
            refresh_bus_list(app);
            refresh_fx_list(app);
            super::home::rebuild_mixer(app);
            if !loaded {
                show_error(
                    &frame,
                    "Add plugin",
                    &format!(
                        "{name} was added but could not be loaded, so it will not process audio."
                    ),
                );
            }
        }
    }
    dialog.destroy();
}

fn remove_plugin(app: &Rc<App>, list: &ListBox) {
    let target = selected_target(app);
    let Some(slot) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    fx::remove_plugin(app, target, slot);
    refresh_fx_list(app);
}

fn move_plugin(app: &Rc<App>, list: &ListBox, towards_start: bool) {
    let target = selected_target(app);
    let Some(slot) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    if fx::move_plugin(app, target, slot, towards_start) {
        refresh_fx_list(app);
        let new_slot = if towards_start { slot - 1 } else { slot + 1 };
        app.widgets(|w| w.fx_list.set_selection(new_slot as u32, true));
    }
}

/// Applies the Bypass checkbox state to the selected plugin.
fn set_bypass(app: &Rc<App>, list: &ListBox, bypass: bool) {
    let target = selected_target(app);
    let Some(slot) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    fx::set_bypass(app, target, slot, bypass);
    refresh_fx_list(app);
}

/// Reflects the selected plugin's bypass state in the Bypass checkbox (and
/// disables it when no plugin is selected).
fn sync_bypass_check(app: &Rc<App>) {
    let target = selected_target(app);
    // Bound to the FX list's selection-changed event, so this runs on every
    // arrow key: project to the one flag it needs rather than cloning the
    // chain (and every plugin's serialized state with it).
    let selected = app.widgets(|w| w.fx_list.get_selection()).flatten();
    let bypassed = selected.and_then(|i| {
        fx::with_slots(app, target, |slots| {
            slots.get(i as usize).map(|s| s.bypass).unwrap_or(false)
        })
    });
    app.widgets(|w| match bypassed {
        Some(bypassed) => {
            w.fx_bypass.set_value(bypassed);
            w.fx_bypass.enable(true);
        }
        None => {
            w.fx_bypass.set_value(false);
            w.fx_bypass.enable(false);
        }
    });
}

fn edit_parameters(app: &Rc<App>, list: &ListBox) {
    let target = selected_target(app);
    let Some(slot) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    let Some(instance) = fx::instance_at(app, target, slot) else {
        if let Some(frame) = app.widgets(|w| w.frame.clone()) {
            show_info(
                &frame,
                "Edit parameters",
                "This plugin is not loaded, so it has no parameters to edit.",
            );
        }
        return;
    };
    super::fx_params::edit_parameters(app, target, slot, instance);
}

fn open_interface(app: &Rc<App>, list: &ListBox) {
    let target = selected_target(app);
    let Some(slot) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    let Some(instance) = fx::instance_at(app, target, slot) else {
        if let Some(frame) = app.widgets(|w| w.frame.clone()) {
            show_info(
                &frame,
                "Open interface",
                "This plugin is not loaded, so it has no interface to open.",
            );
        }
        return;
    };
    super::fx_editor::open_editor(app, target, slot, instance);
}

// --- Chain library, import/export, and the missing-plugin dialog (S4) ---

/// A human label for the selected chain target, used in dialog messages.
fn target_name(app: &Rc<App>, target: ChainTarget) -> String {
    match target {
        ChainTarget::Master => "Master output".to_string(),
        ChainTarget::Bus(i) => app
            .config
            .borrow()
            .buses
            .buses
            .get(i)
            .map(|b| b.name.clone())
            .unwrap_or_default(),
    }
}

fn save_chain_to_library(app: &Rc<App>) {
    let target = selected_target(app);
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    // Capture current plugin state before saving.
    fx::snapshot_chain(app, target);
    let slots = fx::slots_for(app, target);
    if slots.is_empty() {
        show_info(
            &frame,
            "Save chain",
            "This chain is empty; there is nothing to save.",
        );
        return;
    }
    let default_name = target_name(app, target);
    let dialog = TextEntryDialog::builder(&frame, "Name for this chain:", "Save chain")
        .with_default_value(&default_name)
        .build();
    if dialog.show_modal() == ID_OK {
        if let Some(name) = dialog.get_value() {
            let name = name.trim().to_string();
            if !name.is_empty() {
                let mut library = app.chain_library.borrow_mut();
                let unique = crate::fx::unique_chain_name(&library.chains, &name);
                library.chains.push(crate::fx::NamedChain {
                    name: unique.clone(),
                    slots,
                });
                crate::fx::save_library(&library);
                drop(library);
                show_info(&frame, "Save chain", &format!("Saved chain \"{unique}\"."));
            }
        }
    }
    dialog.destroy();
}

fn load_chain_from_library(app: &Rc<App>) {
    let target = selected_target(app);
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let names: Vec<String> = app
        .chain_library
        .borrow()
        .chains
        .iter()
        .map(|c| c.name.clone())
        .collect();
    if names.is_empty() {
        show_info(&frame, "Load chain", "There are no saved chains yet.");
        return;
    }
    let Some(chain_index) = pick_chain(app, &frame, &names) else {
        return;
    };
    let slots = app.chain_library.borrow().chains[chain_index].slots.clone();
    apply_chain(app, target, slots);
}

/// A small Load / Delete / Cancel picker over the saved chain names. Returns
/// the chosen chain index to load, or `None` if the user cancelled or only
/// deleted.
fn pick_chain(app: &Rc<App>, frame: &Frame, names: &[String]) -> Option<usize> {
    let dialog = Dialog::builder(frame, "Load chain")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(360, 300)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&panel)
        .with_label("Saved chains")
        .build();
    let list = ListBox::builder(&panel).build();
    super::set_accessible_name(&list, "Saved chains");
    super::help::tag(&list, "dialog.loadChain.list", "Saved FX chains list");
    for name in names {
        list.append(name);
    }
    if !names.is_empty() {
        list.set_selection(0, true);
    }
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let load = Button::builder(&panel).with_label("&Load").build();
    let delete = Button::builder(&panel).with_label("&Delete").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    super::help::tag(&load, "dialog.loadChain.load", "Load selected chain button");
    super::help::tag(
        &delete,
        "dialog.loadChain.delete",
        "Delete selected chain button",
    );
    for b in [&load, &delete, &cancel] {
        buttons.add(b, 0, SizerFlag::All, 4);
    }
    sizer.add(&label, 0, SizerFlag::All, 4);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // ID_OK carries "load the selection"; a delete re-runs the picker.
    {
        let dialog = dialog.clone();
        load.on_click(move |_| dialog.end_modal(ID_OK));
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
    {
        let app = app.clone();
        let list = list.clone();
        delete.on_click(move |_| {
            if let Some(index) = list.get_selection() {
                {
                    let mut library = app.chain_library.borrow_mut();
                    if (index as usize) < library.chains.len() {
                        library.chains.remove(index as usize);
                        crate::fx::save_library(&library);
                    }
                }
                list.clear();
                for chain in &app.chain_library.borrow().chains {
                    list.append(&chain.name);
                }
                if list.get_count() > 0 {
                    list.set_selection(0, true);
                }
            }
        });
    }

    let result = if dialog.show_modal() == ID_OK {
        list.get_selection().map(|i| i as usize)
    } else {
        None
    };
    dialog.destroy();
    result
}

fn import_chain_file(app: &Rc<App>) {
    let target = selected_target(app);
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let dialog = FileDialog::builder(&frame)
        .with_message("Import FX chain")
        .with_wildcard("Pubsplash FX chain (*.pubfx)|*.pubfx|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    let path = if dialog.show_modal() == ID_OK {
        dialog.get_path()
    } else {
        None
    };
    if let Some(path) = path {
        match crate::fx::import_chain(std::path::Path::new(&path)) {
            Ok(chain) => {
                let slots = chain.slots.clone();
                {
                    let mut library = app.chain_library.borrow_mut();
                    let unique = crate::fx::unique_chain_name(&library.chains, &chain.name);
                    library.chains.push(crate::fx::NamedChain {
                        name: unique,
                        slots: chain.slots,
                    });
                    crate::fx::save_library(&library);
                }
                let apply = MessageDialog::builder(
                    &frame,
                    "Chain imported and added to your library. Apply it to the selected bus now?",
                    "Import chain",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build();
                if apply.show_modal() == ID_YES {
                    apply_chain(app, target, slots);
                }
            }
            Err(e) => show_error(&frame, "Import chain", &e),
        }
    }
}

fn export_chain_file(app: &Rc<App>) {
    let target = selected_target(app);
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    fx::snapshot_chain(app, target);
    let slots = fx::slots_for(app, target);
    if slots.is_empty() {
        show_info(
            &frame,
            "Export chain",
            "This chain is empty; there is nothing to export.",
        );
        return;
    }
    let name = target_name(app, target);
    let dialog = FileDialog::builder(&frame)
        .with_message("Export FX chain")
        .with_default_file(&format!("{name}.pubfx"))
        .with_wildcard("Pubsplash FX chain (*.pubfx)|*.pubfx")
        .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
        .build();
    if dialog.show_modal() == ID_OK {
        if let Some(mut path) = dialog.get_path() {
            if !path.to_lowercase().ends_with(".pubfx") {
                path.push_str(".pubfx");
            }
            let chain = crate::fx::NamedChain { name, slots };
            match crate::fx::export_chain(&chain, std::path::Path::new(&path)) {
                Ok(()) => show_info(&frame, "Export chain", "Chain exported."),
                Err(e) => show_error(&frame, "Export chain", &e),
            }
        }
    }
}

/// Applies a set of slots to a target chain, first checking that every plugin
/// is installed. Missing plugins are reported; the user can apply with the
/// available subset or cancel.
fn apply_chain(app: &Rc<App>, target: ChainTarget, slots: Vec<crate::config::FxSlotConfig>) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let resolution = {
        let cache = app.plugins.borrow();
        crate::fx::resolve_chain(&slots, &cache)
    };
    if !resolution.missing.is_empty() {
        if !missing_plugin_dialog(&frame, &resolution) {
            return;
        }
    }
    // Apply only the plugins that resolve on this machine.
    fx::replace_chain(app, target, resolution.valid);
    refresh_bus_list(app);
    refresh_fx_list(app);
    super::home::rebuild_mixer(app);
}

/// Shows which plugins in a chain are missing. Returns true if the user chose
/// to apply the chain with the available subset (only offered when at least
/// one plugin resolves).
fn missing_plugin_dialog(frame: &Frame, resolution: &crate::fx::ChainResolution) -> bool {
    let dialog = Dialog::builder(frame, "Missing plugins")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(420, 320)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let intro = StaticText::builder(&panel)
        .with_label(&format!(
            "{} of this chain's plugins are not installed on this machine:",
            resolution.missing.len()
        ))
        .build();
    let list = ListBox::builder(&panel).build();
    super::set_accessible_name(&list, "Missing plugins");
    super::help::tag(&list, "dialog.missingPlugins.list", "Missing plugins list");
    for plugin in &resolution.missing {
        list.append(&plugin.display());
    }
    sizer.add(&intro, 0, SizerFlag::All, 8);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let has_valid = !resolution.valid.is_empty();
    if has_valid {
        let apply = Button::builder(&panel)
            .with_label(&format!(
                "&Apply with {} available plugin{}",
                resolution.valid.len(),
                if resolution.valid.len() == 1 { "" } else { "s" }
            ))
            .build();
        let cancel = Button::builder(&panel).with_label("&Cancel").build();
        super::help::tag(
            &apply,
            "dialog.missingPlugins.apply",
            "Apply chain with available plugins button",
        );
        buttons.add(&apply, 0, SizerFlag::All, 4);
        buttons.add(&cancel, 0, SizerFlag::All, 4);
        {
            let dialog = dialog.clone();
            apply.on_click(move |_| dialog.end_modal(ID_OK));
        }
        {
            let dialog = dialog.clone();
            cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
        }
    } else {
        let note = StaticText::builder(&panel)
            .with_label("None of the chain's plugins are available, so it cannot be applied.")
            .build();
        sizer.add(&note, 0, SizerFlag::All, 8);
        let ok = Button::builder(&panel).with_label("&OK").build();
        buttons.add(&ok, 0, SizerFlag::All, 4);
        let dialog = dialog.clone();
        ok.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    let apply = dialog.show_modal() == ID_OK;
    dialog.destroy();
    apply
}
