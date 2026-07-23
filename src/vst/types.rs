//! Types shared between the main app and the `pubsplash-scan` helper binary
//! (which includes this file via `#[path]`). Keep this file free of
//! dependencies on the rest of the crate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    #[default]
    Vst2,
    Vst3,
}

impl PluginFormat {
    pub fn display_name(self) -> &'static str {
        match self {
            PluginFormat::Vst2 => "VST2",
            PluginFormat::Vst3 => "VST3",
        }
    }
}

/// One discovered, successfully scanned plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct PluginInfo {
    /// Path of the binary a host would load. For VST3 bundles this is the
    /// DLL inside the bundle's architecture folder, not the bundle root.
    pub path: String,
    pub format: PluginFormat,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// VST2 four-character unique id.
    pub unique_id: Option<i32>,
    /// VST3 class ids (hex) of the plugin's audio module classes.
    pub class_ids: Vec<String>,
    /// Size and mtime of the binary when scanned, for change detection.
    pub file_size: u64,
    pub modified: u64,
}

/// A file that was scanned and turned out not to be a usable plugin (or
/// crashed the scan helper). Remembered so "scan for new" skips it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct RejectedEntry {
    pub path: String,
    pub file_size: u64,
    pub modified: u64,
    pub reason: String,
}

/// The plugin cache persisted at `%LOCALAPPDATA%\pubsplash\vst_plugins.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluginCache {
    pub version: u32,
    pub plugins: Vec<PluginInfo>,
    pub rejected: Vec<RejectedEntry>,
}

pub const CACHE_VERSION: u32 = 1;

impl Default for PluginCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            plugins: Vec::new(),
            rejected: Vec::new(),
        }
    }
}

/// What the scan helper prints (as JSON) for one successfully loaded plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ScanOutput {
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub unique_id: Option<i32>,
    pub class_ids: Vec<String>,
}
