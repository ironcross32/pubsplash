//! The progress dialog shown while plugins are being scanned.
//!
//! This is deliberately a plain `Dialog` and not a `wxProgressDialog`. On MSW
//! the latter is a native task dialog living on its own thread, and everything
//! about that fought this app:
//!
//! - Its constructor spins `while (wxEventLoop::GetActive()->Dispatch())` until
//!   that thread reports a window. With `wxPD_CAN_SKIP` the window never
//!   arrives at all on this wx build, so the constructor never returns — no
//!   dialog, and a scan that looks frozen while the worker runs on invisibly.
//!   Verified in isolation: `can_abort` and `smooth` build, `can_skip` hangs.
//! - `Update`/`Pulse` yield for every event category but user input, so every
//!   call re-entered the pump timers from inside a `RefCell` borrow.
//! - Its range is fixed at construction, and replacing it to change the range
//!   deadlocks: dropping one only appends to `wxPendingDelete`, processed at
//!   idle, and the replacement's spin loop never idles.
//!
//! This dialog is **modeless** and does nothing on its own: the pump writes to
//! it, and Cancel and Skip set the worker's atomics from their click handlers.
//! No nested event loop is involved at any point, which is what makes the whole
//! class of problem above go away. Nothing here may grow a modal dialog or a
//! `Yield` for the same reason.

use super::ID_CANCEL;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wxdragon::prelude::*;

/// Progress as a percentage, for the bar and the spoken summary.
///
/// `total == 0` reads as complete: there is nothing to scan, so the bar must
/// not sit at zero waiting for progress that will never come.
pub fn scan_percent(done: usize, total: usize) -> i32 {
    if total == 0 {
        return 100;
    }
    (done.min(total) * 100 / total) as i32
}

/// The live scan dialog. Dropping it destroys the window, so the scan ending —
/// or the Preferences dialog closing out from under it — cleans up by dropping
/// [`super::ScanUi`].
pub struct ScanDialog {
    dialog: Dialog,
    gauge: Gauge,
    status: TextCtrl,
}

impl Drop for ScanDialog {
    fn drop(&mut self) {
        self.dialog.destroy();
    }
}

impl ScanDialog {
    /// Builds and shows the dialog, wiring Cancel and Skip to the worker's
    /// flags.
    ///
    /// The buttons hold the `Arc`s themselves rather than an `App`, so a click
    /// needs no borrow of anything and cannot be tripped up by whatever the
    /// pump is in the middle of.
    pub fn show(parent: &Dialog, cancel: Arc<AtomicBool>, skip: Arc<AtomicBool>) -> Self {
        let dialog = Dialog::builder(parent, "Scanning plugins")
            .with_style(DialogStyle::DefaultDialogStyle)
            .with_size(480, 220)
            .build();
        let panel = Panel::builder(&dialog).build();
        let sizer = BoxSizer::builder(Orientation::Vertical).build();

        // A native progress bar reports its own percentage to a screen reader,
        // so all it needs from us is a name. Its range is fixed at 100 and fed
        // a percentage, so the plugin count arriving later never has to re-range
        // it.
        let gauge_label = StaticText::builder(&panel).with_label("Progress").build();
        let gauge = Gauge::builder(&panel).with_range(100).build();
        super::set_accessible_name(&gauge, "Scan progress");
        super::help::tag(&gauge, "dialog.scan.progress", "Scan progress bar");

        // Read-only, so it is somewhere to tab to and read the current plugin
        // on demand. Rewriting it announces nothing by itself, which is what
        // keeps a scan of hundreds of plugins from talking continuously.
        let status_label = StaticText::builder(&panel).with_label("Status").build();
        let status = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ReadOnly)
            .with_value("Looking for plugins in the configured folders...")
            .build();
        super::set_accessible_name(&status, "Scan status");
        super::help::tag(&status, "dialog.scan.status", "Scan status text");

        // Skip is the default item, so ENTER skips the plugin the scan is stuck
        // on — the thing you want to be able to do quickly. ESCAPE reaches
        // Cancel through its `ID_CANCEL`, as everywhere else in the app.
        let skip_button = super::ok_button(&panel, "&Skip this plugin");
        super::help::tag(
            &skip_button,
            "dialog.scan.skip",
            "Skip the plugin being scanned button",
        );
        let cancel_button = Button::builder(&panel)
            .with_id(ID_CANCEL)
            .with_label("&Cancel scan")
            .build();
        super::help::tag(
            &cancel_button,
            "dialog.scan.cancel",
            "Cancel the scan button",
        );

        let buttons = BoxSizer::builder(Orientation::Horizontal).build();
        buttons.add(&skip_button, 0, SizerFlag::All, 4);
        buttons.add(&cancel_button, 0, SizerFlag::All, 4);

        sizer.add(&gauge_label, 0, SizerFlag::All, 4);
        sizer.add(&gauge, 0, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add(&status_label, 0, SizerFlag::All, 4);
        sizer.add(&status, 0, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add_sizer(&buttons, 0, SizerFlag::AlignCenterHorizontal, 0);
        panel.set_sizer(sizer, true);

        {
            let skip = skip.clone();
            let status = status.clone();
            skip_button.on_click(move |event| {
                skip.store(true, Ordering::Relaxed);
                status.set_value("Skipping this plugin...");
                // The scan reports back on its own; do not let this reach
                // wxDialogBase and close the dialog behind us.
                event.event.skip(false);
            });
        }
        {
            let cancel = cancel.clone();
            let status = status.clone();
            cancel_button.on_click(move |event| {
                cancel.store(true, Ordering::Relaxed);
                // A helper that is part-way through a plugin has to finish or
                // be killed first, so say that rather than vanishing.
                status.set_value("Cancelling the scan...");
                event.event.skip(false);
            });
        }

        dialog.show(true);
        // Focus starts on Cancel rather than on the default Skip: nothing is
        // being scanned yet, so there is nothing to skip, and "Cancel scan"
        // read aloud says plainly what the dialog is for.
        cancel_button.set_focus();
        Self {
            dialog,
            gauge,
            status,
        }
    }

    /// Enumeration is over and the plugin count is known.
    pub fn counted(&self, total: usize) {
        self.status
            .set_value(&format!("Found {total} plugins to scan."));
    }

    /// One plugin done.
    pub fn scanned(&self, done: usize, total: usize, current: &str) {
        self.gauge.set_value(scan_percent(done, total));
        self.status
            .set_value(&format!("Scanned {done} of {total}: {current}"));
    }
}

#[cfg(test)]
mod tests {
    use super::scan_percent;

    #[test]
    fn empty_scan_is_complete() {
        // Nothing to scan: the bar must not sit at 0 waiting for a Progress
        // event that will never come.
        assert_eq!(scan_percent(0, 0), 100);
    }

    #[test]
    fn the_last_plugin_reaches_the_maximum() {
        assert_eq!(scan_percent(161, 161), 100);
        assert_eq!(scan_percent(3, 3), 100);
    }

    #[test]
    fn a_short_scan_still_advances() {
        assert_eq!(scan_percent(0, 3), 0);
        assert_eq!(scan_percent(1, 3), 33);
        assert_eq!(scan_percent(2, 3), 66);
    }

    #[test]
    fn overshooting_is_clamped() {
        assert_eq!(scan_percent(5, 3), 100);
    }
}
