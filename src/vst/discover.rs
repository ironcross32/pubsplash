//! Walks the configured plugin folders and produces the list of scan
//! candidates: VST2 DLLs that really export a VST entry point, single-file
//! VST3 plugins, and VST3 bundle folders. Wrong-architecture binaries are
//! counted and skipped; DLLs without a VST export are silently ignored.

use super::pe;
use super::types::PluginFormat;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Architecture folder inside a VST3 bundle (`Name.vst3\Contents\<arch>\`).
#[cfg(target_arch = "aarch64")]
const ARCH_DIR: &str = "arm64-win";
#[cfg(not(target_arch = "aarch64"))]
const ARCH_DIR: &str = "x86_64-win";

const MAX_DEPTH: u32 = 8;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub format: PluginFormat,
    /// The binary a host would load.
    pub path: PathBuf,
    /// Human-readable name used in progress messages (file stem).
    pub display: String,
    /// For VST3 bundles, the bundle root folder (for moduleinfo.json lookup).
    pub bundle: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub candidates: Vec<Candidate>,
    /// Plugins found but built for another CPU architecture (e.g. 32-bit).
    pub skipped_other_arch: usize,
}

/// Enumerating large folder trees can take a while; `cancel` aborts the walk
/// early (the partial result is then discarded by the caller).
pub fn discover(folders: &[String], cancel: &AtomicBool) -> Discovery {
    let mut discovery = Discovery::default();
    let mut seen = HashSet::new();
    for folder in folders {
        walk(Path::new(folder), 0, &mut discovery, &mut seen, cancel);
    }
    discovery
        .candidates
        .sort_by_key(|a| a.display.to_lowercase());
    discovery
}

fn walk(
    dir: &Path,
    depth: u32,
    discovery: &mut Discovery,
    seen: &mut HashSet<String>,
    cancel: &AtomicBool,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::debug!("Skipping unreadable plugin folder {}", dir.display());
        return;
    };
    for entry in entries.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            if has_extension(&path, "vst3") {
                bundle_candidate(&path, discovery, seen);
            } else {
                walk(&path, depth + 1, discovery, seen, cancel);
            }
        } else if has_extension(&path, "vst3") {
            file_candidate(&path, PluginFormat::Vst3, None, discovery, seen);
        } else if has_extension(&path, "dll") {
            file_candidate(&path, PluginFormat::Vst2, None, discovery, seen);
        }
    }
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn display_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// A `Name.vst3` folder: the binary lives at `Contents\<arch>\Name.vst3`.
fn bundle_candidate(bundle: &Path, discovery: &mut Discovery, seen: &mut HashSet<String>) {
    let Some(file_name) = bundle.file_name() else {
        return;
    };
    let binary = bundle.join("Contents").join(ARCH_DIR).join(file_name);
    if binary.is_file() {
        file_candidate(
            &binary,
            PluginFormat::Vst3,
            Some(bundle.to_path_buf()),
            discovery,
            seen,
        );
        return;
    }
    // No binary for our architecture; if any other architecture folder has
    // one, count the bundle as skipped rather than pretending it isn't there.
    let contents = bundle.join("Contents");
    let Ok(entries) = std::fs::read_dir(&contents) else {
        log::debug!("VST3 bundle {} has no Contents folder", bundle.display());
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.is_dir()
            && dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-win"))
            && dir.join(file_name).is_file()
        {
            discovery.skipped_other_arch += 1;
            return;
        }
    }
}

fn file_candidate(
    path: &Path,
    format: PluginFormat,
    bundle: Option<PathBuf>,
    discovery: &mut Discovery,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(path.to_string_lossy().to_lowercase()) {
        return;
    }
    let info = match pe::inspect(path) {
        Ok(info) => info,
        Err(e) => {
            log::debug!("Ignoring {}: {e}", path.display());
            return;
        }
    };
    let required: &[&str] = match format {
        // Old VST2 plugins export "main" instead of "VSTPluginMain".
        PluginFormat::Vst2 => &["VSTPluginMain", "main"],
        PluginFormat::Vst3 => &["GetPluginFactory"],
    };
    if !info.exports_any(required) {
        // Just some DLL living in a plugin folder; not a plugin at all.
        return;
    }
    if info.machine != pe::native_machine() {
        discovery.skipped_other_arch += 1;
        return;
    }
    discovery.candidates.push(Candidate {
        format,
        path: path.to_path_buf(),
        display: display_of(bundle.as_deref().unwrap_or(path)),
        bundle,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn missing_folder_is_ignored() {
        let discovery = discover(
            &["C:\\definitely\\not\\a\\real\\folder".to_string()],
            &no_cancel(),
        );
        assert!(discovery.candidates.is_empty());
        assert_eq!(discovery.skipped_other_arch, 0);
    }

    #[test]
    fn plain_dlls_are_not_candidates() {
        // System32 is full of DLLs, none of which export VSTPluginMain.
        // Walking a small known subfolder keeps the test fast.
        let dir = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32\\Speech");
        let discovery = discover(&[dir.to_string_lossy().to_string()], &no_cancel());
        assert!(
            discovery
                .candidates
                .iter()
                .all(|c| c.format != PluginFormat::Vst2),
            "no system DLL should be misidentified as a VST2 plugin"
        );
    }

    /// Prints what discovery finds in the standard folders on this machine.
    #[test]
    #[ignore]
    fn scan_real_folders() {
        let folders = super::super::default_folders();
        println!("folders: {folders:#?}");
        let discovery = discover(&folders, &no_cancel());
        for c in &discovery.candidates {
            println!("{:?} {} ({})", c.format, c.display, c.path.display());
        }
        println!(
            "{} candidates, {} skipped (other arch)",
            discovery.candidates.len(),
            discovery.skipped_other_arch
        );
    }
}
