//! VST plugin discovery, scanning, and the on-disk plugin cache.
//!
//! Actual plugin loading happens in the separate `pubsplash-scan` helper
//! binary (`src/bin/scan_helper.rs`), which shares `types.rs` (and the
//! loader code in `vst2.rs`/`vst3.rs`) via `#[path]` includes — those two
//! loader modules are *not* part of this module and never get compiled into
//! the main app.

pub mod discover;
pub mod host2;
mod moduleinfo;
mod pe;
pub mod scan;
mod types;

pub use types::*;

use std::path::{Path, PathBuf};

/// `%LOCALAPPDATA%\pubsplash\vst_plugins.json`, beside config.json.
pub fn cache_path() -> PathBuf {
    crate::config::config_dir().join("vst_plugins.json")
}

/// Loads the plugin cache. Missing or corrupt files yield an empty cache;
/// a corrupt file is backed up like a corrupt config would be.
pub fn load_cache() -> PluginCache {
    load_cache_from(&cache_path())
}

pub fn load_cache_from(path: &Path) -> PluginCache {
    match crate::json_store::load(path, "Plugin cache") {
        crate::json_store::Load::Ok(cache) => cache,
        // An empty cache is just an unscanned one; nothing to write back.
        _ => PluginCache::default(),
    }
}

pub fn save_cache(cache: &PluginCache) {
    save_cache_to(cache, &cache_path());
}

pub fn save_cache_to(cache: &PluginCache, path: &Path) {
    crate::json_store::save(cache, path, "plugin cache");
}

/// The standard Windows VST folders that exist on this machine, plus any
/// folder named by the `HKLM\SOFTWARE\VST\VSTPluginsPath` registry value
/// (and its 32-bit Wow6432Node twin). Used as the default folder list.
pub fn default_folders() -> Vec<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(pf) = std::env::var("ProgramFiles") {
        let pf = PathBuf::from(pf);
        for sub in [
            "Common Files\\VST",
            "Common Files\\VST2",
            "Common Files\\VST3",
            "Common Files\\Steinberg\\VST2",
            "Steinberg\\VSTPlugins",
        ] {
            candidates.push(pf.join(sub));
        }
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        let pf86 = PathBuf::from(pf86);
        for sub in ["Common Files\\VST3", "Steinberg\\VSTPlugins"] {
            candidates.push(pf86.join(sub));
        }
    }
    // Official user-level VST3 location.
    if let Some(local) = dirs::data_local_dir() {
        candidates.push(local.join("Programs\\Common\\VST3"));
    }
    for subkey in ["SOFTWARE\\VST", "SOFTWARE\\Wow6432Node\\VST"] {
        if let Some(path) = registry_string(subkey, "VSTPluginsPath") {
            candidates.push(PathBuf::from(path));
        }
    }

    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| seen.insert(p.to_lowercase()))
        .collect()
}

/// Reads a REG_SZ value under HKEY_LOCAL_MACHINE.
fn registry_string(subkey: &str, value: &str) -> Option<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::PCWSTR;

    let subkey: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let mut buf = [0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if result != ERROR_SUCCESS {
        return None;
    }
    // size is in bytes and includes the terminating null.
    let len = (size as usize / 2).saturating_sub(1);
    let text = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    let text = text.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("pubsplash-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn cache_roundtrip() {
        let path = temp_path("vst_cache_roundtrip.json");
        let cache = PluginCache {
            version: CACHE_VERSION,
            plugins: vec![PluginInfo {
                path: "C:\\Plugins\\Great Reverb.dll".into(),
                format: PluginFormat::Vst2,
                name: "Great Reverb".into(),
                vendor: "Example Audio".into(),
                version: "1.2".into(),
                unique_id: Some(0x47725276),
                class_ids: vec![],
                file_size: 123456,
                modified: 1_700_000_000,
            }],
            rejected: vec![RejectedEntry {
                path: "C:\\Plugins\\broken.dll".into(),
                file_size: 42,
                modified: 1_700_000_001,
                reason: "plugin crashed while loading".into(),
            }],
        };
        save_cache_to(&cache, &path);
        assert_eq!(load_cache_from(&path), cache);
    }

    #[test]
    fn corrupt_cache_is_backed_up_and_emptied() {
        let path = temp_path("vst_cache_corrupt.json");
        let backup = path.with_extension("json.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_cache_from(&path), PluginCache::default());
        assert!(backup.exists());
    }

    #[test]
    fn missing_cache_is_empty() {
        let path = temp_path("vst_cache_missing.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_cache_from(&path), PluginCache::default());
    }

    #[test]
    fn default_folders_exist_and_are_unique() {
        let folders = default_folders();
        let mut seen = std::collections::HashSet::new();
        for f in &folders {
            assert!(Path::new(f).is_dir(), "{f} should exist");
            assert!(seen.insert(f.to_lowercase()), "{f} listed twice");
        }
    }
}
