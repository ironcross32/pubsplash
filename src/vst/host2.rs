//! In-process VST2 hosting for live FX. Unlike the scan-time loader in
//! `vst2.rs` (which runs in the out-of-process `pubsplash-scan` helper and
//! only reads a plugin's identity), this loads plugins into the main app and
//! runs their audio processing.
//!
//! ## Threading contract
//!
//! [`Vst2Plugin`] is `Send + Sync`, but that safety rests on a division of
//! labour that every VST2 host uses and that callers here must honour:
//!
//! - **UI thread only**: [`Vst2Plugin::load`], `Drop`, and every method that
//!   dispatches (parameter *names*, chunks, the editor, mains-changed).
//! - **Engine (audio) thread only**: [`Vst2Plugin::process_replacing`], and
//!   never concurrently with itself.
//! - **Either thread**: [`Vst2Plugin::get_parameter`] /
//!   [`Vst2Plugin::set_parameter`] — plugins are required to tolerate these
//!   during processing, and hosts universally call them from the UI thread.

use crate::config::{FxSlotConfig, ParamValue, PluginRef};
use crate::vst::PluginInfo;
use std::ffi::{CStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::core::{PCWSTR, s};

/// AEffect.magic — 'VstP'.
const K_EFFECT_MAGIC: i32 = 0x5673_7450;

// AEffect.flags bits.
const EFF_FLAGS_HAS_EDITOR: i32 = 1 << 0;
const EFF_FLAGS_CAN_REPLACING: i32 = 1 << 4;
const EFF_FLAGS_PROGRAM_CHUNKS: i32 = 1 << 5;

// Plugin dispatcher opcodes.
const EFF_OPEN: i32 = 0;
const EFF_CLOSE: i32 = 1;
const EFF_SET_SAMPLE_RATE: i32 = 10;
const EFF_SET_BLOCK_SIZE: i32 = 11;
const EFF_MAINS_CHANGED: i32 = 12;
const EFF_EDIT_GET_RECT: i32 = 13;
const EFF_EDIT_OPEN: i32 = 14;
const EFF_EDIT_CLOSE: i32 = 15;
const EFF_EDIT_IDLE: i32 = 19;
const EFF_GET_CHUNK: i32 = 23;
const EFF_SET_CHUNK: i32 = 24;
const EFF_GET_PARAM_LABEL: i32 = 6;
const EFF_GET_PARAM_DISPLAY: i32 = 7;
const EFF_GET_PARAM_NAME: i32 = 8;
const EFF_CAN_BE_AUTOMATED: i32 = 26;
const EFF_STRING2PARAMETER: i32 = 27;

// Host callback opcodes.
const AUDIO_MASTER_AUTOMATE: i32 = 0;
const AUDIO_MASTER_VERSION: i32 = 1;
const AUDIO_MASTER_IDLE: i32 = 3;
const AUDIO_MASTER_GET_TIME: i32 = 7;
const AUDIO_MASTER_SIZE_WINDOW: i32 = 15;
const AUDIO_MASTER_GET_SAMPLE_RATE: i32 = 16;
const AUDIO_MASTER_GET_BLOCK_SIZE: i32 = 17;
const AUDIO_MASTER_GET_CURRENT_PROCESS_LEVEL: i32 = 23;
const AUDIO_MASTER_GET_VENDOR_STRING: i32 = 32;
const AUDIO_MASTER_GET_PRODUCT_STRING: i32 = 33;
const AUDIO_MASTER_GET_VENDOR_VERSION: i32 = 34;
const AUDIO_MASTER_GET_LANGUAGE: i32 = 35;
const AUDIO_MASTER_CAN_DO: i32 = 37;
const AUDIO_MASTER_UPDATE_DISPLAY: i32 = 42;
const AUDIO_MASTER_BEGIN_EDIT: i32 = 43;
const AUDIO_MASTER_END_EDIT: i32 = 44;

const SCAN_SAMPLE_RATE: f32 = 48_000.0;
const SCAN_BLOCK_SIZE: i32 = 480;

type Dispatcher =
    unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type ProcessProc = unsafe extern "C" fn(*mut AEffect, *const *mut f32, *const *mut f32, i32);
type SetParameterProc = unsafe extern "C" fn(*mut AEffect, i32, f32);
type GetParameterProc = unsafe extern "C" fn(*mut AEffect, i32) -> f32;
type HostCallback =
    extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type EntryPoint = unsafe extern "C" fn(HostCallback) -> *mut AEffect;

#[repr(C)]
struct AEffect {
    magic: i32,
    dispatcher: Option<Dispatcher>,
    process: *mut c_void,
    set_parameter: Option<SetParameterProc>,
    get_parameter: Option<GetParameterProc>,
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
    process_replacing: Option<ProcessProc>,
    process_double_replacing: *mut c_void,
    future: [u8; 56],
}

#[repr(C)]
struct ERect {
    top: i16,
    left: i16,
    bottom: i16,
    right: i16,
}

/// VstTimeInfo, returned from audioMasterGetTime.
#[repr(C)]
#[derive(Clone, Copy)]
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

/// The engine advances this by the block size each block so plugins that
/// query transport time see it move.
static SAMPLE_POS: AtomicU64 = AtomicU64::new(0);

/// Requests from `audioMasterSizeWindow`, drained by the UI pump to resize
/// open editor frames. Each entry is (AEffect pointer as usize, width,
/// height).
static SIZE_REQUESTS: Mutex<Vec<(usize, i32, i32)>> = Mutex::new(Vec::new());

/// Called by the engine once per block.
pub fn advance_transport(frames: u64) {
    SAMPLE_POS.fetch_add(frames, Ordering::Relaxed);
}

/// Drains pending editor-resize requests (UI thread).
pub fn take_size_requests() -> Vec<(usize, i32, i32)> {
    std::mem::take(&mut SIZE_REQUESTS.lock().unwrap())
}

thread_local! {
    static TIME_INFO: std::cell::UnsafeCell<VstTimeInfo> =
        std::cell::UnsafeCell::new(VstTimeInfo {
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
            // kVstTempoValid | kVstTimeSigValid.
            flags: (1 << 10) | (1 << 13),
        });
}

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

fn host_can_do(ptr: *mut c_void) -> isize {
    if ptr.is_null() {
        return 0;
    }
    let text = unsafe { CStr::from_ptr(ptr as *const i8) }
        .to_string_lossy()
        .to_string();
    match text.as_str() {
        "sendVstTimeInfo" | "sizeWindow" | "supplyIdle" | "startStopProcess" => 1,
        _ => 0,
    }
}

extern "C" fn host_callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    match opcode {
        AUDIO_MASTER_VERSION => 2400,
        AUDIO_MASTER_AUTOMATE | AUDIO_MASTER_IDLE | AUDIO_MASTER_UPDATE_DISPLAY
        | AUDIO_MASTER_BEGIN_EDIT | AUDIO_MASTER_END_EDIT => 0,
        AUDIO_MASTER_GET_TIME => TIME_INFO.with(|cell| {
            let info = cell.get();
            unsafe {
                (*info).sample_pos = SAMPLE_POS.load(Ordering::Relaxed) as f64;
            }
            info as isize
        }),
        AUDIO_MASTER_GET_SAMPLE_RATE => SCAN_SAMPLE_RATE as isize,
        AUDIO_MASTER_GET_BLOCK_SIZE => SCAN_BLOCK_SIZE as isize,
        AUDIO_MASTER_GET_CURRENT_PROCESS_LEVEL => 2, // realtime
        AUDIO_MASTER_GET_VENDOR_STRING | AUDIO_MASTER_GET_PRODUCT_STRING => {
            fill_host_string(ptr, "Pubsplash")
        }
        AUDIO_MASTER_GET_VENDOR_VERSION => 1,
        AUDIO_MASTER_GET_LANGUAGE => 1, // English
        AUDIO_MASTER_CAN_DO => host_can_do(ptr),
        AUDIO_MASTER_SIZE_WINDOW => {
            if let Ok(mut q) = SIZE_REQUESTS.lock() {
                q.push((effect as usize, index, value as i32));
            }
            1
        }
        _ => 0,
    }
}

fn base64_encode(input: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let pad = chunk.iter().filter(|&&c| c == b'=').count();
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = if c == b'=' { 0 } else { val(c)? };
            n |= v << (18 - 6 * i);
        }
        out.push((n >> 16 & 0xff) as u8);
        if pad < 2 {
            out.push((n >> 8 & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
    }
    Some(out)
}

pub struct Vst2Plugin {
    module: HMODULE,
    effect: *mut AEffect,
    pub info: PluginRef,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub has_editor: bool,
    pub uses_chunks: bool,
    editor_open: AtomicBool,
}

// SAFETY: upheld by the threading contract documented at the module top.
unsafe impl Send for Vst2Plugin {}
unsafe impl Sync for Vst2Plugin {}

impl Vst2Plugin {
    /// Loads and prepares a plugin for processing. UI thread only; may take
    /// hundreds of milliseconds. `slot` supplies saved state to restore.
    pub fn load(info: &PluginInfo, slot: &FxSlotConfig) -> Result<Vst2Plugin, String> {
        let wide: Vec<u16> = Path::new(&info.path)
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect();
        unsafe {
            let module =
                LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
                    .map_err(|e| format!("could not load DLL: {e}"))?;
            let entry = GetProcAddress(module, s!("VSTPluginMain"))
                .or_else(|| GetProcAddress(module, s!("main")))
                .ok_or_else(|| {
                    let _ = FreeLibrary(module);
                    "no VST2 entry point".to_string()
                })?;
            let entry: EntryPoint = std::mem::transmute(entry);

            let effect = entry(host_callback);
            let fail = |module: HMODULE, msg: &str| {
                let _ = FreeLibrary(module);
                Err(msg.to_string())
            };
            if effect.is_null() {
                return fail(module, "plugin returned no effect");
            }
            if (*effect).magic != K_EFFECT_MAGIC {
                return fail(module, "not a VST2 plugin (bad AEffect magic)");
            }
            let flags = (*effect).flags;
            if flags & EFF_FLAGS_CAN_REPLACING == 0 || (*effect).process_replacing.is_none() {
                return fail(module, "plugin cannot process (no processReplacing)");
            }
            if (*effect).num_outputs <= 0 {
                return fail(module, "plugin has no audio outputs");
            }
            let Some(dispatcher) = (*effect).dispatcher else {
                return fail(module, "plugin has no dispatcher");
            };

            dispatcher(effect, EFF_OPEN, 0, 0, std::ptr::null_mut(), 0.0);
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
                SCAN_BLOCK_SIZE as isize,
                std::ptr::null_mut(),
                0.0,
            );

            let plugin = Vst2Plugin {
                module,
                effect,
                info: PluginRef::from_info(info),
                num_params: (*effect).num_params,
                num_inputs: (*effect).num_inputs,
                num_outputs: (*effect).num_outputs,
                has_editor: flags & EFF_FLAGS_HAS_EDITOR != 0,
                uses_chunks: flags & EFF_FLAGS_PROGRAM_CHUNKS != 0,
                editor_open: AtomicBool::new(false),
            };

            // Restore saved state before switching processing on.
            plugin.restore(slot);
            dispatcher(effect, EFF_MAINS_CHANGED, 0, 1, std::ptr::null_mut(), 0.0);
            Ok(plugin)
        }
    }

    fn dispatcher(&self) -> Dispatcher {
        // load() guarantees a dispatcher exists.
        unsafe { (*self.effect).dispatcher.unwrap_unchecked() }
    }

    fn restore(&self, slot: &FxSlotConfig) {
        if self.uses_chunks {
            if let Some(chunk) = slot.chunk.as_ref().and_then(|c| base64_decode(c)) {
                self.set_chunk(&chunk);
                return;
            }
        }
        for pv in &slot.params {
            if pv.index >= 0 && pv.index < self.num_params {
                self.set_parameter(pv.index, pv.value);
            }
        }
    }

    pub fn get_parameter(&self, index: i32) -> f32 {
        unsafe {
            match (*self.effect).get_parameter {
                Some(f) => f(self.effect, index),
                None => 0.0,
            }
        }
    }

    pub fn set_parameter(&self, index: i32, value: f32) {
        unsafe {
            if let Some(f) = (*self.effect).set_parameter {
                f(self.effect, index, value.clamp(0.0, 1.0));
            }
        }
    }

    fn param_string(&self, opcode: i32, index: i32) -> String {
        let mut buf = [0u8; 256];
        unsafe {
            self.dispatcher()(
                self.effect,
                opcode,
                index,
                0,
                buf.as_mut_ptr() as *mut c_void,
                0.0,
            );
            CStr::from_bytes_until_nul(&buf)
                .map(|s| s.to_string_lossy().trim().to_string())
                .unwrap_or_default()
        }
    }

    pub fn param_name(&self, index: i32) -> String {
        self.param_string(EFF_GET_PARAM_NAME, index)
    }

    pub fn param_display(&self, index: i32) -> String {
        self.param_string(EFF_GET_PARAM_DISPLAY, index)
    }

    pub fn param_label(&self, index: i32) -> String {
        self.param_string(EFF_GET_PARAM_LABEL, index)
    }

    /// Asks the plugin to parse `text` (in its own display units, e.g. "-6 dB")
    /// and set parameter `index` to the matching value. Returns whether the
    /// plugin supported the conversion; this VST2 opcode is optional and many
    /// plugins do not implement it. Must be called on the UI thread (dispatcher).
    pub fn string_to_parameter(&self, index: i32, text: &str) -> bool {
        // The plugin reads (and may write) a null-terminated string in this
        // buffer, so it must be writable and large enough.
        let mut buf = [0u8; 256];
        let bytes = text.as_bytes();
        let n = bytes.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        unsafe {
            self.dispatcher()(
                self.effect,
                EFF_STRING2PARAMETER,
                index,
                0,
                buf.as_mut_ptr() as *mut c_void,
                0.0,
            ) != 0
        }
    }

    pub fn can_be_automated(&self, index: i32) -> bool {
        unsafe {
            self.dispatcher()(self.effect, EFF_CAN_BE_AUTOMATED, index, 0, std::ptr::null_mut(), 0.0)
                != 0
        }
    }

    fn get_chunk(&self) -> Option<Vec<u8>> {
        let mut data: *mut c_void = std::ptr::null_mut();
        unsafe {
            // index 1 = the current program's chunk.
            let size = self.dispatcher()(
                self.effect,
                EFF_GET_CHUNK,
                1,
                0,
                &mut data as *mut *mut c_void as *mut c_void,
                0.0,
            );
            if size <= 0 || data.is_null() {
                return None;
            }
            Some(std::slice::from_raw_parts(data as *const u8, size as usize).to_vec())
        }
    }

    fn set_chunk(&self, data: &[u8]) {
        unsafe {
            self.dispatcher()(
                self.effect,
                EFF_SET_CHUNK,
                1,
                data.len() as isize,
                data.as_ptr() as *mut c_void,
                0.0,
            );
        }
    }

    /// Captures the plugin's current state for persistence: a base64 chunk
    /// when supported, otherwise the full parameter list.
    pub fn snapshot(&self) -> (Option<String>, Vec<ParamValue>) {
        if self.uses_chunks {
            if let Some(chunk) = self.get_chunk() {
                return (Some(base64_encode(&chunk)), Vec::new());
            }
        }
        let params = (0..self.num_params)
            .map(|index| ParamValue {
                index,
                value: self.get_parameter(index),
            })
            .collect();
        (None, params)
    }

    /// The editor's (width, height) in pixels, if it has one.
    pub fn editor_rect(&self) -> Option<(i32, i32)> {
        if !self.has_editor {
            return None;
        }
        let mut rect_ptr: *mut ERect = std::ptr::null_mut();
        unsafe {
            let ok = self.dispatcher()(
                self.effect,
                EFF_EDIT_GET_RECT,
                0,
                0,
                &mut rect_ptr as *mut *mut ERect as *mut c_void,
                0.0,
            );
            if ok == 0 || rect_ptr.is_null() {
                return None;
            }
            let r = &*rect_ptr;
            Some(((r.right - r.left) as i32, (r.bottom - r.top) as i32))
        }
    }

    /// Opens the plugin editor as a child of `hwnd`.
    pub fn editor_open(&self, hwnd: *mut c_void) {
        unsafe {
            self.dispatcher()(self.effect, EFF_EDIT_OPEN, 0, 0, hwnd, 0.0);
        }
        self.editor_open.store(true, Ordering::Relaxed);
    }

    pub fn editor_close(&self) {
        if self.editor_open.swap(false, Ordering::Relaxed) {
            unsafe {
                self.dispatcher()(self.effect, EFF_EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
            }
        }
    }

    pub fn editor_idle(&self) {
        unsafe {
            self.dispatcher()(self.effect, EFF_EDIT_IDLE, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }

    /// The AEffect pointer as an integer, matching `audioMasterSizeWindow`
    /// requests to this instance.
    pub fn effect_id(&self) -> usize {
        self.effect as usize
    }

    /// Processes one block. Engine thread only. `inputs`/`outputs` are arrays
    /// of channel pointers of length `num_inputs`/`num_outputs`.
    ///
    /// # Safety
    /// The pointers must be valid for `frames` samples each.
    pub unsafe fn process_replacing(
        &self,
        inputs: *const *mut f32,
        outputs: *const *mut f32,
        frames: i32,
    ) {
        if let Some(process) = unsafe { (*self.effect).process_replacing } {
            unsafe { process(self.effect, inputs, outputs, frames) };
        }
    }
}

impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        // UI thread by contract.
        self.editor_close();
        unsafe {
            let dispatcher = self.dispatcher();
            dispatcher(self.effect, EFF_MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
            dispatcher(self.effect, EFF_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
            let _ = FreeLibrary(self.module);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aeffect_layout_matches_vst24() {
        // If these offsets drift, every FFI call reads the wrong field.
        assert_eq!(std::mem::offset_of!(AEffect, magic), 0);
        assert_eq!(std::mem::offset_of!(AEffect, dispatcher), 8);
        assert_eq!(std::mem::offset_of!(AEffect, flags), 56);
        assert_eq!(std::mem::offset_of!(AEffect, unique_id), 112);
        assert_eq!(std::mem::offset_of!(AEffect, process_replacing), 120);
        assert_eq!(std::mem::offset_of!(AEffect, process_double_replacing), 128);
    }

    #[test]
    fn base64_roundtrips() {
        for case in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"\x00\x01\x02\xff\xfe",
        ] {
            let encoded = base64_encode(case);
            assert_eq!(base64_decode(&encoded).unwrap(), case, "case {case:?}");
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
    }

    /// Loads a real VST2 plugin and runs a block through it. Requires the
    /// plugin to be installed, so it is ignored by default:
    /// `cargo test loads_and_processes_real_plugin -- --include-ignored`.
    #[test]
    #[ignore]
    fn loads_and_processes_real_plugin() {
        let path = r"C:\Program Files\Common Files\VST2\dfx Buffer Override.dll";
        if !std::path::Path::new(path).exists() {
            eprintln!("plugin not installed; skipping");
            return;
        }
        let info = PluginInfo {
            path: path.into(),
            format: crate::vst::PluginFormat::Vst2,
            name: "Buffer Override".into(),
            ..Default::default()
        };
        let plugin = Vst2Plugin::load(&info, &FxSlotConfig::default()).expect("load");
        assert!(plugin.num_params > 0);
        assert!(plugin.num_outputs >= 1);

        // A snapshot must restore without changing the reported state.
        let (chunk, params) = plugin.snapshot();
        let slot = FxSlotConfig {
            chunk,
            params,
            ..Default::default()
        };
        plugin.restore(&slot);

        // Process one block of a 1 kHz-ish ramp; just assert it doesn't crash
        // and produces finite output.
        let frames = SCAN_BLOCK_SIZE as usize;
        let n_in = plugin.num_inputs.max(1) as usize;
        let n_out = plugin.num_outputs.max(1) as usize;
        let mut ins: Vec<Vec<f32>> = (0..n_in)
            .map(|_| (0..frames).map(|i| (i as f32 * 0.01).sin() * 0.2).collect())
            .collect();
        let mut outs: Vec<Vec<f32>> = (0..n_out).map(|_| vec![0.0; frames]).collect();
        let in_ptrs: Vec<*mut f32> = ins.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let out_ptrs: Vec<*mut f32> = outs.iter_mut().map(|c| c.as_mut_ptr()).collect();
        unsafe {
            plugin.process_replacing(in_ptrs.as_ptr(), out_ptrs.as_ptr(), frames as i32);
        }
        assert!(outs[0].iter().all(|s| s.is_finite()));
    }
}
