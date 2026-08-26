//! Pubsplash's own effects, which run on a single source.
//!
//! Distinct from `audio::fx_chain`, which hosts third-party VST plugins on a
//! bus. Everything here is owned, plain DSP with no plugin behind it, which is
//! what makes it safe to drop on the audio thread and cheap enough to leave in
//! circuit doing nothing — a hosted plugin is neither.
//!
//! A source's effects run in `mix_one_block`'s second pass, after the source's
//! own volume and mute and before it reaches master or any bus send. So they
//! are inside everything downstream, and in particular they come before any
//! effect on a bus the source is sent to.
//!
//! Adding an effect is a variant here, a module beside this one, and an arm in
//! [`ActiveEffect::process`] — plus the config and UI halves in
//! `config::EffectConfig` and `ui::source_dialog`.

pub mod ducker;

pub use ducker::{Ducker, DuckerSpec};

/// One effect as the engine holds it, carrying whatever state it needs between
/// blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum ActiveEffect {
    Ducker(Ducker),
}

/// One effect's settings, as the UI hands them down.
///
/// Separate from [`ActiveEffect`] because a spec is inert data the UI thread
/// builds and compares, while an `ActiveEffect` owns running state that must
/// never be rebuilt just because a setting moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectSpec {
    Ducker(DuckerSpec),
}

impl EffectSpec {
    /// Whether two specs describe the same effect, so state can be carried
    /// across a rebuild rather than restarted.
    fn same_kind(&self, other: &ActiveEffect) -> bool {
        matches!(
            (self, other),
            (EffectSpec::Ducker(_), ActiveEffect::Ducker(_))
        )
    }
}

impl ActiveEffect {
    pub fn new(spec: EffectSpec) -> Self {
        match spec {
            EffectSpec::Ducker(spec) => ActiveEffect::Ducker(Ducker::new(spec)),
        }
    }

    /// Takes new settings, keeping running state.
    fn adopt(&mut self, spec: EffectSpec) {
        match (self, spec) {
            (ActiveEffect::Ducker(ducker), EffectSpec::Ducker(spec)) => ducker.adopt(spec),
        }
    }

    /// Runs one block. Engine thread only.
    ///
    /// `levels` is every source's post-fader RMS for this block, indexed as the
    /// engine's source list is — see [`ducker`] for why an effect is handed
    /// levels rather than other sources' audio.
    pub fn process(&mut self, block: &mut [f32], levels: &[f32]) {
        match self {
            ActiveEffect::Ducker(ducker) => ducker.process(block, levels),
        }
    }
}

/// Rebuilds a source's effects from `specs`, carrying surviving effects' state
/// across.
///
/// The same problem `rebuild_sends` solves, and for the same reason: a
/// `SetRouting` rebuilds every source from scratch, and those arrive for
/// reasons that have nothing to do with effects — the two-second application
/// poll re-syncs whenever a captured program's pid moves. Building fresh
/// effects each time would reset a ducker's gain to unity mid-duck, so the
/// music would jump back to full while someone was still talking.
///
/// Matched by position and kind rather than by identity: there is no identity
/// to match on, an effect being nothing but its settings. A slot that changed
/// kind, or that has no counterpart, is genuinely new and starts fresh.
pub fn rebuild(old: Vec<ActiveEffect>, specs: &[EffectSpec]) -> Vec<ActiveEffect> {
    let mut old = old;
    let mut rebuilt = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        match old.get_mut(index).filter(|e| spec.same_kind(e)) {
            Some(existing) => {
                existing.adopt(*spec);
                rebuilt.push(existing.clone());
            }
            None => rebuilt.push(ActiveEffect::new(*spec)),
        }
    }
    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer::BLOCK_SAMPLES;

    fn spec(duck_to: u32) -> EffectSpec {
        EffectSpec::Ducker(DuckerSpec::new(Some(0), duck_to, 5, 20, 300, 250))
    }

    /// The whole point of `rebuild`: a re-sync that has nothing to do with the
    /// effect must not undo a duck that is in progress.
    #[test]
    fn a_rebuild_carries_a_duck_in_progress_across() {
        let mut effects = vec![ActiveEffect::new(spec(25))];
        for _ in 0..6 {
            effects[0].process(&mut vec![1.0f32; BLOCK_SAMPLES], &[0.5]);
        }
        let ActiveEffect::Ducker(before) = &effects[0];
        let ducked = before.clone();

        let rebuilt = rebuild(effects, &[spec(25)]);
        let ActiveEffect::Ducker(after) = &rebuilt[0];
        assert_eq!(after, &ducked, "the same running state, not a fresh one");

        // And it is still ducking, rather than having snapped back to unity.
        let mut effects = rebuilt;
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        effects[0].process(&mut block, &[0.5]);
        assert!(block[0] < 0.3, "still ducked, got {}", block[0]);
    }

    #[test]
    fn a_rebuild_applies_the_new_settings() {
        let effects = vec![ActiveEffect::new(spec(25))];
        let rebuilt = rebuild(effects, &[spec(40)]);
        let ActiveEffect::Ducker(ducker) = &rebuilt[0];
        assert!((ducker.spec().duck_to - 0.4).abs() < 1e-6);
    }

    #[test]
    fn added_and_removed_slots_are_handled() {
        let effects = vec![ActiveEffect::new(spec(25))];
        let grown = rebuild(effects, &[spec(25), spec(50)]);
        assert_eq!(grown.len(), 2);
        let shrunk = rebuild(grown, &[]);
        assert!(shrunk.is_empty());
    }
}
