#![allow(dead_code)]
//! Where Pubsplash keeps everything it writes: settings, logs, crash dumps,
//! caches, sound packs and the update staging area.
//!
//! There are two layouts, decided once per process from the folder the running
//! executable sits in:
//!
//! - **Portable** — a copy unpacked from the release ZIP, which the ZIP's
//!   `portable.txt` marker identifies. Everything goes in `user_data\` beside
//!   the executable. That is what portable has to mean: the folder is the whole
//!   installation, so it can be carried on a stick, copied between machines, or
//!   deleted without leaving anything behind. A copy that scatters its settings
//!   through `%LOCALAPPDATA%` is an installed app you have to unpack yourself.
//! - **Everything else** — an NSIS install or a source build:
//!   `%LOCALAPPDATA%\pubsplash`, as it has always been. An installed copy must
//!   not write beside its executable, because a per-machine install lives in
//!   Program Files, which an unelevated Pubsplash cannot write to.
//!
//! Resolved from `current_exe`, never the working directory, which a shortcut's
//! "Start in" can point anywhere — the same rule [`crate::update::install_kind`],
//! `find_doc` and `vst::scan::helper_path` follow. Resolved once and cached,
//! because the answer cannot change while the process runs and every write path
//! in the app asks for it.
//!
//! This module is `#[path]`-included into the standalone soundpack binaries,
//! which have no `crate::config`, so it depends on nothing but `std`, `dirs` and
//! `log` — the same reason `audio/convert.rs` depends on nothing but `hound`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The marker the release workflow writes into the portable ZIP. Its presence is
/// the whole signal; the text inside is for whoever opens it.
pub const PORTABLE_MARKER: &str = "portable.txt";

/// What a portable copy's data folder is called, beside the executable.
pub const PORTABLE_DATA_DIR: &str = "user_data";

/// The settings file, named here because [`migrate_from_legacy`] keys off it.
pub const SETTINGS_FILE: &str = "config.json";

/// Entries under the data root that belong to this machine rather than to the
/// user, and so are not worth carrying across in a migration: rotated logs, crash
/// dumps, and the update scratch area (which is cleared at every start anyway).
const MACHINE_LOCAL: [&str; 3] = ["logs", "crashes", "update"];

struct Root {
    path: PathBuf,
    portable: bool,
}

static ROOT: OnceLock<Root> = OnceLock::new();

fn resolved() -> &'static Root {
    ROOT.get_or_init(|| match portable_root() {
        Some(path) => Root {
            path,
            portable: true,
        },
        None => Root {
            path: legacy_root(),
            portable: false,
        },
    })
}

/// The folder every other path in the app hangs off.
pub fn root() -> &'static Path {
    &resolved().path
}

/// Whether this copy keeps its data beside its executable.
pub fn is_portable() -> bool {
    resolved().portable
}

/// Whether `dir` holds a portable copy of Pubsplash. Shared with
/// [`crate::update::install_kind`] so that "this is the portable build" is
/// decided in exactly one place — the layout that updates itself by overwriting
/// its own folder is the same layout that keeps its data inside it.
pub fn is_portable_dir(dir: &Path) -> bool {
    dir.join(PORTABLE_MARKER).is_file()
}

/// `%LOCALAPPDATA%\pubsplash` — where every non-portable layout keeps its data,
/// and where a portable copy from before this existed left its own.
pub fn legacy_root() -> PathBuf {
    dirs::data_local_dir()
        .expect("LOCALAPPDATA should always exist on Windows")
        .join("pubsplash")
}

/// The data folder for a portable copy, or `None` if this is not one.
fn portable_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    portable_root_in(exe.parent()?)
}

/// The pure half of [`portable_root`], so the rule is testable without a real
/// portable install.
fn portable_root_in(install_dir: &Path) -> Option<PathBuf> {
    is_portable_dir(install_dir).then(|| install_dir.join(PORTABLE_DATA_DIR))
}

/// Brings a portable copy's existing settings in from `%LOCALAPPDATA%`, once.
///
/// Portable copies used to store everything there like an installed one does, so
/// for anybody updating from such a build the settings, logins, scenes and FX
/// chains are all sitting in the old place. Starting them from scratch would
/// look exactly like losing them.
///
/// Keyed on the settings file rather than on the folder existing, because
/// `logging::init` creates the folder before this runs and any number of things
/// might later create it earlier still. Copies rather than moves, so an installed
/// Pubsplash on the same machine keeps working; and skips anything that already
/// exists in the new folder, so it can never overwrite newer settings.
///
/// Returns where the data came from, for the caller to log — logging is not up
/// yet at the point the folder is first touched, and a migration nobody can see
/// happen is a support call.
pub fn migrate_from_legacy() -> Option<PathBuf> {
    if !is_portable() {
        return None;
    }
    let root = root();
    if root.join(SETTINGS_FILE).exists() {
        return None;
    }
    let legacy = legacy_root();
    if !legacy.join(SETTINGS_FILE).is_file() {
        return None;
    }
    copy_settings(&legacy, root);
    Some(legacy)
}

/// Copies the user's half of `from` into `to`. Best effort per entry: a single
/// unreadable file is worth a log line, not an abandoned migration that leaves
/// half the settings behind.
fn copy_settings(from: &Path, to: &Path) {
    if let Err(e) = std::fs::create_dir_all(to) {
        log::warn!("Could not create {}: {e}", to.display());
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if MACHINE_LOCAL.iter().any(|skip| name.eq_ignore_ascii_case(skip)) {
            continue;
        }
        let target = to.join(&name);
        if target.exists() {
            continue;
        }
        let source = entry.path();
        let copied = match entry.file_type() {
            Ok(kind) if kind.is_dir() => copy_dir(&source, &target),
            _ => std::fs::copy(&source, &target).map(|_| ()),
        };
        if let Err(e) = copied {
            log::warn!("Could not bring {} across: {e}", source.display());
        }
    }
}

/// Recursive directory copy. Only `soundpacks\` reaches this in practice, but a
/// general one is shorter than a special case.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up, so these tests leave nothing
    /// in the temp folder even when one fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pubsplash-data-dir-{name}"));
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

    #[test]
    fn the_marker_puts_the_data_beside_the_executable() {
        let scratch = Scratch::new("marker");
        write(&scratch.0, PORTABLE_MARKER, "");
        assert_eq!(
            portable_root_in(&scratch.0),
            Some(scratch.0.join(PORTABLE_DATA_DIR))
        );
    }

    #[test]
    fn without_the_marker_there_is_no_portable_root() {
        let scratch = Scratch::new("no-marker");
        write(&scratch.0, "uninstall.exe", "");
        assert_eq!(portable_root_in(&scratch.0), None);
    }

    /// The data folder must never be the install folder itself: the portable
    /// updater replaces the files it ships at the top level, and the user's
    /// settings have to sit somewhere it does not look.
    #[test]
    fn the_portable_root_is_a_subfolder() {
        let scratch = Scratch::new("subfolder");
        write(&scratch.0, PORTABLE_MARKER, "");
        let root = portable_root_in(&scratch.0).expect("a portable root");
        assert!(root.starts_with(&scratch.0));
        assert_ne!(root, scratch.0);
    }

    #[test]
    fn migration_carries_settings_but_not_this_machines_scratch() {
        let scratch = Scratch::new("migrate");
        let legacy = scratch.sub("legacy");
        let new_root = scratch.0.join("user_data");
        write(&legacy, SETTINGS_FILE, "{}");
        write(&legacy, "fx_chains.json", "[]");
        let packs = scratch.sub("legacy/soundpacks");
        write(&packs, "mine.pspack", "pack");
        write(&scratch.sub("legacy/logs"), "pubsplash_rCURRENT.log", "noise");
        write(&scratch.sub("legacy/crashes"), "dump.dmp", "noise");

        copy_settings(&legacy, &new_root);

        assert!(new_root.join(SETTINGS_FILE).is_file());
        assert!(new_root.join("fx_chains.json").is_file());
        assert!(new_root.join("soundpacks/mine.pspack").is_file());
        assert!(!new_root.join("logs").exists());
        assert!(!new_root.join("crashes").exists());
    }

    /// Whatever is already in the new folder wins: the migration runs on a
    /// folder the app may have started writing to, and older settings must never
    /// land on top of newer ones.
    #[test]
    fn migration_never_overwrites_what_is_already_there() {
        let scratch = Scratch::new("no-overwrite");
        let legacy = scratch.sub("legacy");
        let new_root = scratch.sub("user_data");
        write(&legacy, SETTINGS_FILE, "old");
        write(&new_root, SETTINGS_FILE, "new");

        copy_settings(&legacy, &new_root);

        assert_eq!(
            std::fs::read_to_string(new_root.join(SETTINGS_FILE)).unwrap(),
            "new"
        );
    }
}
