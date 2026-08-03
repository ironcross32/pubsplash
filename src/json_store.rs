//! Loading and saving the app's JSON files: the config, the FX chain library,
//! and the plugin cache.
//!
//! All three want the same three things, and each used to spell them out
//! separately: parse or recover, never leave a half-written file behind, and
//! keep a copy of anything unusable rather than silently dropping it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;

/// What [`load`] found.
pub enum Load<T> {
    /// Parsed successfully.
    Ok(T),
    /// Nothing usable is at that path any more: either there was no file, or
    /// there was a corrupt one and it has been renamed aside. Either way the
    /// path is free for a caller that wants to write defaults there.
    Absent,
    /// A file is there and could not be read at all — a permissions problem,
    /// say. Distinct from [`Load::Absent`] on purpose: writing defaults over
    /// something we could not even look at would destroy it.
    Unreadable,
}

/// Reads `path` as JSON. `what` names the file in log messages, capitalized as
/// the start of a sentence ("Config file").
pub fn load<T: DeserializeOwned>(path: &Path, what: &str) -> Load<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Load::Absent,
        Err(e) => {
            log::error!("Failed to read {what}: {e}; using defaults without saving");
            return Load::Unreadable;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Load::Ok(value),
        Err(e) => {
            log::error!("{what} is corrupt ({e}); backing it up and using defaults");
            back_up(path, what);
            Load::Absent
        }
    }
}

/// Renames a corrupt file aside, without overwriting an earlier backup — the
/// previous one may well be the last good copy the user has.
fn back_up(path: &Path, what: &str) {
    let first = path.with_extension("json.bak");
    let mut backup = first.clone();
    for n in 2..100 {
        if !backup.exists() {
            break;
        }
        backup = path.with_extension(format!("json.bak{n}"));
    }
    if let Err(e) = std::fs::rename(path, &backup) {
        log::error!("Failed to back up corrupt {what}: {e}");
    }
}

/// Serializes `value` to `path`, creating the parent directory if needed.
pub fn save<T: Serialize>(value: &T, path: &Path, what: &str) {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::error!("Failed to create the directory for {what}: {e}");
        return;
    }
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            if let Err(e) = write_atomic(path, &json) {
                log::error!("Failed to write {what}: {e}");
            }
        }
        Err(e) => log::error!("Failed to serialize {what}: {e}"),
    }
}

/// Writes `contents` to `path` via a sibling temp file and a rename, so an
/// interrupted write can never leave a half-written file behind.
///
/// The config is saved often enough that a crash mid-write is a real risk, and
/// a truncated file reads as corrupt — which costs the user every scene,
/// source, bus and FX chain they have. `rename` over an existing file is atomic
/// on NTFS.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leaving the temp file behind would make the next save fail too.
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("pubsplash-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_second_corruption_does_not_destroy_the_first_backup() {
        let path = temp_path("store-backup.json");
        for n in 1..4 {
            let _ = std::fs::remove_file(path.with_extension(format!("json.bak{n}")));
        }
        let _ = std::fs::remove_file(path.with_extension("json.bak"));

        std::fs::write(&path, "first corruption").unwrap();
        assert!(matches!(load::<u32>(&path, "Test file"), Load::Absent));
        std::fs::write(&path, "second corruption").unwrap();
        assert!(matches!(load::<u32>(&path, "Test file"), Load::Absent));

        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            "first corruption",
            "the first backup must survive the second corruption"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak2")).unwrap(),
            "second corruption"
        );
    }

    #[test]
    fn a_second_save_replaces_the_first() {
        // `write_atomic` renames the temp file over a destination that already
        // exists. That works on Windows because `std::fs::rename` is
        // `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` — an external review
        // claimed otherwise, which would mean every save after the first
        // silently kept the old value. This pins the behaviour.
        let path = temp_path("store-replace.json");
        let _ = std::fs::remove_file(&path);

        save(&1u32, &path, "Test file");
        assert!(matches!(load::<u32>(&path, "Test file"), Load::Ok(1)));
        save(&2u32, &path, "Test file");
        assert!(matches!(load::<u32>(&path, "Test file"), Load::Ok(2)));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unreadable_file_is_not_confused_with_a_missing_one() {
        let missing = temp_path("store-missing.json");
        let _ = std::fs::remove_file(&missing);
        assert!(matches!(load::<u32>(&missing, "Test file"), Load::Absent));

        // A directory where a file is expected reads as an error that is not
        // NotFound, which is the shape of the case this distinguishes.
        let dir = temp_path("store-as-a-directory.json");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(load::<u32>(&dir, "Test file"), Load::Unreadable));
    }
}
