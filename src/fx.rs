//! The FX chain library: named plugin chains persisted in a single file
//! (`%LOCALAPPDATA%\pubsplash\fx_chains.json`), plus standalone `.pubfx`
//! export/import so chains can be shared across machines, and the resolution
//! logic that matches a chain's plugin references against the local plugin
//! cache.

use crate::config::{FxSlotConfig, PluginRef};
use crate::vst::{PluginCache, PluginFormat, PluginInfo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CHAIN_LIB_VERSION: u32 = 1;

/// All named chains, stored together in one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FxChainLibrary {
    pub version: u32,
    pub chains: Vec<NamedChain>,
}

impl Default for FxChainLibrary {
    fn default() -> Self {
        Self {
            version: CHAIN_LIB_VERSION,
            chains: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct NamedChain {
    pub name: String,
    pub slots: Vec<FxSlotConfig>,
}

/// The standalone export file format (`.pubfx`): one chain plus a version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ChainFile {
    pub version: u32,
    pub chain: NamedChain,
}

/// `%LOCALAPPDATA%\pubsplash\fx_chains.json`, beside config.json.
pub fn library_path() -> PathBuf {
    crate::config::config_dir().join("fx_chains.json")
}

/// Loads the chain library. Missing or corrupt files yield an empty library;
/// a corrupt file is backed up like a corrupt config would be.
pub fn load_library() -> FxChainLibrary {
    load_library_from(&library_path())
}

pub fn load_library_from(path: &Path) -> FxChainLibrary {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<FxChainLibrary>(&text) {
            Ok(library) => library,
            Err(e) => {
                log::error!("FX chain library is corrupt ({e}); backing it up and starting empty");
                let backup = path.with_extension("json.bak");
                if let Err(e) = std::fs::rename(path, &backup) {
                    log::error!("Failed to back up corrupt FX chain library: {e}");
                }
                FxChainLibrary::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FxChainLibrary::default(),
        Err(e) => {
            log::error!("Failed to read FX chain library: {e}");
            FxChainLibrary::default()
        }
    }
}

pub fn save_library(library: &FxChainLibrary) {
    save_library_to(library, &library_path());
}

pub fn save_library_to(library: &FxChainLibrary, path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::error!("Failed to create FX chain library directory: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(library) {
        Ok(json) => {
            if let Err(e) = crate::config::write_atomic(path, &json) {
                log::error!("Failed to write FX chain library: {e}");
            }
        }
        Err(e) => log::error!("Failed to serialize FX chain library: {e}"),
    }
}

/// Writes one chain as a standalone `.pubfx` file.
pub fn export_chain(chain: &NamedChain, path: &Path) -> Result<(), String> {
    let file = ChainFile {
        version: CHAIN_LIB_VERSION,
        chain: chain.clone(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("could not serialize the chain: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Reads a `.pubfx` file back into a chain.
pub fn import_chain(path: &Path) -> Result<NamedChain, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let file: ChainFile = serde_json::from_str(&text)
        .map_err(|e| format!("this is not a valid FX chain file: {e}"))?;
    if file.version > CHAIN_LIB_VERSION {
        return Err(format!(
            "this chain was exported by a newer version of Pubsplash (chain format {}, this version understands {})",
            file.version, CHAIN_LIB_VERSION
        ));
    }
    if file.chain.name.trim().is_empty() {
        return Err("the chain file has no chain name".to_string());
    }
    Ok(file.chain)
}

/// Returns `base` if it isn't taken among `existing`, else "base 2",
/// "base 3", ...
pub fn unique_chain_name(existing: &[NamedChain], base: &str) -> String {
    let base = base.trim();
    let taken = |name: &str| existing.iter().any(|c| c.name == name);
    if !taken(base) {
        return base.to_string();
    }
    for n in 2.. {
        let candidate = format!("{base} {n}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

impl PluginRef {
    pub fn from_info(info: &PluginInfo) -> PluginRef {
        PluginRef {
            format: info.format,
            name: info.name.clone(),
            unique_id: info.unique_id,
            class_id: info.class_ids.first().cloned(),
            path: info.path.clone(),
        }
    }

    /// Finds this plugin in the local cache. Primary key is the format's
    /// stable id (VST2 unique_id, VST3 class id); among several matches the
    /// same path wins, then the same name. Falls back to format + name when
    /// no id is stored.
    pub fn resolve<'a>(&self, cache: &'a PluginCache) -> Option<&'a PluginInfo> {
        let id_matches: Vec<&PluginInfo> = cache
            .plugins
            .iter()
            .filter(|p| p.format == self.format)
            .filter(|p| match self.format {
                PluginFormat::Vst2 => self.unique_id.is_some() && p.unique_id == self.unique_id,
                PluginFormat::Vst3 => match &self.class_id {
                    Some(id) => p.class_ids.iter().any(|c| c == id),
                    None => false,
                },
            })
            .collect();
        if !id_matches.is_empty() {
            return id_matches
                .iter()
                .find(|p| p.path.eq_ignore_ascii_case(&self.path))
                .or_else(|| id_matches.iter().find(|p| p.name == self.name))
                .copied()
                .or_else(|| id_matches.first().copied());
        }
        // No id stored (or nothing matched it): fall back to format + name.
        cache
            .plugins
            .iter()
            .filter(|p| p.format == self.format && p.name == self.name)
            .find(|p| p.path.eq_ignore_ascii_case(&self.path))
            .or_else(|| {
                cache.plugins.iter().find(|p| {
                    p.format == self.format && !self.name.is_empty() && p.name == self.name
                })
            })
    }

    /// "Name (VST2)" — how a plugin reference reads in dialogs and lists.
    pub fn display(&self) -> String {
        format!("{} ({})", self.name, self.format.display_name())
    }
}

/// The outcome of matching a chain against the local plugin cache.
pub struct ChainResolution {
    /// Slots whose plugin exists here, in original chain order.
    pub valid: Vec<FxSlotConfig>,
    /// References that could not be resolved, for the missing-plugin dialog.
    pub missing: Vec<PluginRef>,
}

pub fn resolve_chain(slots: &[FxSlotConfig], cache: &PluginCache) -> ChainResolution {
    let mut valid = Vec::new();
    let mut missing = Vec::new();
    for slot in slots {
        if slot.plugin.resolve(cache).is_some() {
            valid.push(slot.clone());
        } else {
            missing.push(slot.plugin.clone());
        }
    }
    ChainResolution { valid, missing }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("pubsplash-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn vst2_info(name: &str, id: i32, path: &str) -> PluginInfo {
        PluginInfo {
            path: path.into(),
            format: PluginFormat::Vst2,
            name: name.into(),
            unique_id: Some(id),
            ..Default::default()
        }
    }

    fn vst2_ref(name: &str, id: i32, path: &str) -> PluginRef {
        PluginRef {
            format: PluginFormat::Vst2,
            name: name.into(),
            unique_id: Some(id),
            class_id: None,
            path: path.into(),
        }
    }

    fn cache_with(plugins: Vec<PluginInfo>) -> PluginCache {
        PluginCache {
            plugins,
            ..Default::default()
        }
    }

    fn slot(plugin: PluginRef) -> FxSlotConfig {
        FxSlotConfig {
            plugin,
            ..Default::default()
        }
    }

    #[test]
    fn library_roundtrip() {
        let path = temp_path("fx_lib_roundtrip.json");
        let library = FxChainLibrary {
            version: CHAIN_LIB_VERSION,
            chains: vec![NamedChain {
                name: "Voice".into(),
                slots: vec![slot(vst2_ref("Comp", 1, "C:\\p\\comp.dll"))],
            }],
        };
        save_library_to(&library, &path);
        assert_eq!(load_library_from(&path), library);
    }

    #[test]
    fn corrupt_library_is_backed_up_and_emptied() {
        let path = temp_path("fx_lib_corrupt.json");
        let backup = path.with_extension("json.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_library_from(&path), FxChainLibrary::default());
        assert!(backup.exists());
    }

    #[test]
    fn missing_library_is_empty() {
        let path = temp_path("fx_lib_missing.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_library_from(&path), FxChainLibrary::default());
    }

    #[test]
    fn export_import_roundtrip() {
        let path = temp_path("chain.pubfx");
        let chain = NamedChain {
            name: "Stream limiter".into(),
            slots: vec![slot(vst2_ref("Limiter", 42, "C:\\p\\lim.dll"))],
        };
        export_chain(&chain, &path).unwrap();
        assert_eq!(import_chain(&path).unwrap(), chain);
    }

    #[test]
    fn import_rejects_garbage_and_newer_versions() {
        let path = temp_path("chain_bad.pubfx");
        std::fs::write(&path, "hello").unwrap();
        assert!(import_chain(&path).is_err());

        let newer = temp_path("chain_newer.pubfx");
        let mut file = ChainFile {
            version: CHAIN_LIB_VERSION + 1,
            chain: NamedChain {
                name: "X".into(),
                slots: vec![],
            },
        };
        std::fs::write(&newer, serde_json::to_string(&file).unwrap()).unwrap();
        assert!(import_chain(&newer).is_err());

        file.version = CHAIN_LIB_VERSION;
        file.chain.name = String::new();
        std::fs::write(&newer, serde_json::to_string(&file).unwrap()).unwrap();
        assert!(import_chain(&newer).is_err(), "blank chain name rejected");
    }

    #[test]
    fn unique_names_count_up() {
        let chains = vec![
            NamedChain {
                name: "Voice".into(),
                slots: vec![],
            },
            NamedChain {
                name: "Voice 2".into(),
                slots: vec![],
            },
        ];
        assert_eq!(unique_chain_name(&chains, "Music"), "Music");
        assert_eq!(unique_chain_name(&chains, "Voice"), "Voice 3");
    }

    #[test]
    fn resolve_prefers_id_then_path_then_name() {
        let cache = cache_with(vec![
            vst2_info("Comp", 1, "C:\\a\\comp.dll"),
            vst2_info("Comp Mk2", 1, "C:\\b\\comp2.dll"),
            vst2_info("Other", 2, "C:\\c\\other.dll"),
        ]);
        // Same id twice: path breaks the tie.
        let by_path = vst2_ref("whatever", 1, "C:\\B\\COMP2.DLL");
        assert_eq!(by_path.resolve(&cache).unwrap().name, "Comp Mk2");
        // Same id, unknown path: name breaks the tie.
        let by_name = vst2_ref("Comp", 1, "D:\\moved\\comp.dll");
        assert_eq!(by_name.resolve(&cache).unwrap().path, "C:\\a\\comp.dll");
        // No id: falls back to format + name.
        let mut no_id = vst2_ref("Other", 0, "D:\\gone.dll");
        no_id.unique_id = None;
        assert_eq!(no_id.resolve(&cache).unwrap().unique_id, Some(2));
        // Unknown entirely.
        let unknown = vst2_ref("Nope", 99, "D:\\nope.dll");
        assert!(unknown.resolve(&cache).is_none());
    }

    #[test]
    fn resolve_vst3_by_class_id() {
        let mut info = PluginInfo {
            format: PluginFormat::Vst3,
            name: "Verb".into(),
            path: "C:\\v\\verb.vst3".into(),
            ..Default::default()
        };
        info.class_ids = vec!["AABB".into()];
        let cache = cache_with(vec![info]);
        let vref = PluginRef {
            format: PluginFormat::Vst3,
            name: "Verb".into(),
            unique_id: None,
            class_id: Some("AABB".into()),
            path: "D:\\elsewhere.vst3".into(),
        };
        assert!(vref.resolve(&cache).is_some());
        let wrong = PluginRef {
            class_id: Some("CCDD".into()),
            name: "Different".into(),
            ..vref
        };
        assert!(wrong.resolve(&cache).is_none());
    }

    #[test]
    fn resolve_chain_splits_valid_and_missing() {
        let cache = cache_with(vec![vst2_info("Comp", 1, "C:\\a\\comp.dll")]);
        let slots = vec![
            slot(vst2_ref("Comp", 1, "C:\\a\\comp.dll")),
            slot(vst2_ref("Gone", 7, "C:\\x\\gone.dll")),
        ];
        let res = resolve_chain(&slots, &cache);
        assert_eq!(res.valid.len(), 1);
        assert_eq!(res.missing.len(), 1);
        assert_eq!(res.missing[0].name, "Gone");
    }
}
