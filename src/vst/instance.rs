//! One live plugin, of either format, behind a single interface.
//!
//! The formats differ in how they take audio — VST2 wants raw per-channel
//! pointers, VST3 wants owned buffers — so [`PluginInstance::process`] is the
//! only place that knows which is which. It returns a [`Processed`] rather
//! than a bool per format: a chain must never be able to leave its output
//! buffer unwritten and promote the previous plugin's block as this one's.
//!
//! Threading follows the stricter of the two contracts (see `host3.rs`):
//! everything except [`PluginInstance::process`] is UI-thread only.

use super::host2::Vst2Plugin;
use super::host3::Vst3Plugin;
use crate::audio::mixer::CHANNELS;
use crate::config::{ParamValue, PluginRef};
use std::sync::Arc;

/// Deliberately not `Clone`: every owner holds an `Arc<PluginInstance>`, and
/// cloning the *inner* `Arc` instead would hand the engine an owning reference
/// the `FxRuntime.retiring` bookkeeping does not track — letting the audio
/// thread become the last owner and run COM teardown.
pub enum PluginInstance {
    Vst2(Arc<Vst2Plugin>),
    Vst3(Arc<Vst3Plugin>),
}

/// What a plugin did with a block, and therefore how much of its output the
/// chain should mix in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Processed {
    /// `out` holds the plugin's output; keep it fully in circuit.
    Wet,
    /// `out` holds the plugin's output, but the plugin is about to become
    /// unavailable (a UI operation is waiting to take it). Fade it out of
    /// circuit now, while there is still real audio to fade.
    Retiring,
    /// The plugin could not run this block and `out` was not written.
    Passthrough,
}

/// How a plugin's output combines with the signal already in the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixMode {
    /// An effect: its output replaces what came in.
    Replace,
    /// A generator with no audio input (an instrument, a tone source): its
    /// output is summed with what came in, so it adds to the bus rather than
    /// silencing everything ahead of it.
    Add,
}

/// Per-chain scratch for the VST2 pointer plumbing, allocated once so nothing
/// here allocates on the audio thread.
pub struct PtrScratch {
    in_ptrs: Vec<*mut f32>,
    out_ptrs: Vec<*mut f32>,
    /// `(L+R)/2` for a mono-input plugin.
    mono: Vec<f32>,
    /// Fed to plugin inputs beyond the two we have; never read back.
    silence: Vec<f32>,
    /// Absorbs plugin outputs beyond the two we keep.
    junk: Vec<f32>,
}

impl PtrScratch {
    pub fn new(max_io: usize, frames: usize) -> Self {
        PtrScratch {
            in_ptrs: vec![std::ptr::null_mut(); max_io.max(CHANNELS)],
            out_ptrs: vec![std::ptr::null_mut(); max_io.max(CHANNELS)],
            mono: vec![0.0; frames],
            silence: vec![0.0; frames],
            junk: vec![0.0; frames],
        }
    }
}

impl PluginInstance {
    pub fn info(&self) -> &PluginRef {
        match self {
            Self::Vst2(plugin) => &plugin.info,
            Self::Vst3(plugin) => &plugin.info,
        }
    }

    pub fn num_inputs(&self) -> i32 {
        match self {
            Self::Vst2(plugin) => plugin.num_inputs,
            Self::Vst3(plugin) => plugin.num_inputs(),
        }
    }

    pub fn num_outputs(&self) -> i32 {
        match self {
            Self::Vst2(plugin) => plugin.num_outputs,
            Self::Vst3(plugin) => plugin.num_outputs(),
        }
    }

    pub fn mix_mode(&self) -> MixMode {
        match self {
            Self::Vst2(_) => MixMode::Replace,
            Self::Vst3(plugin) if plugin.is_generator() => MixMode::Add,
            Self::Vst3(_) => MixMode::Replace,
        }
    }

    pub fn num_params(&self) -> i32 {
        match self {
            Self::Vst2(plugin) => plugin.num_params,
            Self::Vst3(plugin) => plugin.num_params(),
        }
    }

    pub fn get_parameter(&self, index: i32) -> f32 {
        match self {
            Self::Vst2(plugin) => plugin.get_parameter(index),
            Self::Vst3(plugin) => plugin.get_parameter(index),
        }
    }

    pub fn set_parameter(&self, index: i32, value: f32) {
        match self {
            Self::Vst2(plugin) => plugin.set_parameter(index, value),
            Self::Vst3(plugin) => plugin.set_parameter(index, value),
        }
    }

    pub fn param_name(&self, index: i32) -> String {
        match self {
            Self::Vst2(plugin) => plugin.param_name(index),
            Self::Vst3(plugin) => plugin.param_name(index),
        }
    }

    pub fn param_display(&self, index: i32) -> String {
        match self {
            Self::Vst2(plugin) => plugin.param_display(index),
            Self::Vst3(plugin) => plugin.param_display(index),
        }
    }

    pub fn param_label(&self, index: i32) -> String {
        match self {
            Self::Vst2(plugin) => plugin.param_label(index),
            Self::Vst3(plugin) => plugin.param_label(index),
        }
    }

    pub fn can_be_automated(&self, index: i32) -> bool {
        match self {
            Self::Vst2(plugin) => plugin.can_be_automated(index),
            Self::Vst3(plugin) => plugin.can_be_automated(index),
        }
    }

    pub fn string_to_parameter(&self, index: i32, text: &str) -> bool {
        match self {
            Self::Vst2(plugin) => plugin.string_to_parameter(index, text),
            Self::Vst3(plugin) => plugin.string_to_parameter(index, text),
        }
    }

    /// Re-reads the plugin's parameter list, for formats that can change it at
    /// runtime. Call before showing parameters to the user.
    pub fn refresh_params(&self) {
        match self {
            Self::Vst2(_) => {}
            Self::Vst3(plugin) => plugin.refresh_params(),
        }
    }

    pub fn has_editor(&self) -> bool {
        match self {
            Self::Vst2(plugin) => plugin.has_editor,
            Self::Vst3(plugin) => plugin.has_editor(),
        }
    }

    pub fn editor_rect(&self) -> Option<(i32, i32)> {
        match self {
            Self::Vst2(plugin) => plugin.editor_rect(),
            Self::Vst3(plugin) => plugin.editor_rect(),
        }
    }

    pub fn editor_open(&self, hwnd: *mut std::ffi::c_void) -> bool {
        match self {
            Self::Vst2(plugin) => {
                plugin.editor_open(hwnd);
                true
            }
            Self::Vst3(plugin) => plugin.editor_open(hwnd),
        }
    }

    pub fn editor_close(&self) {
        match self {
            Self::Vst2(plugin) => plugin.editor_close(),
            Self::Vst3(plugin) => plugin.editor_close(),
        }
    }

    pub fn editor_idle(&self) {
        match self {
            Self::Vst2(plugin) => plugin.editor_idle(),
            Self::Vst3(plugin) => plugin.editor_idle(),
        }
    }

    pub fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        match self {
            Self::Vst2(_) => None,
            Self::Vst3(plugin) => plugin.take_editor_resize_request(),
        }
    }

    /// Drains parameter edits the plugin's own editor reported, which some
    /// formats stash on the audio thread. Call once a tick per open editor.
    pub fn drain_editor_edits(&self) {
        match self {
            Self::Vst2(_) => {}
            Self::Vst3(plugin) => plugin.drain_editor_edits(),
        }
    }

    /// Follows the slot's bypass state, so a plugin gets its "flush tails"
    /// hook instead of resuming with stale state.
    pub fn set_processing(&self, on: bool) {
        match self {
            Self::Vst2(_) => {}
            Self::Vst3(plugin) => plugin.set_processing(on),
        }
    }

    pub fn effect_id(&self) -> usize {
        match self {
            Self::Vst2(plugin) => plugin.effect_id(),
            Self::Vst3(plugin) => plugin.effect_id(),
        }
    }

    /// The plugin's state for the config file, or `None` when there is nothing
    /// worth writing — callers must leave the stored settings alone rather
    /// than overwrite them with a failure.
    pub fn snapshot(&self) -> Option<(Option<String>, Vec<ParamValue>)> {
        match self {
            Self::Vst2(plugin) => Some(plugin.snapshot()),
            Self::Vst3(plugin) => plugin.snapshot(),
        }
    }

    /// Runs one block. Engine thread only, and never concurrently with itself.
    ///
    /// `dry` is the signal coming in and `out` receives the plugin's output.
    /// `out` is only meaningful when the result is not
    /// [`Processed::Passthrough`].
    pub fn process(
        &self,
        dry: &mut [Vec<f32>; CHANNELS],
        out: &mut [Vec<f32>; CHANNELS],
        scratch: &mut PtrScratch,
    ) -> Processed {
        match self {
            Self::Vst2(plugin) => {
                let frames = out[0].len().min(dry[0].len());
                let n_in = (plugin.num_inputs.max(0) as usize).min(scratch.in_ptrs.len());
                let n_out = (plugin.num_outputs.max(0) as usize).min(scratch.out_ptrs.len());

                // A mono-input plugin gets the same (L+R)/2 downmix the VST3
                // path uses, rather than the left channel alone.
                if n_in == 1 {
                    for (mono, (left, right)) in scratch.mono[..frames]
                        .iter_mut()
                        .zip(dry[0].iter().zip(&dry[1]))
                    {
                        *mono = (left + right) * 0.5;
                    }
                }

                let mono = scratch.mono.as_mut_ptr();
                let silence = scratch.silence.as_mut_ptr();
                let junk = scratch.junk.as_mut_ptr();
                let (left_in, right_in) = dry.split_at_mut(1);
                let (left_out, right_out) = out.split_at_mut(1);
                for (i, slot) in scratch.in_ptrs[..n_in].iter_mut().enumerate() {
                    *slot = match i {
                        0 if n_in == 1 => mono,
                        0 => left_in[0].as_mut_ptr(),
                        1 => right_in[0].as_mut_ptr(),
                        _ => silence,
                    };
                }
                for (o, slot) in scratch.out_ptrs[..n_out].iter_mut().enumerate() {
                    *slot = match o {
                        0 => left_out[0].as_mut_ptr(),
                        1 => right_out[0].as_mut_ptr(),
                        _ => junk,
                    };
                }

                // SAFETY: the pointer arrays are exactly `n_in`/`n_out` long,
                // clamped to the scratch the chain sized to this plugin, and
                // every one of them points at a buffer of at least `frames`
                // f32s owned by the calling chain. Engine thread only, per the
                // host2 threading contract.
                unsafe {
                    plugin.process_replacing(
                        scratch.in_ptrs.as_ptr(),
                        scratch.out_ptrs.as_ptr(),
                        frames as i32,
                    );
                }

                // Mono-out plugin: mirror its single output to both channels.
                if n_out == 1 {
                    right_out[0][..frames].copy_from_slice(&left_out[0][..frames]);
                }
                Processed::Wet
            }
            Self::Vst3(plugin) => plugin.process_stereo(dry, out),
        }
    }
}

/// `FxChain` needs `unsafe impl Send` for its raw pointer scratch, which would
/// otherwise mask a plugin that stopped being thread-safe. Keep that checked
/// here, where it is about the plugin rather than about the pointers.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PluginInstance>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_is_never_narrower_than_the_stereo_bus() {
        let scratch = PtrScratch::new(0, 480);
        assert_eq!(scratch.in_ptrs.len(), CHANNELS);
        assert_eq!(scratch.out_ptrs.len(), CHANNELS);
        assert_eq!(scratch.mono.len(), 480);
    }
}
