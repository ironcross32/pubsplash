//! Feeds sound-event cues into the mixer, one worker thread per source.
//!
//! Pushing a cue to `ExternalFeeds` cannot be done inline: `feed_all` paces
//! itself to the mixer's drain rate, so it takes about as long as the sound
//! lasts, and the UI thread is the one thread that must never wait. It used to
//! be `std::thread::spawn` per cue — and the cue that fires most often is
//! "incoming chat", once per message. A busy stream therefore spawned a thread
//! per message per sound-event source, all of them sleeping in 20 ms slices
//! against the same audio rings, which is scheduling noise arriving exactly when
//! the mixer is busiest.
//!
//! One long-lived worker per source replaces that. Its queue is bounded and
//! **drop-oldest**, reusing [`crate::tts::queue::Queue`] for the same reason the
//! speaker does: under a flood the newest cue is the one worth playing, and a
//! backlog of stale ones is worse than silence — cues would keep sounding long
//! after the messages that caused them scrolled away.
//!
//! Everything here runs on the UI thread except the worker bodies.

use crate::audio::ExternalFeeds;
use crate::tts::queue::Queue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// How many cues may be waiting for one source.
///
/// Small on purpose. Cues are short and overlap into mush well before this, so a
/// deeper queue would only add latency between the event and the sound that is
/// supposed to mark it.
const QUEUE_DEPTH: usize = 8;

type Samples = Arc<Vec<f32>>;

struct Worker {
    queue: Queue<Samples>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn start(source_name: String, feeds: ExternalFeeds) -> Self {
        let queue: Queue<Samples> = Queue::new(QUEUE_DEPTH);
        let thread = std::thread::Builder::new()
            .name("cue-feed".into())
            .spawn({
                let queue = queue.clone();
                move || {
                    while let Some(samples) = queue.pop() {
                        feeds.feed_all(&source_name, &samples, "Sound events");
                    }
                }
            })
            .ok();
        Self { queue, thread }
    }

    /// Closes the queue and waits for the worker to finish the cue it is on.
    ///
    /// The join is bounded by that one cue — `feed_all` returns as soon as the
    /// source's ring is gone, which is what happens when a scene switch retires
    /// it — so this is safe on the UI thread.
    fn stop(mut self) {
        self.queue.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The live workers, keyed by `SourceConfig.name` — the identity key
/// `ExternalFeeds` is routed by, never a display name.
#[derive(Default)]
pub struct CueFeeds {
    workers: RefCell<HashMap<String, Worker>>,
}

impl CueFeeds {
    /// Queues `samples` for the named source, starting its worker if this is the
    /// first cue it has had. Never blocks.
    pub fn play(&self, source_name: &str, feeds: &ExternalFeeds, samples: Samples) {
        let mut workers = self.workers.borrow_mut();
        let worker = workers
            .entry(source_name.to_string())
            .or_insert_with(|| Worker::start(source_name.to_string(), feeds.clone()));
        worker.queue.push(samples);
    }

    /// Retires the workers whose sources are no longer in the active scene.
    ///
    /// Called from `home::on_sources_changed`, beside `sync_engine_sources`,
    /// because that is the moment `ExternalFeeds` gains and loses its rings: a
    /// worker whose source is gone would otherwise sit blocked on an empty queue
    /// for the rest of the session.
    pub fn retain(&self, live_names: &std::collections::HashSet<String>) {
        let retired: Vec<Worker> = {
            let mut workers = self.workers.borrow_mut();
            let gone: Vec<String> = workers
                .keys()
                .filter(|name| !live_names.contains(*name))
                .cloned()
                .collect();
            gone.iter()
                .filter_map(|name| workers.remove(name))
                .collect()
        };
        // Joined outside the borrow: `stop` waits on a thread, and nothing that
        // waits should be holding a `RefCell` the rest of the UI reaches for.
        for worker in retired {
            worker.stop();
        }
    }

    /// Retires every worker. Used at shutdown.
    pub fn stop_all(&self) {
        let retired: Vec<Worker> = self.workers.borrow_mut().drain().map(|(_, w)| w).collect();
        for worker in retired {
            worker.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module exists for: a flood costs one worker, not
    /// one thread per cue, and the cues that survive it are the newest.
    #[test]
    fn a_flood_keeps_the_newest_cues_and_discards_the_rest() {
        let queue: Queue<u32> = Queue::new(QUEUE_DEPTH);
        for cue in 0..1_000 {
            queue.push(cue);
        }
        assert_eq!(queue.dropped(), 1_000 - QUEUE_DEPTH as u64);
        assert_eq!(queue.pop(), Some(1_000 - QUEUE_DEPTH as u32));
    }

    #[test]
    fn a_retired_worker_stops_rather_than_blocking_forever() {
        let feeds = ExternalFeeds::default();
        let cues = CueFeeds::default();
        cues.play("Sound events 1", &feeds, Arc::new(vec![0.0; 64]));
        assert_eq!(cues.workers.borrow().len(), 1);

        // The source is not in the new scene, so its worker must go with it.
        cues.retain(&std::collections::HashSet::new());
        assert!(cues.workers.borrow().is_empty());
    }

    #[test]
    fn a_source_that_survives_a_scene_edit_keeps_its_worker() {
        let feeds = ExternalFeeds::default();
        let cues = CueFeeds::default();
        cues.play("Sound events 1", &feeds, Arc::new(vec![0.0; 64]));

        let live: std::collections::HashSet<String> =
            ["Sound events 1".to_string()].into_iter().collect();
        cues.retain(&live);
        assert_eq!(cues.workers.borrow().len(), 1);
        cues.stop_all();
        assert!(cues.workers.borrow().is_empty());
    }
}
