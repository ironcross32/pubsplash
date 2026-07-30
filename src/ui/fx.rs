//! FX chain lifecycle on the UI side: instantiating plugins, keeping the
//! live instances (`App.fx`) in lockstep with `config.buses`, and pushing the
//! resulting chains to the audio engine.
//!
//! Plugin instances are format-neutral `PluginInstance`s. The UI registry holds
//! one Arc per live instance; the engine holds clones inside its chains.
//! Replaced engine chains are returned through its retirement channel and
//! reclaimed on the UI thread after `EngineEvent::BusesApplied`.
//!
//! Releasing the UI's own reference is deferred the same way, through
//! [`orphan`]. Dropping the last one runs the plugin's teardown — arbitrary
//! third-party code that commonly pumps the Windows message queue, re-entering
//! wx handlers and, from there, this module. Doing that with `app.fx` mutably
//! borrowed panics the `RefCell`, and wxdragon swallows the panic, so the only
//! symptom is a chain edit that half happened. Every structural edit therefore
//! moves the instances out first and lets the pump release them.

use super::App;
use crate::audio::fx_chain::{FxChain, FxUnit};
use crate::audio::{BusSpec, EngineCommand, RoutingUpdate};
use crate::config::{FxSlotConfig, PluginRef};
use crate::vst::host2::Vst2Plugin;
use crate::vst::host3::Vst3Plugin;
use crate::vst::{PluginFormat, PluginInstance};
use std::rc::Rc;
use std::sync::Arc;

/// Which chain a chain-editing action targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainTarget {
    Bus(usize),
    Master,
}

/// Why a configured slot has no live plugin. "Not installed" and "installed
/// but would not load" want different words in front of the user: telling
/// someone to install a plugin they already have, when the real problem is
/// that its output bus is 5.1, sends them looking in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotError {
    /// Nothing in the plugin cache matches this reference.
    NotInstalled,
    /// The plugin was found but could not be hosted, with the reason.
    LoadFailed(String),
}

/// One slot that could not be filled, for the summary dialogs.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotFailure {
    pub plugin: PluginRef,
    pub error: SlotError,
}

impl SlotFailure {
    /// A single bullet naming the plugin and, when we know it, why.
    pub fn describe(&self) -> String {
        match &self.error {
            SlotError::NotInstalled => self.plugin.display(),
            SlotError::LoadFailed(reason) => format!("{}: {reason}", self.plugin.display()),
        }
    }
}

/// Instantiates one slot's plugin.
fn load_slot(app: &Rc<App>, slot: &FxSlotConfig) -> Result<Arc<PluginInstance>, SlotError> {
    let info = {
        let cache = app.plugins.borrow();
        match slot.plugin.resolve(&cache) {
            Some(info) => info.clone(),
            None => {
                log::warn!("Plugin {} is not in the scan cache", slot.plugin.display());
                return Err(SlotError::NotInstalled);
            }
        }
    };
    let instance = match info.format {
        PluginFormat::Vst2 => Vst2Plugin::load(&info, slot).map(PluginInstance::Vst2),
        PluginFormat::Vst3 => Vst3Plugin::load(&info, slot).map(PluginInstance::Vst3),
    };
    instance.map(Arc::new).map_err(|e| {
        log::error!("Failed to load plugin {}: {e}", info.path);
        SlotError::LoadFailed(e)
    })
}

/// Instantiates every configured chain at startup, filling `App.fx`. Returns
/// the slots that could not be filled, de-duplicated, for a single summary
/// dialog. Config is never rewritten — failed slots stay as gaps and come back
/// when the problem is fixed and the plugins are rescanned.
pub fn instantiate_all(app: &Rc<App>) -> Vec<SlotFailure> {
    let mut failures: Vec<SlotFailure> = Vec::new();
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

    let load_chain = |app: &Rc<App>, slots: &[FxSlotConfig], failures: &mut Vec<SlotFailure>| {
        slots
            .iter()
            .map(|slot| match load_slot(app, slot) {
                Ok(instance) => Some(instance),
                Err(error) => {
                    let failure = SlotFailure {
                        plugin: slot.plugin.clone(),
                        error,
                    };
                    // The same plugin on two buses is one problem, not two.
                    if !failures.contains(&failure) {
                        failures.push(failure);
                    }
                    None
                }
            })
            .collect::<Vec<_>>()
    };

    let mut fx = app.fx.borrow_mut();
    fx.buses = bus_chains
        .iter()
        .map(|slots| load_chain(app, slots, &mut failures))
        .collect();
    fx.master = load_chain(app, &master_chain, &mut failures);
    drop(fx);

    failures
}

/// Where a config slot sits in the engine chain built from `instances`.
///
/// [`build_chain`] skips slots whose plugin is missing, so the two numberings
/// are not the same one: with a not-installed plugin at slot 0, config slot 1
/// is chain unit 0. Anything addressing a *unit* — `EngineCommand::SetFxBypass`
/// is the only one — has to translate, or it acts on the wrong plugin.
/// `None` means the slot has no live plugin and so no unit at all.
///
/// Generic only so it can be tested: a `PluginInstance` cannot be conjured up
/// without a real plugin on disk, and all this reads is which slots are filled.
fn chain_index<T>(instances: &[Option<T>], slot: usize) -> Option<usize> {
    instances.get(slot)?.as_ref()?;
    Some(instances[..slot].iter().flatten().count())
}

/// Builds an engine `FxChain` from the live instances and the config's
/// bypass flags. Skips `None` (missing) slots.
fn build_chain(instances: &[Option<Arc<PluginInstance>>], slots: &[FxSlotConfig]) -> FxChain {
    let units = instances
        .iter()
        .zip(slots.iter())
        .filter_map(|(instance, slot)| {
            instance
                .as_ref()
                .map(|plugin| FxUnit::new(plugin.clone(), slot.bypass))
        })
        .collect();
    FxChain::new(units)
}

/// Builds the current bus set (with live plugins), ready to hand to the engine.
pub fn bus_specs(app: &Rc<App>) -> Vec<BusSpec> {
    let monitors = app.run.borrow().monitors.clone();
    let config = app.config.borrow();
    let fx = app.fx.borrow();
    config
        .buses
        .buses
        .iter()
        .enumerate()
        .map(|(i, bus)| BusSpec {
            volume: bus.volume,
            muted: bus.muted,
            chain: build_chain(
                fx.buses.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
                &bus.chain,
            ),
            monitor: monitors.bus(i),
        })
        .collect()
}

/// Builds the current master chain (with live plugins).
pub fn master_chain_spec(app: &Rc<App>) -> FxChain {
    let config = app.config.borrow();
    let fx = app.fx.borrow();
    build_chain(&fx.master, &config.buses.master_chain)
}

/// Builds the current bus set and master chain, for the callers that change
/// both at once.
pub fn routing_specs(app: &Rc<App>) -> (Vec<BusSpec>, FxChain) {
    (bus_specs(app), master_chain_spec(app))
}

/// Pushes the current bus set and master chain (with live plugins) to the
/// engine. Call after any chain change. After a *bus* change use
/// `App::sync_engine_routing` instead, so the sources' send indices are
/// updated in the same command.
pub fn sync_engine_buses(app: &Rc<App>) {
    let (buses, master_chain) = routing_specs(app);
    app.engine
        .send(EngineCommand::SetRouting(Box::new(RoutingUpdate {
            sources: None,
            buses: Some(buses),
            master_chain: Some(master_chain),
        })));
}

/// Pushes just the chain the edit touched.
///
/// Rebuilding *every* chain for a one-plugin edit is not free and not
/// harmless: each replaced chain allocates fresh scratch, returns the old one
/// through the retirement channel, and — because `FxUnit::new` resets the
/// wet/dry gain — snaps every plugin that happened to be mid-fade back to its
/// endpoint. Editing a plugin on one bus should not audibly disturb the master.
fn sync_engine_chain(app: &Rc<App>, target: ChainTarget) {
    let update = match target {
        ChainTarget::Bus(_) => RoutingUpdate {
            buses: Some(bus_specs(app)),
            ..Default::default()
        },
        ChainTarget::Master => RoutingUpdate {
            master_chain: Some(master_chain_spec(app)),
            ..Default::default()
        },
    };
    app.engine.send(EngineCommand::SetRouting(Box::new(update)));
}

// --- Structural updates that keep `App.fx` aligned with `config.buses` ---

pub fn on_bus_added(app: &Rc<App>) {
    app.fx.borrow_mut().buses.push(Vec::new());
}

/// Parks instances taken out of the registry so the pump releases them, rather
/// than dropping them here. See the module docs.
fn orphan(app: &Rc<App>, instances: impl IntoIterator<Item = Option<Arc<PluginInstance>>>) {
    app.orphaned_plugins
        .borrow_mut()
        .extend(instances.into_iter().flatten());
}

/// Releases every parked instance. Called from the pump, where nothing is
/// borrowed, so a plugin whose teardown re-enters the UI finds it in one piece.
pub fn release_orphans(app: &Rc<App>) {
    let orphans = std::mem::take(&mut *app.orphaned_plugins.borrow_mut());
    if orphans.is_empty() {
        return;
    }
    log::info!("Releasing {} retired plugin instances", orphans.len());
    drop(orphans);
}

pub fn on_bus_removed(app: &Rc<App>, index: usize) {
    // Bus indices (and thus editor targets) shift; close open editors.
    super::fx_editor::close_all(app);
    // Monitoring is held by bus index too, so the same shift would leave it
    // pointing at a bus the user never chose.
    app.clear_bus_monitors();
    let removed = {
        let mut fx = app.fx.borrow_mut();
        (index < fx.buses.len()).then(|| fx.buses.remove(index))
    };
    orphan(app, removed.into_iter().flatten());
}

pub fn on_bus_moved(app: &Rc<App>, a: usize, b: usize) {
    super::fx_editor::close_all(app);
    app.clear_bus_monitors();
    let mut fx = app.fx.borrow_mut();
    if a < fx.buses.len() && b < fx.buses.len() {
        fx.buses.swap(a, b);
    }
}

// --- Chain editing (add / remove / move / bypass a plugin) ---

fn instances_mut<'a>(
    fx: &'a mut super::FxRuntime,
    target: ChainTarget,
) -> Option<&'a mut Vec<Option<Arc<PluginInstance>>>> {
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

/// Adds a plugin to the end of a chain, loading it immediately. On failure the
/// slot is still added as a config entry (so the user can see and remove it)
/// but will not process, and the reason comes back for the caller to show.
/// Saves, re-syncs the engine, and rebuilds the mixer.
pub fn add_plugin(app: &Rc<App>, target: ChainTarget, plugin: PluginRef) -> Result<(), SlotError> {
    let slot = FxSlotConfig {
        plugin,
        ..Default::default()
    };
    let loaded = load_slot(app, &slot);
    let outcome = loaded.as_ref().map(|_| ()).map_err(Clone::clone);
    with_slots_mut(app, target, |slots| slots.push(slot));
    if let Some(list) = instances_mut(&mut app.fx.borrow_mut(), target) {
        list.push(loaded.ok());
    }
    after_chain_edit(app, target);
    outcome
}

pub fn remove_plugin(app: &Rc<App>, target: ChainTarget, slot: usize) {
    // Editor slot indices would go stale; close any open editors first.
    super::fx_editor::close_all(app);
    with_slots_mut(app, target, |slots| {
        if slot < slots.len() {
            slots.remove(slot);
        }
    });
    let removed = {
        let mut fx = app.fx.borrow_mut();
        instances_mut(&mut fx, target)
            .filter(|list| slot < list.len())
            .map(|list| list.remove(slot))
    };
    orphan(app, removed);
    after_chain_edit(app, target);
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
        after_chain_edit(app, target);
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
    // The engine addresses chain *units*, and a slot whose plugin is missing
    // has none — so there is nothing to tell the engine, and the config flag
    // (which is what the list row shows) is the whole change.
    let unit = {
        let fx = app.fx.borrow();
        let instances = match target {
            ChainTarget::Bus(i) => fx.buses.get(i).map(|v| v.as_slice()),
            ChainTarget::Master => Some(fx.master.as_slice()),
        };
        instances.and_then(|list| chain_index(list, slot))
    };
    if let (Some(slot), Some(instance)) = (unit, instance_at(app, target, slot)) {
        // Order matters. Un-bypassing must start the plugin before the engine
        // is told to bring it back, or the first blocks fail; bypassing tells
        // the engine first so the chain's fade-out runs while the plugin is
        // still processing.
        if bypass {
            app.engine
                .send(EngineCommand::SetFxBypass { bus, slot, bypass });
            instance.set_processing(false);
        } else {
            instance.set_processing(true);
            app.engine
                .send(EngineCommand::SetFxBypass { bus, slot, bypass });
        }
    }
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
    // No snapshot means the plugin could not tell us anything this time.
    // Writing a placeholder here would replace the user's tuned settings in
    // config.json with defaults, so leave what is already stored alone.
    let Some((chunk, params)) = instance.snapshot() else {
        log::warn!(
            "Keeping the stored settings for {}: it reported no state to save",
            instance.info().display()
        );
        return;
    };
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
    let count = with_slots(app, target, |slots| slots.len()).unwrap_or(0);
    for slot in 0..count {
        snapshot_slot(app, target, slot);
    }
}

/// Replaces a chain wholesale: retires the old instances, instantiates the
/// new slots, updates config, and re-syncs the engine. Used when applying a
/// saved or imported chain.
pub fn replace_chain(app: &Rc<App>, target: ChainTarget, slots: Vec<FxSlotConfig>) {
    super::fx_editor::close_all(app);
    let old = {
        let mut fx = app.fx.borrow_mut();
        instances_mut(&mut fx, target)
            .map(std::mem::take)
            .unwrap_or_default()
    };
    orphan(app, old);
    let instances: Vec<_> = slots.iter().map(|slot| load_slot(app, slot).ok()).collect();
    with_slots_mut(app, target, |c| *c = slots);
    if let Some(list) = instances_mut(&mut app.fx.borrow_mut(), target) {
        *list = instances;
    }
    after_chain_edit(app, target);
}

/// Reads the live instance for a slot, if loaded.
pub fn instance_at(app: &Rc<App>, target: ChainTarget, slot: usize) -> Option<Arc<PluginInstance>> {
    let fx = app.fx.borrow();
    let list = match target {
        ChainTarget::Bus(i) => fx.buses.get(i),
        ChainTarget::Master => Some(&fx.master),
    };
    list.and_then(|l| l.get(slot)).and_then(|o| o.clone())
}

fn after_chain_edit(app: &Rc<App>, target: ChainTarget) {
    app.save_config();
    sync_engine_chain(app, target);
}

/// What a chain entry's live instance is doing, for its list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    Loaded,
    NotInstalled,
    LoadFailed,
}

/// The label for a chain entry in the FX list: "1. Name (VST3)", with bypass
/// and failure annotations. The format is part of the name because a plugin
/// commonly ships as both a VST2 and a VST3, and a screen-reader user has no
/// other way to tell which one is in the chain.
pub fn slot_label(index: usize, slot: &FxSlotConfig, status: SlotStatus) -> String {
    let mut label = format!("{}. {}", index + 1, slot.plugin.display());
    match status {
        SlotStatus::NotInstalled => label.push_str(" (not installed)"),
        SlotStatus::LoadFailed => label.push_str(" (failed to load)"),
        SlotStatus::Loaded if slot.bypass => label.push_str(" (bypassed)"),
        SlotStatus::Loaded => {}
    }
    label
}

/// Whether a slot's plugin exists in the scan cache. Distinguishes "you don't
/// have this plugin" from "you have it and it would not load" without keeping
/// a parallel error list in sync with the chains.
pub fn slot_status(app: &Rc<App>, target: ChainTarget, slot: usize, loaded: bool) -> SlotStatus {
    if loaded {
        return SlotStatus::Loaded;
    }
    let installed = with_slots(app, target, |slots| {
        let cache = app.plugins.borrow();
        slots
            .get(slot)
            .is_some_and(|s| s.plugin.resolve(&cache).is_some())
    })
    .unwrap_or(false);
    if installed {
        SlotStatus::LoadFailed
    } else {
        SlotStatus::NotInstalled
    }
}

/// Clones a target's chain. Prefer [`with_slots`] — `FxSlotConfig` owns
/// `chunk`, the plugin's serialized state, which is base64 and can run to
/// hundreds of kilobytes per plugin, so cloning a chain to read a name or a
/// flag is expensive out of all proportion to what it is for.
pub fn slots_for(app: &Rc<App>, target: ChainTarget) -> Vec<FxSlotConfig> {
    slots_of(app, target)
}

/// Reads a target's chain without cloning it. The read-only twin of
/// [`with_slots_mut`]; like it, the closure form is what keeps the `config`
/// borrow from overlapping an `app.widgets(...)` borrow at the call site.
pub fn with_slots<R>(
    app: &Rc<App>,
    target: ChainTarget,
    f: impl FnOnce(&[FxSlotConfig]) -> R,
) -> Option<R> {
    let config = app.config.borrow();
    match target {
        ChainTarget::Bus(i) => config.buses.buses.get(i).map(|b| f(&b.chain)),
        ChainTarget::Master => Some(f(&config.buses.master_chain)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Filled slots as `Some`, missing ones as `None` — the only thing
    /// `chain_index` looks at.
    fn slots(present: [bool; 4]) -> Vec<Option<()>> {
        present.iter().map(|p| p.then_some(())).collect()
    }

    #[test]
    fn every_slot_is_its_own_unit_when_nothing_is_missing() {
        let list = slots([true; 4]);
        for slot in 0..4 {
            assert_eq!(chain_index(&list, slot), Some(slot));
        }
    }

    #[test]
    fn a_missing_plugin_shifts_every_slot_after_it() {
        // A not-installed plugin at slot 0 and another at slot 2: the engine
        // chain holds only the two live ones, as units 0 and 1. Sending the
        // config index here is what bypassed the wrong plugin.
        let list = slots([false, true, false, true]);
        assert_eq!(chain_index(&list, 1), Some(0));
        assert_eq!(chain_index(&list, 3), Some(1));
    }

    #[test]
    fn a_slot_with_no_plugin_has_no_unit() {
        let list = slots([false, true, false, true]);
        assert_eq!(chain_index(&list, 0), None);
        assert_eq!(chain_index(&list, 2), None);
        // And neither has a slot past the end.
        assert_eq!(chain_index(&list, 9), None);
        assert_eq!(chain_index::<()>(&[], 0), None);
    }
}
