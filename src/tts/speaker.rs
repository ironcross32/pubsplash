//! The speech router: one queue in, two very different workers out.
//!
//! SAPI has to run on a COM apartment thread that owns its `ISpVoice`, and it
//! speaks locally as well as synthesizing. Every other engine is a blocking
//! network call that must not go anywhere near that thread — a wedged HTTP
//! request would take local speech down with it. So [`Speaker`] holds two
//! senders and dispatches on the engine id.
//!
//! Both queues are **bounded and drop-oldest**. A chat flood is the normal
//! failure mode here: the reference implementation's unbounded queue means a
//! busy stream buries the speaker minutes deep in stale messages, and for a
//! paid engine it means paying to synthesize them. Dropping the oldest keeps
//! speech current, which is what a listener actually wants.

use super::engine::{SynthRequest, TtsError};
use super::engines;
use super::queue::Queue;
use crate::audio::ExternalFeeds;
use crate::config::SpeechConfig;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Roughly ten seconds of backlog at a conversational pace. Past this, the
/// oldest message is dropped.
pub(super) const QUEUE_DEPTH: usize = 8;

/// One utterance, with everything the worker needs to render and route it.
#[derive(Debug, Clone)]
pub struct SpeakRequest {
    pub engine: String,
    pub synth: SynthRequest,
    /// Name of the TTS source in the audio engine to feed.
    ///
    /// Every engine's samples go here and nowhere else. There is no "speak
    /// locally instead" flag: the mixer is the one path out, and the source's
    /// own routing decides how far along it the speech gets — its strip is
    /// always heard locally, and "Send speech to the stream" decides whether it
    /// also reaches master and the buses. See `App::source_specs`.
    pub source_name: String,
    /// Credentials and limits, snapshotted at enqueue time so the worker never
    /// touches the UI thread's `Config`.
    pub speech: SpeechConfig,
}

/// Something a worker wants the user to know about. Drained by the UI pump.
#[derive(Debug, Clone)]
pub struct SpeechProblem {
    pub engine: String,
    pub message: String,
}

/// Handle to the speech workers. Dropping it stops them.
pub struct Speaker {
    sapi: Queue<super::sapi::SapiRequest>,
    network: Queue<SpeakRequest>,
    problems: Problems,
}

impl Speaker {
    pub fn start(feeds: ExternalFeeds) -> Self {
        let problems = Problems::default();
        let sapi = super::sapi::start(feeds.clone(), problems.clone());
        let network = Queue::new(QUEUE_DEPTH);
        start_network_worker(network.clone(), feeds, problems.clone());
        Self {
            sapi,
            network,
            problems,
        }
    }

    /// Queues an utterance. Never blocks; drops the oldest queued message when
    /// the worker is already behind.
    pub fn speak(&self, request: SpeakRequest) {
        if engines::is_network(&request.engine) {
            self.network.push(request);
        } else {
            self.sapi.push(super::sapi::SapiRequest {
                synth: request.synth,
                source_name: request.source_name,
            });
        }
    }

    /// Takes any problems reported since the last call, for the UI to announce.
    pub fn take_problems(&self) -> Vec<SpeechProblem> {
        self.problems.take()
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.sapi.close();
        self.network.close();
    }
}

/// Problems collected by the workers, drained by the UI pump.
///
/// Rate-limited per engine: a chat flood against a wrong API key would
/// otherwise queue one identical complaint per message, and a screen reader
/// would read every one of them.
#[derive(Clone, Default)]
pub(super) struct Problems(Arc<Mutex<ProblemState>>);

#[derive(Default)]
struct ProblemState {
    pending: Vec<SpeechProblem>,
    last: Option<(String, Instant)>,
}

/// How long an identical complaint is suppressed for.
const PROBLEM_INTERVAL: Duration = Duration::from_secs(60);

impl Problems {
    pub(super) fn report(&self, engine: &str, error: &TtsError) {
        let message = error.to_string();
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        let key = format!("{engine}: {message}");
        if let Some((previous, at)) = &state.last {
            if *previous == key && at.elapsed() < PROBLEM_INTERVAL {
                return;
            }
        }
        state.last = Some((key, Instant::now()));
        state.pending.push(SpeechProblem {
            engine: engine.to_string(),
            message,
        });
    }

    fn take(&self) -> Vec<SpeechProblem> {
        self.0
            .lock()
            .map(|mut state| std::mem::take(&mut state.pending))
            .unwrap_or_default()
    }
}

fn start_network_worker(queue: Queue<SpeakRequest>, feeds: ExternalFeeds, problems: Problems) {
    let spawned = std::thread::Builder::new()
        .name("tts-net".into())
        .spawn(move || {
            let mut last_request: Option<Instant> = None;
            let mut reported_drops = 0;
            while let Some(request) = queue.pop() {
                // Messages discarded because the queue was full. Worth a line:
                // it is the difference between "the engine is broken" and
                // "chat is arriving faster than it can be spoken".
                let dropped = queue.dropped();
                if dropped > reported_drops {
                    log::debug!(
                        "Speech queue overflowed; {} message(s) skipped so far",
                        dropped
                    );
                    reported_drops = dropped;
                }

                // Floor on how often a paid API is called. Applied before
                // synthesis so a burst is spread out rather than refused.
                let interval = Duration::from_millis(request.speech.min_request_interval_ms);
                if let Some(previous) = last_request {
                    let elapsed = previous.elapsed();
                    if elapsed < interval {
                        std::thread::sleep(interval - elapsed);
                    }
                }
                last_request = Some(Instant::now());

                let Some(engine) = engines::build(&request.engine, &request.speech) else {
                    continue;
                };
                let synth = request.synth.truncated(request.speech.max_chars());
                // The model and voice as the engine will actually resolve them,
                // and the character count *after* truncation — the figure a
                // per-character biller charges for. Read before synthesis so a
                // failed request still counts against the API tab's totals.
                let model = engine.usage_model(&synth);
                let voice = engine.usage_voice(&synth);
                let characters = synth.text.chars().count();
                // `feed_all` sleeps while the ring drains, which is exactly why
                // this is not the audio thread. Engines that stream call the
                // sink many times, so the first words reach the mixer while the
                // rest is still being generated.
                let mut fed = 0usize;
                let result = engine.synth_to(&synth, &mut |samples| {
                    fed += samples.len();
                    feeds.feed_all(&request.source_name, samples, "TTS");
                });
                super::usage::record(engine.id(), characters, model, voice, result.is_err());
                match result {
                    Ok(()) if fed == 0 => {
                        log::debug!("{} returned no audio", request.engine);
                    }
                    Ok(()) => {}
                    Err(error) => {
                        log::warn!("{} synthesis failed: {error}", engine.display_name());
                        problems.report(engine.id(), &error);
                    }
                }
            }
        });
    if let Err(error) = spawned {
        log::error!("Could not start the speech worker: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error() -> TtsError {
        TtsError::NotConfigured("The OpenAI API key is not set.".into())
    }

    #[test]
    fn identical_problems_are_reported_once_per_interval() {
        let problems = Problems::default();
        for _ in 0..10 {
            problems.report("openai", &error());
        }
        let taken = problems.take();
        assert_eq!(taken.len(), 1, "a flood must not become ten announcements");
        assert_eq!(taken[0].engine, "openai");
        assert!(taken[0].message.contains("OpenAI API key"));
        // Draining doesn't lift the suppression; the problem is still current.
        problems.report("openai", &error());
        assert!(problems.take().is_empty());
    }

    /// Suppression is per message, so a *different* failure still gets through
    /// immediately — otherwise one noisy engine would mask a real problem.
    #[test]
    fn a_different_problem_is_reported_immediately() {
        let problems = Problems::default();
        problems.report("openai", &error());
        problems.report("edge", &TtsError::Network("host unreachable".into()));
        let taken = problems.take();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[1].engine, "edge");
    }

    #[test]
    fn taking_problems_clears_them() {
        let problems = Problems::default();
        problems.report("gtts", &TtsError::Other("boom".into()));
        assert_eq!(problems.take().len(), 1);
        assert!(problems.take().is_empty());
    }

    /// Network engines go to the network worker and SAPI does not, which is
    /// the whole reason the router exists.
    #[test]
    fn requests_are_routed_by_engine() {
        assert!(engines::is_network(engines::EDGE));
        assert!(engines::is_network(engines::OPENAI));
        assert!(!engines::is_network(engines::SAPI));
        // An engine id from a newer build must not be routed to the network
        // worker, which would build nothing and silently drop the message.
        assert!(!engines::is_network("piper"));
    }
}
