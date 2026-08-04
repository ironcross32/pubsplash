//! Automatic updates, served entirely by GitHub.
//!
//! There is no server behind this. The release workflow publishes a small
//! [`Manifest`] (`latest.json`) alongside each release, plus every updater-facing
//! artifact under a stable, version-free name; Pubsplash reads the manifest from
//! `releases/latest/download/latest.json`, compares its `version` against
//! `CARGO_PKG_VERSION`, and — only if the user says yes — downloads the artifact
//! that suits this install layout, verifies it, and hands it to the helper.
//!
//! **This is its own domain, not part of `net`.** `net_loop` awaits each command
//! arm inline, so a multi-megabyte download parked in it would stall
//! `StopStream` and `Shutdown` — the two commands that must never wait. Updates
//! also have nothing to do with a streaming session. So this owns a worker
//! thread and a channel of its own, in the shape of `tts::catalog::start_refresh`
//! and the Mastodon result channel, and the UI drains it from the pump.
//!
//! Nothing here polls: a worker sends and rings the idle doorbell, exactly as
//! `net::EventSender::send` does, and the pump runs on that wake-up. The startup
//! check is one shot — never a timer, which would spend a user's connection on
//! their behalf all session.
//!
//! **Applying an update is the helper's job, never this process's.** Windows
//! will not let a running exe be replaced, and the NSIS installer wants
//! Pubsplash gone before it writes. So both paths end the same way: stage
//! everything, copy `pubsplash-update.exe` out to `runner\` (outside any folder
//! being overwritten), spawn it detached, and quit. See `src/bin/updater.rs`.

pub mod http;
pub mod install_kind;
pub mod manifest;
pub mod version;

use install_kind::{Install, InstallKind};
use manifest::Manifest;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// What asked for a check, which decides how quiet the answer is.
///
/// A startup check is unbidden, so it speaks only when it has something the user
/// wants: no update, or no internet, and it says nothing at all. A manual check
/// answers a button the user just pressed, so every outcome gets a reply —
/// silence there would read as a broken button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Startup,
    Manual,
}

impl Trigger {
    /// Whether an uneventful outcome is worth telling the user about.
    pub fn reports_quiet_outcomes(self) -> bool {
        matches!(self, Trigger::Manual)
    }
}

/// What the helper is to be told to do once everything is staged.
#[derive(Debug, Clone)]
pub enum ApplyPlan {
    /// Run the new NSIS installer. It handles per-user vs per-machine and asks
    /// for elevation itself when it needs it.
    Installer { setup: PathBuf },
    /// Overwrite a portable folder from a verified, already-extracted staging
    /// directory, then start Pubsplash again.
    Portable {
        staging: PathBuf,
        target: PathBuf,
        relaunch: PathBuf,
    },
}

/// Progress and outcomes from the update workers, drained by the UI pump.
#[derive(Debug)]
pub enum UpdateEvent {
    /// The running version is the newest there is.
    UpToDate { trigger: Trigger },
    /// A newer release exists. The UI asks the user before anything downloads.
    Available {
        trigger: Trigger,
        manifest: Box<Manifest>,
        kind: InstallKind,
    },
    /// The check itself failed — no network, GitHub unreachable, a manifest that
    /// would not parse.
    CheckFailed { trigger: Trigger, message: String },
    /// Bytes received so far, at most once a second.
    Progress { done: u64, total: u64 },
    /// The download is verified and is being unpacked. Portable updates only —
    /// the installer path has nothing to unpack and goes straight to `Ready`.
    /// Sent because extracting a ten-megabyte archive takes long enough that a
    /// dialog frozen at 100% would read as a hang.
    Unpacking,
    /// Downloaded, verified, and (for a portable update) extracted. The UI can
    /// now launch the helper and quit.
    Ready { plan: Box<ApplyPlan> },
    /// The download or the staging failed, or the user cancelled it.
    DownloadFailed { message: String, cancelled: bool },
}

/// Sends update events and wakes the UI to collect them.
///
/// The doorbell lives here rather than at each call site, for the reason
/// `net::EventSender` gives: a new event cannot then be added without it, which
/// would leave the UI sitting on a finished download until some other wake-up
/// happened to arrive.
#[derive(Clone)]
pub struct EventSender(crossbeam_channel::Sender<UpdateEvent>);

impl EventSender {
    pub fn new(inner: crossbeam_channel::Sender<UpdateEvent>) -> Self {
        EventSender(inner)
    }

    fn send(&self, event: UpdateEvent) {
        let _ = self.0.send(event);
        wxdragon::wake_up_idle();
    }
}

/// Asks GitHub what the newest release is.
///
/// Returns immediately; the answer arrives as an [`UpdateEvent`]. `install` is
/// resolved on the UI thread and passed in, so the worker never touches
/// `current_exe` and the layout the user is told about is the one that was
/// checked.
pub fn check(trigger: Trigger, install: Install, events: EventSender) {
    let spawned = std::thread::Builder::new()
        .name("update-check".to_string())
        .spawn(move || {
            let result = runtime().block_on(http::fetch_manifest());
            let manifest = match result {
                Ok(manifest) => manifest,
                Err(message) => {
                    log::warn!("Update check failed: {message}");
                    events.send(UpdateEvent::CheckFailed { trigger, message });
                    return;
                }
            };
            if version::is_newer(&manifest.version, version::current()) {
                log::info!(
                    "Update available: {} (running {})",
                    manifest.version,
                    version::current()
                );
                events.send(UpdateEvent::Available {
                    trigger,
                    manifest: Box::new(manifest),
                    kind: install.kind,
                });
            } else {
                log::info!(
                    "No update: GitHub's latest is {}, running {}",
                    manifest.version,
                    version::current()
                );
                events.send(UpdateEvent::UpToDate { trigger });
            }
        });
    if let Err(e) = spawned {
        log::warn!("Could not start the update check: {e}");
    }
}

/// Downloads the artifact for this layout, verifies it, and stages it.
///
/// Returns immediately. Set `cancel` to stop it; the partial download is removed
/// and a [`UpdateEvent::DownloadFailed`] with `cancelled` set comes back, so the
/// UI can close its dialog without reporting a fault the user caused on purpose.
pub fn stage(
    manifest: Manifest,
    install: Install,
    cancel: Arc<AtomicBool>,
    events: EventSender,
) {
    let spawned = std::thread::Builder::new()
        .name("update-download".to_string())
        .spawn(move || {
            match runtime().block_on(run_staging(&manifest, &install, &cancel, &events)) {
                Ok(plan) => {
                    log::info!("Update {} staged", manifest.version);
                    events.send(UpdateEvent::Ready {
                        plan: Box::new(plan),
                    });
                }
                Err(message) => {
                    let cancelled = message == http::CANCELLED;
                    if cancelled {
                        log::info!("Update download cancelled");
                    } else {
                        log::warn!("Update download failed: {message}");
                    }
                    events.send(UpdateEvent::DownloadFailed { message, cancelled });
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("Could not start the update download: {e}");
    }
}

async fn run_staging(
    manifest: &Manifest,
    install: &Install,
    cancel: &AtomicBool,
    events: &EventSender,
) -> Result<ApplyPlan, String> {
    // Checked before a single byte is downloaded: a layout that cannot be
    // updated in place has no artifact to fetch, and finding that out after a
    // ten-megabyte download would be a waste of the user's connection.
    if !install.can_self_update() {
        return Err(
            "This copy of Pubsplash was not installed in a way it can update on its own."
                .to_string(),
        );
    }
    let asset = manifest.asset_for(install.kind).ok_or_else(|| {
        "This copy of Pubsplash was not installed in a way it can update on its own.".to_string()
    })?;

    // A fresh working directory every time: a leftover from an abandoned attempt
    // must never be mistaken for part of this one.
    let work = work_dir();
    let _ = std::fs::remove_dir_all(&work);
    let download = work.join("download").join(&asset.name);

    http::download(asset, &download, cancel, |progress| {
        let http::Progress::Bytes { done, total } = progress;
        events.send(UpdateEvent::Progress { done, total });
    })
    .await?;

    match install.kind {
        InstallKind::Installed => Ok(ApplyPlan::Installer { setup: download }),
        InstallKind::Portable => {
            events.send(UpdateEvent::Unpacking);
            let staging = work.join("staging");
            extract(&download, &staging)?;
            // The new build must be complete before a single file of the old one
            // is touched. An archive missing pubsplash.exe is a broken release,
            // and finding that out halfway through the copy is unrecoverable.
            let relaunch = staging.join(MAIN_BINARY);
            if !relaunch.is_file() {
                return Err(format!(
                    "The downloaded update does not contain {MAIN_BINARY}, so it has not been used."
                ));
            }
            Ok(ApplyPlan::Portable {
                staging,
                target: install.dir.clone(),
                relaunch: install.dir.join(MAIN_BINARY),
            })
        }
        InstallKind::Unknown => {
            Err("This copy of Pubsplash cannot update itself in place.".to_string())
        }
    }
}

/// Pubsplash's own executable name, as it appears in both shipped layouts.
pub const MAIN_BINARY: &str = "pubsplash.exe";
/// The helper that applies an update once Pubsplash has exited.
pub const HELPER_BINARY: &str = "pubsplash-update.exe";

/// `%LOCALAPPDATA%\pubsplash\update` — downloads, staging, and the helper copy.
///
/// Under the data directory rather than the install directory on purpose: a
/// per-machine install lives in Program Files, which an unelevated Pubsplash
/// cannot write to, and a portable update must not stage into the very folder it
/// is about to overwrite.
pub fn work_dir() -> PathBuf {
    crate::config::config_dir().join("update")
}

/// Where the helper is run from — outside any directory it may be replacing, so
/// it cannot be overwriting itself.
pub fn runner_dir() -> PathBuf {
    work_dir().join("runner")
}

/// Clears anything a previous update left behind. Called once at startup.
///
/// This is what eventually removes the helper: a running process cannot delete
/// its own image, so the copy in `runner\` outlives the update that used it and
/// is cleaned up on the next launch. Best-effort throughout — a file still held
/// by something is simply retried next time.
pub fn cleanup_leftovers() {
    let work = work_dir();
    if !work.exists() {
        return;
    }
    match std::fs::remove_dir_all(&work) {
        Ok(()) => log::debug!("Cleared {}", work.display()),
        Err(e) => log::debug!("Could not clear {} yet: {e}", work.display()),
    }
}

/// Unpacks the portable ZIP into `destination`.
///
/// Entries are flattened to their file name. The archive is our own, built by
/// `Compress-Archive` from a flat `bundle\` directory, so there are no
/// directories in it to preserve.
///
/// Nothing can escape `destination`, by two independent means. `enclosed_name`
/// returns `None` for any entry naming a parent directory, an absolute path or a
/// drive letter, and that is treated as a broken archive and refuses the *whole*
/// update rather than skipping the entry — a release containing such a path is
/// not one to take half of. Taking `file_name` of what survives then discards
/// any interior directories, so the join below can only ever produce a direct
/// child.
fn extract(archive: &Path, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("Could not open the downloaded update: {e}"))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("The downloaded update is not a readable archive: {e}"))?;
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;

    let mut written = 0usize;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| format!("The downloaded update could not be read: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .enclosed_name()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .ok_or_else(|| {
                "The downloaded update contains an entry with an unusable name.".to_string()
            })?;
        let target = destination.join(&name);
        let mut out = std::fs::File::create(&target)
            .map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        written += 1;
    }
    if written == 0 {
        return Err("The downloaded update is empty, so it has not been used.".to_string());
    }
    log::info!("Extracted {written} files to {}", destination.display());
    Ok(())
}

/// A current-thread runtime per worker.
///
/// Not `tts::net::block_on`'s: that one is shared with the `tts-net` worker, and
/// a ten-megabyte download parked on it would hold up every queued chat message.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building the update runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_manual_check_reports_quiet_outcomes() {
        assert!(Trigger::Manual.reports_quiet_outcomes());
        assert!(!Trigger::Startup.reports_quiet_outcomes());
    }

    #[test]
    fn the_work_directory_sits_under_the_data_directory() {
        let work = work_dir();
        assert!(work.starts_with(crate::config::config_dir()));
        assert!(runner_dir().starts_with(&work));
    }

    /// A portable update stages into the data directory, never into the folder
    /// it is about to overwrite.
    #[test]
    fn staging_never_lands_inside_an_install() {
        let install = PathBuf::from(r"C:\Program Files\Pubsplash");
        assert!(!work_dir().starts_with(&install));
    }

    /// A scratch tree that cleans itself up.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pubsplash-extract-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Builds a deflate ZIP, the way `Compress-Archive` does.
    fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let file = std::fs::File::create(path).expect("creating the archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("adding an entry");
            writer.write_all(body).expect("writing an entry");
        }
        writer.finish().expect("finishing the archive");
    }

    #[test]
    fn extracts_every_file_flat() {
        let scratch = Scratch::new("flat");
        let archive = scratch.0.join("update.zip");
        build_zip(
            &archive,
            &[
                ("pubsplash.exe", b"new exe"),
                ("readme.html", b"new docs"),
                ("portable.txt", b"marker"),
            ],
        );
        let staging = scratch.0.join("staging");
        extract(&archive, &staging).expect("extraction succeeds");

        assert_eq!(
            std::fs::read(staging.join("pubsplash.exe")).unwrap(),
            b"new exe"
        );
        assert_eq!(
            std::fs::read(staging.join("readme.html")).unwrap(),
            b"new docs"
        );
        assert!(staging.join("portable.txt").is_file());
    }

    /// An entry naming a path outside the destination refuses the whole update.
    /// Skipping just that entry would apply a partial release, which is worse:
    /// an archive that malformed is not one to take half of.
    #[test]
    fn a_traversing_entry_refuses_the_whole_archive() {
        let scratch = Scratch::new("traversal");
        let archive = scratch.0.join("evil.zip");
        build_zip(
            &archive,
            &[
                ("pubsplash.exe", b"new exe"),
                ("../../escaped.txt", b"nope"),
            ],
        );
        let staging = scratch.0.join("staging");
        assert!(extract(&archive, &staging).is_err());
        // Above all, nothing was written outside the staging directory.
        assert!(!scratch.0.join("escaped.txt").exists());
        assert!(!std::env::temp_dir().join("escaped.txt").exists());
    }

    /// Interior directories are flattened away, so every entry lands as a direct
    /// child of the staging directory — which is the shape `apply_portable` in
    /// the helper expects.
    #[test]
    fn nested_entries_are_flattened() {
        let scratch = Scratch::new("nested");
        let archive = scratch.0.join("nested.zip");
        build_zip(&archive, &[("nested/dir/pubsplash.exe", b"new exe")]);
        let staging = scratch.0.join("staging");
        extract(&archive, &staging).expect("extraction succeeds");

        assert!(staging.join("pubsplash.exe").is_file());
        assert!(!staging.join("nested").exists());
    }

    #[test]
    fn an_empty_archive_is_an_error() {
        let scratch = Scratch::new("empty");
        let archive = scratch.0.join("empty.zip");
        build_zip(&archive, &[]);
        assert!(extract(&archive, &scratch.0.join("staging")).is_err());
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_reported() {
        let scratch = Scratch::new("not-a-zip");
        let archive = scratch.0.join("truncated.zip");
        std::fs::write(&archive, b"this is not a zip file").expect("writing the decoy");
        assert!(extract(&archive, &scratch.0.join("staging")).is_err());
    }
}
