//! The audio engine: owns capture threads and the mixer loop, encodes MP3
//! while streaming, and reacts to UI commands (volumes, mutes, scene
//! switches) without ever blocking the UI thread.

pub mod capture;
pub mod device;
pub mod encoder;
pub mod fx_chain;
pub mod mixer;
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
}

pub enum EngineCommand {
    /// Replace the active source set (scene switch, reorder, add/remove).
    /// Index order matches the mixer order.
    SetSources(Vec<SourceSpec>),
    SetSourceVolume(usize, u32),
    SetSourceMute(usize, bool),
    /// Replace the bus set, including each bus's FX chain. Send `SetSources`
    /// (or `SetSourceSends`) after a bus reorder so send indices match again.
    SetBuses(Vec<BusSpec>),
    SetBusVolume(usize, u32),
    SetBusMute(usize, bool),
    /// Replace the master output's FX chain.
    SetMasterChain(FxChain),
    /// Toggle bypass on one plugin. `bus: None` targets the master chain.
    SetFxBypass {
        bus: Option<usize>,
        slot: usize,
        bypass: bool,
    },
    SetMasterVolume(u32),
    SetMasterMute(bool),
    /// Begin encoding; encoded MP3 chunks flow into the sender (consumed by
    /// the Icecast task on the network runtime).
    StartEncoding {
        bitrate_kbps: u32,
        out: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
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
    /// A capture source failed (device unplugged, process exited...).
    SourceError { name: String, message: String },
    /// Encoding stopped because the outgoing channel closed.
    EncodingStopped,
    /// The engine finished applying a `SetBuses`; any plugin instances the
    /// UI retired are now unreferenced by the audio thread.
    BusesApplied,
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

impl ExternalFeeds {
    /// Pushes as many samples as fit into the named source's ring. Callers
    /// stream long audio by retrying the remainder as the mixer drains.
    pub fn push(&self, name: &str, samples: &[f32]) -> FeedResult {
        let mut map = self.0.lock().unwrap();
        let Some(producer) = map.get_mut(name) else {
            return FeedResult::Gone;
        };
        let mut accepted = 0;
        for &sample in samples {
            if producer.push(sample).is_err() {
                return FeedResult::Full { accepted };
            }
            accepted += 1;
        }
        FeedResult::Done
    }

    fn insert(&self, name: String, producer: rtrb::Producer<f32>) {
        self.0.lock().unwrap().insert(name, producer);
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

struct ActiveSource {
    #[allow(dead_code)]
    name: String,
    strip: ChannelStrip,
    consumer: rtrb::Consumer<f32>,
    stop: Arc<AtomicBool>,
    /// Scratch block for this source's pull, reused across iterations.
    scratch: Vec<f32>,
    to_master: bool,
    sends: Vec<ActiveSend>,
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
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let feeds = ExternalFeeds::default();
        let feeds_clone = feeds.clone();
        let thread = std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || engine_loop(cmd_rx, event_tx, feeds_clone))
            .expect("spawning audio engine thread");
        Self {
            commands: cmd_tx,
            events: event_rx,
            external_feeds: feeds,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: EngineCommand) {
        let _ = self.commands.send(command);
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.commands.send(EngineCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn engine_loop(
    commands: Receiver<EngineCommand>,
    events: Sender<EngineEvent>,
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

    let (capture_err_tx, capture_err_rx) = crossbeam_channel::unbounded::<(String, String)>();
    let mut sources: Vec<ActiveSource> = Vec::new();
    let mut buses: Vec<ActiveBus> = Vec::new();
    let mut master_chain = FxChain::empty();
    let mut master = ChannelStrip::new(100, false);
    let mut encoder: Option<(
        encoder::Mp3Encoder,
        tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    )> = None;
    // Recording runs on its own encoder so it works with or without streaming.
    let mut rec_encoder: Option<encoder::Mp3Encoder> = None;
    let mut recorder: Option<recorder::Recorder> = None;

    let block_period = Duration::from_millis(10);
    let mut next_tick = Instant::now() + block_period;
    let mut mix_block = vec![0f32; BLOCK_SAMPLES];
    let mut send_scratch = vec![0f32; BLOCK_SAMPLES];
    let mut pcm_i16: Vec<i16> = Vec::with_capacity(BLOCK_SAMPLES);

    loop {
        // Drain pending commands without blocking the mix cadence.
        loop {
            match commands.try_recv() {
                Ok(EngineCommand::SetSources(specs)) => {
                    stop_sources(&mut sources);
                    feeds.clear();
                    for spec in specs {
                        let (producer, consumer) = RingBuffer::new(RING_CAPACITY);
                        let stop = Arc::new(AtomicBool::new(false));
                        match spec.feed {
                            FeedKind::Capture(kind) => {
                                capture::spawn(
                                    spec.name.clone(),
                                    kind,
                                    producer,
                                    stop.clone(),
                                    capture_err_tx.clone(),
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
                        });
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
                Ok(EngineCommand::SetBuses(specs)) => {
                    buses = specs
                        .into_iter()
                        .map(|spec| ActiveBus {
                            strip: ChannelStrip::new(spec.volume, spec.muted),
                            chain: spec.chain,
                            buffer: vec![0f32; BLOCK_SAMPLES],
                        })
                        .collect();
                    let _ = events.send(EngineEvent::BusesApplied);
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
                Ok(EngineCommand::SetMasterChain(chain)) => {
                    master_chain = chain;
                    let _ = events.send(EngineEvent::BusesApplied);
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
                Ok(EngineCommand::StartEncoding { bitrate_kbps, out }) => {
                    match encoder::Mp3Encoder::new(bitrate_kbps) {
                        Ok(enc) => encoder = Some((enc, out)),
                        Err(e) => log::error!("Failed to create MP3 encoder: {e}"),
                    }
                }
                Ok(EngineCommand::StopEncoding) => {
                    if let Some((enc, out)) = encoder.take() {
                        if let Ok(tail) = enc.finish() {
                            let _ = out.send(tail);
                        }
                    }
                }
                Ok(EngineCommand::StartRecording { bitrate_kbps, path }) => {
                    if recorder.is_none() {
                        match (
                            encoder::Mp3Encoder::new(bitrate_kbps),
                            recorder::Recorder::new(&path),
                        ) {
                            (Ok(enc), Ok(rec)) => {
                                rec_encoder = Some(enc);
                                recorder = Some(rec);
                            }
                            (Err(e), _) => {
                                log::error!("Failed to create recording encoder: {e}")
                            }
                            (_, Err(e)) => {
                                log::error!("Failed to start recording {path:?}: {e}")
                            }
                        }
                    }
                }
                Ok(EngineCommand::StopRecording) => {
                    finalize_recording(&mut rec_encoder, &mut recorder);
                }
                Ok(EngineCommand::Shutdown) => {
                    finalize_recording(&mut rec_encoder, &mut recorder);
                    stop_sources(&mut sources);
                    return;
                }
                Err(_) => break,
            }
        }

        // Surface capture errors.
        while let Ok((name, message)) = capture_err_rx.try_recv() {
            let _ = events.send(EngineEvent::SourceError { name, message });
        }

        mix_one_block(
            &mut sources,
            &mut buses,
            &mut master_chain,
            &mut master,
            &mut mix_block,
            &mut send_scratch,
        );
        crate::vst::host2::advance_transport(mixer::BLOCK_FRAMES as u64);

        // Convert the block to i16 once and feed the stream and recording
        // encoders (either, both, or neither may be active).
        if encoder.is_some() || rec_encoder.is_some() {
            mixer::to_i16(&mix_block, &mut pcm_i16);
        }

        if let Some((enc, out)) = &mut encoder {
            match enc.encode(&pcm_i16) {
                Ok(bytes) if !bytes.is_empty() => {
                    if out.send(bytes.to_vec()).is_err() {
                        // Consumer went away (stream ended remotely).
                        encoder = None;
                        let _ = events.send(EngineEvent::EncodingStopped);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    log::error!("MP3 encode error: {e}");
                    encoder = None;
                    let _ = events.send(EngineEvent::EncodingStopped);
                }
            }
        }

        if let (Some(enc), Some(rec)) = (&mut rec_encoder, &mut recorder) {
            match enc.encode(&pcm_i16) {
                Ok(bytes) if !bytes.is_empty() => rec.write(bytes),
                Ok(_) => {}
                Err(e) => {
                    log::error!("Recording encode error: {e}");
                    rec_encoder = None;
                }
            }
        }

        // Keep a steady 10 ms cadence, absorbing scheduling jitter.
        let now = Instant::now();
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
fn mix_one_block(
    sources: &mut [ActiveSource],
    buses: &mut [ActiveBus],
    master_chain: &mut FxChain,
    master: &mut ChannelStrip,
    mix_block: &mut [f32],
    send_scratch: &mut [f32],
) {
    mix_block.fill(0.0);
    for bus in buses.iter_mut() {
        bus.buffer.fill(0.0);
    }
    for source in sources.iter_mut() {
        let available = source.consumer.slots();
        let take = available.min(BLOCK_SAMPLES);
        source.scratch[..take].iter_mut().for_each(|s| *s = 0.0);
        for slot in source.scratch[..take].iter_mut() {
            if let Ok(sample) = source.consumer.pop() {
                *slot = sample;
            }
        }
        source.scratch[take..].fill(0.0);
        source.strip.process(&mut source.scratch);
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
        mixer::mix_into(mix_block, &bus.buffer);
    }
    master_chain.process(mix_block);
    master.process(mix_block);
}

fn stop_sources(sources: &mut Vec<ActiveSource>) {
    for source in sources.iter() {
        source.stop.store(true, Ordering::Relaxed);
    }
    sources.clear();
}

/// Flushes the recording encoder's tail into the file and closes it. Safe to
/// call when nothing is recording.
fn finalize_recording(
    rec_encoder: &mut Option<encoder::Mp3Encoder>,
    recorder: &mut Option<recorder::Recorder>,
) {
    if let Some(enc) = rec_encoder.take() {
        if let Ok(tail) = enc.finish() {
            if let Some(rec) = recorder {
                rec.write(&tail);
            }
        }
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
        }
    }

    fn test_bus(volume: u32, muted: bool) -> ActiveBus {
        ActiveBus {
            strip: ChannelStrip::new(volume, muted),
            chain: FxChain::empty(),
            buffer: vec![0f32; BLOCK_SAMPLES],
        }
    }

    fn run_block(sources: &mut [ActiveSource], buses: &mut [ActiveBus]) -> Vec<f32> {
        let mut master_chain = FxChain::empty();
        let mut master = ChannelStrip::new(100, false);
        let mut mix_block = vec![0f32; BLOCK_SAMPLES];
        let mut send_scratch = vec![0f32; BLOCK_SAMPLES];
        mix_one_block(
            sources,
            buses,
            &mut master_chain,
            &mut master,
            &mut mix_block,
            &mut send_scratch,
        );
        mix_block
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
    fn stale_send_index_is_skipped() {
        let mut sources = vec![test_source(1.0, 100, false, true, vec![send(5, 100)])];
        let mut buses = vec![test_bus(100, false)];
        let out = run_block(&mut sources, &mut buses);
        assert!((out[0] - 1.0).abs() < 1e-6, "direct path unaffected");
    }
}
