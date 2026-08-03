//! Configuration loading, saving, and corruption recovery.
//!
//! The config lives at `%LOCALAPPDATA%\pubsplash\config.json`. A missing file
//! is regenerated from defaults. A corrupt file is renamed to `config.json.bak`
//! and replaced with defaults so the app always starts.

use crate::secret::Secret;
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
    pub speech: SpeechConfig,
    pub keybinds: crate::keybind::KeybindsConfig,
    pub mastodon: MastodonConfig,
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
            speech: SpeechConfig::default(),
            keybinds: crate::keybind::KeybindsConfig::default(),
            mastodon: MastodonConfig::default(),
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

    /// Gives every text-to-speech source a saved section for the engine it is
    /// currently using. A source written before per-engine sections existed has
    /// only the flat fields, and this is what carries them across.
    pub fn fix_up_tts_profiles(&mut self) {
        for scene in &mut self.scenes.scenes {
            for source in &mut scene.sources {
                if let SourceKindConfig::Tts(tts) = &mut source.kind {
                    tts.fix_up();
                }
            }
        }
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
    /// VST3 parameter id. Older configs and VST2 snapshots leave this empty.
    pub id: Option<u32>,
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
    /// All known streaming service profiles. The main Audiopub site is always present.
    pub sites: Vec<SiteConfig>,
    /// Stable id of the service to auto-connect to on launch, if any.
    ///
    /// Older configs stored an Audiopub URL here; lookups still accept that.
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
        let main_index = self.sites.iter().position(SiteConfig::is_main);
        match main_index {
            Some(index) => {
                let mut main = self.sites.remove(index);
                main.repair_main_site();
                self.sites.insert(0, main);
            }
            None => self.sites.insert(0, SiteConfig::main_site()),
        }

        let mut used = std::collections::HashSet::new();
        for index in 0..self.sites.len() {
            if self.sites[index].is_main() {
                self.sites[index].repair_main_site();
            } else {
                self.sites[index].repair_defaults(index + 1);
            }
            if !used.insert(self.sites[index].id.clone()) {
                self.sites[index].id = format!("service-{}", index + 1);
                used.insert(self.sites[index].id.clone());
            }
        }
    }

    pub fn site(&self, id_or_url: &str) -> Option<&SiteConfig> {
        self.sites
            .iter()
            .find(|s| s.id == id_or_url || s.url == id_or_url)
    }

    #[allow(dead_code)]
    pub fn site_mut(&mut self, id_or_url: &str) -> Option<&mut SiteConfig> {
        self.sites
            .iter_mut()
            .find(|s| s.id == id_or_url || s.url == id_or_url)
    }

    pub fn next_service_id(&self) -> String {
        for n in 1.. {
            let id = format!("service-{n}");
            if self.sites.iter().all(|s| s.id != id) {
                return id;
            }
        }
        unreachable!()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamingServiceType {
    #[default]
    Audiopub,
    Icecast,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SiteConfig {
    pub id: String,
    pub nickname: String,
    pub service_type: StreamingServiceType,
    /// Audiopub site URL.
    pub url: String,
    /// Audiopub login email.
    pub email: String,
    /// Audiopub login password.
    pub password: Secret,
    /// Raw Icecast server host or address, without the port.
    pub icecast_server: String,
    pub icecast_port: u16,
    /// Raw Icecast mount point, with or without a leading slash.
    pub icecast_mount: String,
    pub icecast_username: String,
    pub icecast_password: Secret,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            nickname: String::new(),
            service_type: StreamingServiceType::Audiopub,
            url: String::new(),
            email: String::new(),
            password: Secret::default(),
            icecast_server: String::new(),
            icecast_port: 8000,
            icecast_mount: String::new(),
            icecast_username: "source".to_string(),
            icecast_password: Secret::default(),
        }
    }
}

impl SiteConfig {
    pub fn main_site() -> Self {
        Self {
            id: MAIN_SITE_URL.to_string(),
            nickname: "Audiopub".to_string(),
            service_type: StreamingServiceType::Audiopub,
            url: MAIN_SITE_URL.to_string(),
            ..Default::default()
        }
    }

    pub fn audiopub(id: String, nickname: String) -> Self {
        Self {
            id,
            nickname,
            service_type: StreamingServiceType::Audiopub,
            ..Default::default()
        }
    }

    pub fn icecast(id: String, nickname: String) -> Self {
        Self {
            id,
            nickname,
            service_type: StreamingServiceType::Icecast,
            ..Default::default()
        }
    }

    pub fn is_main(&self) -> bool {
        self.url == MAIN_SITE_URL
    }

    pub fn display_name(&self) -> String {
        let nickname = self.nickname.trim();
        if !nickname.is_empty() {
            return nickname.to_string();
        }
        match self.service_type {
            StreamingServiceType::Audiopub if !self.url.trim().is_empty() => self.url.clone(),
            StreamingServiceType::Icecast if !self.icecast_server.trim().is_empty() => {
                self.icecast_server.clone()
            }
            StreamingServiceType::Audiopub => "Audiopub service".to_string(),
            StreamingServiceType::Icecast => "Icecast service".to_string(),
        }
    }

    pub fn icecast_username(&self) -> String {
        let username = self.icecast_username.trim();
        if username.is_empty() {
            "source".to_string()
        } else {
            username.to_string()
        }
    }

    fn repair_main_site(&mut self) {
        self.id = MAIN_SITE_URL.to_string();
        self.nickname = "Audiopub".to_string();
        self.service_type = StreamingServiceType::Audiopub;
        self.url = MAIN_SITE_URL.to_string();
        self.repair_defaults(1);
    }

    fn repair_defaults(&mut self, ordinal: usize) {
        if self.id.trim().is_empty() {
            self.id = if !self.url.trim().is_empty() {
                self.url.clone()
            } else {
                format!("service-{ordinal}")
            };
        }
        if self.nickname.trim().is_empty() {
            self.nickname = self.display_name();
        }
        if self.icecast_port == 0 {
            self.icecast_port = 8000;
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

/// The linked Mastodon account and the announcement settings around it.
///
/// One account, not a list: announcing a stream is a single act, and a
/// per-account fan-out is a feature nobody has asked for. The credentials are
/// [`Secret`]s in both directions — the app's client secret and the access token
/// are as good as a password, and `Secret`'s hand-written `Debug` is what keeps
/// them out of the rotating log file that users are asked to share.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MastodonConfig {
    /// Base URL of the server, e.g. `https://mastodon.social`. Empty when no
    /// account is linked.
    pub instance: String,
    /// The OAuth app registered with `instance`. Re-registered on every
    /// authorization, because the redirect URI carries a loopback port.
    pub client_id: String,
    pub client_secret: Secret,
    pub access_token: Secret,
    /// `@user@host`, shown in Preferences so the user can see what is linked.
    pub account: String,
    /// Seeds the "Post to Mastodon when this stream starts" box in the Set
    /// stream info dialog. The per-stream box is what actually decides.
    pub post_on_start: bool,
    /// Seeds the "Post periodic still-streaming announcements" box.
    pub periodic: bool,
    /// How often a still-streaming post goes out, in minutes. One of
    /// `mastodon::INTERVALS`; `fix_up` snaps anything else to the nearest.
    pub interval_minutes: u32,
    pub templates: Vec<crate::mastodon::Template>,
    /// When the last post succeeded, as seconds since the Unix epoch. Kept
    /// alongside the in-memory stamp so the flood gate survives a restart being
    /// used to work around it.
    pub last_post_unix: u64,
}

impl Default for MastodonConfig {
    fn default() -> Self {
        Self {
            instance: String::new(),
            client_id: String::new(),
            client_secret: Secret::default(),
            access_token: Secret::default(),
            account: String::new(),
            post_on_start: false,
            periodic: false,
            interval_minutes: crate::mastodon::DEFAULT_INTERVAL_MINUTES,
            templates: Vec::new(),
            last_post_unix: 0,
        }
    }
}

impl MastodonConfig {
    /// True when there is an account to post as.
    pub fn is_linked(&self) -> bool {
        !self.instance.is_empty() && !self.access_token.is_empty()
    }

    /// Forgets the account. The server-side revoke is separate and best-effort;
    /// this is the part that must always happen.
    pub fn unlink(&mut self) {
        self.instance = String::new();
        self.client_id = String::new();
        self.client_secret = Secret::default();
        self.access_token = Secret::default();
        self.account = String::new();
    }

    /// Repairs a hand-edited or future-version file: drops templates that would
    /// not survive expansion, and snaps the interval to an offered value so the
    /// dropdown always has a selection.
    pub fn fix_up(&mut self) {
        self.interval_minutes = crate::mastodon::clamp_interval(self.interval_minutes);
        self.templates
            .retain(|template| match crate::mastodon::validate(&template.text) {
                Ok(()) => true,
                Err(error) => {
                    log::warn!(
                        "Dropping an unusable Mastodon template ({error}): {:?}",
                        template.text
                    );
                    false
                }
            });
        crate::mastodon::sort(&mut self.templates);
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
    /// The active sound pack: a file name (not a path) inside
    /// `soundpack::packs_dir()`. Empty means the pack built into the
    /// executable. One pack serves the interface cues and every Sound Events
    /// source; there is no per-source selection.
    pub pack: String,
}

impl Default for SoundsConfig {
    fn default() -> Self {
        Self {
            play_startup: true,
            play_shutdown: true,
            pack: String::new(),
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

/// Provider-specific behavior for one text-to-speech source.
///
/// The outer `Option` on [`TtsSourceConfig`] distinguishes an older source,
/// which still uses the legacy global model/engine/language settings, from a
/// source saved by the per-source settings dialog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "provider", content = "settings", rename_all = "snake_case")]
pub enum TtsEngineSettings {
    ElevenLabs(ElevenLabsTtsSettings),
    OpenAi(OpenAiTtsSettings),
    Azure(AzureTtsSettings),
    Google(GoogleTtsSettings),
    Polly(PollyTtsSettings),
    Gtts(GttsTtsSettings),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ElevenLabsTtsSettings {
    /// Empty means let ElevenLabs choose its default model.
    pub model: String,
    /// ISO 639-1 language code; empty omits the request field.
    pub language_code: String,
    pub stability: Option<f64>,
    pub similarity_boost: Option<f64>,
    pub style: Option<f64>,
    pub speaker_boost: Option<bool>,
    /// Play each chunk as it arrives rather than waiting for the whole
    /// utterance. On by default, and for a source saved before this existed:
    /// `#[serde(default)]` fills the missing field from `Default`, below.
    pub stream: bool,
}

impl Default for ElevenLabsTtsSettings {
    fn default() -> Self {
        Self {
            model: String::new(),
            language_code: String::new(),
            stability: None,
            similarity_boost: None,
            style: None,
            speaker_boost: None,
            stream: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OpenAiTtsSettings {
    pub model: String,
    /// Voice directions supported by GPT-4o mini TTS; empty is omitted.
    pub instructions: String,
}

impl Default for OpenAiTtsSettings {
    fn default() -> Self {
        Self {
            model: "gpt-4o-mini-tts".into(),
            instructions: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AzureTtsSettings {
    pub style: String,
    /// Azure accepts 0.01..=2.0 when a style is selected.
    pub style_degree: f64,
    pub role: String,
}

impl Default for AzureTtsSettings {
    fn default() -> Self {
        Self {
            style: String::new(),
            style_degree: 1.0,
            role: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GoogleTtsSettings {
    pub language_code: String,
    pub effects_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct PollyTtsSettings {
    /// Empty omits Engine and lets Polly use `standard`.
    pub engine: String,
    pub language_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GttsTtsSettings {
    /// The translate.google domain suffix, such as `co.uk`; empty means `com`.
    pub tld: String,
    /// None preserves the provider default, while Some selects normal/slow.
    pub slow: Option<bool>,
}

/// One engine's saved state within a text-to-speech source.
///
/// Every engine the user has configured keeps a section of its own, so moving a
/// source to another engine and back is lossless — a voice id, a model, a rate
/// mean nothing to the next engine, and before this they were simply discarded
/// when the source was saved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TtsEngineProfile {
    pub voice: String,
    /// 0-100.
    pub volume: u32,
    /// SAPI-style rate, -10..=10.
    pub rate: i32,
    /// -50..=50.
    pub pitch: i32,
    /// The provider-specific settings, or `None` for an engine that has none.
    pub settings: Option<TtsEngineSettings>,
}

impl Default for TtsEngineProfile {
    fn default() -> Self {
        Self {
            voice: String::new(),
            volume: 100,
            rate: 0,
            pitch: 0,
            settings: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TtsSourceConfig {
    /// A [`crate::tts::EngineId`] value; unknown strings fall back to SAPI.
    pub engine: String,
    /// Engine-specific voice identifier; empty means the engine default.
    pub voice: String,
    /// 0-100.
    pub volume: u32,
    /// SAPI-style rate, -10..=10. Network engines scale this to their own
    /// range — see `tts::engine::SynthRequest`.
    pub rate: i32,
    /// -50..=50, in whatever unit the engine uses. Engines without a pitch
    /// control ignore it; Polly applies it only with its standard engine.
    pub pitch: i32,
    /// Provider-specific voice behavior. `None` denotes a legacy source.
    pub provider_settings: Option<TtsEngineSettings>,
    /// Saved state per engine id, so an engine configured earlier comes back as
    /// it was left. The entry for `engine` mirrors the fields above, which stay
    /// the ones the running app reads — nothing outside the editing dialog
    /// needs to know about the other sections.
    pub engines: std::collections::BTreeMap<String, TtsEngineProfile>,
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
            pitch: 0,
            provider_settings: None,
            engines: std::collections::BTreeMap::new(),
            output_to_stream: true,
        }
    }
}

impl TtsSourceConfig {
    /// The flat fields as a profile: what the selected engine is set to.
    pub fn active_profile(&self) -> TtsEngineProfile {
        TtsEngineProfile {
            voice: self.voice.clone(),
            volume: self.volume,
            rate: self.rate,
            pitch: self.pitch,
            settings: self.provider_settings.clone(),
        }
    }

    /// Ensures the selected engine has a saved section, seeding it from the
    /// flat fields. A source saved before per-engine sections existed has none,
    /// and without this its settings would look like an engine never touched.
    pub fn fix_up(&mut self) {
        let engine = crate::tts::engines::resolve_id(&self.engine).to_string();
        if !self.engines.contains_key(&engine) {
            let profile = self.active_profile();
            self.engines.insert(engine, profile);
        }
    }
}

/// Credentials and limits shared by every text-to-speech source.
///
/// Credentials and endpoints are global so secrets are not repeated in every
/// scene. The model, language, engine mode, and expressive settings live on
/// each source; the three legacy fields remain as fallbacks for old configs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SpeechConfig {
    pub openai_api_key: Secret,
    pub elevenlabs_api_key: Secret,
    /// Legacy fallback for sources without per-source provider settings.
    pub elevenlabs_model: String,
    pub azure_key: Secret,
    pub azure_region: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: Secret,
    pub aws_region: String,
    /// Legacy fallback for sources without per-source provider settings.
    pub aws_engine: String,
    pub google_api_key: Secret,
    /// Legacy fallback for sources without per-source provider settings.
    pub google_language_code: String,
    /// WebSocket URL of a Star coagulator, e.g. `ws://localhost:4567`.
    pub star_host: String,
    /// Longest message a network engine will synthesize. Chat can carry a wall
    /// of text, and the paid engines bill by the character, so messages are
    /// truncated rather than sent whole.
    pub max_chars: usize,
    /// Floor on the gap between two network syntheses. A chat flood would
    /// otherwise be a burst of billed API calls.
    pub min_request_interval_ms: u64,
    /// Which engine's settings the Speech preferences tab was last showing.
    ///
    /// A UI convenience, not an engine setting: the tab shows one engine at a
    /// time, and a user who is midway through entering an ElevenLabs key should
    /// not have to find it again after closing the dialog. Empty (or unknown)
    /// resolves to SAPI like any other engine id.
    pub last_engine: String,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            openai_api_key: Secret::default(),
            elevenlabs_api_key: Secret::default(),
            elevenlabs_model: "eleven_multilingual_v2".into(),
            azure_key: Secret::default(),
            azure_region: String::new(),
            aws_access_key_id: String::new(),
            aws_secret_access_key: Secret::default(),
            aws_region: "us-east-1".into(),
            aws_engine: "neural".into(),
            google_api_key: Secret::default(),
            google_language_code: "en-US".into(),
            star_host: "ws://localhost:4567".into(),
            max_chars: Self::DEFAULT_MAX_CHARS,
            min_request_interval_ms: Self::DEFAULT_MIN_INTERVAL_MS,
            last_engine: String::new(),
        }
    }
}

impl SpeechConfig {
    pub const DEFAULT_MAX_CHARS: usize = 500;
    pub const DEFAULT_MIN_INTERVAL_MS: u64 = 750;

    /// The effective character cap; 0 in the file means "use the default".
    pub fn max_chars(&self) -> usize {
        if self.max_chars == 0 {
            Self::DEFAULT_MAX_CHARS
        } else {
            self.max_chars
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
    match crate::json_store::load::<Config>(path, "Config file") {
        crate::json_store::Load::Ok(mut config) => {
            config.connection.ensure_main_site();
            config.scenes.ensure_default_scene();
            config.fix_up_routing();
            config.fix_up_tts_profiles();
            config.keybinds.fix_up();
            config.mastodon.fix_up();
            config
        }
        // Missing, or corrupt and now renamed aside: either way the path is
        // free, so write the defaults the app is about to run on.
        crate::json_store::Load::Absent => {
            let config = Config::default();
            save_to(&config, path);
            config
        }
        crate::json_store::Load::Unreadable => Config::default(),
    }
}

pub fn save(config: &Config) {
    save_to(config, &config_path());
}

pub fn save_to(config: &Config, path: &PathBuf) {
    crate::json_store::save(config, path, "config file");
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
        // Loading gives the default text-to-speech source a section for the
        // engine it uses, so the expected value has to be repaired the same way.
        config.fix_up_tts_profiles();
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
    fn old_config_without_mastodon_loads_unlinked() {
        let path = temp_path("old_mastodon.json");
        std::fs::write(&path, r#"{ "audio": { "bitrate_kbps": 128 } }"#).unwrap();
        let config = load_from(&path);
        assert_eq!(config.mastodon, MastodonConfig::default());
        assert!(!config.mastodon.is_linked());
        assert!(!config.mastodon.post_on_start);
        assert_eq!(
            config.mastodon.interval_minutes,
            crate::mastodon::DEFAULT_INTERVAL_MINUTES,
            "an absent interval must still select a row in the dropdown"
        );
    }

    /// A hand-edited or downgraded file must not be able to leave the app with a
    /// template it cannot expand or an interval the dropdown cannot show.
    #[test]
    fn mastodon_settings_are_repaired_on_load() {
        let path = temp_path("mastodon_repair.json");
        let json = r#"{
            "mastodon": {
                "interval_minutes": 77,
                "templates": [
                    { "kind": "continuation", "text": "still going {url}" },
                    { "kind": "start", "text": "broken {nonsense}" },
                    { "kind": "start", "text": "live at {url}" }
                ]
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        assert_eq!(config.mastodon.interval_minutes, 90, "77 snaps to 90");
        // The unusable one is gone, and what is left is in list order.
        let rows: Vec<String> = config
            .mastodon
            .templates
            .iter()
            .map(|t| t.list_label())
            .collect();
        assert_eq!(
            rows,
            [
                "Start of stream: live at {url}",
                "Stream continuation: still going {url}",
            ]
        );
    }

    /// The access token is a `Secret`, so a plaintext one from a hand-edited
    /// file is taken as-is and written back encrypted.
    #[test]
    fn a_plaintext_mastodon_token_is_re_encrypted_on_save() {
        let path = temp_path("mastodon_token.json");
        let json = r#"{
            "mastodon": {
                "instance": "https://mastodon.social",
                "access_token": "plaintext-token",
                "client_secret": "plaintext-secret"
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        assert!(config.mastodon.is_linked());
        assert_eq!(config.mastodon.access_token.as_str(), "plaintext-token");
        save_to(&config, &path);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("plaintext-token") && !on_disk.contains("plaintext-secret"),
            "credentials must not survive a save in the clear"
        );
        assert_eq!(
            load_from(&path).mastodon.access_token.as_str(),
            "plaintext-token"
        );
        // And `Debug` must never print either of them: the log file is rotated
        // and users are asked to share it.
        let dumped = format!("{:?}", config.mastodon);
        assert!(!dumped.contains("plaintext"), "{dumped}");
    }

    #[test]
    fn unlinking_forgets_every_credential() {
        let mut config = MastodonConfig {
            instance: "https://mastodon.social".into(),
            client_id: "id".into(),
            client_secret: Secret::new("secret"),
            access_token: Secret::new("token"),
            account: "@me@mastodon.social".into(),
            post_on_start: true,
            ..MastodonConfig::default()
        };
        config.unlink();
        assert!(!config.is_linked());
        assert!(config.access_token.is_empty());
        assert!(config.client_secret.is_empty());
        assert!(config.client_id.is_empty());
        assert!(config.account.is_empty());
        // Preferences that are not credentials survive: unlinking is not a
        // reset of how the user wants announcements to work.
        assert!(config.post_on_start);
    }

    #[test]
    fn old_config_without_keybinds_gains_the_defaults() {
        // Upgrading from a build with no keybinds must hand the user F9 and F10
        // rather than nothing at all.
        let path = temp_path("old_keybinds.json");
        let json = r#"{ "audio": { "bitrate_kbps": 128 } }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        assert_eq!(
            config.keybinds,
            crate::keybind::KeybindsConfig::default(),
            "an absent keybinds key must fall back to the defaults"
        );
    }

    #[test]
    fn a_config_with_every_keybind_removed_stays_empty() {
        // The counterpart: `#[serde(default)]` fills an *absent* key, so a user
        // who deletes every binding must not have them handed back on restart.
        let path = temp_path("no_keybinds.json");
        let mut config = Config::default();
        config.keybinds.binds.clear();
        save_to(&config, &path);
        assert!(load_from(&path).keybinds.binds.is_empty());
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
        assert!(
            config.sounds.pack.is_empty(),
            "a config with no pack chosen must use the built-in one"
        );
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
                    id: None,
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
        // Loading gives the default text-to-speech source a section for the
        // engine it uses, so the expected value has to be repaired the same way.
        config.fix_up_tts_profiles();
        assert_eq!(load_from(&path), config);
    }

    /// Also the migration guarantee for credentials: the bare-string
    /// `"password"` below is what every settings file written before they were
    /// encrypted looks like, and it must still load (the next save re-writes it
    /// as `enc:`).
    #[test]
    fn old_audiopub_site_defaults_to_audiopub_with_nickname() {
        let path = temp_path("old_audiopub_site.json");
        let json = r#"{
            "connection": {
                "sites": [{
                    "url": "https://example.org/",
                    "email": "dj@example.org",
                    "password": "secret"
                }],
                "last_used_site": "https://example.org/"
            }
        }"#;
        std::fs::write(&path, json).unwrap();
        let config = load_from(&path);
        let service = config.connection.site("https://example.org/").unwrap();
        assert_eq!(service.service_type, StreamingServiceType::Audiopub);
        assert_eq!(service.nickname, "https://example.org/");
        assert_eq!(service.email, "dj@example.org");
        assert_eq!(service.password.as_str(), "secret");
    }

    #[test]
    fn main_site_is_repaired_if_edited() {
        let path = temp_path("main_site_repair.json");
        let mut config = Config::default();
        config.connection.sites[0].id = "changed".into();
        config.connection.sites[0].nickname = "Changed".into();
        config.connection.sites[0].service_type = StreamingServiceType::Icecast;
        config.connection.sites[0].icecast_server = "ice.example.org".into();
        save_to(&config, &path);
        let loaded = load_from(&path);
        let main = &loaded.connection.sites[0];
        assert_eq!(main.id, MAIN_SITE_URL);
        assert_eq!(main.nickname, "Audiopub");
        assert_eq!(main.service_type, StreamingServiceType::Audiopub);
        assert_eq!(main.url, MAIN_SITE_URL);
    }

    #[test]
    fn icecast_service_roundtrips() {
        let path = temp_path("icecast_service.json");
        let mut config = Config::default();
        config.connection.sites.push(SiteConfig {
            id: "icecast-1".into(),
            nickname: "Station".into(),
            service_type: StreamingServiceType::Icecast,
            icecast_server: "ice.example.org".into(),
            icecast_port: 9000,
            icecast_mount: "/live".into(),
            icecast_username: "dj".into(),
            icecast_password: Secret::new("secret"),
            ..Default::default()
        });
        save_to(&config, &path);
        let loaded = load_from(&path);
        let service = loaded.connection.site("icecast-1").unwrap();
        assert_eq!(service.service_type, StreamingServiceType::Icecast);
        assert_eq!(service.nickname, "Station");
        assert_eq!(service.icecast_server, "ice.example.org");
        assert_eq!(service.icecast_port, 9000);
        assert_eq!(service.icecast_mount, "/live");
        assert_eq!(service.icecast_username, "dj");
        assert_eq!(service.icecast_password.as_str(), "secret");
    }

    /// The settings file must never contain a site credential in the clear.
    ///
    /// Both passwords were plain `String`s for a long time while the API keys
    /// beside them were already `Secret`s, so this asserts against the written
    /// file rather than the type: it fails just the same if a future
    /// credential field is added to [`SiteConfig`] as a bare `String`.
    #[test]
    fn saved_site_credentials_are_never_written_in_the_clear() {
        let path = temp_path("site_credentials_encrypted.json");
        let mut config = Config::default();
        config.connection.sites.push(SiteConfig {
            id: "site-1".into(),
            nickname: "Station".into(),
            service_type: StreamingServiceType::Icecast,
            email: "dj@example.org".into(),
            password: Secret::new("hunter2-login"),
            icecast_server: "ice.example.org".into(),
            icecast_password: Secret::new("hunter2-source"),
            ..Default::default()
        });
        save_to(&config, &path);

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("hunter2-login") && !written.contains("hunter2-source"),
            "a site password leaked into config.json: {written}"
        );
        // The email is not a secret and must still be readable, so this is not
        // passing merely because nothing was written.
        assert!(written.contains("dj@example.org"));

        let service = load_from(&path).connection.site("site-1").unwrap().clone();
        assert_eq!(service.password.as_str(), "hunter2-login");
        assert_eq!(service.icecast_password.as_str(), "hunter2-source");
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

    #[test]
    fn legacy_tts_sources_load_without_provider_settings() {
        let source: TtsSourceConfig = serde_json::from_str(
            r#"{"engine":"elevenlabs","voice":"voice-id","volume":75,"rate":2,"pitch":0,"output_to_stream":true}"#,
        )
        .unwrap();
        assert_eq!(source.engine, "elevenlabs");
        assert_eq!(source.provider_settings, None);
    }

    /// An ElevenLabs source saved before streaming existed must come back with
    /// it on, not off — a missing `bool` would otherwise default to `false`.
    #[test]
    fn elevenlabs_settings_without_a_stream_field_default_to_streaming() {
        let source: TtsSourceConfig = serde_json::from_str(
            r#"{"engine":"elevenlabs","provider_settings":{"provider":"eleven_labs","settings":{"model":"eleven_flash_v2_5"}}}"#,
        )
        .unwrap();
        let Some(TtsEngineSettings::ElevenLabs(settings)) = source.provider_settings else {
            panic!(
                "expected ElevenLabs settings, got {:?}",
                source.provider_settings
            );
        };
        assert_eq!(settings.model, "eleven_flash_v2_5");
        assert!(settings.stream);
    }

    #[test]
    fn every_tts_provider_settings_variant_roundtrips() {
        let settings = vec![
            TtsEngineSettings::ElevenLabs(ElevenLabsTtsSettings {
                model: "eleven_v3".into(),
                language_code: "fr".into(),
                stability: Some(0.4),
                similarity_boost: Some(0.8),
                style: Some(0.2),
                speaker_boost: Some(true),
                stream: false,
            }),
            TtsEngineSettings::OpenAi(OpenAiTtsSettings {
                model: "gpt-4o-mini-tts".into(),
                instructions: "warm".into(),
            }),
            TtsEngineSettings::Azure(AzureTtsSettings {
                style: "cheerful".into(),
                style_degree: 1.2,
                role: "YoungAdultFemale".into(),
            }),
            TtsEngineSettings::Google(GoogleTtsSettings {
                language_code: "de-DE".into(),
                effects_profile: "headphone-class-device".into(),
            }),
            TtsEngineSettings::Polly(PollyTtsSettings {
                engine: "neural".into(),
                language_code: "en-GB".into(),
            }),
            TtsEngineSettings::Gtts(GttsTtsSettings {
                tld: "co.uk".into(),
                slow: Some(true),
            }),
        ];
        for provider_settings in settings {
            let source = TtsSourceConfig {
                provider_settings: Some(provider_settings),
                ..Default::default()
            };
            let json = serde_json::to_string(&source).unwrap();
            let loaded: TtsSourceConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(loaded, source);
        }
    }

    /// A source saved before per-engine sections existed keeps its settings:
    /// they become the section for the engine it is using, so switching away and
    /// back finds them again.
    #[test]
    fn a_source_without_engine_sections_gains_one_for_its_own_engine() {
        let mut source: TtsSourceConfig = serde_json::from_str(
            r#"{"engine":"elevenlabs","voice":"voice-id","volume":75,"rate":2,"pitch":-3,
                "provider_settings":{"provider":"eleven_labs","settings":{"model":"eleven_v3"}}}"#,
        )
        .unwrap();
        assert!(source.engines.is_empty());
        source.fix_up();
        let profile = source
            .engines
            .get("elevenlabs")
            .expect("elevenlabs section");
        assert_eq!(profile.voice, "voice-id");
        assert_eq!(profile.volume, 75);
        assert_eq!(profile.rate, 2);
        assert_eq!(profile.pitch, -3);
        assert_eq!(profile.settings, source.provider_settings);
    }

    /// An unknown engine id resolves to SAPI at runtime, so that is the section
    /// its settings belong in — not one named after an engine nothing can build.
    #[test]
    fn an_unknown_engine_is_filed_under_the_engine_it_resolves_to() {
        let mut source = TtsSourceConfig {
            engine: "no-such-engine".into(),
            voice: "Zira".into(),
            ..Default::default()
        };
        source.fix_up();
        assert_eq!(source.engines.len(), 1);
        assert_eq!(source.engines["sapi"].voice, "Zira");
    }

    #[test]
    fn fix_up_leaves_an_existing_section_alone() {
        let mut source = TtsSourceConfig {
            engine: "sapi".into(),
            voice: "David".into(),
            engines: [(
                "sapi".to_string(),
                TtsEngineProfile {
                    voice: "Zira".into(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        source.fix_up();
        assert_eq!(source.engines["sapi"].voice, "Zira");
    }

    #[test]
    fn engine_sections_roundtrip() {
        let source = TtsSourceConfig {
            engine: "azure".into(),
            engines: [
                (
                    "azure".to_string(),
                    TtsEngineProfile {
                        voice: "en-US-JennyNeural".into(),
                        volume: 80,
                        rate: 1,
                        pitch: 5,
                        settings: Some(TtsEngineSettings::Azure(AzureTtsSettings {
                            style: "cheerful".into(),
                            style_degree: 1.5,
                            role: String::new(),
                        })),
                    },
                ),
                (
                    "gtts".to_string(),
                    TtsEngineProfile {
                        settings: Some(TtsEngineSettings::Gtts(GttsTtsSettings {
                            tld: "co.uk".into(),
                            slow: Some(true),
                        })),
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let json = serde_json::to_string(&source).unwrap();
        let loaded: TtsSourceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, source);
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
    /// Whether these cues are mixed into the outgoing stream. They always play
    /// locally for the broadcaster; with this off, only the broadcaster hears
    /// them.
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
