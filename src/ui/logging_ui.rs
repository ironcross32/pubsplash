//! The "Logging & debugging" tab in Preferences.
//!
//! Three things: the log level, a way to reach the log files, and a way to
//! package them up. The last is the point of the tab — a bug report is only as
//! good as the log attached to it, and asking a screen-reader user to find a
//! folder under `%LOCALAPPDATA%`, select the right six files and zip them by
//! hand is asking for a report with the wrong files in it.
//!
//! The capture runs inline on the UI thread. Rotation caps the logs at 5 MiB
//! each and five files, so the worst case is around 30 MB of deflate — about a
//! second, during which the audio engine, the Icecast sender and the chat feed
//! are all untouched, because none of them is this thread.

use std::rc::Rc;

use wxdragon::prelude::*;

use super::{App, show_error, show_info};
use crate::logging;

pub fn build_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // --- Log level -------------------------------------------------------

    let (level_group, level_box) = super::group_box(panel, "Log level");

    let level_label = StaticText::builder(&level_box)
        .with_label("How much detail to record")
        .build();
    let level_choice = Choice::builder(&level_box).build();
    super::set_accessible_name(&level_choice, "How much detail to record");
    super::help::tag(
        &level_choice,
        "dialog.preferences.logging.level",
        "Log level choice",
    );
    for level in logging::LEVELS {
        level_choice.append(&logging::level_label(level));
    }
    level_choice.set_selection(logging::level_index(&app.config.borrow().logging.level) as u32);
    level_group.add(&level_label, 0, SizerFlag::All, 4);
    level_group.add(&level_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);

    // A disabled control with nothing to explain it reads as broken, so the
    // reason is spelled out rather than left for the user to deduce.
    if logging::env_pinned() {
        level_choice.enable(false);
        let pinned = StaticText::builder(&level_box)
            .with_label(
                "The level is fixed for this run by a PUBSPLASH_LOG_* environment \
                 variable, which overrides this setting.",
            )
            .build();
        level_group.add(&pinned, 0, SizerFlag::All, 4);
    }

    {
        // One `set_new_spec` per arrow key, which is cheap — no settle timer
        // here, unlike the sound-pack picker whose apply loads a whole pack.
        let app = app.clone();
        level_choice.on_selection_changed(move |_| {
            let Some(index) = level_choice.get_selection() else {
                return;
            };
            let Some(level) = logging::LEVELS.get(index as usize) else {
                return;
            };
            app.config.borrow_mut().logging.level = (*level).to_string();
            app.save_config();
            logging::set_level(level);
        });
    }

    sizer.add_sizer(&level_group, 0, SizerFlag::Expand | SizerFlag::All, 4);

    // --- Diagnostics -----------------------------------------------------

    let (diag_group, diag_box) = super::group_box(panel, "Diagnostics");

    let where_text = StaticText::builder(&diag_box)
        .with_label(&format!("Logs are kept in {}", logging::logs_dir().display()))
        .build();
    diag_group.add(&where_text, 0, SizerFlag::All, 4);

    // ALT+O and ALT+P: mnemonics are dialog-wide (`::IsDialogMessage` searches
    // the whole Preferences dialog, not the current page), and every other
    // letter these two buttons might want is claimed by another tab or by
    // Close — a, b, d, e, f, g, i, k, l, m, n, r, s, u, v.
    let open_folder = Button::builder(&diag_box)
        .with_label("&Open logs folder")
        .build();
    super::set_accessible_name(&open_folder, "Open logs folder");
    super::help::tag(
        &open_folder,
        "dialog.preferences.logging.openFolder",
        "Open logs folder button",
    );
    diag_group.add(&open_folder, 0, SizerFlag::All, 4);
    {
        let dialog = *dialog;
        open_folder.on_click(move |_| {
            let dir = logging::logs_dir();
            // Created first: `ShellExecuteW` on a path that does not exist only
            // reports a number, and on a run that has logged nothing yet the
            // folder may genuinely not be there. Same reason as `open_data_dir`.
            if let Err(e) = std::fs::create_dir_all(&dir) {
                show_error(
                    &dialog,
                    "Open logs folder",
                    &format!("Could not create {}: {e}", dir.display()),
                );
                return;
            }
            if let Err(e) = super::shell_open(&dir.to_string_lossy()) {
                show_error(
                    &dialog,
                    "Open logs folder",
                    &format!("Could not open {}: {e}", dir.display()),
                );
            }
        });
    }

    let compress = Button::builder(&diag_box)
        .with_label("Com&press logs...")
        .build();
    super::set_accessible_name(&compress, "Compress logs");
    super::help::tag(
        &compress,
        "dialog.preferences.logging.compress",
        "Compress logs button",
    );
    diag_group.add(&compress, 0, SizerFlag::All, 4);
    {
        let dialog = *dialog;
        compress.on_click(move |_| compress_logs(&dialog));
    }

    sizer.add_sizer(&diag_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
}

/// Asks where to put the archive, then writes it.
///
/// The result is a modal, which is right here by the test in CLAUDE.md: it
/// answers a button the user just pressed and is standing there waiting on,
/// rather than arriving on its own mid-broadcast.
fn compress_logs(parent: &Dialog) {
    let picker = FileDialog::builder(parent)
        .with_message("Save the collected logs")
        .with_default_dir(&default_dir().to_string_lossy())
        .with_default_file(&archive_filename())
        .with_wildcard("ZIP archive (*.zip)|*.zip")
        .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
        .build();
    if picker.show_modal() != ID_OK {
        return;
    }
    let Some(mut path) = picker.get_path() else {
        return;
    };
    // The user is free to delete the extension in the name box; the file is a
    // ZIP either way, so it may as well say so.
    if !path.to_lowercase().ends_with(".zip") {
        path.push_str(".zip");
    }

    match logging::capture(std::path::Path::new(&path)) {
        Ok(count) => show_info(
            parent,
            "Compress logs",
            &format!(
                "Collected {count} file{} into {path}.\n\nThe archive holds the log files and \
                 any crash reports. It contains no passwords or API keys.",
                if count == 1 { "" } else { "s" }
            ),
        ),
        Err(e) => show_error(parent, "Compress logs", &e),
    }
}

/// Where the save dialog opens: the desktop, falling back to the user's
/// documents and then to the data directory. Somewhere the user can find again
/// when they go to attach the file, which the logs folder is not.
fn default_dir() -> std::path::PathBuf {
    dirs::desktop_dir()
        .or_else(dirs::document_dir)
        .unwrap_or_else(crate::config::config_dir)
}

/// `pubsplash-logs_<yyyy-mm-dd>_<HH-MM-SS>.zip`, from local time — the same
/// shape as `super::recording_filename`, so two captures never collide and they
/// sort in the order they were taken.
fn archive_filename() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "pubsplash-logs_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.zip",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_name_is_a_zip_stamped_with_the_time() {
        let name = archive_filename();
        assert!(name.starts_with("pubsplash-logs_"), "{name}");
        assert!(name.ends_with(".zip"), "{name}");
        // `pubsplash-logs_` + `yyyy-mm-dd` + `_` + `HH-MM-SS` + `.zip`
        assert_eq!(name.len(), "pubsplash-logs_0000-00-00_00-00-00.zip".len());
    }
}
