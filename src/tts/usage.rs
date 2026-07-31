//! What each speech engine has been asked to do this session.
//!
//! The API tab's data. Held in a process-global store rather than in `Runtime`
//! because the two things that write it — the `tts-net` worker and the SAPI
//! apartment thread — are not the UI thread and must not touch `App`. The UI
//! reads it by polling [`generation`] from the pump, exactly as it already
//! polls [`super::catalog::generation`]; that saves threading another channel
//! through [`super::speaker::Speaker`] for something the user looks at rarely.
//!
//! Nothing here is persisted. "This session" is the whole scope: the store
//! starts empty on every launch, and an engine appears in it only once it has
//! actually been asked to speak.
//!
//! **Credits are not a general concept.** Of the nine engines only ElevenLabs
//! publishes a balance reachable with the credentials the app stores (see
//! [`fetch_balance`]); Azure, Google and Polly keep theirs behind entirely
//! different APIs, and Edge, Google Translate, Star and SAPI have no account at
//! all. So [`EngineUsage::balance`] is `None` for almost everything, and the UI
//! says so rather than showing a zero that would read as "no credits left".

use super::engine::TtsError;
use super::engines;
use crate::config::SpeechConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{OnceLock, RwLock};

/// A provider-reported credit balance.
///
/// Character-denominated, because the one provider that reports one bills in
/// characters. `used`/`limit` are as the provider gave them, not a delta from
/// when Pubsplash started — it is the number the user sees on their dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Balance {
    pub used: u64,
    pub limit: u64,
    /// Plan name ("free", "creator"...), empty if the provider did not say.
    pub tier: String,
    /// When the allowance resets, as a Unix timestamp, if the provider said.
    pub resets_unix: Option<i64>,
}

impl Balance {
    /// Characters left before the allowance runs out. Saturating: a provider
    /// that reports an overage would otherwise wrap to something enormous.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }
}

/// One engine's session tally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineUsage {
    /// Stable id from [`engines::ALL`].
    pub engine: &'static str,
    /// Utterances handed to the engine, successful or not.
    pub requests: u64,
    /// Characters submitted, counted *after* `SynthRequest::truncated` — the
    /// figure a per-character biller would charge for.
    pub characters: u64,
    pub failures: u64,
    /// Models seen this session. Empty for engines with no model concept
    /// (Azure, Google, Edge, Google Translate, Star, SAPI), which is why the
    /// UI distinguishes "none used" from "this engine has no models".
    pub models: BTreeSet<String>,
    /// Voice *ids*, not labels. ElevenLabs' are opaque keys; the UI resolves
    /// them through `crate::tts::cached_voice_label` at display time so a
    /// catalog refresh improves old rows.
    pub voices: BTreeSet<String>,
    /// Monotonic use counter, for the reverse-chronological sort. A counter
    /// rather than an `Instant` so ordering is exact and testable.
    pub last_used: u64,
    pub balance: Option<Balance>,
}

impl EngineUsage {
    fn new(engine: &'static str) -> Self {
        Self {
            engine,
            requests: 0,
            characters: 0,
            failures: 0,
            models: BTreeSet::new(),
            voices: BTreeSet::new(),
            last_used: 0,
            balance: None,
        }
    }

    /// The engine's name as the picker and mixer show it.
    pub fn display_name(&self) -> &'static str {
        engines::display_name(self.engine)
    }
}

#[derive(Default)]
struct UsageState {
    engines: BTreeMap<&'static str, EngineUsage>,
    /// Bumped by every mutation; the UI pump compares it against its own copy.
    generation: u64,
    /// Handed out as `last_used`, so ordering never depends on the clock.
    sequence: u64,
}

static STATE: OnceLock<RwLock<UsageState>> = OnceLock::new();

fn state() -> &'static RwLock<UsageState> {
    STATE.get_or_init(|| RwLock::new(UsageState::default()))
}

/// Records one utterance. Called from the speech workers, never the UI thread.
///
/// `characters` is the post-truncation length. `model` and `voice` are what the
/// engine resolved the request to — see [`super::engine::SpeechEngine::usage_model`]
/// — so a source left on its defaults still records the model that was billed.
pub fn record(
    engine: &str,
    characters: usize,
    model: Option<String>,
    voice: Option<String>,
    failed: bool,
) {
    let id = engines::resolve_id(engine);
    let Ok(mut state) = state().write() else {
        return;
    };
    state.sequence += 1;
    state.generation += 1;
    let sequence = state.sequence;
    let entry = state
        .engines
        .entry(id)
        .or_insert_with(|| EngineUsage::new(id));
    entry.requests += 1;
    entry.characters += characters as u64;
    if failed {
        entry.failures += 1;
    }
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        entry.models.insert(model.trim().to_string());
    }
    if let Some(voice) = voice.filter(|v| !v.trim().is_empty()) {
        entry.voices.insert(voice.trim().to_string());
    }
    entry.last_used = sequence;
    drop(state);
    wxdragon::wake_up_idle();
}

/// Attaches a freshly fetched balance.
///
/// Only ever called for an engine already in the store: the refresh walks
/// [`snapshot`], so a balance can never be the thing that makes an unused
/// engine appear on the tab.
pub fn set_balance(engine: &'static str, balance: Balance) {
    let Ok(mut state) = state().write() else {
        return;
    };
    if let Some(entry) = state.engines.get_mut(engine) {
        entry.balance = Some(balance);
        state.generation += 1;
    }
    drop(state);
    wxdragon::wake_up_idle();
}

/// Every engine used this session, most recently used first.
pub fn snapshot() -> Vec<EngineUsage> {
    let Ok(state) = state().read() else {
        return Vec::new();
    };
    let mut usage: Vec<EngineUsage> = state.engines.values().cloned().collect();
    usage.sort_by_key(|entry| std::cmp::Reverse(entry.last_used));
    usage
}

/// Bumped by every mutation, so the pump can tell whether to redraw.
pub fn generation() -> u64 {
    state().read().map(|state| state.generation).unwrap_or(0)
}

/// Forgets everything. Tests only — the app has no "clear usage" affordance,
/// since a session's totals losing their meaning halfway through it would make
/// the tab useless.
#[cfg(test)]
fn reset() {
    if let Ok(mut state) = state().write() {
        *state = UsageState::default();
    }
}

/// Whether this engine has a balance worth asking the provider for.
///
/// Drives both the refresh loop and the UI's "unavailable" wording, so the two
/// can never disagree about which engines were even tried.
pub fn reports_balance(engine: &str) -> bool {
    engines::resolve_id(engine) == engines::ELEVENLABS
}

/// Asks a provider what is left on the account.
///
/// Blocking; runs on [`start_balance_refresh`]'s thread. Adding a provider that
/// grows a usable credit endpoint is one arm here plus its client function,
/// matching [`super::catalog::discover`].
pub fn fetch_balance(engine: &str, speech: &SpeechConfig) -> Result<Balance, TtsError> {
    match engines::resolve_id(engine) {
        engines::ELEVENLABS => super::engines::elevenlabs::subscription(speech),
        _ => Err(TtsError::Other(
            "This provider reports no credit data.".into(),
        )),
    }
}

/// One engine's refresh outcome, for the UI to report.
#[derive(Debug)]
pub struct BalanceResult {
    pub engine: &'static str,
    pub error: TtsError,
}

/// Fetches balances for every engine used so far that publishes one.
///
/// Successes are written straight into the store — the pump notices the
/// generation bump and redraws. Only failures come back on `sender`, because a
/// user who pressed Refresh and got nothing deserves to be told why, and the
/// caller puts that in the chat list rather than a modal.
pub fn start_balance_refresh(
    speech: SpeechConfig,
    sender: crossbeam_channel::Sender<BalanceResult>,
) {
    let engines: Vec<&'static str> = snapshot()
        .into_iter()
        .map(|usage| usage.engine)
        .filter(|engine| reports_balance(engine))
        .collect();
    let spawned = std::thread::Builder::new()
        .name("tts-usage-refresh".into())
        .spawn(move || {
            for engine in engines {
                match fetch_balance(engine, &speech) {
                    Ok(balance) => set_balance(engine, balance),
                    Err(error) => {
                        log::warn!("Could not read the {engine} balance: {error}");
                        if sender.send(BalanceResult { engine, error }).is_err() {
                            break;
                        }
                        wxdragon::wake_up_idle();
                    }
                }
            }
        });
    if let Err(error) = spawned {
        log::error!("Could not start the balance refresh: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store is global, so the tests share it and must not run in
    /// parallel against each other.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        let guard = SERIAL
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset();
        guard
    }

    #[test]
    fn an_engine_that_never_spoke_does_not_appear() {
        let _guard = lock();
        record(engines::OPENAI, 10, Some("tts-1".into()), None, false);
        let usage = snapshot();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].engine, engines::OPENAI);
    }

    #[test]
    fn engines_are_ordered_by_most_recent_use() {
        let _guard = lock();
        record(engines::OPENAI, 1, None, None, false);
        record(engines::ELEVENLABS, 1, None, None, false);
        record(engines::SAPI, 1, None, None, false);
        let order: Vec<&str> = snapshot().iter().map(|u| u.engine).collect();
        assert_eq!(order, vec![engines::SAPI, engines::ELEVENLABS, engines::OPENAI]);

        // Speaking again on an older engine moves it back to the top.
        record(engines::OPENAI, 1, None, None, false);
        let order: Vec<&str> = snapshot().iter().map(|u| u.engine).collect();
        assert_eq!(order, vec![engines::OPENAI, engines::SAPI, engines::ELEVENLABS]);
    }

    #[test]
    fn tallies_accumulate_and_models_and_voices_are_deduped() {
        let _guard = lock();
        record(
            engines::ELEVENLABS,
            100,
            Some("eleven_v3".into()),
            Some("rachel".into()),
            false,
        );
        record(
            engines::ELEVENLABS,
            50,
            Some("eleven_v3".into()),
            Some("adam".into()),
            false,
        );
        record(
            engines::ELEVENLABS,
            0,
            Some("eleven_flash_v2_5".into()),
            Some("rachel".into()),
            true,
        );
        let usage = snapshot();
        let entry = &usage[0];
        assert_eq!(entry.requests, 3);
        assert_eq!(entry.characters, 150);
        assert_eq!(entry.failures, 1);
        assert_eq!(
            entry.models.iter().cloned().collect::<Vec<_>>(),
            vec!["eleven_flash_v2_5".to_string(), "eleven_v3".to_string()]
        );
        assert_eq!(
            entry.voices.iter().cloned().collect::<Vec<_>>(),
            vec!["adam".to_string(), "rachel".to_string()]
        );
    }

    #[test]
    fn blank_models_and_voices_are_not_recorded() {
        let _guard = lock();
        record(
            engines::AZURE,
            5,
            Some("   ".into()),
            Some(String::new()),
            false,
        );
        let usage = snapshot();
        assert!(usage[0].models.is_empty());
        assert!(usage[0].voices.is_empty());
    }

    #[test]
    fn a_balance_only_attaches_to_an_engine_already_used() {
        let _guard = lock();
        set_balance(
            engines::ELEVENLABS,
            Balance {
                used: 1,
                limit: 2,
                ..Balance::default()
            },
        );
        assert!(snapshot().is_empty());

        record(engines::ELEVENLABS, 1, None, None, false);
        set_balance(
            engines::ELEVENLABS,
            Balance {
                used: 1_204,
                limit: 10_000,
                tier: "creator".into(),
                resets_unix: None,
            },
        );
        let balance = snapshot()[0].balance.clone().expect("balance");
        assert_eq!(balance.remaining(), 8_796);
    }

    #[test]
    fn an_overage_does_not_wrap_to_a_huge_remaining() {
        let balance = Balance {
            used: 12_000,
            limit: 10_000,
            ..Balance::default()
        };
        assert_eq!(balance.remaining(), 0);
    }

    #[test]
    fn generation_moves_on_every_mutation() {
        let _guard = lock();
        let before = generation();
        record(engines::SAPI, 1, None, None, false);
        assert!(generation() > before);
    }

    #[test]
    fn only_elevenlabs_reports_a_balance() {
        assert!(reports_balance(engines::ELEVENLABS));
        for (id, _) in engines::ALL {
            if *id != engines::ELEVENLABS {
                assert!(!reports_balance(id), "{id} should report no balance");
            }
        }
        let error = fetch_balance(engines::OPENAI, &SpeechConfig::default()).unwrap_err();
        assert!(error.to_string().contains("no credit data"), "{error}");
    }
}
