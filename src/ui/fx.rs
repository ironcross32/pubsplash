//! FX chain lifecycle on the UI side: instantiating plugins, keeping the
//! live instances (`App.fx`) in lockstep with `config.buses`, and pushing the
//! resulting chains to the audio engine.
//!
//! Plugin instances are `Arc<Vst2Plugin>`. The UI registry holds one Arc per
//! live instance; the engine holds clones inside its chains. Because a plugin
//! must be destroyed on the UI thread, removals move the Arc into
//! `FxRuntime.retiring` and it is dropped only after the engine acknowledges
//! the new bus set with `EngineEvent::BusesApplied`.

use super::App;
use crate::audio::fx_chain::{FxChain, FxUnit};
use crate::audio::{BusSpec, EngineCommand};
use crate::config::{FxSlotConfig, PluginRef};
use crate::vst::host2::Vst2Plugin;
use crate::vst::PluginFormat;
use std::rc::Rc;
use std::sync::Arc;

/// Which chain a chain-editing action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainTarget {
    Bus(usize),
    Master,
}

/// Instantiates one slot's plugin, or returns `None` if it can't be hosted
/// here (missing from the cache, or a format we don't process yet).
fn load_slot(app: &Rc<App>, slot: &FxSlotConfig) -> Option<Arc<Vst2Plugin>> {
    if slot.plugin.format != PluginFormat::Vst2 {
        // VST3 processing is a later milestone; identity is still stored.
        return None;
    }
    let cache = app.plugins.borrow();
    let info = slot.plugin.resolve(&cache)?.clone();
    drop(cache);
    match Vst2Plugin::load(&info, slot) {
        Ok(plugin) => Some(Arc::new(plugin)),
        Err(e) => {
            log::error!("Failed to load plugin {}: {e}", info.path);
            None
        }
    }
}

/// Instantiates every configured chain at startup, filling `App.fx`. Returns
/// the plugin references that could not be loaded, for a single summary
/// dialog. Config is never rewritten — missing plugins stay as gaps and come
/// back when installed and rescanned.
pub fn instantiate_all(app: &Rc<App>) -> Vec<PluginRef> {
    let mut missing = Vec::new();
    let (bus_chains, master_chain) = {
        let config = app.config.borrow();
        (
            config
                .buses
                .buses
                .iter()
                .map(|b| b.chain.clone())
                .collect::<Vec<_>>(),
            config.buses.master_chain.clone(),
        )
    };

    let load_chain = |app: &Rc<App>, slots: &[FxSlotConfig], missing: &mut Vec<PluginRef>| {
        slots
            .iter()
            .map(|slot| {
                let instance = load_slot(app, slot);
                if instance.is_none() {
                    missing.push(slot.plugin.clone());
                }
                instance
            })
            .collect::<Vec<_>>()
    };

    let mut fx = app.fx.borrow_mut();
    fx.buses = bus_chains
        .iter()
        .map(|slots| load_chain(app, slots, &mut missing))
        .collect();
    fx.master = load_chain(app, &master_chain, &mut missing);
    drop(fx);

    missing
}

/// Builds an engine `FxChain` from the live instances and the config's
/// bypass flags. Skips `None` (missing) slots.
fn build_chain(instances: &[Option<Arc<Vst2Plugin>>], slots: &[FxSlotConfig]) -> FxChain {
    let units = instances
        .iter()
        .zip(slots.iter())
        .filter_map(|(instance, slot)| {
            instance.as_ref().map(|plugin| FxUnit {
                plugin: plugin.clone(),
                bypass: slot.bypass,
            })
        })
        .collect();
    FxChain::new(units)
}

/// Pushes the current bus set and master chain (with live plugins) to the
/// engine. Call after any chain or bus change.
pub fn sync_engine_buses(app: &Rc<App>) {
    let config = app.config.borrow();
    let fx = app.fx.borrow();
    let mut specs = Vec::new();
    for (i, bus) in config.buses.buses.iter().enumerate() {
        let instances = fx.buses.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        specs.push(BusSpec {
            volume: bus.volume,
            muted: bus.muted,
            chain: build_chain(instances, &bus.chain),
        });
    }
    app.engine.send(EngineCommand::SetBuses(specs));
    app.engine.send(EngineCommand::SetMasterChain(build_chain(
        &fx.master,
        &config.buses.master_chain,
    )));
}

// --- Structural updates that keep `App.fx` aligned with `config.buses` ---

pub fn on_bus_added(app: &Rc<App>) {
    app.fx.borrow_mut().buses.push(Vec::new());
}

pub fn on_bus_removed(app: &Rc<App>, index: usize) {
    // Bus indices (and thus editor targets) shift; close open editors.
    super::fx_editor::close_all(app);
    let mut fx = app.fx.borrow_mut();
    if index < fx.buses.len() {
        let removed = fx.buses.remove(index);
        fx.retiring.extend(removed.into_iter().flatten());
    }
}

pub fn on_bus_moved(app: &Rc<App>, a: usize, b: usize) {
    super::fx_editor::close_all(app);
    let mut fx = app.fx.borrow_mut();
    if a < fx.buses.len() && b < fx.buses.len() {
        fx.buses.swap(a, b);
    }
}

// --- Chain editing (add / remove / move / bypass a plugin) ---

fn instances_mut<'a>(
    fx: &'a mut super::FxRuntime,
    target: ChainTarget,
) -> Option<&'a mut Vec<Option<Arc<Vst2Plugin>>>> {
    match target {
        ChainTarget::Bus(i) => fx.buses.get_mut(i),
        ChainTarget::Master => Some(&mut fx.master),
    }
}

/// Reads the config chain for a target.
fn slots_of(app: &Rc<App>, target: ChainTarget) -> Vec<FxSlotConfig> {
    let config = app.config.borrow();
    match target {
        ChainTarget::Bus(i) => config
            .buses
            .buses
            .get(i)
            .map(|b| b.chain.clone())
            .unwrap_or_default(),
        ChainTarget::Master => config.buses.master_chain.clone(),
    }
}

fn with_slots_mut<R>(
    app: &Rc<App>,
    target: ChainTarget,
    f: impl FnOnce(&mut Vec<FxSlotConfig>) -> R,
) -> Option<R> {
    let mut config = app.config.borrow_mut();
    match target {
        ChainTarget::Bus(i) => config.buses.buses.get_mut(i).map(|b| f(&mut b.chain)),
        ChainTarget::Master => Some(f(&mut config.buses.master_chain)),
    }
}

/// Adds a plugin to the end of a chain, loading it immediately. Returns true
/// if the plugin loaded (else it's still added as a config slot but won't
/// process). Saves, re-syncs the engine, and rebuilds the mixer.
pub fn add_plugin(app: &Rc<App>, target: ChainTarget, plugin: PluginRef) -> bool {
    let slot = FxSlotConfig {
        plugin,
        ..Default::default()
    };
    let instance = load_slot(app, &slot);
    let loaded = instance.is_some();
    with_slots_mut(app, target, |slots| slots.push(slot));
    if let Some(list) = instances_mut(&mut app.fx.borrow_mut(), target) {
        list.push(instance);
    }
    after_chain_edit(app);
    loaded
}

pub fn remove_plugin(app: &Rc<App>, target: ChainTarget, slot: usize) {
    // Editor slot indices would go stale; close any open editors first.
    super::fx_editor::close_all(app);
    with_slots_mut(app, target, |slots| {
        if slot < slots.len() {
            slots.remove(slot);
        }
    });
    {
        let mut fx = app.fx.borrow_mut();
        if let Some(list) = instances_mut(&mut fx, target) {
            if slot < list.len() {
                if let Some(instance) = list.remove(slot) {
                    fx.retiring.push(instance);
                }
            }
        }
    }
    after_chain_edit(app);
}

pub fn move_plugin(app: &Rc<App>, target: ChainTarget, slot: usize, towards_start: bool) -> bool {
    let other = if towards_start {
        slot.checked_sub(1)
    } else {
        Some(slot + 1)
    };
    let Some(other) = other else {
        return false;
    };
    super::fx_editor::close_all(app);
    let moved = with_slots_mut(app, target, |slots| {
        if slot < slots.len() && other < slots.len() {
            slots.swap(slot, other);
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if moved {
        if let Some(list) = instances_mut(&mut app.fx.borrow_mut(), target) {
            list.swap(slot, other);
        }
        after_chain_edit(app);
    }
    moved
}

pub fn set_bypass(app: &Rc<App>, target: ChainTarget, slot: usize, bypass: bool) {
    with_slots_mut(app, target, |slots| {
        if let Some(s) = slots.get_mut(slot) {
            s.bypass = bypass;
        }
    });
    // Bypass can be applied in place without rebuilding chains.
    let bus = match target {
        ChainTarget::Bus(i) => Some(i),
        ChainTarget::Master => None,
    };
    app.engine
        .send(EngineCommand::SetFxBypass { bus, slot, bypass });
    app.save_config();
}

/// Persists a slot's current plugin state (chunk or params) back into config.
pub fn snapshot_slot(app: &Rc<App>, target: ChainTarget, slot: usize) {
    let instance = {
        let fx = app.fx.borrow();
        let list = match target {
            ChainTarget::Bus(i) => fx.buses.get(i),
            ChainTarget::Master => Some(&fx.master),
        };
        list.and_then(|l| l.get(slot)).and_then(|o| o.clone())
    };
    let Some(instance) = instance else {
        return;
    };
    let (chunk, params) = instance.snapshot();
    with_slots_mut(app, target, |slots| {
        if let Some(s) = slots.get_mut(slot) {
            s.chunk = chunk;
            s.params = params;
        }
    });
    app.save_config();
}

/// Snapshots every loaded slot in a chain back into config (before saving or
/// exporting it).
pub fn snapshot_chain(app: &Rc<App>, target: ChainTarget) {
    let count = slots_of(app, target).len();
    for slot in 0..count {
        snapshot_slot(app, target, slot);
    }
}

/// Replaces a chain wholesale: retires the old instances, instantiates the
/// new slots, updates config, and re-syncs the engine. Used when applying a
/// saved or imported chain.
pub fn replace_chain(app: &Rc<App>, target: ChainTarget, slots: Vec<FxSlotConfig>) {
    super::fx_editor::close_all(app);
    {
        let mut fx = app.fx.borrow_mut();
        if let Some(list) = instances_mut(&mut fx, target) {
            let old = std::mem::take(list);
            fx.retiring.extend(old.into_iter().flatten());
        }
    }
    let instances: Vec<_> = slots.iter().map(|slot| load_slot(app, slot)).collect();
    with_slots_mut(app, target, |c| *c = slots);
    if let Some(list) = instances_mut(&mut app.fx.borrow_mut(), target) {
        *list = instances;
    }
    after_chain_edit(app);
}

/// Reads the live instance for a slot, if loaded.
pub fn instance_at(
    app: &Rc<App>,
    target: ChainTarget,
    slot: usize,
) -> Option<Arc<Vst2Plugin>> {
    let fx = app.fx.borrow();
    let list = match target {
        ChainTarget::Bus(i) => fx.buses.get(i),
        ChainTarget::Master => Some(&fx.master),
    };
    list.and_then(|l| l.get(slot)).and_then(|o| o.clone())
}

fn after_chain_edit(app: &Rc<App>) {
    app.save_config();
    sync_engine_buses(app);
}

/// The label for a chain entry in the FX list: "1. Name", with bypass/missing
/// annotations. `loaded` is whether the live instance exists.
pub fn slot_label(index: usize, slot: &FxSlotConfig, loaded: bool) -> String {
    let mut label = format!("{}. {}", index + 1, slot.plugin.name);
    if !loaded {
        label.push_str(" (missing)");
    } else if slot.bypass {
        label.push_str(" (bypassed)");
    }
    label
}

pub fn slots_for(app: &Rc<App>, target: ChainTarget) -> Vec<FxSlotConfig> {
    slots_of(app, target)
}
