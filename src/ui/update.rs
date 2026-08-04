//! The UI half of automatic updates: asking, showing progress, and handing over
//! to the helper.
//!
//! The worker side is `src/update/`; this drains what it sends, from the pump.
//!
//! **These dialogs are modals on purpose.** The rule the connection and speech
//! notices follow — report in the log and the Home tab, because they arrive
//! unbidden and can land mid-broadcast — does not apply to an update, and the
//! test in `CLAUDE.md` is why. A startup check is unbidden but only ever speaks
//! when it has a question that needs answering, and the whole feature is a
//! question: nothing downloads, and nothing is replaced, without the user
//! saying so. A manual check answers a button press. Both are things the user is
//! sitting in front of.
//!
//! What *is* observed is the broadcast: [`is_a_bad_moment`] refuses to interrupt
//! a live stream or a recording. An update prompt during a show is exactly the
//! modal nobody can afford, and the check is cheap to repeat next launch.

use super::{App, show_error, show_info};
use crate::update::{ApplyPlan, Trigger, UpdateEvent, install_kind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wxdragon::prelude::*;

/// The update in flight, if any.
#[derive(Default)]
pub struct UpdateState {
    /// Set while a check or a download is running, so a second Check for
    /// updates press (or a startup check racing a manual one) cannot start a
    /// duplicate.
    pub busy: bool,
    /// The download's cancel flag, held so the dialog's button can set it.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The progress dialog. Dropping the last reference destroys the window.
    ///
    /// An `Rc` so the pump can clone a handle out under a short borrow and
    /// release the borrow *before* calling into wx. Nothing here may hold an
    /// `update_state` borrow across a widget call: wx can re-enter, and the
    /// second borrow would be a panic. Same rule `pump_scan_events` follows for
    /// `app.scan`.
    pub dialog: Option<Rc<super::update_dialog::UpdateDialog>>,
    /// The dialog an interactive check was started from, while it is still open.
    ///
    /// Every message box below is parented on this when it is set, and that is
    /// not cosmetic: a box owned by the main frame, shown while the modal
    /// Preferences dialog is up, is dismissed back onto a window the modal has
    /// *disabled*. wx restores a top-level window's last-focused child when that
    /// window is activated, so owning the box correctly is what puts the caret
    /// back on the button the user pressed; own it wrongly and Windows hands
    /// focus to whatever it can find, which is how a check with nothing on the
    /// other end left the keyboard stranded outside the Preferences dialog.
    ///
    /// Cleared by `preferences::show` before it destroys the dialog — a check
    /// outlives the window that started it, and a handle to a destroyed window
    /// must never be parented on.
    pub parent: Option<Dialog>,
}

impl UpdateState {
    /// A handle to the progress dialog, with the borrow already released.
    fn dialog(&self) -> Option<Rc<super::update_dialog::UpdateDialog>> {
        self.dialog.clone()
    }
}

/// One of the two windows an update notice can be parented on.
enum Owner {
    Dialog(Dialog),
    Frame(Frame),
}

impl Owner {
    fn as_widget(&self) -> &dyn WxWidget {
        match self {
            Owner::Dialog(dialog) => dialog,
            Owner::Frame(frame) => frame,
        }
    }
}

/// The window this update's notices belong to: the dialog that started the
/// check if it is still open, otherwise the main frame.
///
/// Returns an owned handle, so the caller holds neither an `update_state` nor a
/// `widgets` borrow while a modal runs its nested event loop.
fn notice_owner(app: &Rc<App>) -> Option<Owner> {
    let parent = app.update_state.borrow().parent;
    if let Some(dialog) = parent {
        return Some(Owner::Dialog(dialog));
    }
    app.widgets(|w| Owner::Frame(w.frame))
}

/// Records the dialog an interactive check is being started from, so the answer
/// comes back to the window the user is sitting in.
///
/// Called by `preferences::show` for the whole time the dialog is open, rather
/// than at the moment of the press: the answer arrives on the pump, long after
/// the click handler has returned, so there is nothing to hand it at that point.
/// The same call with `None` is what clears it before the dialog is destroyed.
pub fn set_notice_parent(app: &Rc<App>, parent: Option<Dialog>) {
    app.update_state.borrow_mut().parent = parent;
}

/// Starts a check, unless one is already running.
///
/// The install layout is resolved here, on the UI thread, so the worker never
/// touches `current_exe` and the answer the user is given describes the copy
/// that was actually checked.
pub fn start_check(app: &Rc<App>, trigger: Trigger) {
    if app.update_state.borrow().busy {
        log::debug!("Update check already running; ignoring the {trigger:?} request");
        return;
    }
    let Some(install) = install_kind::detect() else {
        log::warn!("Could not work out where Pubsplash is installed; skipping the update check");
        return;
    };
    app.update_state.borrow_mut().busy = true;
    log::info!("Checking for updates ({trigger:?}, {:?})", install.kind);
    crate::update::check(
        trigger,
        install,
        crate::update::EventSender::new(app.update_tx.clone()),
    );
}

/// Whether now is a bad time to interrupt with an update question.
///
/// A live stream or a running recording is the one state where a modal is
/// genuinely unaffordable — it steals focus from a broadcaster mid-show, and the
/// answer they would give is "not now" anyway. The check costs nothing to repeat
/// at the next launch.
fn is_a_bad_moment(app: &Rc<App>) -> bool {
    // A standalone recording is not covered by the streaming check, and matters
    // just as much: the file being written is thrown away if the app restarts.
    app.is_streaming_or_starting() || app.run.borrow().recording_started.is_some()
}

/// Drains everything the update workers have reported. Called from the pump.
///
/// Every borrow is released before a dialog opens: a modal runs a nested event
/// loop, and although `App::pumping` stops that loop re-entering this function,
/// a live `RefCell` borrow held across it would still be a panic waiting for the
/// first handler that wants the same cell.
pub fn drain_results(app: &Rc<App>) {
    while let Ok(event) = app.update_rx.try_recv() {
        match event {
            UpdateEvent::UpToDate { trigger } => {
                app.update_state.borrow_mut().busy = false;
                if trigger.reports_quiet_outcomes() && let Some(owner) = notice_owner(app) {
                    let version = crate::update::version::current();
                    show_info(
                        owner.as_widget(),
                        "Check for updates",
                        &format!("Pubsplash {version} is the latest version."),
                    );
                }
            }

            UpdateEvent::CheckFailed { trigger, message } => {
                app.update_state.borrow_mut().busy = false;
                // A startup check that cannot reach GitHub says nothing at all.
                // The user did not ask, and an error box on every launch behind
                // a captive portal or a firewall would be its own bug.
                if trigger.reports_quiet_outcomes() && let Some(owner) = notice_owner(app) {
                    show_error(owner.as_widget(), "Check for updates", &message);
                }
            }

            UpdateEvent::Available {
                trigger,
                manifest,
                kind,
            } => {
                app.update_state.borrow_mut().busy = false;
                if is_a_bad_moment(app) {
                    log::info!(
                        "Update {} is available; not asking while streaming or recording",
                        manifest.version
                    );
                    continue;
                }
                offer(app, trigger, *manifest, kind);
            }

            UpdateEvent::Progress { done, total } => {
                // Cloned out and the borrow dropped before the widget call.
                let dialog = app.update_state.borrow().dialog();
                if let Some(dialog) = dialog {
                    dialog.progressed(done, total);
                }
            }

            UpdateEvent::DownloadFailed { message, cancelled } => {
                // Drop the dialog before anything else: it must be gone before a
                // message box opens over where it was, and its `Drop` destroys
                // the window.
                finish_download(app);
                if !cancelled && let Some(owner) = notice_owner(app) {
                    show_error(owner.as_widget(), "Update", &message);
                }
            }

            UpdateEvent::Unpacking => {
                let dialog = app.update_state.borrow().dialog();
                if let Some(dialog) = dialog {
                    dialog.unpacking();
                }
            }

            UpdateEvent::Ready { plan } => {
                finish_download(app);
                apply(app, *plan);
            }
        }
    }
}

/// Clears the in-flight download state, destroying the progress dialog.
///
/// The dialog is taken out under the borrow and dropped *after* it is released:
/// dropping it calls `Dialog::destroy`, and no borrow may be live across a call
/// into wx.
fn finish_download(app: &Rc<App>) {
    let dialog = {
        let mut state = app.update_state.borrow_mut();
        state.busy = false;
        state.cancel = None;
        state.dialog.take()
    };
    drop(dialog);
}

/// Asks whether to take the update, and starts the download if so.
fn offer(app: &Rc<App>, trigger: Trigger, manifest: crate::update::manifest::Manifest, kind: install_kind::InstallKind) {
    // The prompt belongs to whatever the user is looking at; the progress dialog
    // that may follow it is deliberately still the frame's, because a download
    // outlives the Preferences dialog and must not be destroyed with it.
    let (Some(owner), Some(frame)) = (notice_owner(app), app.widgets(|w| w.frame)) else {
        return;
    };
    let current = crate::update::version::current();

    // A layout we do not recognise — a source build, or a copy someone has
    // pulled files out of — must never overwrite itself. Say what is available
    // and let the user decide what to do about it.
    if kind == install_kind::InstallKind::Unknown {
        if !trigger.reports_quiet_outcomes() {
            log::info!(
                "Update {} available, but this copy cannot update itself in place",
                manifest.version
            );
        }
        let ask = MessageDialog::builder(
            owner.as_widget(),
            &format!(
                "Pubsplash {} is available; you are running {current}. This copy was not \
                 installed in a way Pubsplash can update on its own. Open the download page?",
                manifest.version
            ),
            "Update available",
        )
        .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
        .build();
        let answer = ask.show_modal();
        ask.destroy();
        if answer == ID_YES && let Err(e) = super::shell_open(&manifest.notes_url) {
            log::warn!("Could not open the release page: {e}");
        }
        return;
    }

    let what_happens = match kind {
        install_kind::InstallKind::Installed => {
            "Pubsplash will download it, then close and run the installer."
        }
        // Named explicitly: the folder being replaced is the surprising half,
        // and someone keeping their own files beside a portable copy deserves
        // to know before they say yes.
        install_kind::InstallKind::Portable => {
            "Pubsplash will download it, then close, replace its own files, and start again."
        }
        install_kind::InstallKind::Unknown => unreachable!("handled above"),
    };
    let ask = MessageDialog::builder(
        owner.as_widget(),
        &format!(
            "Pubsplash {} is available; you are running {current}. {what_happens} \
             Download and install it now?",
            manifest.version
        ),
        "Update available",
    )
    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
    .build();
    let answer = ask.show_modal();
    ask.destroy();
    if answer != ID_YES {
        log::info!("Update {} declined", manifest.version);
        return;
    }

    let Some(install) = install_kind::detect() else {
        show_error(
            owner.as_widget(),
            "Update",
            "Could not work out where Pubsplash is installed, so nothing was changed.",
        );
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let dialog = super::update_dialog::UpdateDialog::show(&frame, &manifest.version, cancel.clone());
    {
        let mut state = app.update_state.borrow_mut();
        state.busy = true;
        state.cancel = Some(cancel.clone());
        state.dialog = Some(Rc::new(dialog));
    }
    crate::update::stage(
        manifest,
        install,
        cancel,
        crate::update::EventSender::new(app.update_tx.clone()),
    );
}

/// Launches the helper and closes Pubsplash.
///
/// Order matters and is not negotiable. The helper is copied out to
/// `runner\` first, because a portable update overwrites the install directory
/// and would otherwise be replacing the very binary it is running from. It is
/// spawned *detached* so it is not tied to a process that is about to exit. And
/// only then does the frame close — through `close(false)`, the same path the
/// Exit menu item takes, so the config is flushed, plugin editors come down and
/// the shutdown cue plays. The helper waits for this process to actually go
/// away before it touches anything.
fn apply(app: &Rc<App>, plan: ApplyPlan) {
    let (Some(owner), Some(frame)) = (notice_owner(app), app.widgets(|w| w.frame)) else {
        return;
    };
    let helper = match stage_helper() {
        Ok(helper) => helper,
        Err(message) => {
            show_error(owner.as_widget(), "Update", &message);
            return;
        }
    };

    let pid = std::process::id().to_string();
    let mut command = std::process::Command::new(&helper);
    command.arg("--wait-pid").arg(&pid);
    match &plan {
        ApplyPlan::Installer { setup } => {
            command.arg("--run-installer").arg(setup);
        }
        ApplyPlan::Portable {
            staging,
            target,
            relaunch,
        } => {
            command
                .arg("--apply")
                .arg(staging)
                .arg("--target")
                .arg(target)
                .arg("--relaunch")
                .arg(relaunch);
        }
    }
    // DETACHED_PROCESS so it does not inherit this process's console, and a new
    // process group so nothing that signals Pubsplash's group on the way out can
    // reach the one process that has to survive it.
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    if let Err(e) = command.spawn() {
        show_error(
            owner.as_widget(),
            "Update",
            &format!(
                "Could not start the updater ({}): {e}. Nothing has been changed.",
                helper.display()
            ),
        );
        return;
    }
    log::info!("Updater started; closing Pubsplash to let it work");
    frame.close(false);
}

/// Copies `pubsplash-update.exe` to the work directory and returns the copy.
///
/// Never run from beside `pubsplash.exe`: for a portable update that directory
/// is about to be overwritten, and Windows will not replace a running image.
fn stage_helper() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
    let source = exe.with_file_name(crate::update::HELPER_BINARY);
    if !source.is_file() {
        return Err(format!(
            "The updater ({}) is missing. Reinstall Pubsplash to restore it.",
            source.display()
        ));
    }
    let dir = crate::update::runner_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let destination = dir.join(crate::update::HELPER_BINARY);
    std::fs::copy(&source, &destination).map_err(|e| {
        format!(
            "Could not prepare the updater at {}: {e}",
            destination.display()
        )
    })?;
    Ok(destination)
}

/// Whether a cancel has been asked for. Used by the tests below and by the
/// dialog's button, which owns the same `Arc`.
#[allow(dead_code)]
fn is_cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_is_idle() {
        let state = UpdateState::default();
        assert!(!state.busy);
        assert!(state.cancel.is_none());
        assert!(state.dialog.is_none());
    }

    #[test]
    fn cancelling_is_visible_through_the_shared_flag() {
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(!is_cancelled(&cancel));
        // What the dialog's button does.
        cancel.store(true, Ordering::Relaxed);
        assert!(is_cancelled(&cancel));
    }
}
