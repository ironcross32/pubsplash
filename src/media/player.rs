//! One media player: a thread that decodes a folder of music into a source's
//! ring, and the handle the UI drives it with.
//!
//! ## Why a thread per source
//!
//! Feeding [`ExternalFeeds`] is paced by the mixer's drain rate — pushing a
//! track takes as long as the track lasts — so it cannot happen on the UI
//! thread, and it cannot happen on the audio thread either: this opens files,
//! allocates, and runs a decoder. It is the same shape as
//! [`crate::ui::cue_feed`], one worker per source, except that this one is
//! never idle while its scene is active.
//!
//! ## Decoding
//!
//! A track is decoded *as it plays*, one packet at a time, and never held in
//! memory whole: five minutes of 48 kHz stereo f32 is 115 MB, and a library
//! folder is hours of it. Each packet is widened to stereo and resampled to
//! [`SAMPLE_RATE`] through a [`StereoStream`], which carries its interpolation
//! state across packets — resampling each packet on its own would put a click
//! at every packet boundary, forty times a second.
//!
//! ## Waiting
//!
//! Every wait in here is a `recv_timeout` on the command channel rather than a
//! sleep, so pause, skip and shutdown are acted on the moment they arrive
//! however long the timeout is. The timeouts themselves exist only for the one
//! thing that has no event behind it: the ring filling up, which drains at the
//! mixer's own pace.

use crate::audio::convert::{StereoStream, convert_to_stereo};
use crate::audio::mixer::SAMPLE_RATE;
use crate::audio::{ExternalFeeds, FeedResult};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::{Playlist, scan_folder, track_title};

/// How far ahead of the mixer this worker is allowed to decode, in samples.
///
/// The source's ring holds a whole second, and filling it would be the obvious
/// thing to do — but everything in that ring **will be heard**, whatever the
/// user presses next. Pause and next track would then take up to a second to
/// take effect, on a control whose whole job is to be immediate. A quarter of a
/// second is still ten times the length of a decoded packet, so a disk seek or
/// a slow file cannot starve the mixer, and it is short enough that a skip
/// sounds instant.
const MAX_QUEUED: usize =
    (crate::audio::mixer::SAMPLE_RATE as usize / 4) * crate::audio::mixer::CHANNELS;

/// How long to wait for a command when the only other thing to wait for is the
/// ring draining. A fraction of [`MAX_QUEUED`], so topping the ring back up is
/// never late.
const RING_WAIT: Duration = Duration::from_millis(50);

/// How long to wait between attempts when the source's ring is not there yet.
/// A scene switch sends the engine its new sources and starts these workers in
/// the same breath, so a worker can beat its own ring into existence by a block
/// or two.
const NO_RING_WAIT: Duration = Duration::from_millis(100);

/// How long a missing ring is tolerated before it is worth a log line.
const NO_RING_PATIENCE: Duration = Duration::from_secs(3);

/// How long to wait, when there is nothing to play, before looking at the
/// folder again — the case where the user is filling it while Pubsplash runs.
const EMPTY_RESCAN: Duration = Duration::from_secs(15);

/// How often the folder is re-read while playing, so files added during a long
/// session join the rotation. Checked between tracks, never mid-track.
const RESCAN_INTERVAL: Duration = Duration::from_secs(300);

/// How long to wait after a run of unplayable files before trying again.
/// Without it, a folder Pubsplash cannot read would spin this thread at the
/// speed of `File::open`.
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// How many files in a row may fail before that backoff.
///
/// Capped well below the size of a real library on purpose. The failure this
/// guards against is not one bad file — that is skipped and forgotten — but the
/// whole folder becoming unreadable at once, which is what a disconnected drive
/// looks like. Walking a thousand paths to find that out would take a thousand
/// log lines to say it.
const FAILURE_STREAK: usize = 10;

/// What a media player is doing, for the source's label in the Sources list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PlaybackState {
    /// No folder is set: the source is configured but has nothing to play.
    #[default]
    NoFolder,
    /// The folder holds no file this build can decode.
    NoFiles,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub state: PlaybackState,
    /// The track playing (or paused), without its extension.
    pub track: Option<String>,
    /// The track a skip would move to, without its extension.
    ///
    /// Published by the worker rather than worked out by the caller because the
    /// playlist lives here — and it is what "skip to the next track" is
    /// announced as, so the announcement names the track the user is about to
    /// hear instead of telling them something they already know.
    pub next: Option<String>,
}

/// What the UI can ask a running player to do.
pub enum Command {
    PlayPause,
    /// Abandon the current track and start the next one.
    Next,
    /// Abandon the current track and play this file, then carry on with the
    /// folder. The file does not have to be in the folder, or in any folder this
    /// source knows about — it is a one-off, and nothing about it is remembered.
    PlayFile(std::path::PathBuf),
    /// The source's settings changed. The folder is re-read; playback continues
    /// with the next track from the new list.
    Reload { folder: String, shuffle: bool },
}

/// A running media player. Dropping the handle does **not** stop the thread —
/// use [`Player::stop`], which is what guarantees the old worker is gone before
/// a new one can be pushing into the same ring.
pub struct Player {
    commands: Sender<Command>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// What this worker was started with, so the UI can tell whether a config
    /// edit actually changed anything.
    pub folder: String,
    pub shuffle: bool,
}

impl Player {
    pub fn start(
        source_name: String,
        feeds: ExternalFeeds,
        folder: String,
        shuffle: bool,
        generation: Arc<AtomicU64>,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let status = Arc::new(Mutex::new(Status::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("media-player".into())
            .spawn({
                let mut worker = Worker {
                    source: source_name,
                    feeds,
                    commands: rx,
                    status: status.clone(),
                    generation,
                    stop: stop.clone(),
                    folder: folder.clone(),
                    shuffle,
                    paused: false,
                    playlist: Playlist::new(Vec::new(), shuffle),
                    pending: None,
                    last_scan: None,
                    failures: 0,
                };
                move || worker.run()
            })
            .ok();
        Self {
            commands: tx,
            status,
            stop,
            thread,
            folder,
            shuffle,
        }
    }

    pub fn status(&self) -> Status {
        crate::audio::device::lock_recovering(&self.status, "Media player status").clone()
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The worker's thread id, so a test can tell "this player is still the one
    /// that was playing" from "it was stopped and started again".
    #[cfg(test)]
    pub fn thread_id(&self) -> Option<std::thread::ThreadId> {
        self.thread.as_ref().map(|t| t.thread().id())
    }

    /// Stops the worker and waits for it to let go of the ring.
    ///
    /// The join is what makes this safe rather than tidy: a detached worker
    /// would keep pushing into `ExternalFeeds` by source *name*, and a scene
    /// with a media player of the same name would find a stranger's audio in
    /// its ring. Every wait in the worker is a `recv_timeout`, and dropping the
    /// sender ends all of them at once, so this returns in about the time it
    /// takes to notice — never the length of a track.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.commands);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How a step ended: what the caller should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Carry on with what you were doing.
    Go,
    /// Stop the current track and pick another.
    NextTrack,
    /// The playlist has been replaced; stop the current track.
    Reloaded,
    /// The worker is finished.
    Stop,
}

struct Worker {
    source: String,
    feeds: ExternalFeeds,
    commands: Receiver<Command>,
    status: Arc<Mutex<Status>>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    folder: String,
    shuffle: bool,
    paused: bool,
    playlist: Playlist,
    /// A file the user picked with Open file, to play before going back to the
    /// folder. Taken by the next pass round the loop, so it survives being set
    /// from anywhere a command is drained.
    pending: Option<PathBuf>,
    /// When the folder was last read, so a long session picks up new files.
    last_scan: Option<Instant>,
    /// Tracks that failed to produce any audio since the last one that did.
    failures: usize,
}

impl Worker {
    /// The waits in here test only for [`Flow::Stop`], which is safe *here* and
    /// nowhere else: every one of them is followed by `continue`, and the top of
    /// this loop is where a skip or a reload is acted on anyway. Inside
    /// [`Worker::feed`] the same shape drops the command on the floor — see the
    /// note there.
    fn run(&mut self) {
        self.rescan();
        loop {
            if self.stopping() {
                return;
            }
            // A file the user opened outright is played before anything the
            // folder has to say — including when the folder is empty, or not set
            // at all, which is a perfectly reasonable way to use this source.
            if let Some(path) = self.pending.take() {
                self.set_status(PlaybackState::Playing, Some(track_title(&path)));
                if self.play(&path) == Flow::Stop {
                    return;
                }
                continue;
            }
            if self.playlist.is_empty() {
                let state = if self.folder.trim().is_empty() {
                    PlaybackState::NoFolder
                } else {
                    PlaybackState::NoFiles
                };
                self.set_status(state, None);
                if self.wait(EMPTY_RESCAN) == Flow::Stop {
                    return;
                }
                if self.folder_is_stale(EMPTY_RESCAN) {
                    self.rescan();
                }
                continue;
            }
            if self.paused {
                // Paused with nothing to do: the wait ends the moment a command
                // arrives, so this costs one wakeup a second.
                if self.wait(Duration::from_secs(1)) == Flow::Stop {
                    return;
                }
                continue;
            }
            // A run of files that produced no audio at all: back off rather
            // than reopening them as fast as the disk allows, and read the
            // folder again — the usual cause is that it is not there any more.
            if self.failures >= FAILURE_STREAK.min(self.playlist.len().max(1)) {
                self.failures = 0;
                log::warn!(
                    "Media player {:?}: nothing in {} could be played; trying again shortly",
                    self.source,
                    self.folder
                );
                self.last_scan = None;
                if self.wait(FAILURE_BACKOFF) == Flow::Stop {
                    return;
                }
                continue;
            }
            if self.folder_is_stale(RESCAN_INTERVAL) {
                self.rescan();
            }
            let Some(track) = self.playlist.next() else {
                continue;
            };
            self.set_status(PlaybackState::Playing, Some(track_title(&track)));
            if self.play(&track) == Flow::Stop {
                return;
            }
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Re-reads the folder. An unchanged list keeps the current playlist, so a
    /// rescan mid-session does not reshuffle what is already in progress.
    fn rescan(&mut self) {
        self.last_scan = Some(Instant::now());
        let folder = self.folder.trim();
        let files = if folder.is_empty() {
            Vec::new()
        } else {
            scan_folder(Path::new(folder))
        };
        if files.as_slice() == self.playlist.files() {
            return;
        }
        log::info!(
            "Media player {:?}: {} playable files in {:?}",
            self.source,
            files.len(),
            folder
        );
        self.failures = 0;
        self.playlist = Playlist::new(files, self.shuffle);
    }

    fn folder_is_stale(&self, after: Duration) -> bool {
        self.last_scan.is_none_or(|at| at.elapsed() >= after)
    }

    /// Publishes what this player is doing. The *upcoming* track is read off
    /// the playlist here rather than passed in, so every path that changes the
    /// status refreshes it — a rescan or a reload moves it as surely as a track
    /// ending does.
    fn set_status(&self, state: PlaybackState, track: Option<String>) {
        let next = Status {
            state,
            track,
            next: self.playlist.peek().map(|path| track_title(&path)),
        };
        let mut status =
            crate::audio::device::lock_recovering(&self.status, "Media player status");
        if *status == next {
            return;
        }
        *status = next;
        drop(status);
        // The pump notices the counter moved and re-derives the source's label;
        // see `ui::media`.
        self.generation.fetch_add(1, Ordering::Relaxed);
        wxdragon::wake_up_idle();
    }

    /// Waits up to `timeout` for a command, acting on whatever arrives.
    fn wait(&mut self, timeout: Duration) -> Flow {
        match self.commands.recv_timeout(timeout) {
            Ok(command) => self.apply(command),
            Err(RecvTimeoutError::Timeout) => {
                if self.stopping() {
                    Flow::Stop
                } else {
                    Flow::Go
                }
            }
            // The handle is gone, which only happens through `Player::stop`.
            Err(RecvTimeoutError::Disconnected) => Flow::Stop,
        }
    }

    /// Acts on everything already queued, without waiting.
    fn drain_commands(&mut self) -> Flow {
        loop {
            match self.commands.try_recv() {
                Ok(command) => match self.apply(command) {
                    Flow::Go => {}
                    other => return other,
                },
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    return if self.stopping() { Flow::Stop } else { Flow::Go };
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => return Flow::Stop,
            }
        }
    }

    fn apply(&mut self, command: Command) -> Flow {
        match command {
            Command::PlayPause => {
                self.paused = !self.paused;
                Flow::Go
            }
            Command::Next => {
                // Skipping while paused starts playing again: the user asked
                // for a different track, not for a different silence.
                self.paused = false;
                Flow::NextTrack
            }
            // The same skip, to a file of the user's choosing rather than the
            // playlist's. Nothing is remembered: when it ends, the folder picks
            // up exactly where it would have.
            Command::PlayFile(path) => {
                self.paused = false;
                self.pending = Some(path);
                Flow::NextTrack
            }
            Command::Reload { folder, shuffle } => {
                self.folder = folder;
                self.shuffle = shuffle;
                // Forces a real rescan even if the list is unchanged, since the
                // shuffle setting may be what moved.
                self.playlist = Playlist::new(Vec::new(), shuffle);
                self.rescan();
                Flow::Reloaded
            }
        }
    }

    /// Decodes one file into the source's ring, in step with the mixer.
    fn play(&mut self, path: &Path) -> Flow {
        log::debug!("Media player {:?}: playing {}", self.source, path.display());
        match self.decode(path) {
            Ok(flow) => flow,
            Err(message) => {
                // One line per unreadable file, then on to the next: a folder
                // with a broken file in it is not a broken media player.
                log::warn!(
                    "Media player {:?}: could not play {}: {message}",
                    self.source,
                    path.display()
                );
                self.failures += 1;
                Flow::Go
            }
        }
    }

    fn decode(&mut self, path: &Path) -> Result<Flow, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                // Gapless playback trims the encoder padding an MP3 carries at
                // both ends, which is the difference between a seamless album
                // and a click between every track.
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| e.to_string())?;
        let mut format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| "the file has no audio track".to_string())?;
        let track_id = track.id;
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| e.to_string())?;

        let mut buffer: Option<SampleBuffer<f32>> = None;
        let mut resampler: Option<(u32, StereoStream)> = None;
        let mut played_anything = false;

        loop {
            // Commands first, so skip and pause are answered between packets
            // rather than at the end of the track.
            match self.drain_commands() {
                Flow::Go => {}
                other => return Ok(other),
            }
            if self.paused && self.wait_while_paused() == Flow::Stop {
                return Ok(Flow::Stop);
            }
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                // Symphonia reports a clean end of stream as an IO error, and a
                // torn tail is still worth having played, so any error here ends
                // the track rather than failing it.
                Err(_) => break,
            };
            if packet.track_id() != track_id {
                continue;
            }
            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => decoded,
                // A damaged packet is skipped; the format layer has already
                // resynchronized by the time it says so.
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    if played_anything {
                        break;
                    }
                    return Err(e.to_string());
                }
            };
            let spec = *decoded.spec();
            let buffer = buffer
                .get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            buffer.copy_interleaved_ref(decoded);
            let channels = spec.channels.count().max(1);
            let stereo = convert_to_stereo(buffer.samples(), channels)?;

            let samples = if spec.rate == SAMPLE_RATE {
                stereo
            } else {
                // Rebuilt only if the file's rate actually changes mid-stream,
                // which is legal in a few containers and vanishingly rare.
                if !matches!(&resampler, Some((rate, _)) if *rate == spec.rate) {
                    resampler = Some((spec.rate, StereoStream::new(spec.rate)));
                }
                match &mut resampler {
                    Some((_, stream)) => stream.push(&stereo),
                    None => unreachable!("a resampler was just installed"),
                }
            };
            if samples.is_empty() {
                continue;
            }
            played_anything = true;
            match self.feed(&samples) {
                Flow::Go => {}
                other => return Ok(other),
            }
        }

        // The resampler holds back the frame it still needed a neighbour for.
        if let Some((_, stream)) = &mut resampler {
            let tail = stream.finish();
            if !tail.is_empty() {
                played_anything = true;
                match self.feed(&tail) {
                    Flow::Go => {}
                    other => return Ok(other),
                }
            }
        }

        if played_anything {
            self.failures = 0;
            Ok(Flow::Go)
        } else {
            Err("no audio could be decoded from it".to_string())
        }
    }

    /// Holds the current track where it is until play resumes. The ring drains
    /// into silence on its own, which is what makes pause immediate.
    fn wait_while_paused(&mut self) -> Flow {
        let track = self.status().track;
        self.set_status(PlaybackState::Paused, track.clone());
        while self.paused {
            match self.wait(Duration::from_secs(1)) {
                Flow::Go => {}
                other => return other,
            }
        }
        self.set_status(PlaybackState::Playing, track);
        Flow::Go
    }

    fn status(&self) -> Status {
        crate::audio::device::lock_recovering(&self.status, "Media player status").clone()
    }

    /// Pushes a decoded chunk into the source's ring, pacing to the mixer.
    ///
    /// **Every wait in here must return what it was handed.** [`Worker::wait`]
    /// takes a command off the channel and acts on it, so a flow it produces is
    /// the only remaining evidence that the command arrived — a caller that
    /// tests it against one variant and carries on has silently eaten a
    /// keypress. This loop is where a media player spends nearly all of its
    /// life (it keeps a quarter of a second queued and waits out the rest), so
    /// nearly every skip the user presses is answered here, and testing only
    /// for [`Flow::Stop`] is what made next track do nothing at all.
    fn feed(&mut self, samples: &[f32]) -> Flow {
        let mut offset = 0;
        let mut missing_since: Option<Instant> = None;
        while offset < samples.len() {
            match self.drain_commands() {
                Flow::Go => {}
                other => return other,
            }
            if self.paused {
                match self.wait_while_paused() {
                    Flow::Go => {}
                    other => return other,
                }
            }
            // Stay only just ahead of the mixer. See `MAX_QUEUED`.
            if self
                .feeds
                .queued(&self.source)
                .is_some_and(|queued| queued >= MAX_QUEUED)
            {
                match self.wait(RING_WAIT) {
                    Flow::Go => {}
                    other => return other,
                }
                continue;
            }
            match self.feeds.push(&self.source, &samples[offset..]) {
                FeedResult::Done => return Flow::Go,
                FeedResult::Full { accepted } => {
                    offset += accepted;
                    missing_since = None;
                    if offset < samples.len() {
                        match self.wait(RING_WAIT) {
                            Flow::Go => {}
                            other => return other,
                        }
                    }
                }
                // The source is not in the mixer right now. Waiting rather than
                // dropping the audio: this is the ordinary state for the block
                // or two between a scene switch and the engine applying it, and
                // a worker whose source is really gone is stopped and joined.
                FeedResult::Gone => {
                    let since = missing_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= NO_RING_PATIENCE {
                        log::debug!(
                            "Media player {:?}: waiting for the mixer to take this source",
                            self.source
                        );
                        missing_since = Some(Instant::now());
                    }
                    match self.wait(NO_RING_WAIT) {
                        Flow::Go => {}
                        other => return other,
                    }
                }
            }
        }
        Flow::Go
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder holding one WAV of a tone, at a rate that is not the engine's
    /// so the resampling path is exercised too.
    fn folder_with_a_tone(name: &str) -> std::path::PathBuf {
        use hound::{SampleFormat, WavSpec, WavWriter};
        let folder = std::env::temp_dir().join(format!("pubsplash-player-{name}"));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let rate = 44_100;
        let spec = WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::create(folder.join("tone.wav"), spec).unwrap();
        // Two seconds, so the ring fills and the player is still mid-track when
        // the test looks.
        for frame in 0..rate * 2 {
            let phase = frame as f32 / rate as f32 * 440.0 * std::f32::consts::TAU;
            let sample = (phase.sin() * 16_384.0) as i16;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        folder
    }

    /// A folder of several named tracks, so a test can tell which one is
    /// playing. Long enough that nothing reaches its end on its own.
    fn folder_of_tracks(name: &str, tracks: &[&str]) -> std::path::PathBuf {
        use hound::{SampleFormat, WavSpec, WavWriter};
        let folder = std::env::temp_dir().join(format!("pubsplash-player-{name}"));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let rate = 48_000;
        for track in tracks {
            let spec = WavSpec {
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            };
            let mut writer =
                WavWriter::create(folder.join(format!("{track}.wav")), spec).unwrap();
            for frame in 0..rate * 30 {
                let phase = frame as f32 / rate as f32 * 440.0 * std::f32::consts::TAU;
                let sample = (phase.sin() * 16_384.0) as i16;
                writer.write_sample(sample).unwrap();
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        folder
    }

    /// Skipping moves to the next track rather than staying where it was.
    #[test]
    fn next_track_starts_the_next_track() {
        let folder = folder_of_tracks("next", &["a", "b"]);
        let feeds = ExternalFeeds::default();
        let mut ring =
            feeds.park_for_test("Media Player", crate::audio::mixer::SAMPLE_RATE as usize);
        let player = Player::start(
            "Media Player".to_string(),
            feeds,
            folder.to_string_lossy().into_owned(),
            false,
            Arc::new(AtomicU64::new(0)),
        );

        // Drain like the mixer would, so the worker is doing real work rather
        // than parked against a full ring.
        let drain = Arc::new(AtomicBool::new(false));
        let drainer = std::thread::spawn({
            let drain = drain.clone();
            move || {
                while !drain.load(Ordering::Relaxed) {
                    let slots = ring.slots();
                    if slots > 0 && let Ok(chunk) = ring.read_chunk(slots) {
                        chunk.commit_all();
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while player.status().track.as_deref() != Some("a") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(player.status().track.as_deref(), Some("a"), "first track");
        // What the skip about to be pressed will be announced as.
        assert_eq!(player.status().next.as_deref(), Some("b"));

        player.send(Command::Next);

        let deadline = Instant::now() + Duration::from_secs(5);
        while player.status().track.as_deref() == Some("a") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let after = player.status();
        drain.store(true, Ordering::Relaxed);
        let _ = drainer.join();
        player.stop();
        let _ = std::fs::remove_dir_all(&folder);

        assert_eq!(after.state, PlaybackState::Playing);
        assert_eq!(after.track.as_deref(), Some("b"), "skip stayed put");
    }

    /// Open file: the chosen track interrupts what was playing, and the folder
    /// picks up afterwards as though it had never happened.
    #[test]
    fn an_opened_file_plays_and_leaves_the_folder_where_it_was() {
        let folder = folder_of_tracks("open", &["a", "b"]);
        // Deliberately somewhere else: the point of the feature is that the file
        // does not have to be in the source's folder.
        let elsewhere = folder_of_tracks("open-elsewhere", &["chosen"]);
        let feeds = ExternalFeeds::default();
        let mut ring =
            feeds.park_for_test("Media Player", crate::audio::mixer::SAMPLE_RATE as usize);
        let player = Player::start(
            "Media Player".to_string(),
            feeds,
            folder.to_string_lossy().into_owned(),
            false,
            Arc::new(AtomicU64::new(0)),
        );

        let drain = Arc::new(AtomicBool::new(false));
        let drainer = std::thread::spawn({
            let drain = drain.clone();
            move || {
                while !drain.load(Ordering::Relaxed) {
                    let slots = ring.slots();
                    if slots > 0 && let Ok(chunk) = ring.read_chunk(slots) {
                        chunk.commit_all();
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let wait_for = |what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while player.status().track.as_deref() != Some(what) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            player.status()
        };

        assert_eq!(wait_for("a").track.as_deref(), Some("a"), "first track");
        player.send(Command::PlayFile(elsewhere.join("chosen.wav")));
        let playing = wait_for("chosen");

        // And on from there: the folder resumes with the track it was up to,
        // not with a reshuffled or restarted list.
        player.send(Command::Next);
        let after = wait_for("b");

        drain.store(true, Ordering::Relaxed);
        let _ = drainer.join();
        player.stop();
        let _ = std::fs::remove_dir_all(&folder);
        let _ = std::fs::remove_dir_all(&elsewhere);

        assert_eq!(playing.track.as_deref(), Some("chosen"));
        assert_eq!(playing.state, PlaybackState::Playing);
        // The folder never stopped being the folder, so a skip out of the
        // opened file is announced as — and plays — the track that was next.
        assert_eq!(playing.next.as_deref(), Some("b"));
        assert_eq!(after.track.as_deref(), Some("b"));
    }

    /// The whole path in one: a file on disk is found, decoded, resampled to
    /// the engine's rate and pushed into the source's ring.
    #[test]
    fn a_folder_of_music_reaches_the_mixer() {
        let folder = folder_with_a_tone("plays");
        let feeds = ExternalFeeds::default();
        let mut ring = feeds.park_for_test("Media Player", crate::audio::mixer::SAMPLE_RATE as usize);
        let player = Player::start(
            "Media Player".to_string(),
            feeds,
            folder.to_string_lossy().into_owned(),
            true,
            Arc::new(AtomicU64::new(0)),
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while ring.slots() < 4_800 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let available = ring.slots();
        let status = player.status();
        player.stop();
        let _ = std::fs::remove_dir_all(&folder);

        assert!(available >= 4_800, "only {available} samples arrived");
        let chunk = ring.read_chunk(4_800).unwrap();
        let (first, _) = chunk.as_slices();
        let peak = first.iter().fold(0f32, |max, s| max.max(s.abs()));
        assert!(peak > 0.4, "the tone arrived silent (peak {peak})");
        assert_eq!(status.state, PlaybackState::Playing);
        assert_eq!(status.track.as_deref(), Some("tone"));
    }

    /// Pausing stops the audio, and the ring drains rather than being refilled.
    #[test]
    fn pausing_stops_the_music() {
        let folder = folder_with_a_tone("pauses");
        let feeds = ExternalFeeds::default();
        let mut ring = feeds.park_for_test("Media Player", crate::audio::mixer::SAMPLE_RATE as usize);
        let player = Player::start(
            "Media Player".to_string(),
            feeds,
            folder.to_string_lossy().into_owned(),
            true,
            Arc::new(AtomicU64::new(0)),
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while ring.slots() < 4_800 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        player.send(Command::PlayPause);
        let paused_by = Instant::now() + Duration::from_secs(2);
        while player.status().state != PlaybackState::Paused && Instant::now() < paused_by {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(player.status().state, PlaybackState::Paused);

        // Drain what was already decoded; nothing more may arrive.
        let queued = ring.slots();
        let chunk = ring.read_chunk(queued).unwrap();
        chunk.commit_all();
        std::thread::sleep(Duration::from_millis(300));
        let after = ring.slots();
        player.stop();
        let _ = std::fs::remove_dir_all(&folder);

        assert_eq!(after, 0, "a paused player kept feeding the mixer");
    }

    /// A player whose source has no ring at all: nothing consumes what it
    /// decodes, which is the state a scene switch leaves it in for a block or
    /// two. It must wait rather than spin, and it must still stop on request.
    #[test]
    fn a_player_with_nowhere_to_feed_still_stops() {
        let player = Player::start(
            "Media Player".to_string(),
            ExternalFeeds::default(),
            String::new(),
            true,
            Arc::new(AtomicU64::new(0)),
        );
        // No folder, so it settles into the "nothing to play" wait.
        let started = Instant::now();
        player.stop();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop must not wait out the rescan timer"
        );
    }

    #[test]
    fn an_unset_folder_reports_itself_rather_than_an_error() {
        let player = Player::start(
            "Media Player".to_string(),
            ExternalFeeds::default(),
            String::new(),
            true,
            Arc::new(AtomicU64::new(0)),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && player.status().state != PlaybackState::NoFolder {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(player.status().state, PlaybackState::NoFolder);
        assert_eq!(player.status().track, None);
        player.stop();
    }
}
