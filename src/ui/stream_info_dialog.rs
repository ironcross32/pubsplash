//! Set stream info dialog: title, description, and whether the stream is
//! archived on the server. Values live for this session only.

use super::App;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Encoder bitrates offered by the Quality dropdown, in kbps. These are the
/// discrete steps `audio::encoder::bitrate_from_kbps` maps to. The Audio Pub
/// server imposes no bitrate limit (it only checks the codec), so the full
/// range is valid.
const QUALITY_KBPS: [u32; 8] = [48, 64, 96, 128, 160, 192, 256, 320];

/// Shows the dialog pre-filled with the current session's stream info.
/// Returns true if the user confirmed with OK (info is then stored in
/// `Runtime::stream_info` and marked as set).
pub fn show(app: &Rc<App>, parent: &Frame) -> bool {
    let dialog = Dialog::builder(parent, "Set stream info")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(480, 400)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let current = app.run.borrow().stream_info.clone();

    let title_label = StaticText::builder(&panel).with_label("Title").build();
    let title_input = TextCtrl::builder(&panel).with_value(&current.title).build();
    super::set_accessible_name(&title_input, "Title");
    super::help::tag(
        &title_input,
        "dialog.streamInfo.title",
        "Stream title input",
    );

    let description_label = StaticText::builder(&panel)
        .with_label("Description")
        .build();
    let description_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine)
        .with_value(&current.description)
        .build();
    super::set_accessible_name(&description_input, "Description");
    super::help::tag(
        &description_input,
        "dialog.streamInfo.description",
        "Stream description input",
    );

    let quality_label = StaticText::builder(&panel).with_label("Quality").build();
    let quality_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&quality_choice, "Quality");
    super::help::tag(
        &quality_choice,
        "dialog.streamInfo.quality",
        "Stream audio quality (bitrate) choice",
    );
    for kbps in QUALITY_KBPS {
        quality_choice.append(&format!("{kbps} kbps"));
    }
    // Persisted in config (unlike the session-only title/description above).
    let current_bitrate = app.config.borrow().audio.bitrate_kbps;
    let quality_index = QUALITY_KBPS
        .iter()
        .position(|&kbps| kbps == current_bitrate)
        .unwrap_or_else(|| QUALITY_KBPS.iter().position(|&kbps| kbps == 128).unwrap());
    quality_choice.set_selection(quality_index as u32);

    let archive_check = CheckBox::builder(&panel)
        .with_label("Archive the stream")
        .build();
    super::set_accessible_name(&archive_check, "Archive the stream");
    super::help::tag(
        &archive_check,
        "dialog.streamInfo.archive",
        "Archive the stream checkbox",
    );
    // Before the user confirms the dialog this session, the "Archive streams by
    // default" preference supplies the initial state; afterwards their explicit
    // choice (in `current.archive`) wins.
    let default_archive = current.archive
        || (!app.run.borrow().stream_info_set
            && app.config.borrow().archiving.archive_streams_by_default);
    archive_check.set_value(default_archive);

    let record_check = CheckBox::builder(&panel)
        .with_label("Record this stream")
        .build();
    super::set_accessible_name(&record_check, "Record this stream");
    super::help::tag(
        &record_check,
        "dialog.streamInfo.record",
        "Record this stream locally checkbox",
    );
    // Same rule as archiving: the "Record streams by default" preference seeds
    // the box until the user confirms the dialog this session.
    let default_record = current.record
        || (!app.run.borrow().stream_info_set
            && app.config.borrow().archiving.record_streams_by_default);
    record_check.set_value(default_record);

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_button = super::ok_button(&panel, "OK");
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel_button = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&ok_button, 0, SizerFlag::All, 4);
    buttons.add(&cancel_button, 0, SizerFlag::All, 4);

    sizer.add(&title_label, 0, SizerFlag::All, 4);
    sizer.add(&title_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&description_label, 0, SizerFlag::All, 4);
    sizer.add(&description_input, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&quality_label, 0, SizerFlag::All, 4);
    sizer.add(&quality_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&archive_check, 0, SizerFlag::All, 8);
    sizer.add(&record_check, 0, SizerFlag::All, 8);
    sizer.add_sizer(&buttons, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // Tabbing into a text field selects its contents so the pre-filled
    // defaults can be overwritten by just typing.
    for input in [&title_input, &description_input] {
        let input_for_focus = input.clone();
        input.on_set_focus(move |event| {
            input_for_focus.select_all();
            event.skip(true);
        });
    }

    {
        let app = app.clone();
        let dialog_for_ok = dialog.clone();
        let title_input = title_input.clone();
        let description_input = description_input.clone();
        let archive_check = archive_check.clone();
        let record_check = record_check.clone();
        let quality_choice = quality_choice.clone();
        ok_button.on_click(move |_| {
            let mut title = title_input.get_value().trim().to_string();
            if title.is_empty() {
                // The server requires a title; fall back to the default.
                title = "Stream".to_string();
            }
            let mut run = app.run.borrow_mut();
            run.stream_info.title = title;
            run.stream_info.description = description_input.get_value().trim().to_string();
            run.stream_info.archive = archive_check.get_value();
            run.stream_info.record = record_check.get_value();
            run.stream_info_set = true;
            drop(run);

            // Quality persists in config, unlike the session-only fields above.
            let kbps = quality_choice
                .get_selection()
                .map(|i| QUALITY_KBPS[i as usize])
                .unwrap_or(128);
            app.config.borrow_mut().audio.bitrate_kbps = kbps;
            app.save_config();

            dialog_for_ok.end_modal(ID_OK);
        });
    }
    {
        let dialog_for_cancel = dialog.clone();
        cancel_button.on_click(move |_| {
            dialog_for_cancel.end_modal(ID_CANCEL);
        });
    }

    let confirmed = dialog.show_modal() == ID_OK;
    dialog.destroy();
    confirmed
}
