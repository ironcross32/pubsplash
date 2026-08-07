//! The audio engine: owns capture threads and the mixer loop, encodes MP3
//! while streaming, and reacts to UI commands (volumes, mutes, scene
//! switches) without ever blocking the UI thread.

pub mod app_list;
pub mod capture;
pub mod convert;
pub mod cue;
pub mod device;
pub mod encoder;
pub mod fx_chain;
pub mod health;
pub mod mixer;
pub mod monitor;
pub mod recorder;

use fx_chain::FxChain;

use capture::CaptureKind;
use crossbeam_channel::{Receiver, Sender};
use mixer::{BLOCK_SAMPLES, ChannelStrip};
use rtrb::RingBuffer;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One second of interleaved stereo f32 per source.
const RING_CAPACITY: usize = mixer::SAMPLE_RATE as usize * mixer::CHANNELS;

/// How a source gets its samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedKind {
    /// A capture thread is spawned (microphone, desktop, application).
    Capture(CaptureKind),
    /// Samples are pushed by another part of the app (TTS, sound events).
    /// The producer half is parked in [`ExternalFeeds`] under the source name.
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub name: String,
    pub volume: u32,
    pub muted: bool,
    pub feed: FeedKind,
    /// Whether the source mixes directly into master (in addition to sends).
    pub to_master: bool,
    pub sends: Vec<SendSpec>,
    /// Whether this source's post-fader signal is played out of the local
    /// monitoring device. Session-only state, so it rides along in the spec
    /// and is re-sent with every `SetSources`.
    pub monitor: bool,
    /// Whether this source is played out of the local monitoring device whether
    /// or not the user asked for it — a source the broadcaster must always
    /// hear, whatever it is doing on the stream. Text-to-speech sets it: chat
    /// being read aloud is for the broadcaster first, and it used to be audible
    /// only if they thought to press `CTRL+M`.
    ///
    /// Distinct from `monitor` rather than folded into it because `monitor` is
    /// a control the user owns, and its state is shown to them; this one is a
    /// property of the source. Both taps are taken as one, so a monitored TTS
    /// strip is heard once, not twice.
    pub local: bool,
}

/// A source's send into a bus, addressed by bus index (matching the order
/// of the most recent `SetBuses`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendSpec {
    pub bus_index: usize,
    /// 0-100.
    pub level: u32,
}

/// One mixing bus. Buses sum their incoming sends, run their FX chain, then
/// apply their own volume/mute strip, then mix into master. Not `Clone`/`Eq`
/// because the chain owns live plugin instances. Buses are addressed by index
/// (matching the UI's order), so the name isn't needed engine-side.
pub struct BusSpec {
    pub volume: u32,
    pub muted: bool,
    pub chain: FxChain,
    /// See [`SourceSpec::monitor`].
    pub monitor: bool,
}

/// The payload of [`EngineCommand::SetRouting`]. Boxed there so the frequently
/// sent little commands (volume, mute) are not each sized to hold this.
#[derive(Default)]
pub struct RoutingUpdate {
    pub sources: Option<Vec<SourceSpec>>,
    pub buses: Option<Vec<BusSpec>>,
    pub master_chain: Option<FxChain>,
}

pub enum EngineCommand {
    /// Replaces routing. Every field of the update that is `Some` is applied in
    /// a single step, before the next block is mixed.
    ///
    /// The three parts are one command rather than three because they are not
    /// independent. Sends address buses by index, so a bus reorder that landed
    /// a block before the matching source update would route a send to the
    /// wrong bus. And splitting the FX halves used to mean two
    /// [`EngineEvent::BusesApplied`]s for one edit, making it impossible for the
    /// UI to know which returned chains were ready to reclaim. One command, one
    /// apply point, one `BusesApplied`.
    ///
    /// `sources` index order matches the mixer order; buses are addressed by
    /// index too.
    SetRouting(Box<RoutingUpdate>),
    SetSourceVolume(usize, u32),
    SetSourceMute(usize, bool),
    /// Play (or stop playing) this source's post-fader signal out of the local
    /// monitoring device. The output device is opened on the first strip that
    /// asks for it and closed again when the last one stops.
    SetSourceMonitor(usize, bool),
    SetBusVolume(usize, u32),
    SetBusMute(usize, bool),
    /// See [`EngineCommand::SetSourceMonitor`].
    SetBusMonitor(usize, bool),
    /// Toggle bypass on one plugin. `bus: None` targets the master chain.
    SetFxBypass {
        bus: Option<usize>,
        slot: usize,
        bypass: bool,
    },
    SetMasterVolume(u32),
    SetMasterMute(bool),
    /// See [`EngineCommand::SetSourceMonitor`]. The master tap is taken after
    /// the master FX chain and fader, so it is exactly what goes out.
    SetMasterMonitor(bool),
    /// Begin encoding; encoded MP3 chunks flow into the sender (consumed by
    /// the Icecast task on the network runtime).
    StartEncoding {
        bitrate_kbps: u32,
        out: tokio::sync::mpsc::Sender<Vec<u8>>,
    },
    StopEncoding,
    /// Begin recording the master mix to `path` as MP3, using a dedicated
    /// encoder independent of streaming. Ignored if a recording is already
    /// active.
    StartRecording {
        bitrate_kbps: u32,
        path: std::path::PathBuf,
    },
    StopRecording,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A capture source failed (device unplugged, process exited, not ready
    /// yet...). The source is not dead: its thread is retrying with a backoff,
    /// and will send `SourceRecovered` if it gets the device back.
    SourceError { name: String, message: String },
    /// A capture source that had reported `SourceError` is running again.
    SourceRecovered { name: String },
    /// The recording is running and the file exists. The UI waits for this
    /// rather than assuming the command worked: creating the file or the
    /// encoder can fail, and a broadcaster who is told a recording is running
    /// finds out otherwise only when they go looking for the file.
    RecordingStarted { path: std::path::PathBuf },
    /// The recording could not be started, or has stopped on its own. The
    /// engine has already finalized whatever existed, so there is nothing left
    /// running by the time this arrives.
    ///
    /// `start` is set only for the first case; see [`RecordingStartFailure`].
    RecordingFailed {
        message: String,
        start: Option<RecordingStartFailure>,
    },
    /// The stream encoder could not be created; nothing is being encoded.
    EncodingFailed { message: String },
    /// Encoding stopped. `reason` is `None` when the outgoing channel simply
    /// closed, which is the ordinary end of a stream, and `Some` when the
    /// encoder itself failed mid-stream — the case the user has to be told
    /// about, because the stream is still nominally up and sending nothing.
    EncodingStopped { reason: Option<String> },
    /// The engine finished applying the FX portion of a `SetRouting` and queued
    /// every replaced chain for reclamation on the UI thread.
    BusesApplied,
}

/// Why a recording failed *to start*, as opposed to one that stopped after
/// running for a while.
///
/// The two are told apart because only the start case reaches the user as a
/// modal: it is the direct answer to a button press with no other visible
/// effect, so it cannot fire on its own and interrupt a live broadcast. A
/// recording that dies mid-way can, and stays a log line and a Home tab state.
/// The fields are what the UI needs to say something a broadcaster can act on
/// rather than repeating an OS error string.
#[derive(Debug, Clone)]
pub struct RecordingStartFailure {
    /// The file the recording was to be written to.
    pub path: std::path::PathBuf,
    /// The filesystem error that stopped the file being created; `None` when
    /// the encoder, not the file, was the problem.
    pub kind: Option<std::io::ErrorKind>,
}

/// The engine thread's end of the event channel.
///
/// Sending also rings the UI's idle doorbell, which is what drains these; see
/// `net::EventSender` for why it lives on the sender rather than at each call
/// site.
#[derive(Clone)]
pub struct EventSender(Sender<EngineEvent>);

impl EventSender {
    fn send(&self, event: EngineEvent) {
        let _ = self.0.send(event);
        wxdragon::wake_up_idle();
    }
}

/// Producers for `External` sources, keyed by source name. The TTS and
/// sound-event subsystems `take()` theirs after a scene change.
#[derive(Default, Clone)]
pub struct ExternalFeeds(Arc<Mutex<HashMap<String, rtrb::Producer<f32>>>>);

/// Result of feeding samples to an external source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedResult {
    /// All samples accepted.
    Done,
    /// Ring full; `accepted` samples were taken. Retry the rest shortly.
    Full { accepted: usize },
    /// No such source (scene switched away); stop feeding.
    Gone,
}

/// Copies `src` into an uninitialized ring slice, initializing all of it.
fn write_uninit(dest: &mut [std::mem::MaybeUninit<f32>], src: &[f32]) {
    for (slot, &sample) in dest.iter_mut().zip(src) {
        slot.write(sample);
    }
}

impl ExternalFeeds {
    /// Pushes as many samples as fit into the named source's ring. Callers
    /// stream long audio by retrying the remainder as the mixer drains.
    pub fn push(&self, name: &str, samples: &[f32]) -> FeedResult {
        // `lock_recovering`, not `unwrap`: a feeder that panics mid-push would
        // otherwise poison the map, and the next push — which happens on the
        // audio thread's ring, from a TTS or cue thread — would panic too.
        let mut map = device::lock_recovering(&self.0, "External feeds");
        let Some(producer) = map.get_mut(name) else {
            return FeedResult::Gone;
        };
        // Written in one bulk chunk rather than a sample at a time: this holds
        // a global mutex, and a few seconds of speech is a six-figure loop.
        let take = producer.slots().min(samples.len());
        if take > 0
            && let Ok(mut chunk) = producer.write_chunk_uninit(take)
        {
            let (first, second) = chunk.as_mut_slices();
            write_uninit(first, &samples[..first.len()]);
            write_uninit(second, &samples[first.len()..take]);
            // SAFETY: both slices were just fully initialized above.
            unsafe { chunk.commit_all() };
        }
        if take == samples.len() {
            FeedResult::Done
        } else {
            FeedResult::Full { accepted: take }
        }
    }

    /// Feeds a whole buffer in, pacing to the mixer's drain rate, and stops if
    /// the source disappears (a scene switch).
    ///
    /// The wait is a plain sleep on purpose. The event-driven alternative is a
    /// condvar the *audio thread* has to signal after every block — a blocking
    /// wait and a lock on the mix path, where a missed notify wedges speech and
    /// a badly placed one stalls the mixer. This poll is bounded, cheap, and
    /// already off the audio thread; it is the one place where "it's only
    /// 20 ms" genuinely holds.
    pub fn feed_all(&self, name: &str, samples: &[f32], what: &str) {
        let mut offset = 0;
        while offset < samples.len() {
            match self.push(name, &samples[offset..]) {
                FeedResult::Done => return,
                FeedResult::Full { accepted } => {
                    offset += accepted;
                    // The ring holds a second of audio; let the mixer drain.
                    std::thread::sleep(Duration::from_millis(20));
                }
                FeedResult::Gone => {
                    log::debug!("{what} source {name:?} no longer active; dropping audio");
                    return;
                }
            }
        }
    }

    fn insert(&self, name: String, producer: rtrb::Producer<f32>) {
        self.0.lock().unwrap().insert(name, producer);
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

struct ActiveSource {
    name: String,
    strip: ChannelStrip,
    consumer: rtrb::Consumer<f32>,
    stop: Arc<AtomicBool>,
    /// Scratch block for this source's pull, reused across iterations.
    scratch: Vec<f32>,
    to_master: bool,
    sends: Vec<ActiveSend>,
    monitor: bool,
    /// See [`SourceSpec::local`].
    local: bool,
    /// What the capture thread has seen, if this source has one.
    stats: Arc<health::CaptureStats>,
    /// What the mixer has seen of this source's ring since the last report.
    window: health::Window,
    /// The counter reading the current window is measured from.
    baseline: health::Counters,
}

/// A live send: the level is a `ChannelStrip` so level changes ramp
/// click-free just like volume changes do.
struct ActiveSend {
    bus_index: usize,
    strip: ChannelStrip,
}

struct ActiveBus {
    strip: ChannelStrip,
    chain: FxChain,
    /// This block's summed sends, reused across iterations.
    buffer: Vec<f32>,
    monitor: bool,
}

/// The live monitoring output: the producer half of the ring the render thread
/// drains, plus its stop flag. Dropped (and stopped) as soon as the last strip
/// stops monitoring, so the playback device is not held open for nothing.
struct MonitorOutput {
    producer: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
}

impl Drop for MonitorOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn active_sends(specs: Vec<SendSpec>) -> Vec<ActiveSend> {
    specs
        .into_iter()
        .map(|s| ActiveSend {
            bus_index: s.bus_index,
            strip: ChannelStrip::new(s.level, false),
        })
        .collect()
}

pub struct AudioEngine {
    commands: Sender<EngineCommand>,
    pub events: Receiver<EngineEvent>,
    /// Used by the TTS and sound-event subsystems (upcoming milestone).
    #[allow(dead_code)]
    pub external_feeds: ExternalFeeds,
    /// FX chains the engine hands back after routing swaps and at shutdown so
    /// they are dropped here rather than on the audio thread.
    retired: Receiver<FxChain>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (retired_tx, retired_rx) = crossbeam_channel::unbounded();
        let feeds = ExternalFeeds::default();
        let feeds_clone = feeds.clone();
        let thread = std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || engine_loop(cmd_rx, EventSender(event_tx), retired_tx, feeds_clone))
            .expect("spawning audio engine thread");
        Self {
            commands: cmd_tx,
            events: event_rx,
            external_feeds: feeds,
            retired: retired_rx,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: EngineCommand) {
        let _ = self.commands.send(command);
    }

    /// Drops every chain the audio thread has finished with on the calling
    /// thread. The UI calls this after `BusesApplied`; the return value also
    /// makes the lifetime contract directly assertable in engine tests.
    pub(crate) fn reclaim_retired_chains(&self) -> usize {
        let mut reclaimed = 0;
        while let Ok(chain) = self.retired.try_recv() {
            // Dropping a chain can run plugin teardown, which is third-party
            // code that can take the process with it (see `crash.rs`). Say so
            // before and after, so a log that stops in between is diagnostic.
            if !chain.is_empty() {
                log::info!("Reclaiming a retired FX chain of {} plugins", chain.len());
            }
            drop(chain);
            reclaimed += 1;
        }
        reclaimed
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.commands.send(EngineCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Now that the engine thread is gone, release the chains it parked:
        // dropping a plugin runs its teardown and `FreeLibrary`, which belongs
        // on this thread.
        self.reclaim_retired_chains();
    }
}

fn engine_loop(
    commands: Receiver<EngineCommand>,
    events: EventSender,
    retired: Sender<FxChain>,
    feeds: ExternalFeeds,
) {
    // Flush denormals to zero (FTZ + DAZ in MXCSR): FX plugins (reverbs,
    // filters) produce denormal tails that otherwise cost orders of
    // magnitude in CPU.
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }

    let (capture_tx, capture_rx) = crossbeam_channel::unbounded::<capture::CaptureReport>();
    // Bumped on every `SetSources`. Capture threads retire asynchronously and a
    // retrying one may be mid-backoff, so reports are stamped with the
    // generation they were spawned for and stale ones are dropped: otherwise a
    // thread on its way out could report a failure against a name that the
    // current source set has running perfectly well.
    let mut epoch: u64 = 0;
    // Which sources are currently reporting failure, so only transitions reach
    // the UI.
    let mut failing: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut sources: Vec<ActiveSource> = Vec::new();
    let mut buses: Vec<ActiveBus> = Vec::new();
    let mut master_chain = FxChain::empty();
    let mut master = ChannelStrip::new(100, false);
    let mut master_monitor = false;
    let mut monitor_out: Option<MonitorOutput> = None;
    let mut encoder: Option<(encoder::Mp3Encoder, tokio::sync::mpsc::Sender<Vec<u8>>)> = None;
    // Consecutive blocks dropped because the outgoing stream buffer was full,
    // so the log says so once a second rather than a hundred times.
    let mut dropped_blocks: u64 = 0;
    // Recording runs on its own encoder so it works with or without streaming.
    let mut rec_encoder: Option<encoder::Mp3Encoder> = None;
    let mut recorder: Option<recorder::RecorderHandle> = None;

    let block_period = Duration::from_millis(10);
    let mut next_tick = Instant::now() + block_period;
    // See `report_capture_health`. Anchored here rather than at the first block
    // so a source added an hour into a session still waits a full window before
    // its first line, and that line therefore covers a full window.
    let mut last_health = Instant::now();
    let mut mix_block = vec![0f32; BLOCK_SAMPLES];
    let mut monitor_block = vec![0f32; BLOCK_SAMPLES];
    let mut send_scratch = vec![0f32; BLOCK_SAMPLES];
    let mut pcm_i16: Vec<i16> = Vec::with_capacity(BLOCK_SAMPLES);

    // The first command is pulled with a blocking receive rather than the
    // try_recv loop below whenever there is nothing to mix. Pubsplash is an app
    // people leave open all day, and until the user configures a source there
    // is no signal, no encoder, no recorder and nothing being monitored — yet
    // the loop still woke 100 times a second to zero two buffers and throw them
    // away. Parking instead takes idle wakeups to zero, which is what lets the
    // package reach a deeper C-state on battery.
    //
    // Deliberately spelled out as "nothing configured and nothing consuming the
    // mix" rather than anything cleverer: the hazard here is a wake condition
    // that is missed, not one that is checked too often. Nothing can arrive
    // while parked except a command — `ExternalFeeds` producers are only
    // installed by `SetRouting`, so a non-empty `sources` means the loop is
    // already running.
    let mut parked_command: Option<EngineCommand> = None;
    loop {
        let idle = sources.is_empty()
            && encoder.is_none()
            && rec_encoder.is_none()
            && recorder.is_none()
            && monitor_out.is_none()
            && !master_monitor;
        if idle && parked_command.is_none() {
            match commands.recv() {
                Ok(command) => parked_command = Some(command),
                // The UI is gone; nothing will ever wake us again. Returning
                // straight out would drop the live chains here, on the audio
                // thread — `idle` says nothing about buses, so a bus with FX
                // and no sources parks in exactly this state.
                Err(_) => {
                    park_chains(&mut buses, &mut master_chain, &retired);
                    return;
                }
            }
            // Re-anchor the cadence: `next_tick` is however long ago the park
            // started, and the catch-up branch would otherwise chase it.
            next_tick = Instant::now() + block_period;
        }

        // Drain pending commands without blocking the mix cadence.
        //
        // Not everything in here is real-time clean, and deliberately so:
        // applying a routing update allocates a ring per source and spawns its
        // capture thread, and starting an encoder or a recording touches LAME
        // and the filesystem. All of it is user-initiated and happens at most
        // once per scene switch or button press, where a block's worth of
        // jitter is not audible. What must never be here is anything that
        // happens *per block* — the disk write for recording used to be, and is
        // now on `RecorderHandle`'s own thread.
        loop {
            match parked_command
                .take()
                .map(Ok)
                .unwrap_or_else(|| commands.try_recv())
            {
                Ok(EngineCommand::SetRouting(update)) => {
                    let RoutingUpdate {
                        sources: new_sources,
                        buses: new_buses,
                        master_chain: new_master_chain,
                    } = *update;
                    // Whether this edit touched FX, and so whether the UI is
                    // waiting to hear that its retired plugins are unreferenced.
                    let touched_fx = new_buses.is_some() || new_master_chain.is_some();
                    if let Some(specs) = new_sources {
                        stop_sources(&mut sources, last_health.elapsed());
                        // The replacement sources deserve a full window before
                        // their first line, and their rings start empty.
                        last_health = Instant::now();
                        feeds.clear();
                        epoch = epoch.wrapping_add(1);
                        // The new threads start from a clean slate, so anything
                        // the old set was reporting is no longer true of this one.
                        for name in failing.drain() {
                            events.send(EngineEvent::SourceRecovered { name });
                        }
                        for spec in specs {
                            let (producer, consumer) = RingBuffer::new(RING_CAPACITY);
                            let stop = Arc::new(AtomicBool::new(false));
                            let stats = Arc::new(match spec.feed {
                                FeedKind::Capture(_) => health::CaptureStats::new(),
                                FeedKind::External => health::CaptureStats::external(),
                            });
                            match spec.feed {
                                FeedKind::Capture(kind) => {
                                    capture::spawn(
                                        spec.name.clone(),
                                        epoch,
                                        kind,
                                        producer,
                                        stop.clone(),
                                        capture_tx.clone(),
                                        stats.clone(),
                                    );
                                }
                                FeedKind::External => {
                                    feeds.insert(spec.name.clone(), producer);
                                }
                            }
                            sources.push(ActiveSource {
                                name: spec.name,
                                strip: ChannelStrip::new(spec.volume, spec.muted),
                                consumer,
                                stop,
                                scratch: vec![0f32; BLOCK_SAMPLES],
                                to_master: spec.to_master,
                                sends: active_sends(spec.sends),
                                monitor: spec.monitor,
                                local: spec.local,
                                stats,
                                window: health::Window::default(),
                                baseline: health::Counters::default(),
                            });
                        }
                    }
                    if let Some(specs) = new_buses {
                        let mut replacement: Vec<ActiveBus> = specs
                            .into_iter()
                            .map(|spec| ActiveBus {
                                strip: ChannelStrip::new(spec.volume, spec.muted),
                                chain: spec.chain,
                                buffer: vec![0f32; BLOCK_SAMPLES],
                                monitor: spec.monitor,
                            })
                            .collect();
                        // Paired by index only to find a likely donor; the
                        // carry-over itself matches plugins by identity, so a
                        // bus reorder simply finds nothing here.
                        for (new, old) in replacement.iter_mut().zip(buses.iter()) {
                            new.chain.adopt_fades_from(&old.chain);
                        }
                        let replaced = std::mem::replace(&mut buses, replacement);
                        for bus in replaced {
                            let _ = retired.send(bus.chain);
                        }
                    }
                    if let Some(mut chain) = new_master_chain {
                        chain.adopt_fades_from(&master_chain);
                        let replaced = std::mem::replace(&mut master_chain, chain);
                        let _ = retired.send(replaced);
                    }
                    if touched_fx {
                        events.send(EngineEvent::BusesApplied);
                    }
                }
                Ok(EngineCommand::SetSourceVolume(i, v)) => {
                    if let Some(s) = sources.get_mut(i) {
                        s.strip.set_volume(v);
                    }
                }
                Ok(EngineCommand::SetSourceMute(i, m)) => {
                    if let Some(s) = sources.get_mut(i) {
                        s.strip.set_muted(m);
                    }
                }
                Ok(EngineCommand::SetSourceMonitor(i, m)) => {
                    if let Some(s) = sources.get_mut(i) {
                        s.monitor = m;
                    }
                }
                Ok(EngineCommand::SetBusVolume(i, v)) => {
                    if let Some(b) = buses.get_mut(i) {
                        b.strip.set_volume(v);
                    }
                }
                Ok(EngineCommand::SetBusMute(i, m)) => {
                    if let Some(b) = buses.get_mut(i) {
                        b.strip.set_muted(m);
                    }
                }
                Ok(EngineCommand::SetBusMonitor(i, m)) => {
                    if let Some(b) = buses.get_mut(i) {
                        b.monitor = m;
                    }
                }
                Ok(EngineCommand::SetFxBypass { bus, slot, bypass }) => match bus {
                    Some(i) => {
                        if let Some(b) = buses.get_mut(i) {
                            b.chain.set_bypass(slot, bypass);
                        }
                    }
                    None => master_chain.set_bypass(slot, bypass),
                },
                Ok(EngineCommand::SetMasterVolume(v)) => master.set_volume(v),
                Ok(EngineCommand::SetMasterMute(m)) => master.set_muted(m),
                Ok(EngineCommand::SetMasterMonitor(m)) => master_monitor = m,
                Ok(EngineCommand::StartEncoding { bitrate_kbps, out }) => {
                    match encoder::Mp3Encoder::new(bitrate_kbps) {
                        Ok(enc) => {
                            encoder = Some((enc, out));
                            dropped_blocks = 0;
                        }
                        Err(e) => {
                            log::error!("Failed to create MP3 encoder: {e}");
                            events.send(EngineEvent::EncodingFailed {
                                message: e.to_string(),
                            });
                        }
                    }
                }
                Ok(EngineCommand::StopEncoding) => {
                    if let Some((enc, out)) = encoder.take()
                        && let Ok(tail) = enc.finish()
                    {
                        let _ = out.try_send(tail);
                    }
                }
                Ok(EngineCommand::StartRecording { bitrate_kbps, path }) => {
                    if recorder.is_none() {
                        // Both halves are built before either is kept, and a
                        // failure keeps neither: half a recording is a file
                        // open forever with nothing written to it, and — since
                        // the guard above is `recorder.is_none()` — a recording
                        // the user can never start again this session.
                        let (failure, kind) = match (
                            encoder::Mp3Encoder::new(bitrate_kbps),
                            recorder::RecorderHandle::start(&path),
                        ) {
                            (Ok(enc), Ok(rec)) => {
                                rec_encoder = Some(enc);
                                recorder = Some(rec);
                                events.send(EngineEvent::RecordingStarted { path: path.clone() });
                                (None, None)
                            }
                            (Err(e), rec) => {
                                if let Ok(rec) = rec {
                                    rec.finish();
                                }
                                (
                                    Some(format!(
                                        "the recording encoder could not be created: {e}"
                                    )),
                                    None,
                                )
                            }
                            (_, Err(e)) => (
                                Some(format!("{} could not be created: {e}", path.display())),
                                Some(e.kind()),
                            ),
                        };
                        if let Some(message) = failure {
                            log::error!("Recording did not start: {message}");
                            events.send(EngineEvent::RecordingFailed {
                                message,
                                start: Some(RecordingStartFailure { path, kind }),
                            });
                        }
                    }
                }
                Ok(EngineCommand::StopRecording) => {
                    finalize_recording(&mut rec_encoder, &mut recorder);
                }
                Ok(EngineCommand::Shutdown) => {
                    finalize_recording(&mut rec_encoder, &mut recorder);
                    stop_sources(&mut sources, last_health.elapsed());
                    park_chains(&mut buses, &mut master_chain, &retired);
                    while let Ok(command) = commands.try_recv() {
                        if let EngineCommand::SetRouting(update) = command {
                            for bus in update.buses.into_iter().flatten() {
                                let _ = retired.send(bus.chain);
                            }
                            if let Some(chain) = update.master_chain {
                                let _ = retired.send(chain);
                            }
                        }
                    }
                    return;
                }
                Err(_) => break,
            }
        }

        // Surface capture state changes, ignoring threads from a retired
        // source set and repeats of what the UI already knows.
        while let Ok(report) = capture_rx.try_recv() {
            if report.epoch != epoch {
                continue;
            }
            let event = match report.state {
                capture::CaptureState::Failed(message) => {
                    if !failing.insert(report.name.clone()) {
                        continue;
                    }
                    EngineEvent::SourceError {
                        name: report.name,
                        message,
                    }
                }
                capture::CaptureState::Running => {
                    if !failing.remove(&report.name) {
                        continue;
                    }
                    EngineEvent::SourceRecovered { name: report.name }
                }
            };
            events.send(event);
        }

        // The playback device is only held open while something is actually
        // being monitored, so this is re-evaluated every block: a scene switch
        // or bus rebuild can retire the last monitored strip without any
        // command that mentions monitoring.
        let wanted = master_monitor
            || sources.iter().any(|s| s.monitor || s.local)
            || buses.iter().any(|b| b.monitor);
        match (wanted, monitor_out.is_some()) {
            (true, false) => {
                let (producer, consumer) = RingBuffer::new(RING_CAPACITY);
                let stop = Arc::new(AtomicBool::new(false));
                monitor::spawn(consumer, stop.clone());
                monitor_out = Some(MonitorOutput { producer, stop });
            }
            // Dropping it sets the stop flag; the thread notices and releases
            // the endpoint.
            (false, true) => monitor_out = None,
            _ => {}
        }

        mix_one_block(
            &mut sources,
            &mut buses,
            &mut master_chain,
            &mut master,
            &mut mix_block,
            &mut monitor_block,
            &mut send_scratch,
            master_monitor,
        );
        crate::vst::host2::advance_transport(mixer::BLOCK_FRAMES as u64);

        if let Some(out) = &mut monitor_out {
            // A short ring means the render thread is behind; writing what
            // fits and dropping the rest is the only option that keeps the
            // mixer on its cadence.
            let take = out.producer.slots().min(monitor_block.len());
            if take > 0
                && let Ok(mut chunk) = out.producer.write_chunk_uninit(take)
            {
                let (first, second) = chunk.as_mut_slices();
                write_uninit(first, &monitor_block[..first.len()]);
                write_uninit(second, &monitor_block[first.len()..take]);
                // SAFETY: both slices were just fully initialized above.
                unsafe { chunk.commit_all() };
            }
        }

        // Convert the block to i16 once and feed the stream and recording
        // encoders (either, both, or neither may be active).
        if encoder.is_some() || rec_encoder.is_some() {
            mixer::to_i16(&mix_block, &mut pcm_i16);
        }

        if let Some((enc, out)) = &mut encoder {
            match enc.encode(&pcm_i16) {
                Ok(bytes) if !bytes.is_empty() => {
                    use tokio::sync::mpsc::error::TrySendError;
                    match out.try_send(bytes.to_vec()) {
                        Ok(()) => dropped_blocks = 0,
                        // The Icecast task is behind — its `write_all` is
                        // waiting on a closed send window. This is a *live*
                        // stream, so dropping the newest block keeps the mixer
                        // on cadence and keeps the queue from growing at the
                        // encoded bitrate for as long as the stall lasts.
                        Err(TrySendError::Full(_)) => {
                            dropped_blocks += 1;
                            // One line per second of stall, not 100.
                            if dropped_blocks % 100 == 1 {
                                log::warn!(
                                    "Outgoing stream buffer is full; dropping audio ({dropped_blocks} blocks so far)"
                                );
                            }
                        }
                        // Consumer went away (stream ended remotely).
                        Err(TrySendError::Closed(_)) => {
                            encoder = None;
                            events.send(EngineEvent::EncodingStopped { reason: None });
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    log::error!("MP3 encode error: {e}");
                    encoder = None;
                    events.send(EngineEvent::EncodingStopped {
                        reason: Some(e.to_string()),
                    });
                }
            }
        }

        // Either failure here ends the recording outright rather than limping.
        // Clearing only `rec_encoder` used to leave `recorder` holding the file
        // open with nothing more ever written to it, and the `recorder.is_none()`
        // guard on `StartRecording` then refused every attempt to start again.
        if let (Some(enc), Some(rec)) = (&mut rec_encoder, &mut recorder) {
            let failure = match enc.encode(&pcm_i16) {
                Ok(bytes) if !bytes.is_empty() => match rec.write(bytes.to_vec()) {
                    recorder::Written::Queued => None,
                    // The writer is behind but alive; it has logged the drop
                    // itself. A recording with a gap in it is still worth
                    // finishing.
                    recorder::Written::Dropped => None,
                    recorder::Written::WriterGone => {
                        Some("the recording writer stopped".to_string())
                    }
                },
                Ok(_) => None,
                Err(e) => Some(format!("the recording encoder failed: {e}")),
            };
            if let Some(message) = failure {
                log::error!("Recording stopped: {message}");
                finalize_recording(&mut rec_encoder, &mut recorder);
                events.send(EngineEvent::RecordingFailed {
                    message,
                    start: None,
                });
            }
        }

        // Keep a steady 10 ms cadence, absorbing scheduling jitter.
        let now = Instant::now();
        if now.duration_since(last_health) >= HEALTH_INTERVAL {
            report_capture_health(&mut sources, now.duration_since(last_health));
            last_health = now;
        }
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else if now - next_tick > Duration::from_secs(1) {
            // Fell far behind (laptop sleep, debugger); resynchronize.
            next_tick = now;
        }
        next_tick += block_period;
    }
}

/// Mixes one 10 ms block: sources into master (if routed there) and into
/// their send buses (post-fader, per-send level), then each bus through its
/// strip into master, then the master strip. Allocation-free.
///
/// `monitor_block` collects the local monitoring mix. Every tap is taken
/// **post-fader and post-mute**, so what a monitored strip sounds like is
/// exactly what it contributes: muting it silences the monitor too, and its
/// volume slider moves the monitor level.
#[allow(clippy::too_many_arguments)]
fn mix_one_block(
    sources: &mut [ActiveSource],
    buses: &mut [ActiveBus],
    master_chain: &mut FxChain,
    master: &mut ChannelStrip,
    mix_block: &mut [f32],
    monitor_block: &mut [f32],
    send_scratch: &mut [f32],
    master_monitor: bool,
) {
    mix_block.fill(0.0);
    monitor_block.fill(0.0);
    for bus in buses.iter_mut() {
        bus.buffer.fill(0.0);
    }
    for source in sources.iter_mut() {
        // Ring occupancy is read here, before the pull, because this is the only
        // place it can be: the level *after* a pull is always near zero, and it
        // is the level before that says how far behind this source has fallen.
        // Integer arithmetic only — the formatting happens elsewhere, once every
        // thirty seconds.
        let fill = source.consumer.slots();
        source.window.fill_now = fill;
        source.window.peak = source.window.peak.max(fill);
        source.window.blocks += 1;
        let filled = mixer::pull_block(&mut source.consumer, &mut source.scratch);
        if filled < source.scratch.len() {
            source.window.starved_blocks += 1;
        }
        source.strip.process(&mut source.scratch);
        // One tap for both reasons a strip can be heard locally. The implicit
        // one is dropped when the strip is already arriving through the master
        // monitor below, so a broadcaster monitoring master does not hear their
        // text-to-speech twice.
        let implicit = source.local && !(master_monitor && source.to_master);
        if source.monitor || implicit {
            mixer::mix_into(monitor_block, &source.scratch);
        }
        if source.to_master {
            mixer::mix_into(mix_block, &source.scratch);
        }
        for send in &mut source.sends {
            // A stale index (bus list changed before sends re-synced) is
            // skipped rather than crashing the audio thread.
            let Some(bus) = buses.get_mut(send.bus_index) else {
                continue;
            };
            send_scratch.copy_from_slice(&source.scratch);
            send.strip.process(send_scratch);
            mixer::mix_into(&mut bus.buffer, send_scratch);
        }
    }
    for bus in buses.iter_mut() {
        bus.chain.process(&mut bus.buffer);
        bus.strip.process(&mut bus.buffer);
        if bus.monitor {
            mixer::mix_into(monitor_block, &bus.buffer);
        }
        mixer::mix_into(mix_block, &bus.buffer);
    }
    master_chain.process(mix_block);
    master.process(mix_block);
    if master_monitor {
        mixer::mix_into(monitor_block, mix_block);
    }
}

/// Hands every live `FxChain` back for the UI thread to drop.
///
/// The audio thread must never be the last owner of a plugin: dropping one runs
/// its teardown and `FreeLibrary`, which `vst::host2` and `vst::host3` both put
/// on the UI thread. So no exit from `engine_loop` may simply let `buses` and
/// `master_chain` fall out of scope — every one of them calls this first, and
/// `AudioEngine::drop` drains the channel after the join.
fn park_chains(buses: &mut Vec<ActiveBus>, master_chain: &mut FxChain, retired: &Sender<FxChain>) {
    for bus in buses.drain(..) {
        let _ = retired.send(bus.chain);
    }
    let _ = retired.send(std::mem::replace(master_chain, FxChain::empty()));
}

/// How often each capture source's health is written to the log.
///
/// Thirty seconds is chosen against the failure it exists to catch: a ring that
/// fills over half an hour, which needs enough readings across that span to show
/// a trend, and a timestamp close enough to the moment the user says "it started
/// crackling" to be matched against it. Idle sources are skipped entirely, so a
/// scene of silent loopback sources costs nothing.
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);

/// Writes one line per source that did anything this window, and starts the next
/// window.
///
/// This is why the log can answer a crackling-microphone report at all. The
/// whole source→mixer path used to be silent: dropped samples, starved blocks
/// and the device's own error flags all went unrecorded, so a user's log said
/// nothing about the one thing that had gone wrong. See `audio::health`.
fn report_capture_health(sources: &mut [ActiveSource], elapsed: Duration) {
    for source in sources.iter_mut() {
        // External feeds have no device behind them, so there is no capture to
        // report on — their samples come from elsewhere in this process.
        if source.stats.is_external() {
            source.window.roll();
            continue;
        }
        let counters = source.stats.snapshot();
        let report = health::Report {
            name: &source.name,
            capacity_samples: RING_CAPACITY,
            window: source.window,
            elapsed,
            delta: counters.since(&source.baseline),
        };
        if !report.idle() {
            log::info!("{}", report.line());
        }
        source.baseline = counters;
        source.window.roll();
    }
}

/// Stops every capture thread and drops the sources. `since` is how long the
/// current health window has been open, so the last line reports a real trend
/// rather than one scaled against a window length that never happened.
fn stop_sources(sources: &mut Vec<ActiveSource>, since: Duration) {
    for source in sources.iter() {
        source.stop.store(true, Ordering::Relaxed);
    }
    // A last line before the rings go away. This is exactly the moment a user
    // works around a misbehaving source by removing and re-adding it, so it is
    // the one reading that says what state the ring was in when they gave up on
    // it — and the replacement ring starts empty, which erases the evidence.
    report_capture_health(sources, since);
    sources.clear();
}

/// Flushes the recording encoder's tail into the file and closes it. Safe to
/// call when nothing is recording.
fn finalize_recording(
    rec_encoder: &mut Option<encoder::Mp3Encoder>,
    recorder: &mut Option<recorder::RecorderHandle>,
) {
    if let Some(enc) = rec_encoder.take()
        && let Ok(tail) = enc.finish()
        && let Some(rec) = recorder.as_ref()
    {
        // The verdict is moot here: the file is closing either way.
        let _ = rec.write(tail.to_vec());
    }
    if let Some(rec) = recorder.take() {
        rec.finish();
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;

    /// A source whose ring is pre-filled with a constant sample value.
    fn test_source(
        value: f32,
        volume: u32,
        muted: bool,
        to_master: bool,
        sends: Vec<SendSpec>,
    ) -> ActiveSource {
        let (mut producer, consumer) = RingBuffer::new(BLOCK_SAMPLES);
        for _ in 0..BLOCK_SAMPLES {
            producer.push(value).unwrap();
        }
        // Producer is dropped; the consumer still yields the buffered block.
        ActiveSource {
            name: "test".into(),
            strip: ChannelStrip::new(volume, muted),
            consumer,
            stop: Arc::new(AtomicBool::new(false)),
            scratch: vec![0f32; BLOCK_SAMPLES],
            to_master,
            sends: active_sends(sends),
            monitor: false,
            local: false,
            stats: Arc::new(health::CaptureStats::new()),
            window: health::Window::default(),
            baseline: health::Counters::default(),
        }
    }

    fn test_bus(volume: u32, muted: bool) -> ActiveBus {
        ActiveBus {
            strip: ChannelStrip::new(volume, muted),
            chain: FxChain::empty(),
            buffer: vec![0f32; BLOCK_SAMPLES],
            monitor: false,
        }
    }

    fn run_block(sources: &mut [ActiveSource], buses: &mut [ActiveBus]) -> Vec<f32> {
        run_block_monitoring(sources, buses, false).0
    }

    /// Runs one block and returns `(master mix, monitor mix)`.
    fn run_block_monitoring(
        sources: &mut [ActiveSource],
        buses: &mut [ActiveBus],
        master_monitor: bool,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut master_chain = FxChain::empty();
        let mut master = ChannelStrip::new(100, false);
        let mut mix_block = vec![0f32; BLOCK_SAMPLES];
        let mut monitor_block = vec![0f32; BLOCK_SAMPLES];
        let mut send_scratch = vec![0f32; BLOCK_SAMPLES];
        mix_one_block(
            sources,
            buses,
            &mut master_chain,
            &mut master,
            &mut mix_block,
            &mut monitor_block,
            &mut send_scratch,
            master_monitor,
        );
        (mix_block, monitor_block)
    }

    fn send(bus_index: usize, level: u32) -> SendSpec {
        SendSpec { bus_index, level }
    }

    #[test]
    fn send_is_post_fader_and_additive_with_direct_path() {
        // Source at half volume, sent to a bus at level 50: master gets the
        // direct 0.5 plus the bus's 0.5 * 0.5 = 0.25.
        let mut sources = vec![test_source(1.0, 50, false, true, vec![send(0, 50)])];
        let mut buses = vec![test_bus(100, false)];
        let out = run_block(&mut sources, &mut buses);
        assert!((out[0] - 0.75).abs() < 1e-6, "got {}", out[0]);
        assert!((out[BLOCK_SAMPLES - 1] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn to_master_off_routes_only_through_bus() {
        let mut sources = vec![test_source(1.0, 100, false, false, vec![send(0, 100)])];
        let mut buses = vec![test_bus(50, false)];
        let out = run_block(&mut sources, &mut buses);
        assert!((out[0] - 0.5).abs() < 1e-6, "bus strip applied: {}", out[0]);
    }

    #[test]
    fn muted_source_is_silent_on_its_sends_too() {
        let mut sources = vec![test_source(1.0, 100, true, true, vec![send(0, 100)])];
        let mut buses = vec![test_bus(100, false)];
        let out = run_block(&mut sources, &mut buses);
        // The mute fade starts from silence for a fresh strip, so the block
        // is fully silent.
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn muted_bus_is_silent_at_master() {
        let mut sources = vec![test_source(1.0, 100, false, false, vec![send(0, 100)])];
        let mut buses = vec![test_bus(100, true)];
        let out = run_block(&mut sources, &mut buses);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn two_sources_sum_into_one_bus() {
        let mut sources = vec![
            test_source(0.25, 100, false, false, vec![send(0, 100)]),
            test_source(0.5, 100, false, false, vec![send(0, 100)]),
        ];
        let mut buses = vec![test_bus(100, false)];
        let out = run_block(&mut sources, &mut buses);
        assert!((out[0] - 0.75).abs() < 1e-6, "got {}", out[0]);
    }

    #[test]
    fn only_monitored_sources_reach_the_monitor_mix() {
        // Half-volume monitored source plus an unmonitored one at full volume:
        // the monitor hears 0.5, master hears both.
        let mut sources = vec![
            test_source(1.0, 50, false, true, vec![]),
            test_source(1.0, 100, false, true, vec![]),
        ];
        sources[0].monitor = true;
        let mut buses = Vec::new();
        let (mix, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!(
            (monitor[0] - 0.5).abs() < 1e-6,
            "post-fader tap: {}",
            monitor[0]
        );
        assert!((mix[0] - 1.5).abs() < 1e-6, "master unaffected: {}", mix[0]);
    }

    #[test]
    fn a_muted_source_is_silent_in_the_monitor_mix() {
        // The tap is post-mute, so muting a strip silences its monitor too.
        let mut sources = vec![test_source(1.0, 100, true, true, vec![])];
        sources[0].monitor = true;
        let mut buses = Vec::new();
        let (_, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!(monitor.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn monitoring_a_bus_hears_it_after_its_own_strip() {
        let mut sources = vec![test_source(1.0, 100, false, false, vec![send(0, 100)])];
        let mut buses = vec![test_bus(50, false)];
        buses[0].monitor = true;
        let (_, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!((monitor[0] - 0.5).abs() < 1e-6, "got {}", monitor[0]);
    }

    #[test]
    fn monitoring_master_hears_the_whole_mix() {
        let mut sources = vec![
            test_source(0.25, 100, false, true, vec![]),
            test_source(0.5, 100, false, true, vec![]),
        ];
        let mut buses = Vec::new();
        let (mix, monitor) = run_block_monitoring(&mut sources, &mut buses, true);
        assert_eq!(mix, monitor, "the master tap is the master mix");
        assert!((monitor[0] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn nothing_monitored_leaves_the_monitor_mix_silent() {
        let mut sources = vec![test_source(1.0, 100, false, true, vec![])];
        let mut buses = Vec::new();
        let (_, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!(monitor.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_local_source_is_heard_without_being_monitored() {
        // A TTS strip kept off the stream: the broadcaster still hears it, at
        // its fader setting, and nothing reaches master.
        let mut sources = vec![test_source(1.0, 50, false, false, vec![])];
        sources[0].local = true;
        let mut buses = Vec::new();
        let (mix, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!(
            (monitor[0] - 0.5).abs() < 1e-6,
            "post-fader: {}",
            monitor[0]
        );
        assert!(mix.iter().all(|&s| s == 0.0), "off the stream");
    }

    #[test]
    fn a_monitored_local_source_is_heard_once() {
        // `CTRL+M` on a strip that is already heard locally must not double it.
        let mut sources = vec![test_source(1.0, 100, false, false, vec![])];
        sources[0].local = true;
        sources[0].monitor = true;
        let mut buses = Vec::new();
        let (_, monitor) = run_block_monitoring(&mut sources, &mut buses, false);
        assert!((monitor[0] - 1.0).abs() < 1e-6, "one tap: {}", monitor[0]);
    }

    #[test]
    fn the_master_monitor_does_not_double_a_local_source() {
        // Monitoring master while a local source is also reaching master: the
        // implicit tap stands down, so the speech is heard exactly once.
        let mut sources = vec![test_source(1.0, 100, false, true, vec![])];
        sources[0].local = true;
        let mut buses = Vec::new();
        let (mix, monitor) = run_block_monitoring(&mut sources, &mut buses, true);
        assert_eq!(mix, monitor, "the master tap alone");
        assert!((monitor[0] - 1.0).abs() < 1e-6, "one tap: {}", monitor[0]);
    }

    #[test]
    fn a_local_source_off_master_survives_the_master_monitor() {
        // The stand-down applies only to a strip master is actually carrying.
        // Off master, the implicit tap is the only way it is heard at all.
        let mut sources = vec![test_source(1.0, 100, false, false, vec![])];
        sources[0].local = true;
        let mut buses = Vec::new();
        let (_, monitor) = run_block_monitoring(&mut sources, &mut buses, true);
        assert!((monitor[0] - 1.0).abs() < 1e-6, "got {}", monitor[0]);
    }

    #[test]
    fn stale_send_index_is_skipped() {
        let mut sources = vec![test_source(1.0, 100, false, true, vec![send(5, 100)])];
        let mut buses = vec![test_bus(100, false)];
        let out = run_block(&mut sources, &mut buses);
        assert!((out[0] - 1.0).abs() < 1e-6, "direct path unaffected");
    }

    #[test]
    fn routing_replacements_are_reclaimed_after_acknowledgement() {
        fn bus_spec() -> BusSpec {
            BusSpec {
                volume: 100,
                muted: false,
                chain: FxChain::empty(),
                monitor: false,
            }
        }

        let engine = AudioEngine::start();
        engine.send(EngineCommand::SetMasterMonitor(true));

        // Queue every replacement before handling any acknowledgement. Master
        // monitoring keeps the engine processing even with no sources.
        for bus_count in [2, 1, 3] {
            engine.send(EngineCommand::SetRouting(Box::new(RoutingUpdate {
                buses: Some((0..bus_count).map(|_| bus_spec()).collect()),
                master_chain: Some(FxChain::empty()),
                ..Default::default()
            })));
        }

        let mut reclaimed = 0;
        for _ in 0..3 {
            assert!(matches!(
                engine.events.recv_timeout(Duration::from_secs(2)),
                Ok(EngineEvent::BusesApplied)
            ));
            reclaimed += engine.reclaim_retired_chains();
        }

        // The updates replace 0, then 2, then 1 bus chain, plus one master
        // chain each time. The final three buses and master remain active.
        assert_eq!(reclaimed, 6);
        assert_eq!(engine.reclaim_retired_chains(), 0);
    }

    /// The engine has two ways out, and both must hand their chains back: a
    /// `Shutdown` command, and the command channel simply closing. The second
    /// one only happens if a sender is dropped without a `Shutdown` — and if it
    /// ever does, letting the loop `return` would run plugin teardown and
    /// `FreeLibrary` on the audio thread, which is the one thing the hosting
    /// contract forbids.
    #[test]
    fn chains_are_handed_back_when_the_command_channel_closes() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let (retired_tx, retired_rx) = crossbeam_channel::unbounded();
        let feeds = ExternalFeeds::default();

        let thread = std::thread::spawn(move || {
            engine_loop(cmd_rx, EventSender(event_tx), retired_tx, feeds)
        });

        // Two buses and a master chain live in the engine, with no sources and
        // nothing monitoring — the state that parks the loop in `commands.recv`.
        cmd_tx
            .send(EngineCommand::SetRouting(Box::new(RoutingUpdate {
                buses: Some(
                    (0..2)
                        .map(|_| BusSpec {
                            volume: 100,
                            muted: false,
                            chain: FxChain::empty(),
                            monitor: false,
                        })
                        .collect(),
                ),
                master_chain: Some(FxChain::empty()),
                ..Default::default()
            })))
            .expect("engine is listening");

        // No `Shutdown`: just let the last sender go.
        drop(cmd_tx);
        thread.join().expect("the engine thread exits cleanly");

        // One chain per bus, one for the master, plus the two the first routing
        // update replaced (the empty initial set has only the master chain).
        let handed_back = retired_rx.try_iter().count();
        assert_eq!(handed_back, 4, "every live chain came back to this thread");
    }
}
