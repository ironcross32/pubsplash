//! WASAPI device discovery for the source-edit UI and capture setup.

use wasapi::{DeviceEnumerator, Direction};

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

/// Resolves a configured device id to a WASAPI device, falling back to the
/// default capture device when `id` is `None` or no longer present.
pub fn capture_device(id: Option<&str>) -> Result<wasapi::Device, String> {
    ensure_com_initialized();
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    if let Some(id) = id {
        match enumerator.get_device(id) {
            Ok(device) => return Ok(device),
            Err(e) => log::warn!("Configured microphone {id:?} not found ({e}); using default"),
        }
    }
    enumerator
        .get_default_device(&Direction::Capture)
        .map_err(|e| e.to_string())
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

/// Finds the PID of a running process by executable name (case-insensitive,
/// with or without `.exe`).
pub fn find_process(name: &str) -> Option<u32> {
    use sysinfo::System;
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let wanted = name.to_ascii_lowercase();
    let wanted_exe = if wanted.ends_with(".exe") {
        wanted.clone()
    } else {
        format!("{wanted}.exe")
    };
    system.processes().iter().find_map(|(pid, process)| {
        let pname = process.name().to_string_lossy().to_ascii_lowercase();
        (pname == wanted || pname == wanted_exe).then(|| pid.as_u32())
    })
}
