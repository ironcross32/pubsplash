#![allow(dead_code)]
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use hound::{SampleFormat, WavReader};
use rand::{RngCore, rngs::OsRng};
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
const ENGINE_SAMPLE_RATE: u32 = 48_000;
const ENGINE_CHANNELS: usize = 2;
const EMBEDDED_DEFAULT_PACK: &[u8] = include_bytes!("../assets/sounds/default/default.pspack");

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
    #[serde(default = "default_pack_name")]
    pub name: String,
    pub pack_id: Uuid,
    pub revision: u64,
}

fn default_pack_name() -> String {
    "sound_pack".into()
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

pub fn sanitize_pack_name(name: &str) -> Result<String, String> {
    let mut sanitized = String::new();
    let mut previous_underscore = false;
    for ch in name.trim().chars() {
        let replacement = ch.is_whitespace()
            || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || ch.is_control();
        let ch = if replacement { '_' } else { ch };
        if ch == '_' {
            if previous_underscore {
                continue;
            }
            previous_underscore = true;
        } else {
            previous_underscore = false;
        }
        sanitized.push(ch);
    }
    let mut sanitized = sanitized.trim_matches('_').trim_matches('.').to_string();
    if sanitized.is_empty() {
        return Err("Enter a sound pack name.".into());
    }
    let upper = sanitized.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        sanitized.push('_');
    }
    Ok(sanitized)
}

pub fn create_named_project(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = sanitize_pack_name(name)?;
    let project = parent.join(&name);
    if project.exists() {
        return Err(format!(
            "{} already exists. Choose a different pack name or open the existing project.",
            project.display()
        ));
    }
    create_project_with_name(&project, &name)?;
    Ok(project)
}

pub fn create_project(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sound_pack");
    let name = sanitize_pack_name(name).unwrap_or_else(|_| "sound_pack".into());
    create_project_with_name(path, &name)
}

fn create_project_with_name(path: &Path, name: &str) -> Result<(), String> {
    fs::create_dir_all(path.join(SOUNDS_DIR)).map_err(|e| e.to_string())?;
    let manifest_path = path.join(MANIFEST_FILE);
    if !manifest_path.exists() {
        let manifest = ProjectManifest {
            name: name.to_string(),
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

pub fn load_embedded_default() -> Result<LoadedPack, String> {
    load_bytes(EMBEDDED_DEFAULT_PACK)
}

/// The embedded default pack, decrypted once for the life of the process.
/// Stream-event cues can fire in bursts (one per incoming chat message), so
/// this must not re-run AES over the whole pack per event. `None` means the
/// baked-in pack could not be read at all, which is logged once.
pub fn embedded_default() -> Option<&'static LoadedPack> {
    static CACHE: std::sync::OnceLock<Option<LoadedPack>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| match load_embedded_default() {
            Ok(pack) => Some(pack),
            Err(e) => {
                log::error!("Could not load the built-in sound pack: {e}");
                None
            }
        })
        .as_ref()
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

pub fn save_single_variants(
    project: &Path,
    assignments: &HashMap<SoundKind, PathBuf>,
) -> Result<usize, String> {
    create_project(project)?;
    let mut validated = Vec::new();
    for sound in SoundKind::ALL {
        let Some(source) = assignments.get(&sound) else {
            continue;
        };
        let bytes =
            fs::read(source).map_err(|e| format!("could not read {}: {e}", source.display()))?;
        let source_name = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("selected file");
        validate_wav(&bytes, source_name)?;
        validated.push((sound, bytes));
    }
    if validated.is_empty() {
        return Err("Choose at least one WAV before saving.".into());
    }

    for sound in SoundKind::ALL {
        for variant in project_variants(project, sound)? {
            fs::remove_file(&variant).map_err(|e| e.to_string())?;
        }
    }

    let sounds = project.join(SOUNDS_DIR);
    fs::create_dir_all(&sounds).map_err(|e| e.to_string())?;
    for (sound, bytes) in &validated {
        let dest = sounds.join(format!("{}_{:02}.wav", sound.filename(), 1));
        fs::write(dest, bytes).map_err(|e| e.to_string())?;
    }
    Ok(validated.len())
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
    let source_channels = usize::from(spec.channels);
    if source_channels == 0 {
        return Err("WAV files must have at least one channel".into());
    }
    if spec.sample_rate == 0 {
        return Err("WAV files must have a non-zero sample rate".into());
    }

    let samples = read_wav_samples(&mut reader, spec)?;
    let stereo = convert_to_stereo(&samples, source_channels)?;
    if spec.sample_rate == ENGINE_SAMPLE_RATE {
        Ok(stereo)
    } else {
        Ok(resample_stereo(
            &stereo,
            spec.sample_rate,
            ENGINE_SAMPLE_RATE,
        ))
    }
}

fn read_wav_samples<R: std::io::Read>(
    reader: &mut WavReader<R>,
    spec: hound::WavSpec,
) -> Result<Vec<f32>, String> {
    if spec.sample_format == SampleFormat::Float {
        reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    } else if spec.bits_per_sample <= 16 {
        if spec.bits_per_sample == 0 {
            return Err("integer WAV files must have at least one bit per sample".into());
        }
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

fn convert_to_stereo(samples: &[f32], source_channels: usize) -> Result<Vec<f32>, String> {
    if samples.len() % source_channels != 0 {
        return Err("WAV data ended in the middle of a frame".into());
    }

    let frames = samples.len() / source_channels;
    let mut stereo = Vec::with_capacity(frames * ENGINE_CHANNELS);
    for frame in samples.chunks_exact(source_channels) {
        stereo.push(frame[0]);
        stereo.push(if source_channels == 1 {
            frame[0]
        } else {
            frame[1]
        });
    }
    Ok(stereo)
}

fn resample_stereo(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let source_frames = samples.len() / ENGINE_CHANNELS;
    if source_frames <= 1 {
        return samples.to_vec();
    }

    let target_frames =
        ((source_frames as f64 * target_rate as f64 / source_rate as f64).round() as usize).max(1);
    let mut resampled = Vec::with_capacity(target_frames * ENGINE_CHANNELS);
    for target_frame in 0..target_frames {
        let source_pos = target_frame as f64 * source_rate as f64 / target_rate as f64;
        let left_frame = (source_pos.floor() as usize).min(source_frames - 1);
        let right_frame = (left_frame + 1).min(source_frames - 1);
        let fraction = (source_pos - left_frame as f64) as f32;

        for channel in 0..ENGINE_CHANNELS {
            let left = samples[left_frame * ENGINE_CHANNELS + channel];
            let right = samples[right_frame * ENGINE_CHANNELS + channel];
            resampled.push(left + (right - left) * fraction);
        }
    }
    resampled
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

pub fn load_bytes(data: &[u8]) -> Result<LoadedPack, String> {
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

fn load_file(path: &Path) -> Result<LoadedPack, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    load_bytes(&data)
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
    decode_wav(bytes)
        .map(|_| ())
        .map_err(|e| format!("{name} is not a readable WAV: {e}"))
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
    use hound::{WavSpec, WavWriter};

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

    #[test]
    fn pack_names_are_sanitized_for_project_folders() {
        assert_eq!(sanitize_pack_name("My Cool Pack").unwrap(), "My_Cool_Pack");
        assert_eq!(sanitize_pack_name(" bad:/name** ").unwrap(), "bad_name");
        assert_eq!(sanitize_pack_name("con").unwrap(), "con_");
        assert!(sanitize_pack_name("  ///  ").is_err());
    }

    #[test]
    fn old_project_manifests_get_default_name() {
        let manifest: ProjectManifest = toml::from_str(
            r#"
pack_id = "00000000-0000-0000-0000-000000000000"
revision = 2
"#,
        )
        .unwrap();
        assert_eq!(manifest.name, "sound_pack");
        assert_eq!(manifest.revision, 2);
    }

    #[test]
    fn mono_44_1_khz_wav_decodes_to_48_khz_stereo() {
        let bytes = test_wav_bytes(44_100, 1, 441);

        let samples = decode_wav(&bytes).unwrap();

        assert_eq!(samples.len(), 480 * ENGINE_CHANNELS);
        for frame in samples.chunks_exact(ENGINE_CHANNELS) {
            assert_eq!(frame[0], frame[1]);
        }
    }

    #[test]
    fn stereo_48_khz_wav_decodes_without_resampling() {
        let bytes = test_wav_bytes(ENGINE_SAMPLE_RATE, 2, 16);

        let samples = decode_wav(&bytes).unwrap();

        assert_eq!(samples.len(), 16 * ENGINE_CHANNELS);
    }

    #[test]
    fn save_single_variants_accepts_readable_non_48_khz_mono_wav() {
        let dir = test_dir("save-mono-wav");
        let project = dir.join("project");
        let source = dir.join("startup.wav");
        fs::write(&source, test_wav_bytes(44_100, 1, 441)).unwrap();

        let mut assignments = HashMap::new();
        assignments.insert(SoundKind::Startup, source);

        let saved = save_single_variants(&project, &assignments).unwrap();

        assert_eq!(saved, 1);
        assert!(project.join(SOUNDS_DIR).join("ui_startup_01.wav").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_bytes_matches_file_load_for_compiled_pack() {
        let dir = test_dir("load-bytes");
        let project = dir.join("project");
        let source = dir.join("startup.wav");
        fs::write(&source, test_wav_bytes(ENGINE_SAMPLE_RATE, 2, 16)).unwrap();

        let mut assignments = HashMap::new();
        assignments.insert(SoundKind::Startup, source);
        save_single_variants(&project, &assignments).unwrap();

        let output = dir.join("test.pspack");
        compile(&project, &output).unwrap();
        let from_file = load(&output).unwrap();
        let bytes = fs::read(&output).unwrap();
        let from_bytes = load_bytes(&bytes).unwrap();

        assert_eq!(from_bytes.pack_id, from_file.pack_id);
        assert_eq!(from_bytes.revision, from_file.revision);
        let loaded_lengths = asset_lengths(&from_bytes, SoundKind::Startup);
        assert!(!loaded_lengths.is_empty());
        assert_eq!(
            loaded_lengths,
            asset_lengths(&from_file, SoundKind::Startup)
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn embedded_default_pack_loads_interface_cues() {
        let pack = load_embedded_default().unwrap();

        for sound in [SoundKind::Startup, SoundKind::Shutdown] {
            let variants = pack.variants(sound).expect("interface cue variants");
            assert!(!variants.is_empty(), "{sound:?} should have a variant");
            decode_wav(&variants[0]).unwrap();
        }
    }

    fn asset_lengths(pack: &LoadedPack, sound: SoundKind) -> Vec<usize> {
        pack.variants(sound)
            .unwrap_or(&[])
            .iter()
            .map(Vec::len)
            .collect()
    }
    fn test_wav_bytes(sample_rate: u32, channels: u16, frames: usize) -> Vec<u8> {
        let dir = test_dir("wav-bytes");
        let path = dir.join("test.wav");
        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for frame in 0..frames {
            for channel in 0..usize::from(channels) {
                writer
                    .write_sample(((frame * 16 + channel) as i16).saturating_mul(8))
                    .unwrap();
            }
        }
        writer.finalize().unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::remove_dir_all(dir).ok();
        bytes
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
