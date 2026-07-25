//! Minimal VST3 module inspection: load the module, get its IPluginFactory,
//! and list the audio module classes. Hand-rolled COM-style vtable calls —
//! no Steinberg SDK bindings. Compiled only into the `pubsplash-scan` helper
//! process (via `#[path]` in `src/bin/scan_helper.rs`).

use super::types::ScanOutput;
use std::ffi::{CStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::core::{PCWSTR, s};

const K_RESULT_OK: i32 = 0;
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

type InitDllFn = unsafe extern "system" fn() -> bool;
type GetFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

#[repr(C)]
struct IPluginFactory {
    vtable: *const IPluginFactoryVtbl,
}

/// FUnknown's three methods followed by IPluginFactory's own.
#[repr(C)]
struct IPluginFactoryVtbl {
    query_interface:
        unsafe extern "system" fn(*mut IPluginFactory, *const c_void, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IPluginFactory) -> u32,
    release: unsafe extern "system" fn(*mut IPluginFactory) -> u32,
    get_factory_info: unsafe extern "system" fn(*mut IPluginFactory, *mut PFactoryInfo) -> i32,
    count_classes: unsafe extern "system" fn(*mut IPluginFactory) -> i32,
    get_class_info: unsafe extern "system" fn(*mut IPluginFactory, i32, *mut PClassInfo) -> i32,
    create_instance: *const c_void,
}

#[repr(C)]
struct PFactoryInfo {
    vendor: [u8; 64],
    url: [u8; 256],
    email: [u8; 128],
    flags: i32,
}

#[repr(C)]
struct PClassInfo {
    cid: [u8; 16],
    cardinality: i32,
    category: [u8; 32],
    name: [u8; 64],
}

fn fixed_string(buf: &[u8]) -> String {
    CStr::from_bytes_until_nul(buf)
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_else(|_| String::from_utf8_lossy(buf).trim().to_string())
}

fn cid_hex(cid: &[u8; 16]) -> String {
    cid.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn scan(path: &Path) -> Result<ScanOutput, String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        // LOAD_WITH_ALTERED_SEARCH_PATH: dependency DLLs shipped next to the
        // plugin must resolve from the plugin's folder, not the helper's.
        let module = LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
            .map_err(|e| format!("could not load module: {e}"))?;

        if let Some(init) = GetProcAddress(module, s!("InitDll")) {
            let init: InitDllFn = std::mem::transmute(init);
            if !init() {
                return Err("InitDll returned false".to_string());
            }
        }

        let get_factory =
            GetProcAddress(module, s!("GetPluginFactory")).ok_or("no GetPluginFactory export")?;
        let get_factory: GetFactoryFn = std::mem::transmute(get_factory);
        let factory = get_factory();
        if factory.is_null() {
            return Err("GetPluginFactory returned null".to_string());
        }
        let vtbl = &*(*factory).vtable;

        let mut factory_info: PFactoryInfo = std::mem::zeroed();
        let vendor = if (vtbl.get_factory_info)(factory, &mut factory_info) == K_RESULT_OK {
            fixed_string(&factory_info.vendor)
        } else {
            String::new()
        };

        let mut name = String::new();
        let mut class_ids = Vec::new();
        let count = (vtbl.count_classes)(factory);
        for index in 0..count {
            let mut class_info: PClassInfo = std::mem::zeroed();
            if (vtbl.get_class_info)(factory, index, &mut class_info) != K_RESULT_OK {
                continue;
            }
            if fixed_string(&class_info.category) != AUDIO_MODULE_CLASS {
                continue;
            }
            class_ids.push(cid_hex(&class_info.cid));
            if name.is_empty() {
                name = fixed_string(&class_info.name);
            }
        }
        (vtbl.release)(factory);
        // ExitDll is deliberately not called: some plugins misbehave when torn
        // down, and this process exits immediately anyway.

        if class_ids.is_empty() {
            return Err("no audio module classes".to_string());
        }
        if name.is_empty() {
            name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
        }
        Ok(ScanOutput {
            name,
            vendor,
            version: String::new(),
            unique_id: None,
            class_ids,
        })
    }
}
