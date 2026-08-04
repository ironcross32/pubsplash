//! The progress dialog shown while an update downloads.
//!
//! Built on exactly the same terms as [`super::scan_dialog`], and for the same
//! reasons — read that file's header for the full account of why
//! `wxProgressDialog` is not an option on MSW. The short version: it is a native
//! task dialog on its own thread whose `Update` yields into every pump timer,
//! and whose constructor can spin forever without ever showing a window.
//!
//! So this is a plain **modeless** `Dialog` that does nothing on its own. The
//! pump writes to it; Cancel sets the worker's atomic from its own click
//! handler and needs no borrow of anything. No nested event loop exists
//! anywhere in it, and nothing here may grow one.
//!
//! The status line is a read-only `TextCtrl` rather than a `StaticText` so it is
//! somewhere to tab to and read on demand. Rewriting it announces nothing by
//! itself, which is what keeps a ten-megabyte download from reading a new number
//! aloud every second — the same rule the scan dialog follows, and the reason
//! `http::PROGRESS_INTERVAL` throttles the worker to one report a second in the
//! first place.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wxdragon::prelude::*;

/// Download progress as a percentage.
///
/// `total == 0` reads as complete rather than zero: a release that did not state
/// a size gives us nothing to count towards, and a bar frozen at 0% for the
/// whole download reads as a hang.
pub fn download_percent(done: u64, total: u64) -> i32 {
    if total == 0 {
        return 100;
    }
    (done.min(total) * 100 / total) as i32
}

/// Megabytes, to one decimal place, for the status line.
///
/// Bytes would be read out digit by digit by a screen reader; "4.2 MB of 9.7 MB"
/// is a sentence.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// The status line for a download in progress.
///
/// Split out from the widget so the wording can be tested without a window.
pub fn progress_line(done: u64, total: u64) -> String {
    if total == 0 {
        return format!("Downloaded {}.", megabytes(done));
    }
    format!(
        "Downloaded {} of {} ({}%).",
        megabytes(done),
        megabytes(total),
        download_percent(done, total)
    )
}

/// The live download dialog. Dropping it destroys the window, so the update
/// finishing — or failing, or being cancelled — cleans up by dropping it.
pub struct UpdateDialog {
    dialog: Dialog,
    gauge: Gauge,
    status: TextCtrl,
}

impl Drop for UpdateDialog {
    fn drop(&mut self) {
        self.dialog.destroy();
    }
}

impl UpdateDialog {
    /// Builds and shows the dialog, wiring Cancel to the worker's flag.
    ///
    /// The button holds the `Arc` itself rather than an `App`, so a click needs
    /// no borrow and cannot be tripped up by whatever the pump is in the middle
    /// of.
    pub fn show(parent: &Frame, version: &str, cancel: Arc<AtomicBool>) -> Self {
        let dialog = Dialog::builder(parent, &format!("Downloading Pubsplash {version}"))
            .with_style(DialogStyle::DefaultDialogStyle)
            .with_size(460, 200)
            .build();
        let panel = Panel::builder(&dialog).build();
        let sizer = BoxSizer::builder(Orientation::Vertical).build();

        // A native progress bar reports its own percentage to a screen reader,
        // so all it needs from us is a name. Fed a percentage against a fixed
        // range of 100, so the file size arriving later never re-ranges it.
        let gauge_label = StaticText::builder(&panel).with_label("Progress").build();
        let gauge = Gauge::builder(&panel).with_range(100).build();
        super::set_accessible_name(&gauge, "Download progress");
        super::help::tag(&gauge, "dialog.update.progress", "Update download progress bar");

        let status_label = StaticText::builder(&panel).with_label("Status").build();
        let status = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ReadOnly)
            .with_value("Connecting to GitHub...")
            .build();
        super::set_accessible_name(&status, "Download status");
        super::help::tag(&status, "dialog.update.status", "Update download status text");

        // Cancel is the only button, so it takes both ENTER and ESCAPE through
        // `dismiss_button`. There is nothing to confirm here — the user already
        // said yes — and the one thing they might want is out.
        let cancel_button = super::dismiss_button(&panel, "&Cancel download");
        super::help::tag(
            &cancel_button,
            "dialog.update.cancel",
            "Cancel the update download button",
        );

        sizer.add(&gauge_label, 0, SizerFlag::All, 4);
        sizer.add(&gauge, 0, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add(&status_label, 0, SizerFlag::All, 4);
        sizer.add(&status, 0, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add(&cancel_button, 0, SizerFlag::AlignCenterHorizontal | SizerFlag::All, 4);
        panel.set_sizer(sizer, true);

        {
            let status = status.clone();
            cancel_button.on_click(move |event| {
                cancel.store(true, Ordering::Relaxed);
                // The worker notices between chunks and reports back, which is
                // what closes this. Say so rather than appearing to do nothing.
                status.set_value("Cancelling...");
                // Do not let this reach `wxDialogBase`'s `ID_CANCEL` handler and
                // close the dialog behind the worker still running.
                event.event.skip(false);
            });
        }

        dialog.show(true);
        cancel_button.set_focus();
        Self {
            dialog,
            gauge,
            status,
        }
    }

    /// One progress report from the worker.
    pub fn progressed(&self, done: u64, total: u64) {
        self.gauge.set_value(download_percent(done, total));
        self.status.set_value(&progress_line(done, total));
    }

    /// The download is verified and is being unpacked.
    ///
    /// Portable updates only. Extracting a ten-megabyte archive takes long
    /// enough that a bar sitting at 100% with a stale byte count would read as a
    /// hang, so the status says what is actually happening.
    pub fn unpacking(&self) {
        self.gauge.set_value(100);
        self.status.set_value("Download complete. Unpacking the update...");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsized_download_reads_as_complete() {
        // Nothing to count towards: a bar frozen at 0% reads as a hang.
        assert_eq!(download_percent(0, 0), 100);
        assert_eq!(download_percent(500, 0), 100);
    }

    #[test]
    fn a_finished_download_reaches_the_maximum() {
        assert_eq!(download_percent(1000, 1000), 100);
        // More than stated should still clamp rather than overshoot the gauge.
        assert_eq!(download_percent(1200, 1000), 100);
    }

    #[test]
    fn progress_advances() {
        assert_eq!(download_percent(0, 1000), 0);
        assert_eq!(download_percent(250, 1000), 25);
        assert_eq!(download_percent(999, 1000), 99);
    }

    #[test]
    fn the_status_line_reads_as_a_sentence() {
        assert_eq!(
            progress_line(5 * 1024 * 1024, 10 * 1024 * 1024),
            "Downloaded 5.0 MB of 10.0 MB (50%)."
        );
    }

    #[test]
    fn an_unsized_download_omits_the_total() {
        assert_eq!(progress_line(1024 * 1024, 0), "Downloaded 1.0 MB.");
    }

    /// These are read aloud, so they follow the same hygiene the log lines do.
    #[test]
    fn status_lines_are_single_spaced_and_single_line() {
        for line in [progress_line(1, 100), progress_line(100, 100), progress_line(7, 0)] {
            assert!(!line.contains("  "), "double space in {line:?}");
            assert!(!line.contains('\n'), "newline in {line:?}");
        }
    }
}
