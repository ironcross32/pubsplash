//! Preferences dialog. Tabbed; currently only the "VST plugins" tab, which
//! manages the plugin folder list and starts scans. Scan progress arrives on
//! the pump (see `pump_scan_events` in `ui/mod.rs`).

use super::{App, ScanUi, WXK_DELETE, show_error};
use crate::vst::scan::{self, ScanMode};
use std::rc::Rc;
use wxdragon::prelude::*;

pub fn show(app: &Rc<App>, frame: &Frame) {
    let dialog = Dialog::builder(frame, "Preferences")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(560, 480)
        .build();

    let notebook = Notebook::builder(&dialog).build();
    let archiving_panel = Panel::builder(&notebook).build();
    notebook.add_page(&archiving_panel, "Archiving", true, None);
    build_archiving_tab(app, &dialog, &archiving_panel);
    let vst_panel = Panel::builder(&notebook).build();
    notebook.add_page(&vst_panel, "VST plugins", false, None);
    build_vst_tab(app, &dialog, &vst_panel);

    let close_button = Button::builder(&dialog).with_label("C&lose").build();
    {
        let dialog = dialog.clone();
        close_button.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);
    dialog_sizer.add(&close_button, 0, SizerFlag::All, 8);
    dialog.set_sizer(dialog_sizer, true);

    dialog.show_modal();
    dialog.destroy();
}

fn build_archiving_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Stream Archiving group.
    let stream_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Stream Archiving")
            .build();
    let archive_default = CheckBox::builder(panel)
        .with_label("Archive streams by default")
        .build();
    super::set_accessible_name(&archive_default, "Archive streams by default");
    super::help::tag(&archive_default, "dialog.preferences.archive.archiveDefault", "Archive streams by default checkbox");
    archive_default.set_value(app.config.borrow().archiving.archive_streams_by_default);
    stream_group.add(&archive_default, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let archive_default = archive_default.clone();
        archive_default.clone().on_toggled(move |_| {
            app.config.borrow_mut().archiving.archive_streams_by_default =
                archive_default.get_value();
            app.save_config();
        });
    }

    // Recording group.
    let recording_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Recording").build();

    let record_default = CheckBox::builder(panel)
        .with_label("Record streams by default")
        .build();
    super::set_accessible_name(&record_default, "Record streams by default");
    super::help::tag(&record_default, "dialog.preferences.archive.recordDefault", "Record streams by default checkbox");
    record_default.set_value(app.config.borrow().archiving.record_streams_by_default);
    recording_group.add(&record_default, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let record_default = record_default.clone();
        record_default.clone().on_toggled(move |_| {
            app.config.borrow_mut().archiving.record_streams_by_default =
                record_default.get_value();
            app.save_config();
        });
    }

    let folder_label = StaticText::builder(panel)
        .with_label("Recording folder")
        .build();
    let folder_input = TextCtrl::builder(panel)
        .with_value(&app.config.borrow().archiving.recording_folder)
        .build();
    super::set_accessible_name(&folder_input, "Recording folder");
    super::help::tag(&folder_input, "dialog.preferences.archive.recordingFolder", "Recording folder path input");
    recording_group.add(&folder_label, 0, SizerFlag::All, 4);
    recording_group.add(&folder_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    {
        let app = app.clone();
        let folder_input = folder_input.clone();
        folder_input.clone().on_text_updated(move |_| {
            app.config.borrow_mut().archiving.recording_folder =
                folder_input.get_value().trim().to_string();
            app.save_config();
        });
    }

    let browse = Button::builder(panel).with_label("&Browse...").build();
    super::help::tag(&browse, "dialog.preferences.archive.browse", "Browse for recording folder button");
    recording_group.add(&browse, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let dialog = dialog.clone();
        let folder_input = folder_input.clone();
        browse.on_click(move |_| {
            let start = app.config.borrow().archiving.recording_dir();
            let picker = DirDialog::builder(
                &dialog,
                "Choose a folder for stream recordings",
                &start.to_string_lossy(),
            )
            .with_style(DirDialogStyle::MustExist.bits())
            .build();
            if picker.show_modal() != ID_OK {
                return;
            }
            let Some(path) = picker.get_path() else {
                return;
            };
            let path = path.trim().to_string();
            if path.is_empty() {
                return;
            }
            folder_input.set_value(&path);
            app.config.borrow_mut().archiving.recording_folder = path;
            app.save_config();
        });
    }

    sizer.add_sizer(&stream_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&recording_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
}

fn build_vst_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let folders_label = StaticText::builder(panel)
        .with_label("Plugin folders")
        .build();
    let folders_list = ListBox::builder(panel).build();
    super::set_accessible_name(&folders_list, "Plugin folders");
    super::help::tag(&folders_list, "dialog.preferences.vst.folderList", "VST plugin folders list");

    let folder_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add_folder = Button::builder(panel).with_label("&Add folder...").build();
    let remove_folder = Button::builder(panel).with_label("Re&move folder").build();
    super::help::tag(&add_folder, "dialog.preferences.vst.addFolder", "Add plugin folder button");
    super::help::tag(&remove_folder, "dialog.preferences.vst.removeFolder", "Remove plugin folder button");
    folder_buttons.add(&add_folder, 0, SizerFlag::All, 4);
    folder_buttons.add(&remove_folder, 0, SizerFlag::All, 4);

    let scan_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let scan_new = Button::builder(panel)
        .with_label("Scan for &new plugins")
        .build();
    let rescan_all = Button::builder(panel)
        .with_label("&Rescan all plugins")
        .build();
    super::help::tag(&scan_new, "dialog.preferences.vst.scanNew", "Scan for new plugins button");
    super::help::tag(&rescan_all, "dialog.preferences.vst.rescanAll", "Rescan all plugins button");
    scan_buttons.add(&scan_new, 0, SizerFlag::All, 4);
    scan_buttons.add(&rescan_all, 0, SizerFlag::All, 4);

    sizer.add(&folders_label, 0, SizerFlag::All, 4);
    sizer.add(&folders_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&folder_buttons, 0, SizerFlag::Expand, 0);
    sizer.add_sizer(&scan_buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    let refresh_folders = {
        let app = app.clone();
        let folders_list = folders_list.clone();
        move |select: Option<&str>| {
            let config = app.config.borrow();
            folders_list.clear();
            let mut select_index = 0u32;
            for (i, folder) in config.plugins.folders.iter().enumerate() {
                folders_list.append(folder);
                if Some(folder.as_str()) == select {
                    select_index = i as u32;
                }
            }
            if folders_list.get_count() > 0 {
                folders_list.set_selection(select_index, true);
            }
        }
    };
    refresh_folders(None);

    {
        let app = app.clone();
        let dialog = dialog.clone();
        let refresh_folders = refresh_folders.clone();
        add_folder.on_click(move |_| {
            let picker = DirDialog::builder(
                &dialog,
                "Choose a folder containing VST plugins",
                "",
            )
            .with_style(DirDialogStyle::MustExist.bits())
            .build();
            if picker.show_modal() != ID_OK {
                return;
            }
            let Some(path) = picker.get_path() else {
                return;
            };
            let path = path.trim().to_string();
            if path.is_empty() {
                return;
            }
            {
                let mut config = app.config.borrow_mut();
                if config
                    .plugins
                    .folders
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(&path))
                {
                    drop(config);
                    show_error(&dialog, "Add folder", "That folder is already in the list.");
                    return;
                }
                config.plugins.folders.push(path.clone());
            }
            app.save_config();
            refresh_folders(Some(&path));
        });
    }

    let do_remove = {
        let app = app.clone();
        let folders_list = folders_list.clone();
        let refresh_folders = refresh_folders.clone();
        move || {
            let Some(index) = folders_list.get_selection().map(|i| i as usize) else {
                return;
            };
            {
                let mut config = app.config.borrow_mut();
                if index >= config.plugins.folders.len() {
                    return;
                }
                config.plugins.folders.remove(index);
            }
            app.save_config();
            refresh_folders(None);
        }
    };
    {
        let do_remove = do_remove.clone();
        remove_folder.on_click(move |_| do_remove());
    }
    folders_list.clone().on_key_down(move |event| match super::key_of(&event) {
        Some((WXK_DELETE, _)) => do_remove(),
        _ => event.skip(true),
    });

    {
        let app = app.clone();
        let dialog = dialog.clone();
        scan_new.on_click(move |_| begin_scan(&app, &dialog, ScanMode::NewOnly));
    }
    {
        let app = app.clone();
        let dialog = dialog.clone();
        rescan_all.on_click(move |_| begin_scan(&app, &dialog, ScanMode::RescanAll));
    }
}

fn begin_scan(app: &Rc<App>, dialog: &Dialog, mode: ScanMode) {
    if app.scan.borrow().is_some() {
        return;
    }
    let folders = app.config.borrow().plugins.folders.clone();
    if folders.is_empty() {
        show_error(
            dialog,
            "Scan plugins",
            "Add at least one plugin folder first.",
        );
        return;
    }
    let existing = app.plugins.borrow().clone();
    match scan::start_scan(folders, mode, existing) {
        Ok(handle) => {
            // Enumeration alone can take a while on big folders; show an
            // indeterminate progress dialog right away. It is replaced with
            // a real one once the worker reports how many plugins it found.
            let progress = ProgressDialog::builder(
                dialog,
                "Scanning VST plugins",
                "Looking for plugins in the configured folders...",
                100,
            )
            .can_abort()
            .build();
            progress.pulse(None);
            *app.scan.borrow_mut() = Some(ScanUi {
                handle,
                progress: Some(progress),
                determinate: false,
                parent: dialog.clone(),
            });
        }
        Err(message) => show_error(dialog, "Scan plugins", &message),
    }
}
