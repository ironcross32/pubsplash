//! Configuration loading, saving, and corruption recovery.
//!
//! The config lives at `%LOCALAPPDATA%\pubsplash\config.json`. A missing file
//! is regenerated from defaults. A corrupt file is renamed to `config.json.bak`
//! and replaced with defaults so the app always starts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAIN_SITE_URL: &str = "https://audiopub.site/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub connection: ConnectionConfig,
    pub audio: AudioConfig,
    pub scenes: ScenesConfig,
    pub logging: LoggingConfig,
    pub plugins: PluginsConfig,
    pub buses: BusesConfig,
    pub archiving: ArchivingConfig,
    pub sounds: SoundsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connection: ConnectionConfig::default(),
            audio: AudioConfig::default(),
            scenes: ScenesConfig::default(),
            logging: LoggingConfig::default(),
            plugins: PluginsConfig::default(),
            buses: BusesConfig::default(),
            archiving: ArchivingConfig::default(),
            sounds: SoundsConfig::default(),
        }
    }
}

impl Config {
    /// Repairs routing after load or import: removes buses with duplicate or
    /// blank names (first occurrence wins), drops sends that reference a bus
    /// that no longer exists, and holds every un-boosted strip at 100 so a
    /// hand-edited (or downgraded) file can't leave a strip silently amplified.
    pub fn fix_up_routing(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.buses
            .buses
            .retain(|b| !b.name.trim().is_empty() && seen.insert(b.name.clone()));
        let names: std::collections::HashSet<String> =
            self.buses.buses.iter().map(|b| b.name.clone()).collect();
        for scene in &mut self.scenes.scenes {
            for source in &mut scene.sources {
                source.sends.retain(|s| names.contains(&s.bus));
                source.volume = clamp_volume(source.volume, source.boost);
            }
        }
        for bus in &mut self.buses.buses {
            bus.volume = clamp_volume(bus.volume, bus.boost);
        }
        self.audio.master_volume = clamp_volume(self.audio.master_volume, self.audio.master_boost);
    }
}

/// The ceiling for a strip's volume: 100 (unity) normally, or
/// [`crate::audio::mixer::MAX_VOLUME`] when the strip's boost is enabled.
pub fn max_volume(boost: bool) -> u32 {
    if boost {
        crate::audio::mixer::MAX_VOLUME
    } else {
        100
    }
}

fn clamp_volume(volume: u32, boost: bool) -> u32 {
    volume.min(max_volume(boost))
}

/// Mixing buses are global: sources in any scene can send to them, and every
/// bus outputs to master. The master output has its own FX chain here too.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct BusesConfig {
    pub buses: Vec<BusConfig>,
    /// FX chain applied to the master mix, after all sources and buses.
    pub master_chain: Vec<FxSlotConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BusConfig {
    pub name: String,
    /// 0-100, or 0-500 when `boost` is set.
    pub volume: u32,
    /// Whether this strip's volume may exceed 100 (up to 500) for make-up gain.
    pub boost: bool,
    pub muted: bool,
    /// The FX chain; list order is processing order.
    pub chain: Vec<FxSlotConfig>,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            volume: 100,
            boost: false,
            muted: false,
            chain: Vec::new(),
        }
    }
}

/// One plugin in an FX chain, with its saved state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct FxSlotConfig {
    pub plugin: PluginRef,
    pub bypass: bool,
    /// Base64 program chunk for plugins that support chunked state.
    pub chunk: Option<String>,
    /// Parameter snapshot for plugins without chunk support.
    pub params: Vec<ParamValue>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ParamValue {
    pub index: i32,
    /// Normalized 0..1.
    pub value: f32,
}

/// Identifies a plugin independently of this machine, so chains can be
/// shared. Resolved against the local plugin cache when applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct PluginRef {
    pub format: crate::vst::PluginFormat,
    /// Display name; also the last-resort match key.
    pub name: String,
    /// VST2 four-character unique id — the primary VST2 match key.
    pub unique_id: Option<i32>,
    /// VST3 class id (hex) — the primary VST3 match key.
    pub class_id: Option<String>,
    /// Last known path; used only to break ties between duplicates.
    pub path: String,
}

/// A per-source send into a bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SendConfig {
    /// Bus name (buses are referenced by name so reordering is safe).
    pub bus: String,
    /// How much of the source's post-fader signal to send, 0-100.
    pub level: u32,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            bus: String::new(),
            level: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluginsConfig {
    /// Folders scanned for VST plugins (Preferences > VST plugins).
    pub folders: Vec<String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            folders: crate::vst::default_folders(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ConnectionConfig {
    /// All known Audio Pub sites. The main site is always present.
    pub sites: Vec<SiteConfig>,
    /// URL of the site to auto-connect to on launch, if any.
    pub last_used_site: Option<String>,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            sites: vec![SiteConfig::main_site()],
            last_used_site: None,
        }
    }
}

impl ConnectionConfig {
    /// Guarantees the permanent main site entry exists (first in the list).
    pub fn ensure_main_site(&mut self) {
        if !self.sites.iter().any(|s| s.url == MAIN_SITE_URL) {
            self.sites.insert(0, SiteConfig::main_site());
        }
    }

    pub fn site(&self, url: &str) -> Option<&SiteConfig> {
        self.sites.iter().find(|s| s.url == url)
    }

    #[allow(dead_code)]
    pub fn site_mut(&mut self, url: &str) -> Option<&mut SiteConfig> {
        self.sites.iter_mut().find(|s| s.url == url)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SiteConfig {
    pub url: String,
    pub email: String,
    pub password: String,
}

impl SiteConfig {
    pub fn main_site() -> Self {
        Self {
            url: MAIN_SITE_URL.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StreamFormat {
    #[default]
    Mp3,
    Aac,
}

impl StreamFormat {
    pub fn display_name(self) -> &'static str {
        match self {
            StreamFormat::Mp3 => "MP3",
            StreamFormat::Aac => "AAC",
        }
    }
}

/// Archiving and local-recording preferences.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArchivingConfig {
    /// When set, the "Archive the stream" checkbox in the stream-info dialog
    /// starts checked on each fresh launch. Defaults to off.
    pub archive_streams_by_default: bool,
    /// When set, the "Record this stream" checkbox in the stream-info dialog
    /// starts checked on each fresh launch. Defaults to off.
    pub record_streams_by_default: bool,
    /// Folder that stream recordings are written to. Empty means "use the
    /// music library" (see `recording_dir`).
    pub recording_folder: String,
}

impl Default for ArchivingConfig {
    fn default() -> Self {
        Self {
            archive_streams_by_default: false,
            record_streams_by_default: false,
            recording_folder: default_recording_dir().to_string_lossy().into_owned(),
        }
    }
}

impl ArchivingConfig {
    /// Resolves the folder recordings are written to: the configured folder, or
    /// the music library if it is blank.
    pub fn recording_dir(&self) -> PathBuf {
        let trimmed = self.recording_folder.trim();
        if trimmed.is_empty() {
            default_recording_dir()
        } else {
            PathBuf::from(trimmed)
        }
    }
}

/// Sound-pack settings that are not tied to a scene or source. The interface
/// cues here play locally through `audio::cue` and never reach the stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoundsConfig {
    /// Whether the startup cue plays when Pubsplash launches.
    pub play_startup: bool,
    /// Whether the shut-down cue plays on exit. With it off, closing the
    /// window does not wait for a sound to finish.
    pub play_shutdown: bool,
}

impl Default for SoundsConfig {
    fn default() -> Self {
        Self {
            play_startup: true,
            play_shutdown: true,
        }
    }
}

/// The default recordings folder: the user's music library
/// (`%USERPROFILE%\Music`), falling back to the config dir if unavailable.
pub fn default_recording_dir() -> PathBuf {
    dirs::audio_dir().unwrap_or_else(config_dir)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioConfig {
    pub format: StreamFormat,
    /// Encoder bitrate in kbps.
    pub bitrate_kbps: u32,
    /// Master output volume, 0-100, or 0-500 when `master_boost` is set.
    pub master_volume: u32,
    /// Whether the master volume may exceed 100 (up to 500) for make-up gain.
    pub master_boost: bool,
    pub master_muted: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            format: StreamFormat::Mp3,
            bitrate_kbps: 128,
            master_volume: 100,
            master_boost: false,
            master_muted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScenesConfig {
    pub scenes: Vec<SceneConfig>,
    /// Name of the active scene.
    pub active_scene: String,
}

impl Default for ScenesConfig {
    fn default() -> Self {
        Self {
            scenes: vec![SceneConfig::default_scene()],
            active_scene: SceneConfig::DEFAULT_NAME.to_string(),
        }
    }
}

impl ScenesConfig {
    /// Guarantees the permanent default scene exists.
    pub fn ensure_default_scene(&mut self) {
        if !self.scenes.iter().any(|s| s.is_default) {
            self.scenes.insert(0, SceneConfig::default_scene());
        }
        if !self.scenes.iter().any(|s| s.name == self.active_scene) {
            self.active_scene = self
                .scenes
                .iter()
                .find(|s| s.is_default)
                .map(|s| s.name.clone())
                .unwrap_or_default();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SceneConfig {
    pub name: String,
    /// The permanent scene created on first launch. Cannot be deleted,
    /// but can be renamed.
    pub is_default: bool,
    pub sources: Vec<SourceConfig>,
}

impl SceneConfig {
    pub const DEFAULT_NAME: &'static str = "Default";

    pub fn default_scene() -> Self {
        Self {
            name: Self::DEFAULT_NAME.to_string(),
            is_default: true,
            sources: vec![
                SourceConfig {
                    name: "Microphone".to_string(),
                    kind: SourceKindConfig::Microphone { device_id: None },
                    ..Default::default()
                },
                SourceConfig {
                    name: "Text-to-Speech".to_string(),
                    kind: SourceKindConfig::Tts(TtsSourceConfig::default()),
                    ..Default::default()
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceConfig {
    pub name: String,
    /// 0-100, or 0-500 when `boost` is set.
    pub volume: u32,
    /// Whether this strip's volume may exceed 100 (up to 500) for make-up gain.
    pub boost: bool,
    pub muted: bool,
    pub kind: SourceKindConfig,
    /// Whether the source's signal goes directly to master. Off means it is
    /// heard only through its bus sends (insert-style routing).
    pub to_master: bool,
    pub sends: Vec<SendConfig>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            volume: 100,
            boost: false,
            muted: false,
            kind: SourceKindConfig::Microphone { device_id: None },
            to_master: true,
            sends: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceKindConfig {
    /// `device_id: None` means the system default capture device.
    Microphone {
        device_id: Option<String>,
    },
    DesktopAudio,
    Application {
        process_name: String,
    },
    Tts(TtsSourceConfig),
    SoundEvents(SoundEventsSourceConfig),
}

impl SourceKindConfig {
    pub fn type_display_name(&self) -> &'static str {
        match self {
            SourceKindConfig::Microphone { .. } => "Microphone",
            SourceKindConfig::DesktopAudio => "Desktop Audio",
            SourceKindConfig::Application { .. } => "Application",
            SourceKindConfig::Tts(_) => "Text-to-Speech",
            SourceKindConfig::SoundEvents(_) => "Sound Events",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TtsSourceConfig {
    pub engine: String,
    /// Engine-specific voice identifier; empty means the engine default.
    pub voice: String,
    /// 0-100.
    pub volume: u32,
    /// SAPI-style rate, -10..=10.
    pub rate: i32,
    /// Whether synthesized speech is mixed into the outgoing stream.
    pub output_to_stream: bool,
}

impl Default for TtsSourceConfig {
    fn default() -> Self {
        Self {
            engine: "sapi".to_string(),
            voice: String::new(),
            volume: 100,
            rate: 0,
            output_to_stream: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoggingConfig {
    /// One of: off, error, warn, info, debug, trace.
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

/// `%LOCALAPPDATA%\pubsplash`
pub fn config_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("LOCALAPPDATA should always exist on Windows")
        .join("pubsplash")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Loads the config, creating it from defaults if missing, and recovering
/// (rename to .bak, rewrite defaults) if corrupt.
pub fn load() -> Config {
    load_from(&config_path())
}

pub fn load_from(path: &PathBuf) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(mut config) => {
                config.connection.ensure_main_site();
                config.scenes.ensure_default_scene();
                config.fix_up_routing();
                config
            }
            Err(e) => {
                log::error!("Config file is corrupt ({e}); backing it up and using defaults");
                let backup = path.with_extension("json.bak");
                if let Err(e) = std::fs::rename(path, &backup) {
                    log::error!("Failed to back up corrupt config: {e}");
                }
                let config = Config::default();
                save_to(&config, path);
                config
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!("No config file found; creating one with defaults");
            let config = Config::default();
            save_to(&config, path);
            config
        }
        Err(e) => {
            log::error!("Failed to read config file: {e}; using defaults without saving");
            Config::default()
        }
    }
}

pub fn save(config: &Config) {
    save_to(config, &config_path());
}

pub fn save_to(config: &Config, path: &PathBuf) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::error!("Failed to create config directory: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = write_atomic(path, &json) {
                log::error!("Failed to write config file: {e}");
            }
        }
        Err(e) => log::error!("Failed to serialize config: {e}"),
    }
}

/// Writes `contents` to `path` via a sibling temp file and a rename, so an
/// interrupted write can never leave a half-written file behind. Config is
/// saved often enough (every slider tick, until the save is debounced) that a
/// truncated file is a real risk, and a truncated file reads as corrupt —
/// which costs the user every scene, source, bus and FX chain they have.
///
/// `rename` over an existing file is atomic on NTFS.
pub fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leaving the temp file around would make the next save fail too.
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
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
    fn missing_file_creates_defaults() {
        let path = temp_path("missing.json");
        let _ = std::fs::remove_file(&path);
        let config = load_from(&path);
        assert_eq!(config, Config::default());
        assert!(path.exists(), "default config should have been written");
    }

    #[test]
    fn an_interrupted_write_leaves_the_previous_file_intact() {
        // Config is saved often enough that a crash mid-write is a real risk,
        // and a truncated file reads as corrupt — which costs the user every
        // scene, source, bus and FX chain they have. `write_atomic` stages the
        // new contents in a sibling temp file, so a crash before the rename
        // leaves only that temp file behind.
        let path = temp_path("atomic.json");
        let temp = path.with_extension("tmp");
        let _ = std::fs::remove_file(&temp);
        let mut config = Config::default();
        config.audio.master_volume = 42;
        save_to(&config, &path);

        // Simulate the crash: the staged write happened, the rename did not.
        std::fs::write(&temp, "{ half-written").unwrap();
        assert_eq!(load_from(&path).audio.master_volume, 42);

        // And a completed write replaces the file and clears the staging area.
        config.audio.master_volume = 7;
        save_to(&config, &path);
        assert_eq!(load_from(&path).audio.master_volume, 7);
        assert!(!temp.exists(), "temp file should be renamed away, not left");
    }

    #[test]
    fn corrupt_file_is_backed_up_and_replaced() {
        let path = temp_path("corrupt.json");
        let backup = path.with_extension("json.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::write(&path, "{ this is not json").unwrap();

        let config = load_from(&path);
        assert_eq!(config, Config::default());
        assert!(backup.exists(), "corrupt file should be renamed to .bak");
        let rewritten: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(rewritten, Config::default());
    }

    #[test]
    fn roundtrip_preserves_settings() {
        let path = temp_path("roundtrip.json");
        let mut config = Config::default();
        config.audio.bitrate_kbps = 192;
        config.scenes.scenes.push(SceneConfig {
            name: "Music".into(),
            is_default: false,
            sources: vec![SourceConfig {
                name: "Desktop".into(),
                volume: 80,
                muted: true,
                kind: SourceKindConfig::DesktopAudio,
                ..Default::default()
            }],
        });
        save_to(&config, &path);
        assert_eq!(load_from(&path), config);
    }

    #[test]
    fn old_config_without_routing_fields_still_loads() {
        // A config saved before buses/sends existed: no `buses`, and sources
        // without `to_master`/`sends`. Everything must default sensibly.
        let path = temp_path("old_config.json");
        let json = r#"{
            "audio": { "bitrate_kbps": 192 },
            "scenes": {
                "scenes": [{
                    "name": "Default",
                    "is_default": true,
                    "sources": [{
                        "name": "Mic",
                        "volume": 90,
                        "muted": false,
                        "kind": { "type": "desktop_audio" }
                    }]
                }],
                "active_scene": "Default"
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        assert!(config.buses.buses.is_empty());
        let source = &config.scenes.scenes[0].sources[0];
        assert!(source.to_master, "to_master must default on");
        assert!(source.sends.is_empty());
        assert!(!source.boost, "volume boost must default off");
        assert!(!config.audio.master_boost, "master boost must default off");
        assert_eq!(config.audio.bitrate_kbps, 192);
    }

    #[test]
    fn old_config_without_sound_settings_still_loads() {
        // A config saved before the interface-sound toggles and the sound
        // events "to the stream" flag existed. All three default to on.
        let path = temp_path("old_sounds.json");
        let json = r#"{
            "scenes": {
                "scenes": [{
                    "name": "Default",
                    "is_default": true,
                    "sources": [{
                        "name": "Sound Events 1",
                        "kind": { "type": "sound_events", "pack_path": "C:\\p\\pack.pspack" }
                    }]
                }],
                "active_scene": "Default"
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        assert!(config.sounds.play_startup, "startup cue must default on");
        assert!(config.sounds.play_shutdown, "shutdown cue must default on");
        let SourceKindConfig::SoundEvents(settings) = &config.scenes.scenes[0].sources[0].kind
        else {
            panic!("expected a Sound Events source");
        };
        assert!(
            settings.output_to_stream,
            "sound events must default to reaching the stream"
        );
        assert_eq!(settings.pack_path, "C:\\p\\pack.pspack");
    }

    #[test]
    fn un_boosted_volumes_are_clamped_on_load() {
        // A file that claims a boosted volume without the boost flag (hand
        // edited, or written by a newer build and opened by an older one).
        let path = temp_path("stale_boost.json");
        let json = r#"{
            "audio": { "master_volume": 400 },
            "scenes": {
                "scenes": [{
                    "name": "Default",
                    "is_default": true,
                    "sources": [
                        { "name": "Loud", "volume": 350, "kind": { "type": "desktop_audio" } },
                        { "name": "Boosted", "volume": 350, "boost": true,
                          "kind": { "type": "desktop_audio" } }
                    ]
                }],
                "active_scene": "Default"
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        let sources = &config.scenes.scenes[0].sources;
        assert_eq!(
            sources[0].volume, 100,
            "un-boosted volume must clamp to 100"
        );
        assert_eq!(sources[1].volume, 350, "boosted volume must survive");
        assert_eq!(config.audio.master_volume, 100);
    }

    #[test]
    fn routing_roundtrip() {
        let path = temp_path("routing_roundtrip.json");
        let mut config = Config::default();
        config.buses.buses.push(BusConfig {
            name: "Voice FX".into(),
            volume: 90,
            boost: false,
            muted: false,
            chain: vec![FxSlotConfig {
                plugin: PluginRef {
                    format: crate::vst::PluginFormat::Vst2,
                    name: "Comp".into(),
                    unique_id: Some(0x434F4D50),
                    class_id: None,
                    path: "C:\\p\\comp.dll".into(),
                },
                bypass: false,
                chunk: Some("AAECAw==".into()),
                params: vec![ParamValue {
                    index: 3,
                    value: 0.25,
                }],
            }],
        });
        config.scenes.scenes[0].sources[0].to_master = false;
        config.scenes.scenes[0].sources[0].sends.push(SendConfig {
            bus: "Voice FX".into(),
            level: 65,
        });
        save_to(&config, &path);
        assert_eq!(load_from(&path), config);
    }

    #[test]
    fn main_site_is_restored_if_deleted() {
        let path = temp_path("nosite.json");
        let mut config = Config::default();
        config.connection.sites.clear();
        save_to(&config, &path);
        let loaded = load_from(&path);
        assert!(
            loaded
                .connection
                .sites
                .iter()
                .any(|s| s.url == MAIN_SITE_URL)
        );
    }
}

/// Settings for one Sound Events source. A source is deliberately independent:
/// two scenes may use different packs or react to different events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoundEventsSourceConfig {
    /// A `.pspack` file or a development pack directory containing
    /// `sound-pack.toml` and `sounds/`.
    ///
    /// Not read yet: every source plays the pack embedded in the executable.
    /// The field is kept, and round-tripped by the edit dialog, so a path set
    /// by an earlier build survives until pack selection lands on the
    /// Preferences "Sound packs" tab.
    pub pack_path: String,
    pub listener_increase: bool,
    pub listener_decrease: bool,
    pub listener_peak_increase: bool,
    pub incoming_chat: bool,
    pub outgoing_chat: bool,
    /// Whether these cues are mixed into the outgoing stream. With it off the
    /// cues play locally instead, so only the broadcaster hears them.
    pub output_to_stream: bool,
}

impl Default for SoundEventsSourceConfig {
    fn default() -> Self {
        Self {
            pack_path: String::new(),
            listener_increase: true,
            listener_decrease: true,
            listener_peak_increase: true,
            incoming_chat: true,
            outgoing_chat: true,
            output_to_stream: true,
        }
    }
}
