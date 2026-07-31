//! Persistent model and voice discovery for text-to-speech engines.

use super::engine::{TtsError, Voice};
use crate::config::SpeechConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    pub label: String,
}

impl CatalogModel {
    pub fn plain(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct CatalogVoice {
    pub id: String,
    pub label: String,
    pub model_ids: Vec<String>,
    pub styles: Vec<String>,
    pub roles: Vec<String>,
    pub language_code: String,
}

impl From<Voice> for CatalogVoice {
    fn from(voice: Voice) -> Self {
        Self {
            id: voice.id,
            label: voice.label,
            model_ids: voice.supported_engines,
            styles: voice.styles,
            roles: voice.roles,
            language_code: voice.language_code,
        }
    }
}

impl From<CatalogVoice> for Voice {
    fn from(voice: CatalogVoice) -> Self {
        Self {
            id: voice.id,
            label: voice.label,
            styles: voice.styles,
            roles: voice.roles,
            language_code: voice.language_code,
            supported_engines: voice.model_ids,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct EngineCatalog {
    pub models: Vec<CatalogModel>,
    pub voices: Vec<CatalogVoice>,
}

impl EngineCatalog {
    pub fn from_voices(models: Vec<CatalogModel>, voices: Vec<Voice>) -> Self {
        let mut result = Self {
            models,
            voices: voices.into_iter().map(Into::into).collect(),
        };
        result.normalize();
        result
    }

    fn normalize(&mut self) {
        for model in &mut self.models {
            model.id = model.id.trim().to_string();
            model.label = model.label.trim().to_string();
            if model.label.is_empty() {
                model.label = model.id.clone();
            }
        }
        self.models.retain(|model| !model.id.is_empty());
        self.models.sort_by(|a, b| {
            a.label
                .to_lowercase()
                .cmp(&b.label.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        self.models.dedup_by(|a, b| a.id == b.id);
        for voice in &mut self.voices {
            voice.id = voice.id.trim().to_string();
            voice.label = voice.label.trim().to_string();
            if voice.label.is_empty() {
                voice.label = voice.id.clone();
            }
            normalize_strings(&mut voice.model_ids);
            normalize_strings(&mut voice.styles);
            normalize_strings(&mut voice.roles);
            voice.language_code = voice.language_code.trim().to_string();
        }
        self.voices.retain(|voice| !voice.id.is_empty());
        self.voices.sort_by(|a, b| {
            a.label
                .to_lowercase()
                .cmp(&b.label.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        self.voices.dedup_by(|a, b| a.id == b.id);
    }
}

fn normalize_strings(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        *value = value.trim().to_string();
    }
    values.retain(|value| !value.is_empty());
    values.sort();
    values.dedup();
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TtsCatalog {
    pub schema_version: u32,
    pub engines: BTreeMap<String, EngineCatalog>,
}

impl Default for TtsCatalog {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            engines: BTreeMap::new(),
        }
    }
}

impl TtsCatalog {
    fn normalize(&mut self) {
        self.schema_version = SCHEMA_VERSION;
        for catalog in self.engines.values_mut() {
            catalog.normalize();
        }
        self.engines.retain(|engine, _| {
            matches!(
                engine.as_str(),
                super::engines::SAPI
                    | super::engines::EDGE
                    | super::engines::AWS
                    | super::engines::AZURE
                    | super::engines::ELEVENLABS
                    | super::engines::GOOGLE
                    | super::engines::OPENAI
            )
        });
    }
}

#[derive(Default)]
struct CatalogState {
    catalog: TtsCatalog,
    generation: u64,
}

static STATE: OnceLock<RwLock<CatalogState>> = OnceLock::new();

fn state() -> &'static RwLock<CatalogState> {
    STATE.get_or_init(|| RwLock::new(CatalogState::default()))
}

pub fn path() -> PathBuf {
    crate::config::config_dir().join("tts_catalog.json")
}

pub fn load() {
    install(load_from(&path()));
}

pub fn install(mut catalog: TtsCatalog) {
    catalog.normalize();
    if let Ok(mut state) = state().write() {
        state.catalog = catalog;
        state.generation = state.generation.wrapping_add(1);
    }
}

pub fn load_from(path: &Path) -> TtsCatalog {
    let mut catalog = match crate::json_store::load(path, "TTS catalog") {
        crate::json_store::Load::Ok(catalog) => catalog,
        crate::json_store::Load::Absent | crate::json_store::Load::Unreadable => {
            TtsCatalog::default()
        }
    };
    catalog.normalize();
    catalog
}

pub fn save_to(catalog: &TtsCatalog, path: &Path) {
    crate::json_store::save(catalog, path, "TTS catalog");
}

pub fn generation() -> u64 {
    state().read().map(|state| state.generation).unwrap_or(0)
}

pub fn models(engine: &str) -> Vec<CatalogModel> {
    let id = super::engines::resolve_id(engine);
    state()
        .read()
        .ok()
        .and_then(|state| {
            state
                .catalog
                .engines
                .get(id)
                .map(|entry| entry.models.clone())
        })
        .unwrap_or_default()
}

pub fn voices(engine: &str, model: &str) -> Option<Vec<Voice>> {
    let id = super::engines::resolve_id(engine);
    let state = state().read().ok()?;
    let entry = state.catalog.engines.get(id)?;
    let model = model.trim();
    Some(
        entry
            .voices
            .iter()
            .filter(|voice| {
                model.is_empty()
                    || voice.model_ids.is_empty()
                    || voice.model_ids.iter().any(|candidate| candidate == model)
            })
            .cloned()
            .map(Into::into)
            .collect(),
    )
}

/// The display label for one voice id, if the catalog knows it.
///
/// Deliberately not `voices(engine, "")` plus a `find`: that clones the whole
/// list, and an ElevenLabs account can hold thousands of voices. This is called
/// from [`crate::source_name::NameContext::build`], which runs on every arrow
/// key in the Scenes list and on the two-second application poll.
pub fn voice_label(engine: &str, voice_id: &str) -> Option<String> {
    let state = state().read().ok()?;
    voice_label_in(&state.catalog, engine, voice_id)
}

/// The catalog-independent half of [`voice_label`], so it can be tested without
/// writing the process-global catalog out from under another test.
fn voice_label_in(catalog: &TtsCatalog, engine: &str, voice_id: &str) -> Option<String> {
    catalog
        .engines
        .get(super::engines::resolve_id(engine))?
        .voices
        .iter()
        .find(|voice| voice.id == voice_id)
        .map(|voice| voice.label.clone())
}

pub fn commit_engine(engine: &str, mut entry: EngineCatalog) -> bool {
    entry.normalize();
    let id = super::engines::resolve_id(engine).to_string();
    let changed = {
        let Ok(mut state) = state().write() else {
            return false;
        };
        if state.catalog.engines.get(&id) == Some(&entry) {
            false
        } else {
            state.catalog.engines.insert(id, entry);
            state.catalog.normalize();
            state.generation = state.generation.wrapping_add(1);
            true
        }
    };
    if changed {
        if let Ok(state) = state().read() {
            save_to(&state.catalog, &path());
        }
    }
    changed
}

pub fn discover(engine: &str, speech: &SpeechConfig) -> Result<EngineCatalog, TtsError> {
    use super::engines;
    let mut result = match engines::resolve_id(engine) {
        engines::SAPI => EngineCatalog::from_voices(
            Vec::new(),
            super::sapi::voice_names()
                .into_iter()
                .map(Voice::plain)
                .collect(),
        ),
        engines::EDGE => EngineCatalog::from_voices(
            Vec::new(),
            engines::build(engines::EDGE, speech)
                .expect("Edge is registered")
                .voices()?,
        ),
        engines::OPENAI => engines::openai::discover(speech)?,
        engines::ELEVENLABS => engines::elevenlabs::discover(speech)?,
        engines::AZURE | engines::GOOGLE => EngineCatalog::from_voices(
            Vec::new(),
            engines::build(engine, speech)
                .expect("engine is registered")
                .voices()?,
        ),
        engines::AWS => engines::polly::discover(speech)?,
        _ => {
            return Err(TtsError::Other(
                "This engine has no discoverable catalog.".into(),
            ));
        }
    };
    result.normalize();
    Ok(result)
}

pub fn startup_engines(speech: &SpeechConfig) -> Vec<&'static str> {
    use super::engines;
    let mut selected = vec![engines::SAPI, engines::EDGE];
    if !speech.openai_api_key.as_str().trim().is_empty() {
        selected.push(engines::OPENAI);
    }
    if !speech.elevenlabs_api_key.as_str().trim().is_empty() {
        selected.push(engines::ELEVENLABS);
    }
    if !speech.azure_key.as_str().trim().is_empty() && !speech.azure_region.trim().is_empty() {
        selected.push(engines::AZURE);
    }
    if !speech.aws_access_key_id.trim().is_empty()
        && !speech.aws_secret_access_key.as_str().trim().is_empty()
        && !speech.aws_region.trim().is_empty()
    {
        selected.push(engines::AWS);
    }
    if !speech.google_api_key.as_str().trim().is_empty() {
        selected.push(engines::GOOGLE);
    }
    selected
}

#[derive(Debug)]
pub struct RefreshResult {
    pub engine: &'static str,
    pub result: Result<EngineCatalog, TtsError>,
}

pub fn start_refresh(speech: SpeechConfig, sender: crossbeam_channel::Sender<RefreshResult>) {
    let engines = startup_engines(&speech);
    let _ = std::thread::Builder::new()
        .name("tts-catalog-refresh".into())
        .spawn(move || {
            for engine in engines {
                let result = discover(engine, &speech);
                if sender.send(RefreshResult { engine, result }).is_err() {
                    break;
                }
                wxdragon::wake_up_idle();
            }
        });
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
    fn absent_and_corrupt_catalogs_are_empty() {
        let absent = temp_path("tts-catalog-absent.json");
        let _ = std::fs::remove_file(&absent);
        assert_eq!(load_from(&absent), TtsCatalog::default());
        let corrupt = temp_path("tts-catalog-corrupt.json");
        let backup = corrupt.with_extension("json.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::write(&corrupt, "{ broken").unwrap();
        assert_eq!(load_from(&corrupt), TtsCatalog::default());
        assert!(backup.exists());
    }

    #[test]
    fn catalog_roundtrips_normalized() {
        let path = temp_path("tts-catalog-roundtrip.json");
        let mut catalog = TtsCatalog::default();
        catalog.engines.insert(
            super::super::engines::OPENAI.into(),
            EngineCatalog {
                models: vec![CatalogModel::plain("tts-1"), CatalogModel::plain("tts-1")],
                voices: vec![CatalogVoice {
                    id: " alloy ".into(),
                    label: String::new(),
                    model_ids: vec!["tts-1".into(), "tts-1".into()],
                    ..Default::default()
                }],
            },
        );
        catalog.normalize();
        save_to(&catalog, &path);
        assert_eq!(load_from(&path), catalog);
        assert_eq!(catalog.engines["openai"].models.len(), 1);
        assert_eq!(catalog.engines["openai"].voices[0].label, "alloy");
    }

    #[test]
    fn interrupted_write_keeps_previous_catalog() {
        let path = temp_path("tts-catalog-atomic.json");
        let mut catalog = TtsCatalog::default();
        catalog
            .engines
            .insert(super::super::engines::SAPI.into(), EngineCatalog::default());
        save_to(&catalog, &path);
        std::fs::write(path.with_extension("tmp"), "{ half-written").unwrap();
        assert_eq!(load_from(&path), catalog);
    }

    /// The lookup behind the ElevenLabs source labels: a voice id the catalog
    /// has resolves to its name, and anything it does not know stays `None` so
    /// the label can leave the detail out rather than print an opaque key.
    #[test]
    fn voice_label_resolves_only_ids_the_catalog_holds() {
        use super::super::engines;
        let mut catalog = TtsCatalog::default();
        catalog.engines.insert(
            engines::ELEVENLABS.into(),
            EngineCatalog {
                models: Vec::new(),
                voices: vec![CatalogVoice {
                    id: "21m00Tcm4TlvDq8ikWAM".into(),
                    label: "Rachel".into(),
                    ..Default::default()
                }],
            },
        );
        assert_eq!(
            voice_label_in(&catalog, engines::ELEVENLABS, "21m00Tcm4TlvDq8ikWAM").as_deref(),
            Some("Rachel")
        );
        assert_eq!(
            voice_label_in(&catalog, engines::ELEVENLABS, "no-such-voice"),
            None
        );
        // An engine with no catalog entry at all, not just no matching voice.
        assert_eq!(
            voice_label_in(&catalog, engines::AZURE, "21m00Tcm4TlvDq8ikWAM"),
            None
        );
    }

    #[test]
    fn startup_refresh_requires_complete_credentials() {
        let mut speech = SpeechConfig::default();
        speech.aws_access_key_id = "id".into();
        assert!(!startup_engines(&speech).contains(&super::super::engines::AWS));
        speech.aws_secret_access_key = crate::secret::Secret::new("secret");
        assert!(startup_engines(&speech).contains(&super::super::engines::AWS));
        assert!(!startup_engines(&speech).contains(&super::super::engines::STAR));
        assert!(!startup_engines(&speech).contains(&super::super::engines::GTTS));
    }
}
