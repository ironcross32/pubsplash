//! pubsplash-update: applies an update once Pubsplash itself has exited.
//!
//! Windows will not let a running executable be replaced, and the NSIS installer
//! wants Pubsplash gone before it writes — so neither half of an update can be
//! done by the process being updated. This is that second process.
//!
//! **It is always run from a copy in `%LOCALAPPDATA%\pubsplash\update\runner\`,
//! never from the install directory.** Two things depend on that. A portable
//! update overwrites the install directory, which would include this binary if
//! it were running from there; and the installer must not be a child of a
//! process that is about to exit. Nothing here deletes the copy — a process
//! cannot unmap its own image — so Pubsplash clears the whole work directory at
//! its next start (`update::cleanup_leftovers`).
//!
//! Usage:
//!
//! ```text
//! pubsplash-update --wait-pid <pid> --run-installer <setup.exe>
//! pubsplash-update --wait-pid <pid> --apply <staging> --target <dir> --relaunch <exe>
//! ```
//!
//! Everything it is handed has already been downloaded, size-checked and
//! SHA-256-verified by Pubsplash, and a portable update has already been
//! extracted and confirmed to contain `pubsplash.exe`. This binary deliberately
//! knows nothing about HTTP, JSON or archives: by the time it runs there is no
//! user interface left to report a subtle failure to, so its only job is the one
//! step that cannot be done earlier.
//!
//! Failure is not silent. A portable update that cannot be completed is rolled
//! back from the move-aside directory and reported with a message box — the user
//! has just watched their app close, and an app that never comes back with no
//! explanation is the worst outcome available.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SYSTEMMODAL, MessageBoxW,
};
use windows::core::PCWSTR;

/// How long to wait for Pubsplash to exit before giving up.
///
/// Generous: shutdown plays a sound cue, flushes the config, tears down hosted
/// VST plugins (which can be slow and occasionally hostile) and joins the net
/// thread. Giving up here is safe — nothing has been touched yet — whereas
/// starting the copy while the old exe is still mapped is not.
const EXIT_TIMEOUT: Duration = Duration::from_secs(90);

/// Per-file retry budget when replacing a portable install.
///
/// An anti-virus scanner or a search indexer routinely holds a file it has just
/// seen appear for a moment. These are the numbers that turn "update failed"
/// into a pause nobody notices.
const FILE_ATTEMPTS: u32 = 10;
const FILE_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Where the old files go while the new ones are written, so a failure partway
/// through can be undone.
const BACKUP_DIR: &str = ".pubsplash-old";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse_args(&args) {
        Ok(command) => command,
        Err(message) => {
            // No message box: a malformed command line means this was run by
            // hand, not by Pubsplash, and there is a console to read.
            eprintln!("pubsplash-update: {message}");
            std::process::exit(2);
        }
    };

    if let Some(pid) = command.wait_pid
        && !wait_for_exit(pid)
    {
        fail("Pubsplash did not close, so the update was not applied. Your existing \
              installation has not been changed.");
    }

    let result = match command.action {
        Action::RunInstaller { setup } => run_installer(&setup),
        Action::ApplyPortable {
            staging,
            target,
            relaunch,
        } => apply_portable(&staging, &target, relaunch.as_deref()),
    };

    if let Err(message) = result {
        fail(&message);
    }
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Action {
    RunInstaller {
        setup: PathBuf,
    },
    ApplyPortable {
        staging: PathBuf,
        target: PathBuf,
        relaunch: Option<PathBuf>,
    },
}

#[derive(Debug, PartialEq)]
struct Command {
    wait_pid: Option<u32>,
    action: Action,
}

/// Parses the argument list. Pure, so the shapes Pubsplash emits can be tested
/// without spawning anything.
fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut wait_pid = None;
    let mut setup = None;
    let mut staging = None;
    let mut target = None;
    let mut relaunch = None;

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = || {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--wait-pid" => {
                wait_pid = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--wait-pid needs a process id".to_string())?,
                );
            }
            "--run-installer" => setup = Some(PathBuf::from(value()?)),
            "--apply" => staging = Some(PathBuf::from(value()?)),
            "--target" => target = Some(PathBuf::from(value()?)),
            "--relaunch" => relaunch = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown option {other}")),
        }
        index += 2;
    }

    let action = match (setup, staging, target) {
        (Some(setup), None, None) => Action::RunInstaller { setup },
        (None, Some(staging), Some(target)) => Action::ApplyPortable {
            staging,
            target,
            relaunch,
        },
        (Some(_), _, _) => {
            return Err("--run-installer cannot be combined with --apply".to_string());
        }
        _ => return Err("expected --run-installer, or --apply with --target".to_string()),
    };
    Ok(Command { wait_pid, action })
}

// ---------------------------------------------------------------------------
// Waiting for Pubsplash to go away
// ---------------------------------------------------------------------------

/// Blocks until process `pid` exits. `true` if it is gone.
///
/// The handle is opened first thing, before anything else runs, so the window in
/// which the pid could be recycled onto a different process is as small as it
/// can be. A failed open means it has already exited, which is the common case
/// when Pubsplash closes quickly.
fn wait_for_exit(pid: u32) -> bool {
    let handle: HANDLE = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => handle,
        Err(_) => return true,
    };
    let millis = u32::try_from(EXIT_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    let result = unsafe { WaitForSingleObject(handle, millis) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result == WAIT_OBJECT_0
}

// ---------------------------------------------------------------------------
// Installed: hand over to NSIS
// ---------------------------------------------------------------------------

/// Starts the downloaded installer and returns.
///
/// Nothing is passed to it: the user picked per-user or per-machine when they
/// first installed, NSIS remembers, and a per-machine install raises its own
/// UAC prompt. A silent install would be worse here, not better — a screen
/// reader user who has just watched their app vanish should see the installer
/// they recognise, and be able to cancel it.
fn run_installer(setup: &Path) -> Result<(), String> {
    if !setup.is_file() {
        return Err(format!(
            "The downloaded installer is missing ({}), so nothing was changed.",
            setup.display()
        ));
    }
    std::process::Command::new(setup)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start the installer ({}): {e}", setup.display()))
}

// ---------------------------------------------------------------------------
// Portable: replace the folder
// ---------------------------------------------------------------------------

/// Replaces every file the update ships, then starts Pubsplash again.
///
/// Each file is *moved* aside into `.pubsplash-old\` rather than deleted, so any
/// failure can be undone: a half-copied folder is an app that will not start and
/// a user with nothing to go back to. Files in the target that the update does
/// not ship are left alone — someone may keep their own things beside a portable
/// copy, and deleting them would be a far worse bug than leaving a stale one.
fn apply_portable(staging: &Path, target: &Path, relaunch: Option<&Path>) -> Result<(), String> {
    let files = shipped_files(staging)?;
    if files.is_empty() {
        return Err("The downloaded update contained no files, so nothing was changed.".to_string());
    }

    let backup = target.join(BACKUP_DIR);
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::create_dir_all(&backup)
        .map_err(|e| format!("Could not prepare {}: {e}", backup.display()))?;

    // Every file that was successfully moved aside, so a failure can put them
    // all back in the order they went.
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();

    for name in &files {
        let source = staging.join(name);
        let destination = target.join(name);
        let saved = backup.join(name);

        if destination.exists()
            && let Err(e) = retry(|| std::fs::rename(&destination, &saved))
        {
            return Err(undo(
                &moved,
                &backup,
                format!(
                    "Could not replace {}: {e}. The update was undone and your existing \
                     version is unchanged.",
                    destination.display()
                ),
            ));
        }
        if destination.exists() {
            // The rename above reported success but the file is still there,
            // which means something recreated it. Better to stop than to race.
            return Err(undo(
                &moved,
                &backup,
                format!(
                    "{} could not be replaced because something else is using it. The \
                     update was undone and your existing version is unchanged.",
                    destination.display()
                ),
            ));
        }
        moved.push((destination.clone(), saved));

        if let Err(e) = retry(|| std::fs::copy(&source, &destination).map(|_| ())) {
            return Err(undo(
                &moved,
                &backup,
                format!(
                    "Could not write {}: {e}. The update was undone and your existing \
                     version is unchanged.",
                    destination.display()
                ),
            ));
        }
    }

    // Past this point the new version is in place and complete. Everything below
    // is tidying, and none of it is worth failing the update over.
    if let Some(exe) = relaunch
        && let Err(e) = std::process::Command::new(exe).spawn()
    {
        return Err(format!(
            "Pubsplash was updated successfully, but could not be started again: {e}. \
             Start it yourself from {}.",
            exe.display()
        ));
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

/// The file names the staged update ships, top level only.
///
/// Flat by construction: `extract` in `src/update/mod.rs` writes only file names
/// into the staging directory, and the release ZIP is itself flat.
fn shipped_files(staging: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(staging)
        .map_err(|e| format!("Could not read the downloaded update: {e}"))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Could not read the downloaded update: {e}"))?;
        if entry.path().is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            files.push(name.to_string());
        }
    }
    files.sort();
    Ok(files)
}

/// Restores everything, clears the move-aside directory, and hands back the
/// message to report.
///
/// The directory is removed on the way out so a failed update leaves no trace in
/// what may be a folder the user keeps their own files in — after the rollback
/// it holds nothing anyway. Every failure path goes through here, so none of
/// them can forget half of it.
fn undo(moved: &[(PathBuf, PathBuf)], backup: &Path, message: String) -> String {
    roll_back(moved);
    let _ = std::fs::remove_dir_all(backup);
    message
}

/// Puts every moved-aside file back where it came from.
///
/// Best-effort by necessity — there is nothing further to fall back on — but in
/// practice these renames are the reverse of ones that just succeeded, onto
/// paths that are now empty.
fn roll_back(moved: &[(PathBuf, PathBuf)]) {
    for (destination, saved) in moved.iter().rev() {
        let _ = std::fs::remove_file(destination);
        let _ = retry(|| std::fs::rename(saved, destination));
    }
}

/// Runs a filesystem operation, retrying briefly while it keeps failing.
///
/// Covers the case that makes naive updaters flaky: a file that is momentarily
/// held by an anti-virus scanner, the search indexer, or Explorer reading an
/// icon. Anything still failing after the budget is a real problem.
fn retry<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(e) => {
                attempt += 1;
                if attempt >= FILE_ATTEMPTS {
                    return Err(e);
                }
                std::thread::sleep(FILE_RETRY_DELAY);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// Reports a failure and exits.
///
/// A message box rather than a log line, and system-modal so it cannot end up
/// behind another window: Pubsplash has already closed, so there is no other
/// surface left, and an app that simply never comes back is the one outcome a
/// user cannot diagnose.
fn fail(message: &str) -> ! {
    let start = Instant::now();
    let text = wide(message);
    let caption = wide("Pubsplash update");
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SYSTEMMODAL,
        );
    }
    let _ = start;
    std::process::exit(1);
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_installer_form() {
        let command = parse_args(&args(&[
            "--wait-pid",
            "1234",
            "--run-installer",
            r"C:\temp\pubsplash-setup.exe",
        ]))
        .expect("the installer form parses");
        assert_eq!(command.wait_pid, Some(1234));
        assert_eq!(
            command.action,
            Action::RunInstaller {
                setup: PathBuf::from(r"C:\temp\pubsplash-setup.exe")
            }
        );
    }

    #[test]
    fn parses_the_portable_form() {
        let command = parse_args(&args(&[
            "--wait-pid",
            "77",
            "--apply",
            r"C:\work\staging",
            "--target",
            r"C:\apps\pubsplash",
            "--relaunch",
            r"C:\apps\pubsplash\pubsplash.exe",
        ]))
        .expect("the portable form parses");
        assert_eq!(command.wait_pid, Some(77));
        assert_eq!(
            command.action,
            Action::ApplyPortable {
                staging: PathBuf::from(r"C:\work\staging"),
                target: PathBuf::from(r"C:\apps\pubsplash"),
                relaunch: Some(PathBuf::from(r"C:\apps\pubsplash\pubsplash.exe")),
            }
        );
    }

    #[test]
    fn rejects_incomplete_or_mixed_commands() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["--wait-pid", "1"])).is_err());
        // --apply without --target has nowhere to write.
        assert!(parse_args(&args(&["--apply", "s"])).is_err());
        assert!(parse_args(&args(&["--target", "t"])).is_err());
        assert!(parse_args(&args(&["--run-installer", "s", "--apply", "x", "--target", "y"])).is_err());
        assert!(parse_args(&args(&["--wait-pid", "not-a-number"])).is_err());
        assert!(parse_args(&args(&["--nonsense", "1"])).is_err());
        assert!(parse_args(&args(&["--run-installer"])).is_err());
    }

    /// A scratch tree that cleans itself up.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pubsplash-updater-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            Scratch(dir)
        }

        fn sub(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).expect("creating a scratch subdirectory");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("writing a scratch file");
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).expect("reading a scratch file")
    }

    #[test]
    fn lists_only_top_level_files() {
        let scratch = Scratch::new("listing");
        let staging = scratch.sub("staging");
        write(&staging, "pubsplash.exe", "new");
        write(&staging, "readme.html", "new");
        scratch.sub("staging/nested");
        let files = shipped_files(&staging).expect("listing succeeds");
        assert_eq!(files, vec!["pubsplash.exe", "readme.html"]);
    }

    #[test]
    fn replaces_every_shipped_file() {
        let scratch = Scratch::new("replace");
        let staging = scratch.sub("staging");
        let target = scratch.sub("target");
        write(&staging, "pubsplash.exe", "new");
        write(&staging, "readme.html", "new docs");
        write(&target, "pubsplash.exe", "old");
        write(&target, "readme.html", "old docs");

        apply_portable(&staging, &target, None).expect("the update applies");

        assert_eq!(read(&target, "pubsplash.exe"), "new");
        assert_eq!(read(&target, "readme.html"), "new docs");
        // The move-aside directory is tidied away on success.
        assert!(!target.join(BACKUP_DIR).exists());
    }

    #[test]
    fn adds_files_the_old_version_did_not_have() {
        let scratch = Scratch::new("added");
        let staging = scratch.sub("staging");
        let target = scratch.sub("target");
        write(&staging, "pubsplash.exe", "new");
        write(&staging, "pubsplash-update.exe", "brand new");
        write(&target, "pubsplash.exe", "old");

        apply_portable(&staging, &target, None).expect("the update applies");

        assert_eq!(read(&target, "pubsplash-update.exe"), "brand new");
    }

    /// Someone's own files sitting beside a portable copy must survive. This is
    /// why the update replaces what it ships rather than emptying the folder.
    #[test]
    fn leaves_unrelated_files_alone() {
        let scratch = Scratch::new("unrelated");
        let staging = scratch.sub("staging");
        let target = scratch.sub("target");
        write(&staging, "pubsplash.exe", "new");
        write(&target, "pubsplash.exe", "old");
        write(&target, "my-notes.txt", "keep me");

        apply_portable(&staging, &target, None).expect("the update applies");

        assert_eq!(read(&target, "my-notes.txt"), "keep me");
    }

    #[test]
    fn an_empty_update_changes_nothing() {
        let scratch = Scratch::new("empty");
        let staging = scratch.sub("staging");
        let target = scratch.sub("target");
        write(&target, "pubsplash.exe", "old");

        assert!(apply_portable(&staging, &target, None).is_err());
        assert_eq!(read(&target, "pubsplash.exe"), "old");
    }

    /// The whole point of the move-aside directory: a file that cannot be
    /// replaced must leave the install exactly as it was, not half updated.
    ///
    /// `readme.html` is held open with no sharing, which is what an anti-virus
    /// scanner or a still-running process looks like. It sorts after
    /// `pubsplash.exe`, so by the time it fails a file has already been
    /// replaced and genuinely has to be put back.
    #[test]
    fn a_file_that_cannot_be_replaced_undoes_the_whole_update() {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        /// Neither FILE_SHARE_READ nor FILE_SHARE_WRITE: an exclusive handle.
        const NO_SHARING: u32 = 0;

        let scratch = Scratch::new("locked");
        let staging = scratch.sub("staging");
        let target = scratch.sub("target");
        write(&staging, "pubsplash.exe", "new");
        write(&staging, "readme.html", "new docs");
        write(&target, "pubsplash.exe", "old");
        write(&target, "readme.html", "old docs");
        write(&target, "my-notes.txt", "keep me");

        let locked = OpenOptions::new()
            .read(true)
            .share_mode(NO_SHARING)
            .open(target.join("readme.html"))
            .expect("locking the file");

        let error = apply_portable(&staging, &target, None)
            .expect_err("a locked file must fail the update");
        assert!(
            error.contains("readme.html"),
            "the message must name the file: {error}"
        );
        drop(locked);

        // Everything is back as it was, including the file that was already
        // replaced before the failure.
        assert_eq!(read(&target, "pubsplash.exe"), "old");
        assert_eq!(read(&target, "readme.html"), "old docs");
        assert_eq!(read(&target, "my-notes.txt"), "keep me");
        // And no move-aside directory is left in the user's folder.
        assert!(!target.join(BACKUP_DIR).exists());
    }

    #[test]
    fn rolling_back_restores_what_was_moved() {
        let scratch = Scratch::new("rollback");
        let target = scratch.sub("target");
        let backup = scratch.sub("target/.pubsplash-old");
        write(&backup, "pubsplash.exe", "original");
        let moved = vec![(
            target.join("pubsplash.exe"),
            backup.join("pubsplash.exe"),
        )];

        roll_back(&moved);

        assert_eq!(read(&target, "pubsplash.exe"), "original");
    }

    #[test]
    fn a_missing_installer_is_reported_not_ignored() {
        let scratch = Scratch::new("no-installer");
        let missing = scratch.0.join("pubsplash-setup.exe");
        assert!(run_installer(&missing).is_err());
    }
}
