//! Capture threads: each audio source that reads from the OS runs one of
//! these, producing interleaved stereo f32 at 48 kHz into a ring buffer the
//! mixer drains.

use crate::audio::device;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use rtrb::Producer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureKind {
    Microphone { device_id: Option<String> },
    DesktopAudio,
    Application { pid: u32 },
}

/// Spawns a capture thread. It runs until `stop` is set or the device fails.
/// Returns the join handle; errors inside the thread are logged and reported
/// through `on_error`.
pub fn spawn(
    name: String,
    kind: CaptureKind,
    mut producer: Producer<f32>,
    stop: Arc<AtomicBool>,
    on_error: crossbeam_channel::Sender<(String, String)>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("capture-{name}"))
        .spawn(move || {
            device::ensure_com_initialized();
            if let Err(e) = run(&kind, &mut producer, &stop) {
                log::error!("Capture source {name:?} failed: {e}");
                let _ = on_error.send((name, e));
            }
        })
        .expect("spawning capture thread")
}

fn run(kind: &CaptureKind, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), String> {
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );

    let mut client = match kind {
        CaptureKind::Microphone { device_id } => device::capture_device(device_id.as_deref())?
            .get_iaudioclient()
            .map_err(|e| e.to_string())?,
        // Desktop Audio is process-exclusion loopback with our own process
        // as the excluded tree: all system audio except Pubsplash itself.
        // This keeps locally played TTS and sound cues out of the capture,
        // so they can never feed back into the stream.
        CaptureKind::DesktopAudio => {
            AudioClient::new_application_loopback_client(std::process::id(), false)
                .map_err(|e| e.to_string())?
        }
        CaptureKind::Application { pid } => {
            AudioClient::new_application_loopback_client(*pid, true).map_err(|e| e.to_string())?
        }
    };

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|e| e.to_string())?;

    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    let blockalign = format.get_blockalign() as usize;

    client.start_stream().map_err(|e| e.to_string())?;

    let mut byte_queue: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        let new_frames = capture
            .get_next_packet_size()
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        if new_frames > 0 {
            byte_queue.reserve(new_frames as usize * blockalign);
            capture
                .read_from_device_to_deque(&mut byte_queue)
                .map_err(|e| e.to_string())?;
            push_f32(&mut byte_queue, producer);
        }
        if event.wait_for_event(1000).is_err() {
            // Timeouts are normal for loopback with no audio playing; only
            // treat it as fatal if the client stopped.
            continue;
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Moves whole f32 samples from the byte queue into the ring, dropping
/// samples when the ring is full (mixer stalled or source unattached).
fn push_f32(bytes: &mut std::collections::VecDeque<u8>, producer: &mut Producer<f32>) {
    while bytes.len() >= 4 {
        let sample = f32::from_le_bytes([
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
            bytes.pop_front().unwrap(),
        ]);
        // Full ring: discard. Capture must never block.
        let _ = producer.push(sample);
    }
}
