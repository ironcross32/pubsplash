//! Minimal hand-rolled VST2 hosting ABI — just enough to load a plugin and
//! ask for its name, vendor, and unique id. Compiled only into the
//! `pubsplash-scan` helper process (via `#[path]` in `src/bin/scan_helper.rs`);
//! the main app never loads plugin DLLs.

use super::types::ScanOutput;
use std::ffi::{CStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::core::{PCWSTR, s};

/// AEffect.magic — 'VstP'.
const K_EFFECT_MAGIC: i32 = 0x5673_7450;

// The nominal setup reported to plugins while probing (nothing is processed).
const SCAN_SAMPLE_RATE: f32 = 48_000.0;
const SCAN_BLOCK_SIZE: isize = 512;

// Plugin dispatcher opcodes.
const EFF_OPEN: i32 = 0;
const EFF_CLOSE: i32 = 1;
const EFF_SET_SAMPLE_RATE: i32 = 10;
const EFF_SET_BLOCK_SIZE: i32 = 11;
const EFF_GET_EFFECT_NAME: i32 = 45;
const EFF_GET_VENDOR_STRING: i32 = 47;
const EFF_GET_VENDOR_VERSION: i32 = 49;

// Host callback opcodes.
const AUDIO_MASTER_VERSION: i32 = 1;
const AUDIO_MASTER_WANT_MIDI: i32 = 6;
const AUDIO_MASTER_GET_TIME: i32 = 7;
const AUDIO_MASTER_GET_SAMPLE_RATE: i32 = 16;
const AUDIO_MASTER_GET_BLOCK_SIZE: i32 = 17;
const AUDIO_MASTER_GET_CURRENT_PROCESS_LEVEL: i32 = 23;
const AUDIO_MASTER_GET_VENDOR_STRING: i32 = 32;
const AUDIO_MASTER_GET_PRODUCT_STRING: i32 = 33;
const AUDIO_MASTER_GET_VENDOR_VERSION: i32 = 34;
const AUDIO_MASTER_GET_LANGUAGE: i32 = 35;

type Dispatcher = unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type HostCallback = extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type EntryPoint = unsafe extern "C" fn(HostCallback) -> *mut AEffect;

#[repr(C)]
struct AEffect {
    magic: i32,
    dispatcher: Option<Dispatcher>,
    process: *mut c_void,
    set_parameter: *mut c_void,
    get_parameter: *mut c_void,
    num_programs: i32,
    num_params: i32,
    num_inputs: i32,
    num_outputs: i32,
    flags: i32,
    resvd1: isize,
    resvd2: isize,
    initial_delay: i32,
    real_qualities: i32,
    off_qualities: i32,
    io_ratio: f32,
    object: *mut c_void,
    user: *mut c_void,
    unique_id: i32,
    version: i32,
}

/// The VST2 SDK's VstTimeInfo, returned from audioMasterGetTime. Some plugins
/// dereference the returned pointer without a null check, so the callback
/// hands out a real (static) one instead of 0.
#[repr(C)]
struct VstTimeInfo {
    sample_pos: f64,
    sample_rate: f64,
    nano_seconds: f64,
    ppq_pos: f64,
    tempo: f64,
    bar_start_pos: f64,
    cycle_start_pos: f64,
    cycle_end_pos: f64,
    time_sig_numerator: i32,
    time_sig_denominator: i32,
    smpte_offset: i32,
    smpte_frame_rate: i32,
    samples_to_next_clock: i32,
    flags: i32,
}

/// kVstTempoValid | kVstTimeSigValid.
const TIME_INFO_FLAGS: i32 = (1 << 10) | (1 << 13);

static SCAN_TIME_INFO: VstTimeInfo = VstTimeInfo {
    sample_pos: 0.0,
    sample_rate: SCAN_SAMPLE_RATE as f64,
    nano_seconds: 0.0,
    ppq_pos: 0.0,
    tempo: 120.0,
    bar_start_pos: 0.0,
    cycle_start_pos: 0.0,
    cycle_end_pos: 0.0,
    time_sig_numerator: 4,
    time_sig_denominator: 4,
    smpte_offset: 0,
    smpte_frame_rate: 0,
    samples_to_next_clock: 0,
    flags: TIME_INFO_FLAGS,
};

/// Copies a host identification string into the plugin-supplied buffer
/// (kVstMaxVendorStrLen/kVstMaxProductStrLen are both 64).
fn fill_host_string(ptr: *mut c_void, text: &str) -> isize {
    if ptr.is_null() {
        return 0;
    }
    let bytes = text.as_bytes();
    let len = bytes.len().min(63);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, len);
        *(ptr as *mut u8).add(len) = 0;
    }
    1
}

extern "C" fn host_callback(
    _effect: *mut AEffect,
    opcode: i32,
    _index: i32,
    _value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    match opcode {
        AUDIO_MASTER_VERSION => 2400,
        AUDIO_MASTER_WANT_MIDI => 1,
        AUDIO_MASTER_GET_TIME => &SCAN_TIME_INFO as *const VstTimeInfo as isize,
        AUDIO_MASTER_GET_SAMPLE_RATE => SCAN_SAMPLE_RATE as isize,
        AUDIO_MASTER_GET_BLOCK_SIZE => SCAN_BLOCK_SIZE,
        // kVstProcessLevelUser: not in a realtime or offline processing call.
        AUDIO_MASTER_GET_CURRENT_PROCESS_LEVEL => 1,
        AUDIO_MASTER_GET_VENDOR_STRING => fill_host_string(ptr, "Pubsplash"),
        AUDIO_MASTER_GET_PRODUCT_STRING => fill_host_string(ptr, "Pubsplash"),
        AUDIO_MASTER_GET_VENDOR_VERSION => 1,
        // kVstLangEnglish.
        AUDIO_MASTER_GET_LANGUAGE => 1,
        _ => 0,
    }
}

unsafe fn effect_string(effect: *mut AEffect, dispatcher: Dispatcher, opcode: i32) -> String {
    let mut buf = [0u8; 256];
    unsafe {
        dispatcher(effect, opcode, 0, 0, buf.as_mut_ptr() as *mut c_void, 0.0);
        CStr::from_bytes_until_nul(&buf)
            .map(|s| s.to_string_lossy().trim().to_string())
            .unwrap_or_default()
    }
}

pub fn scan(path: &Path) -> Result<ScanOutput, String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<u16>>();
    unsafe {
        // LOAD_WITH_ALTERED_SEARCH_PATH: dependency DLLs shipped next to the
        // plugin must resolve from the plugin's folder, not the helper's.
        let module = LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
            .map_err(|e| format!("could not load DLL: {e}"))?;
        let entry = GetProcAddress(module, s!("VSTPluginMain"))
            .or_else(|| GetProcAddress(module, s!("main")))
            .ok_or("no VST2 entry point (VSTPluginMain or main)")?;
        let entry: EntryPoint = std::mem::transmute(entry);

        let effect = entry(host_callback);
        if effect.is_null() {
            return Err("VST2 entry point returned null".to_string());
        }
        if (*effect).magic != K_EFFECT_MAGIC {
            return Err("not a VST2 plugin (bad AEffect magic)".to_string());
        }
        let dispatcher = (*effect).dispatcher.ok_or("plugin has no dispatcher")?;
        dispatcher(effect, EFF_OPEN, 0, 0, std::ptr::null_mut(), 0.0);
        // Real hosts configure these right after open; some plugins expect it
        // before answering anything else.
        dispatcher(
            effect,
            EFF_SET_SAMPLE_RATE,
            0,
            0,
            std::ptr::null_mut(),
            SCAN_SAMPLE_RATE,
        );
        dispatcher(
            effect,
            EFF_SET_BLOCK_SIZE,
            0,
            SCAN_BLOCK_SIZE,
            std::ptr::null_mut(),
            0.0,
        );

        let mut name = effect_string(effect, dispatcher, EFF_GET_EFFECT_NAME);
        if name.is_empty() {
            name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
        }
        let vendor = effect_string(effect, dispatcher, EFF_GET_VENDOR_STRING);
        let vendor_version = dispatcher(
            effect,
            EFF_GET_VENDOR_VERSION,
            0,
            0,
            std::ptr::null_mut(),
            0.0,
        ) as i32;
        let version = if vendor_version != 0 {
            vendor_version.to_string()
        } else {
            (*effect).version.to_string()
        };
        let unique_id = (*effect).unique_id;

        dispatcher(effect, EFF_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
        // The module stays loaded; the helper process exits right after this.

        Ok(ScanOutput {
            name,
            vendor,
            version,
            unique_id: Some(unique_id),
            class_ids: Vec::new(),
        })
    }
}
