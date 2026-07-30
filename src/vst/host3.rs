//! In-process VST3 hosting for live FX, built on the `vst3-host` crate.
//!
//! ## Threading contract
//!
//! `vst3_host::Plugin` is `Send` but not `Sync`, so everything that touches it
//! goes through one mutex and [`Vst3Plugin`] gets `Send + Sync` from the
//! compiler rather than from an `unsafe impl`. That is *not* the whole
//! contract, though — VST3 itself pins several things to the main thread, and
//! callers here must honour the same division of labour as `host2.rs`:
//!
//! - **UI thread only**: [`Vst3Plugin::load`], `Drop` (COM teardown —
//!   `setActive(0)`, `disconnect`, `terminate` — must run on the thread that
//!   created the objects), the `editor_*` methods, [`Vst3Plugin::snapshot`],
//!   [`Vst3Plugin::refresh_params`], [`Vst3Plugin::set_processing`], and every
//!   parameter accessor. VST3 treats `IEditController` as main-thread-only, so
//!   unlike VST2 the parameter calls here are *not* safe from the engine.
//! - **Engine (audio) thread only**: [`Vst3Plugin::process_stereo`], and never
//!   concurrently with itself.
//!
//! ## Staying out of the audio thread's way
//!
//! [`Vst3Plugin::process_stereo`] never blocks: it takes the mutex with
//! `try_lock` and reports [`Processed::Passthrough`] if it cannot have it.
//! Some UI operations hold the mutex for far longer than one 10 ms block —
//! `open_editor` is routinely 50–500 ms of GUI construction, and `save_state`
//! can be megabytes — so those raise the shared
//! [`SuspendFlag`](crate::vst::suspend::SuspendFlag) and wait
//! [`SETTLE`](crate::vst::suspend::SETTLE) before taking the lock, exactly as
//! the VST2 host does. The engine sees the flag, keeps
//! processing, and reports [`Processed::Retiring`] so `FxChain` can fade the
//! plugin out of circuit over 50 ms *before* the lock is actually taken. The
//! result is a clean crossfade in and out instead of a hard switch mid-block.
//!
//! ## Known limitations of `vst3-host` 0.7
//!
//! - `IComponentHandler::restartComponent` is a stub in the crate, so a plugin
//!   that changes its parameter set (any preset load) cannot notify us. We
//!   re-read the list at point of use instead — see [`Vst3Plugin::params`].
//! - `Plugin::save_state` captures `IComponent::getState` only; the paired
//!   `IEditController::getState` (editor page, zoom, A/B slot) has no wrapper,
//!   so those settings do not persist. See [`Vst3Plugin::snapshot`].
//! - `IPlugView::onSize` has no wrapper either, so a plugin that resizes itself
//!   is not told about the new rect. See `ui/fx_editor.rs`.
//! - The crate's Windows module loader has no cache or refcount, so every
//!   instance `LoadLibrary`s its own copy of the plugin DLL and calls `ExitDll`
//!   plus `FreeLibrary` when *that instance* goes away. Windows refcounts the
//!   `HMODULE`, so the per-instance load is only a cost (load time and memory);
//!   the unmap when the last instance goes is the hazard, and
//!   [`crate::vst::module_pin`] holds a permanent reference to stop it. The
//!   `ExitDll` call is still the crate's to make and we cannot suppress it.

use crate::audio::mixer::{BLOCK_FRAMES, CHANNELS, SAMPLE_RATE};
use crate::b64;
use crate::config::{FxSlotConfig, ParamValue, PluginRef};
use crate::vst::PluginInfo;
use crate::vst::instance::Processed;
use crate::vst::suspend::SuspendFlag;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError};
use vst3_host::audio::{AudioBuffers, BusArrangements, SpeakerArrangement};
use vst3_host::{Parameter, Plugin, ProcessMode, Vst3Host, WindowHandle};

/// Sample rate the plugin is configured for, from the mixer's own constant so
/// the two can never silently diverge.
const PLUGIN_SAMPLE_RATE: f64 = SAMPLE_RATE as f64;

pub struct Vst3Plugin {
    inner: Mutex<Vst3Inner>,
    pub info: PluginRef,
    /// Never-reused identity for editor and resize-request matching. See
    /// [`crate::vst::instance::next_effect_id`].
    id: u64,
    /// Parameter metadata. Re-read on demand rather than cached for the
    /// instance lifetime: VST3 lets a plugin change its parameter set at
    /// runtime (any preset load inside its own editor), and the index→ParamID
    /// map going stale would make the parameters dialog announce one parameter
    /// and edit another. Never read from the engine thread.
    params: RwLock<Arc<Vec<Parameter>>>,
    /// Raised while a UI operation is about to hold, or is holding, `inner`
    /// for longer than a mix block. See the module docs.
    suspend: SuspendFlag,
    /// Latches the first `process_audio` failure so the engine thread logs
    /// once instead of a hundred times a second.
    failed: AtomicBool,
    layout: Vst3Layout,
    /// Cached at load: the runtime check creates a throwaway `IPlugView`, and
    /// this is asked on every "Plugin interface" press.
    has_editor: bool,
}

struct Vst3Inner {
    plugin: Plugin,
    buffers: AudioBuffers,
    editor_open: bool,
    processing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Vst3Layout {
    /// 0 for a generator (an instrument, or any plugin with no audio input
    /// bus); otherwise 1 or 2.
    input_channels: usize,
    /// 1 or 2. Mono is upmixed to both output channels.
    output_channels: usize,
}

impl Vst3Plugin {
    pub fn load(info: &PluginInfo, slot: &FxSlotConfig) -> Result<Vst3Plugin, String> {
        // Before the crate maps the module, so its own load/unload pair can
        // never take the last reference. See `module_pin`.
        super::module_pin::pin(&info.path);
        let mut host = Vst3Host::builder()
            .sample_rate(PLUGIN_SAMPLE_RATE)
            .block_size(BLOCK_FRAMES)
            .build()
            .map_err(|e| format!("could not create VST3 host: {e}"))?;
        let mut plugin = host
            .load_plugin(Path::new(&info.path))
            .map_err(|e| format!("could not load VST3 plugin: {e}"))?;
        // The crate releases the factory as soon as it has the component. For a
        // shell module that is what kills us at teardown, so take a reference
        // that outlives every instance. See `module_pin::pin_factory`.
        super::module_pin::pin_factory(&info.path);

        plugin
            .set_process_mode(ProcessMode::Realtime)
            .map_err(|e| format!("could not configure VST3 realtime processing: {e}"))?;
        plugin
            .set_tempo(120.0)
            .map_err(|e| format!("could not set VST3 tempo: {e}"))?;
        plugin
            .set_time_signature(4, 4)
            .map_err(|e| format!("could not set VST3 time signature: {e}"))?;
        plugin
            .set_playing(true)
            .map_err(|e| format!("could not set VST3 transport state: {e}"))?;

        let layout = negotiate_layout(&mut plugin, &info.path)?;
        let params = plugin
            .get_parameters()
            .map_err(|e| format!("could not read VST3 parameters: {e}"))?;
        restore(&mut plugin, &params, slot);
        let has_editor = plugin.has_editor();
        plugin
            .start_processing()
            .map_err(|e| format!("could not start VST3 processing: {e}"))?;

        Ok(Vst3Plugin {
            inner: Mutex::new(Vst3Inner {
                plugin,
                buffers: AudioBuffers::new(
                    layout.input_channels,
                    layout.output_channels,
                    BLOCK_FRAMES,
                    PLUGIN_SAMPLE_RATE,
                ),
                editor_open: false,
                processing: true,
            }),
            info: PluginRef::from_info(info),
            id: crate::vst::instance::next_effect_id(),
            params: RwLock::new(Arc::new(params)),
            suspend: SuspendFlag::new(),
            failed: AtomicBool::new(false),
            layout,
            has_editor,
        })
    }

    /// Takes the plugin mutex, recovering from poisoning rather than
    /// propagating it. A panic under the lock must not turn every later call
    /// into a silent no-op — that path loses the user's saved plugin state and
    /// permanently bypasses the plugin. The crate itself recovers the same way.
    fn lock(&self) -> MutexGuard<'_, Vst3Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current parameter list. Cheap (one `Arc` clone) and lock-free.
    fn params(&self) -> Arc<Vec<Parameter>> {
        self.params
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Re-reads the plugin's parameter list. Call before presenting parameters
    /// to the user: the crate cannot tell us when the set changes, so this is
    /// poll-on-use.
    pub fn refresh_params(&self) {
        let _suspended = self.suspend.raise();
        let mut inner = self.lock();
        Self::reload_params(&mut inner, &self.params);
    }

    fn reload_params(inner: &mut Vst3Inner, cell: &RwLock<Arc<Vec<Parameter>>>) {
        match inner.plugin.get_parameters() {
            Ok(params) => *cell.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(params),
            Err(e) => log::warn!("Could not re-read VST3 parameters: {e}"),
        }
    }

    pub fn num_inputs(&self) -> i32 {
        self.layout.input_channels as i32
    }

    pub fn num_outputs(&self) -> i32 {
        self.layout.output_channels as i32
    }

    /// True for a plugin with no audio input bus — an instrument or other
    /// generator. Its output is summed into the chain rather than replacing it.
    pub fn is_generator(&self) -> bool {
        self.layout.input_channels == 0
    }

    pub fn num_params(&self) -> i32 {
        self.params().len() as i32
    }

    fn param_id(&self, index: i32) -> Option<u32> {
        self.params().get(index as usize).map(|p| p.id)
    }

    pub fn get_parameter(&self, index: i32) -> f32 {
        let Some(id) = self.param_id(index) else {
            return 0.0;
        };
        self.lock().plugin.get_parameter(id).unwrap_or(0.0) as f32
    }

    pub fn set_parameter(&self, index: i32, value: f32) {
        let Some(id) = self.param_id(index) else {
            return;
        };
        let _ = self
            .lock()
            .plugin
            .set_parameter(id, value.clamp(0.0, 1.0) as f64);
    }

    pub fn param_name(&self, index: i32) -> String {
        self.params()
            .get(index as usize)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    pub fn param_display(&self, index: i32) -> String {
        let Some(id) = self.param_id(index) else {
            return String::new();
        };
        let inner = self.lock();
        let value = inner.plugin.get_parameter(id).unwrap_or(0.0);
        inner.plugin.format_parameter(id, value).unwrap_or_default()
    }

    pub fn param_label(&self, index: i32) -> String {
        self.params()
            .get(index as usize)
            .map(|p| p.unit.clone())
            .unwrap_or_default()
    }

    pub fn can_be_automated(&self, index: i32) -> bool {
        self.params()
            .get(index as usize)
            .map(|p| p.can_automate && !p.is_read_only)
            .unwrap_or(false)
    }

    /// `vst3-host` 0.7 wraps `IEditController::getParamStringByValue` but not
    /// its inverse `getParamValueByString`, so typed entry cannot be supported
    /// for any VST3 parameter. The parameters dialog says so out loud rather
    /// than discarding what the user typed in silence.
    pub fn string_to_parameter(&self, _index: i32, _text: &str) -> bool {
        false
    }

    pub fn has_editor(&self) -> bool {
        self.has_editor
    }

    pub fn editor_rect(&self) -> Option<(i32, i32)> {
        self.lock().plugin.get_editor_size().ok()
    }

    pub fn editor_open(&self, hwnd: *mut std::ffi::c_void) -> bool {
        let _suspended = self.suspend.raise();
        let mut inner = self.lock();
        if inner.editor_open {
            return true;
        }
        let handle = WindowHandle::from_hwnd(hwnd);
        match inner.plugin.open_editor(handle) {
            Ok(()) => {
                inner.editor_open = true;
                true
            }
            Err(e) => {
                log::warn!("Could not open VST3 editor for {}: {e}", self.info.path);
                false
            }
        }
    }

    pub fn editor_close(&self) {
        let _suspended = self.suspend.raise();
        let mut inner = self.lock();
        if inner.editor_open {
            let _ = inner.plugin.close_editor();
            inner.editor_open = false;
        }
    }

    pub fn editor_idle(&self) {}

    pub fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        self.lock().plugin.take_editor_resize_request()
    }

    /// Drains the edits the plugin's own editor reported through
    /// `IComponentHandler::performEdit`. Nothing here consumes them, but the
    /// crate stashes them on the **engine** thread inside `process_audio` —
    /// taking a blocking lock and growing a `Vec` — so leaving them undrained
    /// means unbounded growth plus a heap reallocation per block.
    pub fn drain_editor_edits(&self) {
        let _ = self.lock().plugin.get_parameter_changes();
    }

    /// Follows the slot's bypass state. Plugins that use `setProcessing(true)`
    /// as their "flush tails, reset state" hook never see it otherwise, so
    /// un-bypassing resumes with a stale reverb tail.
    pub fn set_processing(&self, on: bool) {
        let _suspended = self.suspend.raise();
        let mut inner = self.lock();
        if inner.processing == on {
            return;
        }
        let result = if on {
            inner.plugin.start_processing()
        } else {
            inner.plugin.stop_processing()
        };
        match result {
            Ok(()) => {
                inner.processing = on;
                self.failed.store(false, Ordering::Relaxed);
            }
            Err(e) => log::warn!(
                "Could not {} VST3 processing for {}: {e}",
                if on { "start" } else { "stop" },
                self.info.path
            ),
        }
    }

    pub fn effect_id(&self) -> u64 {
        self.id
    }

    /// The plugin's state for `config.json`, or `None` when there is nothing
    /// worth writing. Returning a fabricated all-zeros parameter set here
    /// would overwrite a working chain in the config file, so a failure must
    /// be distinguishable from "the user zeroed everything".
    ///
    /// Both the opaque chunk *and* a parameter snapshot are captured: the
    /// chunk is authoritative, and the parameters are the fallback for when a
    /// later build of the plugin refuses the chunk.
    ///
    /// Note this is `IComponent::getState` only. The paired
    /// `IEditController::getState` — editor page, zoom, A/B slot — has no
    /// wrapper in `vst3-host` 0.7, so those settings do not survive a restart.
    pub fn snapshot(&self) -> Option<(Option<String>, Vec<ParamValue>)> {
        let _suspended = self.suspend.raise();
        let mut inner = self.lock();
        Self::reload_params(&mut inner, &self.params);
        let params = self.params();

        let chunk = match inner.plugin.save_state() {
            Ok(state) => Some(b64::encode(&state)),
            Err(e) => {
                log::warn!("Could not save VST3 state for {}: {e}", self.info.path);
                None
            }
        };
        let values: Vec<ParamValue> = params
            .iter()
            .enumerate()
            .map(|(index, param)| ParamValue {
                index: index as i32,
                id: Some(param.id),
                value: inner.plugin.get_parameter(param.id).unwrap_or(0.0) as f32,
            })
            .collect();

        if chunk.is_none() && values.is_empty() {
            return None;
        }
        Some((chunk, values))
    }

    /// Engine thread only. Never blocks: if the mutex is held, or a suspending
    /// UI operation is about to take it, the caller is told to fade this
    /// plugin out of circuit rather than being handed a stale block.
    pub fn process_stereo(
        &self,
        dry: &[Vec<f32>; CHANNELS],
        out: &mut [Vec<f32>; CHANNELS],
    ) -> Processed {
        let retiring = self.suspend.raised();
        let mut guard = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Processed::Passthrough,
        };
        let inner = &mut *guard;
        // Stopped because the slot is bypassed. The chain is still fading it
        // out, so this is expected rather than a failure to report.
        if !inner.processing {
            return Processed::Passthrough;
        }
        copy_stereo_to_vst3_inputs(self.layout.input_channels, dry, &mut inner.buffers);
        for output in &mut inner.buffers.outputs {
            output.fill(0.0);
        }
        if let Err(e) = inner.plugin.process_audio(&mut inner.buffers) {
            // Latched: the crate allocates its error string and the log record
            // allocates another, and this runs a hundred times a second.
            if !self.failed.swap(true, Ordering::Relaxed) {
                log::error!(
                    "VST3 processing failed for {} (bypassing until it recovers): {e}",
                    self.info.path
                );
            }
            return Processed::Passthrough;
        }
        self.failed.store(false, Ordering::Relaxed);
        copy_vst3_outputs_to_stereo(&inner.buffers, out);
        if retiring {
            Processed::Retiring
        } else {
            Processed::Wet
        }
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        // Breadcrumbs, not chatter. Plugin teardown runs third-party COM code
        // and `FreeLibrary`, and when that faults the process dies without a
        // panic or a backtrace (see `crash.rs`) — so which of these lines is
        // the *last* one in the log is the only evidence of how far it got.
        log::info!("VST3 teardown starting for {}", self.info.path);
        // Teardown is the longest hold there is, so it obeys the same rule as
        // every other one in this file. The engine cannot be inside
        // `process_stereo` (it no longer holds a reference to us, which is what
        // let this drop run at all), but a chain still mid-fade would otherwise
        // take the plugin's last block straight to dry.
        let _suspended = self.suspend.raise();
        // Unconditional: skipping this on a poisoned lock would leave
        // `IPlugView::removed()` uncalled, with the plugin's view still
        // attached to an HWND wx is about to destroy.
        let mut inner = self.lock();
        inner.processing = false;
        if inner.editor_open {
            let _ = inner.plugin.close_editor();
            log::info!("VST3 teardown: editor closed for {}", self.info.path);
        }
        let _ = inner.plugin.set_playing(false);
        let _ = inner.plugin.stop_processing();
        log::info!(
            "VST3 teardown: processing stopped for {}; releasing the plugin (terminate, then \
             ExitDll and the module unload) next",
            self.info.path
        );
    }
}

fn restore(plugin: &mut Plugin, params: &[Parameter], slot: &FxSlotConfig) {
    if let Some(state) = slot.chunk.as_deref().and_then(b64::decode) {
        match plugin.load_state(&state) {
            Ok(()) => return,
            Err(e) => log::warn!(
                "Could not restore saved VST3 state ({e}); falling back to the parameter snapshot"
            ),
        }
    }
    for pv in &slot.params {
        let id = pv
            .id
            .or_else(|| params.get(pv.index as usize).map(|p| p.id));
        if let Some(id) = id {
            let _ = plugin.set_parameter(id, pv.value.clamp(0.0, 1.0) as f64);
        }
    }
}

/// Negotiates a layout the mixer can drive: mono or stereo out, and mono,
/// stereo or *no* audio input. A plugin with no input bus is an instrument or
/// other generator; it is allowed, and `FxChain` sums its output into the bus
/// instead of replacing it.
fn negotiate_layout(plugin: &mut Plugin, path: &str) -> Result<Vst3Layout, String> {
    let mut current = match plugin.bus_arrangements() {
        Ok(arrangements) => {
            log::info!(
                "VST3 initial bus layout for {path}: {}",
                arrangement_summary(&arrangements)
            );
            Some(arrangements)
        }
        Err(e) => {
            log::warn!("Could not query initial VST3 bus layout for {path}: {e}");
            None
        }
    };

    let has_input_bus = current.as_ref().is_none_or(|a| !a.inputs.is_empty());
    for expected in requested_layouts(has_input_bus) {
        let Some((request_inputs, request_outputs)) =
            requested_arrangements(current.as_ref(), expected)
        else {
            continue;
        };
        match plugin.set_bus_arrangements(&request_inputs, &request_outputs) {
            Ok(()) => {
                let actual = plugin
                    .bus_arrangements()
                    .map_err(|e| format!("could not query VST3 bus layout after request: {e}"))?;
                log::info!(
                    "VST3 bus layout request for {path}: requested {}, actual {}",
                    layout_summary(&request_inputs, &request_outputs),
                    arrangement_summary(&actual)
                );
                if compatible_layout(&actual) == Some(expected) {
                    return Ok(expected);
                }
                current = Some(actual);
            }
            Err(e) => {
                log::warn!(
                    "VST3 bus layout request failed for {path}: requested {}, error {e}",
                    layout_summary(&request_inputs, &request_outputs)
                );
            }
        }
    }

    if let Some(actual) = current.as_ref().and_then(compatible_layout) {
        log::info!(
            "Using existing compatible VST3 bus layout for {path}: {}",
            arrangement_summary(current.as_ref().unwrap())
        );
        return Ok(actual);
    }

    Err(layout_rejection(current.as_ref()))
}

/// Why a layout could not be used, in words the user can act on. The old
/// message blamed the stereo *output* for every failure, including plugins
/// whose output was fine and whose input simply did not exist.
fn layout_rejection(current: Option<&BusArrangements>) -> String {
    let Some(arrangements) = current else {
        return "this plugin did not report its audio bus layout".to_string();
    };
    let outputs = arrangements.outputs.first().map(|a| a.channel_count());
    match outputs {
        None => "this plugin has no audio output bus, so it cannot go in an FX chain".to_string(),
        Some(n) if n > CHANNELS => format!(
            "this plugin's main output bus is {n} channels; only mono and stereo are supported"
        ),
        _ => {
            let inputs = arrangements
                .inputs
                .first()
                .map(|a| a.channel_count())
                .unwrap_or(0);
            format!(
                "this plugin's main input bus is {inputs} channels; only mono and stereo are supported"
            )
        }
    }
}

/// Layouts to ask for, best first. A plugin with no audio input bus is a
/// generator, so only its output is negotiable.
fn requested_layouts(has_input_bus: bool) -> Vec<Vst3Layout> {
    let inputs: &[usize] = if has_input_bus { &[2, 1] } else { &[0] };
    let mut layouts = Vec::with_capacity(inputs.len() * 2);
    for &output_channels in &[2, 1] {
        for &input_channels in inputs {
            layouts.push(Vst3Layout {
                input_channels,
                output_channels,
            });
        }
    }
    layouts
}

fn arrangement_for(channels: usize) -> Option<SpeakerArrangement> {
    match channels {
        1 => Some(SpeakerArrangement::MONO),
        2 => Some(SpeakerArrangement::STEREO),
        _ => None,
    }
}

/// Builds the `setBusArrangements` arrays for `layout`, rewriting only the
/// main bus and leaving every other bus (sidechains, aux outputs) as the
/// plugin has it. VST3's contract is that the arrays are exactly as long as
/// the plugin's bus counts, so a bus that does not exist is never invented —
/// an array *shorter* than `getBusCount` would have a plugin iterating its own
/// bus count read straight off the end of our heap.
fn requested_arrangements(
    current: Option<&BusArrangements>,
    layout: Vst3Layout,
) -> Option<(Vec<SpeakerArrangement>, Vec<SpeakerArrangement>)> {
    let output = arrangement_for(layout.output_channels)?;

    let (mut inputs, mut outputs) = match current {
        Some(arrangements) => (arrangements.inputs.clone(), arrangements.outputs.clone()),
        // The plugin would not tell us its bus counts; assume the usual single
        // main bus in each direction.
        None => (
            if layout.input_channels == 0 {
                Vec::new()
            } else {
                vec![output]
            },
            vec![output],
        ),
    };

    if layout.input_channels == 0 {
        if !inputs.is_empty() {
            return None;
        }
    } else {
        *inputs.first_mut()? = arrangement_for(layout.input_channels)?;
    }
    *outputs.first_mut()? = output;

    debug_assert!(current.is_none_or(|a| a.inputs.len() == inputs.len()));
    debug_assert!(current.is_none_or(|a| a.outputs.len() == outputs.len()));
    Some((inputs, outputs))
}

fn compatible_layout(arrangements: &BusArrangements) -> Option<Vst3Layout> {
    // No input bus at all is a generator, not a failure.
    let input_channels = arrangements
        .inputs
        .first()
        .map(|a| a.channel_count())
        .unwrap_or(0);
    let output_channels = arrangements.outputs.first()?.channel_count();
    if !matches!(output_channels, 1 | 2) || input_channels > CHANNELS {
        return None;
    }
    Some(Vst3Layout {
        input_channels,
        output_channels,
    })
}

fn copy_stereo_to_vst3_inputs(
    input_channels: usize,
    inputs: &[Vec<f32>; CHANNELS],
    buffers: &mut AudioBuffers,
) {
    for input in &mut buffers.inputs {
        input.fill(0.0);
    }
    match input_channels {
        // A generator takes no input.
        0 => {}
        1 => {
            if let Some(mono) = buffers.inputs.get_mut(0) {
                for (out, (left, right)) in mono.iter_mut().zip(inputs[0].iter().zip(&inputs[1])) {
                    *out = (left + right) * 0.5;
                }
            }
        }
        _ => {
            for (channel, source) in buffers.inputs.iter_mut().zip(inputs) {
                copy_clamped(source, channel);
            }
        }
    }
}

fn copy_vst3_outputs_to_stereo(buffers: &AudioBuffers, outputs: &mut [Vec<f32>; CHANNELS]) {
    if buffers.outputs.len() >= CHANNELS {
        for (target, source) in outputs.iter_mut().zip(&buffers.outputs) {
            copy_clamped(source, target);
        }
    } else if let Some(mono) = buffers.outputs.first() {
        // Mono plugin: the one output feeds both channels, so it lands centred
        // rather than hard left.
        for target in outputs.iter_mut() {
            copy_clamped(mono, target);
        }
    }
}

/// `copy_from_slice` panics on a length mismatch, and this runs on the engine
/// thread — a mixer block-size change would take the mix loop, the live
/// stream, and (by poisoning the mutex) the user's saved plugin state with it.
/// Degrade instead.
fn copy_clamped(source: &[f32], target: &mut [f32]) {
    let n = source.len().min(target.len());
    target[..n].copy_from_slice(&source[..n]);
    target[n..].fill(0.0);
}

fn arrangement_summary(arrangements: &BusArrangements) -> String {
    layout_summary(&arrangements.inputs, &arrangements.outputs)
}

fn layout_summary(inputs: &[SpeakerArrangement], outputs: &[SpeakerArrangement]) -> String {
    format!(
        "inputs [{}], outputs [{}]",
        arrangements_list(inputs),
        arrangements_list(outputs)
    )
}

fn arrangements_list(arrangements: &[SpeakerArrangement]) -> String {
    arrangements
        .iter()
        .map(|a| format!("{}ch/{:#x}", a.channel_count(), a.raw()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SpeakerArrangement` has no 6-channel constant exposed; 5.1 is
    /// kStereo | kLfe | kCenter | kSurroundLeft | kSurroundRight.
    const SURROUND: u64 = 0x3F;

    fn arrangements(
        inputs: Vec<SpeakerArrangement>,
        outputs: Vec<SpeakerArrangement>,
    ) -> BusArrangements {
        BusArrangements { inputs, outputs }
    }

    fn layout(input_channels: usize, output_channels: usize) -> Option<Vst3Layout> {
        Some(Vst3Layout {
            input_channels,
            output_channels,
        })
    }

    #[test]
    fn stereo_in_stereo_out_is_compatible() {
        assert_eq!(
            compatible_layout(&arrangements(
                vec![SpeakerArrangement::STEREO],
                vec![SpeakerArrangement::STEREO],
            )),
            layout(2, 2)
        );
    }

    #[test]
    fn mono_in_stereo_out_is_compatible() {
        assert_eq!(
            compatible_layout(&arrangements(
                vec![SpeakerArrangement::MONO],
                vec![SpeakerArrangement::STEREO],
            )),
            layout(1, 2)
        );
    }

    #[test]
    fn mono_output_is_upmixed_rather_than_rejected() {
        assert_eq!(
            compatible_layout(&arrangements(
                vec![SpeakerArrangement::STEREO],
                vec![SpeakerArrangement::MONO],
            )),
            layout(2, 1)
        );
    }

    #[test]
    fn no_input_bus_is_a_generator_not_a_failure() {
        assert_eq!(
            compatible_layout(&arrangements(Vec::new(), vec![SpeakerArrangement::STEREO])),
            layout(0, 2)
        );
    }

    #[test]
    fn surround_output_is_not_compatible() {
        assert_eq!(
            compatible_layout(&arrangements(
                vec![SpeakerArrangement::STEREO],
                vec![SpeakerArrangement::from_raw(SURROUND)],
            )),
            None
        );
    }

    #[test]
    fn zero_output_is_not_compatible() {
        assert_eq!(
            compatible_layout(&arrangements(vec![SpeakerArrangement::STEREO], Vec::new())),
            None
        );
    }

    #[test]
    fn stereo_in_is_requested_before_mono_in_and_stereo_out_before_mono_out() {
        let requests = requested_layouts(true);
        assert_eq!(
            requests,
            vec![
                Vst3Layout {
                    input_channels: 2,
                    output_channels: 2
                },
                Vst3Layout {
                    input_channels: 1,
                    output_channels: 2
                },
                Vst3Layout {
                    input_channels: 2,
                    output_channels: 1
                },
                Vst3Layout {
                    input_channels: 1,
                    output_channels: 1
                },
            ]
        );
    }

    #[test]
    fn a_generator_only_negotiates_its_output() {
        let requests = requested_layouts(false);
        assert!(requests.iter().all(|l| l.input_channels == 0));
        assert_eq!(
            requests
                .iter()
                .map(|l| l.output_channels)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn requested_arrays_match_the_plugins_bus_counts() {
        // Two input buses (main + sidechain), one output bus.
        let current = arrangements(
            vec![SpeakerArrangement::MONO, SpeakerArrangement::MONO],
            vec![SpeakerArrangement::MONO],
        );
        let (inputs, outputs) = requested_arrangements(
            Some(&current),
            Vst3Layout {
                input_channels: 2,
                output_channels: 2,
            },
        )
        .expect("stereo request is representable");
        assert_eq!(inputs.len(), 2);
        assert_eq!(outputs.len(), 1);
        assert_eq!(inputs[0], SpeakerArrangement::STEREO);
        // The sidechain keeps whatever the plugin had.
        assert_eq!(inputs[1], SpeakerArrangement::MONO);
        assert_eq!(outputs[0], SpeakerArrangement::STEREO);
    }

    #[test]
    fn a_generator_request_never_invents_an_input_bus() {
        let current = arrangements(Vec::new(), vec![SpeakerArrangement::STEREO]);
        let (inputs, outputs) = requested_arrangements(
            Some(&current),
            Vst3Layout {
                input_channels: 0,
                output_channels: 2,
            },
        )
        .expect("output-only request is representable");
        assert!(inputs.is_empty());
        assert_eq!(outputs.len(), 1);
    }

    #[test]
    fn an_input_request_is_skipped_when_the_plugin_has_no_input_bus() {
        let current = arrangements(Vec::new(), vec![SpeakerArrangement::STEREO]);
        assert!(
            requested_arrangements(
                Some(&current),
                Vst3Layout {
                    input_channels: 2,
                    output_channels: 2,
                }
            )
            .is_none()
        );
    }

    #[test]
    fn stereo_inputs_preserve_distinct_channels() {
        let inputs = [vec![1.0, 0.5], vec![-1.0, 0.25]];
        let mut buffers = AudioBuffers::new(2, 2, 2, PLUGIN_SAMPLE_RATE);
        copy_stereo_to_vst3_inputs(2, &inputs, &mut buffers);
        assert_eq!(buffers.inputs[0], inputs[0]);
        assert_eq!(buffers.inputs[1], inputs[1]);
    }

    #[test]
    fn mono_input_downmixes_both_channels() {
        let inputs = [vec![1.0, 0.5], vec![-1.0, 0.25]];
        let mut buffers = AudioBuffers::new(1, 2, 2, PLUGIN_SAMPLE_RATE);
        copy_stereo_to_vst3_inputs(1, &inputs, &mut buffers);
        assert_eq!(buffers.inputs[0], vec![0.0, 0.375]);
    }

    #[test]
    fn a_generator_gets_no_input_at_all() {
        let inputs = [vec![1.0, 0.5], vec![-1.0, 0.25]];
        let mut buffers = AudioBuffers::new(0, 2, 2, PLUGIN_SAMPLE_RATE);
        copy_stereo_to_vst3_inputs(0, &inputs, &mut buffers);
        assert!(buffers.inputs.is_empty());
    }

    #[test]
    fn stereo_outputs_preserve_distinct_channels() {
        let mut buffers = AudioBuffers::new(1, 2, 2, PLUGIN_SAMPLE_RATE);
        buffers.outputs[0].copy_from_slice(&[0.1, 0.2]);
        buffers.outputs[1].copy_from_slice(&[-0.1, -0.2]);
        let mut outputs = [vec![0.0; 2], vec![0.0; 2]];
        copy_vst3_outputs_to_stereo(&buffers, &mut outputs);
        assert_eq!(outputs[0], vec![0.1, 0.2]);
        assert_eq!(outputs[1], vec![-0.1, -0.2]);
    }

    #[test]
    fn mono_output_is_centred_not_left_only() {
        let mut buffers = AudioBuffers::new(2, 1, 2, PLUGIN_SAMPLE_RATE);
        buffers.outputs[0].copy_from_slice(&[0.1, 0.2]);
        let mut outputs = [vec![0.0; 2], vec![0.0; 2]];
        copy_vst3_outputs_to_stereo(&buffers, &mut outputs);
        assert_eq!(outputs[0], vec![0.1, 0.2]);
        assert_eq!(outputs[1], vec![0.1, 0.2]);
    }

    #[test]
    fn copies_degrade_instead_of_panicking_on_a_length_mismatch() {
        let mut target = vec![9.0; 4];
        copy_clamped(&[1.0, 2.0], &mut target);
        assert_eq!(target, vec![1.0, 2.0, 0.0, 0.0]);
        let mut short = vec![9.0; 1];
        copy_clamped(&[1.0, 2.0, 3.0], &mut short);
        assert_eq!(short, vec![1.0]);
    }
}
