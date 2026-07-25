#![allow(dead_code)]
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use hound::{SampleFormat, WavReader};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAGIC: &[u8; 4] = b"PSSP";
const FORMAT_VERSION: u16 = 1;
const MANIFEST_FILE: &str = "sound-pack.toml";
const SOUNDS_DIR: &str = "sounds";

// This is obfuscation, not a security boundary. Pubsplash must be able to decrypt packs locally.
const PACK_KEY: [u8; 32] = *b"Pubsplash sound pack key v1!!!!!";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SoundKind {
    Startup,
    Shutdown,
    ListenerIncrease,
    ListenerDecrease,
    ListenerPeakIncrease,
    IncomingChat,
    OutgoingChat,
}

impl SoundKind {
    pub const INTERFACE: [Self; 2] = [Self::Startup, Self::Shutdown];
    pub const STREAM_EVENTS: [Self; 5] = [
        Self::ListenerIncrease,
        Self::ListenerDecrease,
        Self::ListenerPeakIncrease,
        Self::IncomingChat,
        Self::OutgoingChat,
    ];
    pub const ALL: [Self; 7] = [
        Self::Startup,
        Self::Shutdown,
        Self::ListenerIncrease,
        Self::ListenerDecrease,
        Self::ListenerPeakIncrease,
        Self::IncomingChat,
        Self::OutgoingChat,
    ];

    pub fn filename(self) -> &'static str {
        match self {
            Self::Startup => "ui_startup",
            Self::Shutdown => "ui_shutdown",
            Self::ListenerIncrease => "se_listener_increase",
            Self::ListenerDecrease => "se_listener_decrease",
            Self::ListenerPeakIncrease => "se_listener_peak_increase",
            Self::IncomingChat => "se_incoming_chat",
            Self::OutgoingChat => "se_outgoing_chat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Startup => "Startup",
            Self::Shutdown => "Shut down",
            Self::ListenerIncrease => "Listener count increased",
            Self::ListenerDecrease => "Listener count decreased",
            Self::ListenerPeakIncrease => "Listener peak increased",
            Self::IncomingChat => "Incoming chat message",
            Self::OutgoingChat => "Outgoing chat message",
        }
    }

    pub fn from_stream_event(event: StreamEvent) -> Self {
        match event {
            StreamEvent::ListenerIncrease => Self::ListenerIncrease,
            StreamEvent::ListenerDecrease => Self::ListenerDecrease,
            StreamEvent::ListenerPeakIncrease => Self::ListenerPeakIncrease,
            StreamEvent::IncomingChat => Self::IncomingChat,
            StreamEvent::OutgoingChat => Self::OutgoingChat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamEvent {
    ListenerIncrease,
    ListenerDecrease,
    ListenerPeakIncrease,
    IncomingChat,
    OutgoingChat,
}

impl StreamEvent {
    pub const ALL: [Self; 5] = [
        Self::ListenerIncrease,
        Self::ListenerDecrease,
        Self::ListenerPeakIncrease,
        Self::IncomingChat,
        Self::OutgoingChat,
    ];

    pub fn filename(self) -> &'static str {
        SoundKind::from_stream_event(self).filename()
    }

    pub fn label(self) -> &'static str {
        SoundKind::from_stream_event(self).label()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectManifest {
    pub pack_id: Uuid,
    pub revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PackIndex {
    pack_id: Uuid,
    revision: u64,
    assets: Vec<AssetIndex>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssetIndex {
    sound: SoundKind,
    bytes: usize,
}

pub struct LoadedPack {
    pub pack_id: Uuid,
    pub revision: u64,
    pub assets: HashMap<SoundKind, Vec<Vec<u8>>>,
}

impl LoadedPack {
    pub fn variants(&self, sound: SoundKind) -> Option<&[Vec<u8>]> {
        self.assets.get(&sound).map(Vec::as_slice)
    }
}

pub fn create_project(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path.join(SOUNDS_DIR)).map_err(|e| e.to_string())?;
    let manifest_path = path.join(MANIFEST_FILE);
    if !manifest_path.exists() {
        let manifest = ProjectManifest {
            pack_id: Uuid::new_v4(),
            revision: 0,
        };
        write_manifest(&manifest_path, &manifest)?;
    }
    Ok(())
}

pub fn read_project_manifest(project: &Path) -> Result<ProjectManifest, String> {
    let manifest_path = project.join(MANIFEST_FILE);
    let data = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("could not read {}: {e}", manifest_path.display()))?;
    toml::from_str(&data).map_err(|e| format!("invalid sound pack manifest: {e}"))
}

pub fn compile(project: &Path, output: &Path) -> Result<(), String> {
    let manifest = read_project_manifest(project)?;
    compile_with_revision(project, output, &manifest, manifest.revision)
}

pub fn compile_and_bump(project: &Path, output: &Path) -> Result<u64, String> {
    let mut manifest = read_project_manifest(project)?;
    let next_revision = manifest.revision.saturating_add(1);
    let temp_output = output.with_extension("pspack.tmp");

    compile_with_revision(project, &temp_output, &manifest, next_revision)?;
    manifest.revision = next_revision;
    write_manifest(&project.join(MANIFEST_FILE), &manifest)?;

    if output.exists() {
        fs::remove_file(output).map_err(|e| e.to_string())?;
    }
    fs::rename(&temp_output, output).map_err(|e| e.to_string())?;
    Ok(next_revision)
}

pub fn load(path: &Path) -> Result<LoadedPack, String> {
    if path.is_dir() {
        load_directory(path)
    } else {
        load_file(path)
    }
}

pub fn project_variants(project: &Path, sound: SoundKind) -> Result<Vec<PathBuf>, String> {
    let sounds = project.join(SOUNDS_DIR);
    if !sounds.exists() {
        return Ok(Vec::new());
    }
    let mut numbered: BTreeMap<u32, PathBuf> = BTreeMap::new();
    for entry in fs::read_dir(&sounds).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some((kind, variant)) = parse_filename(name) {
            if kind == sound {
                numbered.insert(variant, path);
            }
        }
    }
    Ok(numbered.into_values().collect())
}

pub fn add_variant(project: &Path, sound: SoundKind, source: &Path) -> Result<PathBuf, String> {
    create_project(project)?;
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    let source_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("selected file");
    validate_wav(&bytes, source_name)?;
    let next = project_variants(project, sound)?.len() + 1;
    let dest = project
        .join(SOUNDS_DIR)
        .join(format!("{}_{:02}.wav", sound.filename(), next));
    fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

pub fn remove_variant(
    project: &Path,
    sound: SoundKind,
    variant_index: usize,
) -> Result<(), String> {
    let variants = project_variants(project, sound)?;
    let Some(target) = variants.get(variant_index) else {
        return Err("no variant is selected".into());
    };
    fs::remove_file(target).map_err(|e| e.to_string())?;
    renumber_variants(project, sound)
}

pub fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut reader = WavReader::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.channels != 2 || spec.sample_rate != 48_000 {
        return Err("sound packs require 48 kHz stereo WAV files".into());
    }
    if spec.sample_format == SampleFormat::Float {
        reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    } else if spec.bits_per_sample <= 16 {
        let max = (1_i32 << (spec.bits_per_sample - 1)) as f32;
        reader
            .samples::<i16>()
            .map(|x| x.map(|n| n as f32 / max))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    } else {
        let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
        reader
            .samples::<i32>()
            .map(|x| x.map(|n| n as f32 / max))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

fn compile_with_revision(
    project: &Path,
    output: &Path,
    manifest: &ProjectManifest,
    revision: u64,
) -> Result<(), String> {
    let loaded = load_directory_with_revision(project, manifest, revision)?;
    if loaded.assets.is_empty() {
        return Err("add at least one WAV before compiling".into());
    }
    let mut index = PackIndex {
        pack_id: manifest.pack_id,
        revision,
        assets: Vec::new(),
    };
    let mut payload = Vec::new();
    for sound in SoundKind::ALL {
        if let Some(variants) = loaded.assets.get(&sound) {
            for bytes in variants {
                index.assets.push(AssetIndex {
                    sound,
                    bytes: bytes.len(),
                });
                payload.extend_from_slice(bytes);
            }
        }
    }
    let index_bytes = bincode::serialize(&index).map_err(|e| e.to_string())?;
    let mut plain = Vec::with_capacity(4 + index_bytes.len() + payload.len());
    plain.extend_from_slice(&(index_bytes.len() as u32).to_le_bytes());
    plain.extend_from_slice(&index_bytes);
    plain.extend_from_slice(&payload);

    let cipher = Aes256Gcm::new_from_slice(&PACK_KEY).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|_| "could not encrypt sound pack".to_string())?;

    let mut out = Vec::with_capacity(42 + encrypted.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(manifest.pack_id.as_bytes());
    out.extend_from_slice(&revision.to_le_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted);
    fs::write(output, out).map_err(|e| e.to_string())
}

fn load_file(path: &Path) -> Result<LoadedPack, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 42 || &data[..4] != MAGIC {
        return Err("not a Pubsplash sound pack".into());
    }
    let format = u16::from_le_bytes(data[4..6].try_into().unwrap());
    if format != FORMAT_VERSION {
        return Err(format!("unsupported sound pack format version {format}"));
    }
    let id = Uuid::from_slice(&data[6..22]).map_err(|e| e.to_string())?;
    let revision = u64::from_le_bytes(data[22..30].try_into().unwrap());
    let cipher = Aes256Gcm::new_from_slice(&PACK_KEY).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&data[30..42]), &data[42..])
        .map_err(|_| {
            "sound pack is corrupt or was encrypted for another Pubsplash version".to_string()
        })?;
    if plain.len() < 4 {
        return Err("sound pack has no manifest".into());
    }
    let index_len = u32::from_le_bytes(plain[..4].try_into().unwrap()) as usize;
    if plain.len() < 4 + index_len {
        return Err("sound pack manifest is truncated".into());
    }
    let index: PackIndex =
        bincode::deserialize(&plain[4..4 + index_len]).map_err(|e| e.to_string())?;
    if index.pack_id != id || index.revision != revision {
        return Err("sound pack header does not match its manifest".into());
    }
    let mut assets: HashMap<SoundKind, Vec<Vec<u8>>> = HashMap::new();
    let mut offset = 4 + index_len;
    for asset in index.assets {
        let end = offset
            .checked_add(asset.bytes)
            .ok_or("invalid asset length")?;
        if end > plain.len() {
            return Err("sound pack asset is truncated".into());
        }
        assets
            .entry(asset.sound)
            .or_default()
            .push(plain[offset..end].to_vec());
        offset = end;
    }
    Ok(LoadedPack {
        pack_id: id,
        revision,
        assets,
    })
}

fn load_directory(project: &Path) -> Result<LoadedPack, String> {
    let manifest = read_project_manifest(project)?;
    load_directory_with_revision(project, &manifest, manifest.revision)
}

fn load_directory_with_revision(
    project: &Path,
    manifest: &ProjectManifest,
    revision: u64,
) -> Result<LoadedPack, String> {
    let sounds = project.join(SOUNDS_DIR);
    let mut grouped: HashMap<SoundKind, BTreeMap<u32, Vec<u8>>> = HashMap::new();
    if sounds.exists() {
        for entry in fs::read_dir(&sounds).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some((sound, variant)) = parse_filename(name) {
                let bytes = fs::read(&path).map_err(|e| e.to_string())?;
                validate_wav(&bytes, name)?;
                grouped.entry(sound).or_default().insert(variant, bytes);
            }
        }
    }

    let mut assets = HashMap::new();
    for (sound, variants) in grouped {
        let max = *variants.keys().next_back().unwrap_or(&0);
        let mut ordered = Vec::new();
        for variant in 1..=max {
            let bytes = variants
                .get(&variant)
                .ok_or_else(|| format!("missing {}_{variant:02}.wav", sound.filename()))?;
            ordered.push(bytes.clone());
        }
        assets.insert(sound, ordered);
    }
    Ok(LoadedPack {
        pack_id: manifest.pack_id,
        revision,
        assets,
    })
}

fn parse_filename(name: &str) -> Option<(SoundKind, u32)> {
    let stem = name.strip_suffix(".wav")?;
    for sound in SoundKind::ALL {
        let base = sound.filename();
        if stem == base {
            return Some((sound, 1));
        }
        let Some(numbered) = stem.strip_prefix(&(base.to_string() + "_")) else {
            continue;
        };
        if !numbered.is_empty() && numbered.bytes().all(|b| b.is_ascii_digit()) {
            return numbered
                .parse()
                .ok()
                .filter(|n: &u32| *n > 0)
                .map(|n| (sound, n));
        }
    }
    None
}

fn validate_wav(bytes: &[u8], name: &str) -> Result<(), String> {
    let reader = WavReader::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{name} is not a readable WAV: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 2 || spec.sample_rate != 48_000 {
        return Err(format!("{name} must be 48 kHz stereo WAV"));
    }
    Ok(())
}

fn renumber_variants(project: &Path, sound: SoundKind) -> Result<(), String> {
    let variants = project_variants(project, sound)?;
    for (i, path) in variants.iter().enumerate() {
        let temp = path.with_extension(format!("renumber-{i}.tmp"));
        fs::rename(path, &temp).map_err(|e| e.to_string())?;
    }
    let temps = project_variants_temp(project, sound)?;
    for (i, temp) in temps.into_iter().enumerate() {
        let dest = project
            .join(SOUNDS_DIR)
            .join(format!("{}_{:02}.wav", sound.filename(), i + 1));
        fs::rename(temp, dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn project_variants_temp(project: &Path, sound: SoundKind) -> Result<Vec<PathBuf>, String> {
    let sounds = project.join(SOUNDS_DIR);
    let mut temps = Vec::new();
    if !sounds.exists() {
        return Ok(temps);
    }
    for entry in fs::read_dir(&sounds).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with(sound.filename()) && name.ends_with(".tmp") {
            temps.push(path);
        }
    }
    temps.sort();
    Ok(temps)
}

fn write_manifest(path: &Path, manifest: &ProjectManifest) -> Result<(), String> {
    let data = toml::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(path, data).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_parse() {
        assert_eq!(
            parse_filename("se_incoming_chat_05.wav"),
            Some((SoundKind::IncomingChat, 5))
        );
        assert_eq!(
            parse_filename("ui_startup.wav"),
            Some((SoundKind::Startup, 1))
        );
        assert_eq!(parse_filename("ui_focus.wav"), None);
    }
}
