//! WASAPI device discovery for the source-edit UI and capture setup, plus the
//! process lookup that binds an Application source to a running executable.

use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use wasapi::{DeviceEnumerator, DeviceState, Direction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// Must be called once on every thread that touches WASAPI.
pub fn ensure_com_initialized() {
    // Ignore "already initialized" results.
    let _ = wasapi::initialize_mta();
}

/// Lists active capture devices (microphones).
pub fn capture_devices() -> Vec<DeviceInfo> {
    ensure_com_initialized();
    let mut devices = Vec::new();
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return devices;
    };
    let Ok(collection) = enumerator.get_device_collection(&Direction::Capture) else {
        return devices;
    };
    for device in &collection {
        let Ok(device) = device else { continue };
        if let (Ok(id), Ok(name)) = (device.get_id(), device.get_friendlyname()) {
            devices.push(DeviceInfo { id, name });
        }
    }
    devices
}

/// Resolves a configured device id to a WASAPI device, or the default capture
/// device when `id` is `None`.
///
/// A configured device is never silently swapped for a different one: going on
/// air through the laptop's built-in microphone because the good one was a
/// moment late to enumerate is worse than going on air silent. The caller
/// retries instead (see `capture::spawn`).
///
/// The state check is the point of this function. `IMMDeviceEnumerator::
/// GetDevice` happily returns endpoints in *any* state, including `Unplugged`
/// and `NotPresent`, and `Activate` on one of those fails with a bare
/// `ERROR_FILE_NOT_FOUND` that reads like a bug in us. USB interfaces sit in
/// exactly that state for a moment while Windows brings them up, which is a
/// window Pubsplash lands in because it opens its sources within a few hundred
/// milliseconds of launch. Rejecting a non-`Active` endpoint here turns that
/// into an accurate, retryable answer.
pub fn capture_device(id: Option<&str>) -> Result<wasapi::Device, String> {
    ensure_com_initialized();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let Some(id) = id else {
        return enumerator
            .get_default_device(&Direction::Capture)
            .map_err(|e| format!("opening the default microphone: {e}"));
    };
    let device = enumerator
        .get_device(id)
        .map_err(|e| format!("looking up the configured microphone: {e}"))?;
    match device.get_state() {
        Ok(DeviceState::Active) => Ok(device),
        Ok(state) => Err(format!("the configured microphone is {state:?}")),
        Err(e) => Err(format!("reading the configured microphone's state: {e}")),
    }
}

/// The default render device (kept for future monitoring output).
#[allow(dead_code)]
pub fn default_render_device() -> Result<wasapi::Device, String> {
    ensure_com_initialized();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| e.to_string())
}

/// A running process matched to an Application source's configured name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProcess {
    pub pid: u32,
    /// The executable's file name as the OS reports it, e.g. `nvda.exe`.
    pub exe: String,
    /// The product name from the executable's version resource (e.g. `NVDA`),
    /// falling back to `exe` when the file carries no description.
    pub display_name: String,
}

/// Matches an Application source's configured name against a process name:
/// case-insensitive, with or without the `.exe` suffix.
fn matches(configured: &str, process_name: &str) -> bool {
    let wanted = configured.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return false;
    }
    let actual = process_name.to_ascii_lowercase();
    actual == wanted || actual == format!("{wanted}.exe")
}

/// Resolves every configured Application name in one process enumeration.
/// The returned map is keyed by the configured name, lowercased and trimmed;
/// names that are blank or not currently running are simply absent.
pub fn resolve_apps(names: &[String]) -> HashMap<String, AppProcess> {
    let mut found = HashMap::new();
    let wanted: Vec<&String> = names.iter().filter(|n| !n.trim().is_empty()).collect();
    if wanted.is_empty() {
        return found;
    }
    // The `System` is kept between calls: this runs off the UI pump every
    // couple of seconds, and a fresh one would re-open every process to read
    // its exe path. Reusing it means `OnlyIfNotSet` really does skip processes
    // already seen, leaving only the enumeration itself.
    let system = SYSTEM.get_or_init(|| Mutex::new(sysinfo::System::new()));
    let Ok(mut system) = system.lock() else {
        return found;
    };
    // Only exe paths are needed, so skip the (much costlier) default refresh.
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    for (pid, process) in system.processes() {
        let process_name = process.name().to_string_lossy().to_string();
        for name in &wanted {
            let key = name.trim().to_ascii_lowercase();
            if found.contains_key(&key) || !matches(name, &process_name) {
                continue;
            }
            let display_name = process
                .exe()
                .and_then(friendly_name)
                .unwrap_or_else(|| process_name.clone());
            found.insert(
                key,
                AppProcess {
                    pid: pid.as_u32(),
                    exe: process_name.clone(),
                    display_name,
                },
            );
        }
    }
    found
}

/// Finds the PID of a running process by executable name (case-insensitive,
/// with or without `.exe`).
pub fn find_process(name: &str) -> Option<u32> {
    let key = name.trim().to_ascii_lowercase();
    resolve_apps(std::slice::from_ref(&name.to_string()))
        .get(&key)
        .map(|app| app.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explorer is always running on a desktop Windows session and always
    /// carries a version resource, so it exercises the whole path: name match
    /// without the `.exe` the user may not have typed, plus the friendly name.
    #[test]
    fn resolves_a_running_process_to_its_friendly_name() {
        let apps = resolve_apps(&["explorer".to_string()]);
        let Some(app) = apps.get("explorer") else {
            // No desktop shell (a bare service session); nothing to assert.
            return;
        };
        assert_eq!(app.exe.to_ascii_lowercase(), "explorer.exe");
        assert!(app.pid != 0);
        assert_eq!(app.display_name, "Windows Explorer");
    }

    /// This runs on the UI thread every couple of seconds, so a repeat poll
    /// must not be something a screen-reader user would feel.
    #[test]
    fn repeat_polls_are_cheap() {
        let names = vec!["explorer".to_string()];
        resolve_apps(&names);
        let start = std::time::Instant::now();
        for _ in 0..5 {
            resolve_apps(&names);
        }
        let each = start.elapsed() / 5;
        assert!(each < std::time::Duration::from_millis(50), "{each:?} per poll");
    }

    #[test]
    fn a_process_that_is_not_running_is_absent() {
        assert!(
            resolve_apps(&["definitely-not-a-real-program".to_string()]).is_empty()
        );
    }
}

/// The process table, reused across refreshes so exe paths are read once per
/// process rather than once per poll.
static SYSTEM: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();

/// Cache of version-resource lookups, keyed by executable path. Parsing the
/// resource is comparatively expensive and the answer never changes for a
/// given file, but this runs on the UI pump every couple of seconds.
static DESCRIPTIONS: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();

/// The name users recognize for an executable, from its version resource.
/// `None` when the file has no version resource, which is common for console
/// tools and portable builds.
///
/// Neither of the two candidate fields wins outright: `nvda.exe` describes
/// itself as "NVDA application (has UIAccess)" but has product "NVDA", while
/// `explorer.exe` is the other way round ("Windows Explorer" against the
/// product "Microsoft® Windows® Operating System"). This name is spoken on
/// every visit to the strip, so take whichever is shorter.
fn friendly_name(path: &Path) -> Option<String> {
    let cache = DESCRIPTIONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(path)
    {
        return cached.clone();
    }
    let name = read_version_strings(path, &["FileDescription", "ProductName"])
        .into_iter()
        .flatten()
        .min_by_key(|s| s.chars().count());
    if let Ok(mut cache) = cache.lock() {
        cache.insert(path.to_path_buf(), name.clone());
    }
    name
}

/// Reads the named `\StringFileInfo\` entries from a file's version resource,
/// in the order requested. Entries that are missing or blank come back `None`.
fn read_version_strings(path: &Path, fields: &[&str]) -> Vec<Option<String>> {
    read_version_strings_inner(path, fields).unwrap_or_else(|| vec![None; fields.len()])
}

fn read_version_strings_inner(path: &Path, fields: &[&str]) -> Option<Vec<Option<String>>> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows_core::PCWSTR;

    /// The `\VarFileInfo\Translation` entry: which language/codepage the
    /// string table under `\StringFileInfo\` is filed under.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct LangCodepage {
        language: u16,
        codepage: u16,
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives every call below,
    // and `block` is sized by the API itself before it is filled.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size,
            block.as_mut_ptr() as *mut std::ffi::c_void,
        )
        .ok()?;

        let mut translations: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let key: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
        if !VerQueryValueW(
            block.as_ptr() as *const std::ffi::c_void,
            PCWSTR(key.as_ptr()),
            &mut translations,
            &mut len,
        )
        .as_bool()
            || len < std::mem::size_of::<LangCodepage>() as u32
        {
            return None;
        }
        let translation = *(translations as *const LangCodepage);

        let read = |field: &str| -> Option<String> {
            let sub_block: Vec<u16> = format!(
                "\\StringFileInfo\\{:04x}{:04x}\\{field}\0",
                translation.language, translation.codepage
            )
            .encode_utf16()
            .collect();
            let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut chars: u32 = 0;
            if !VerQueryValueW(
                block.as_ptr() as *const std::ffi::c_void,
                PCWSTR(sub_block.as_ptr()),
                &mut value,
                &mut chars,
            )
            .as_bool()
                || chars == 0
            {
                return None;
            }
            let text = std::slice::from_raw_parts(value as *const u16, chars as usize);
            let text = String::from_utf16_lossy(text);
            let text = text.trim_end_matches('\0').trim().to_string();
            (!text.is_empty()).then_some(text)
        };
        Some(fields.iter().map(|field| read(field)).collect())
    }
}
