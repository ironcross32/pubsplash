//! SAPI 5 engine: voice enumeration, asynchronous local speech, and
//! synthesis into the outgoing stream.
//!
//! Speech runs on a dedicated COM (STA) thread owning the SAPI voices;
//! requests arrive over a channel so callers never block. When a request
//! has `to_stream` set, the text is synthesized to memory at the mixer
//! format (48 kHz stereo 16-bit) and trickle-fed into the named source's
//! ring in the audio engine, in addition to local playback. Desktop Audio
//! capture excludes Pubsplash's own process, so local playback can never
//! loop back into the stream.

use super::engine::SynthRequest;
use super::queue::Queue;
use crate::audio::ExternalFeeds;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::{
    ISpObjectToken, ISpObjectTokenCategory, ISpStream, ISpVoice, SPF_ASYNC, SpObjectTokenCategory,
    SpStream, SpVoice,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IStream,
    STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET, StructuredStorage::CreateStreamOnHGlobal,
};
use windows::core::{GUID, PCWSTR};

const SPCAT_VOICES: &str = r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices";
/// SPDFID_WaveFormatEx from sapi.h.
const SPDFID_WAVEFORMATEX: GUID = GUID::from_u128(0xC31ADBAE_527F_4FF5_A230_F62BB61FF70C);
const WAVE_FORMAT_PCM: u16 = 1;

/// Installed SAPI voice display names.
pub fn voice_names() -> Vec<String> {
    let mut voices = Vec::new();
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Speech\Voices\Tokens",
            "/s",
            "/ve",
        ])
        .output();
    if let Ok(output) = output {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("(Default)") {
                let value = rest.trim().trim_start_matches("REG_SZ").trim();
                if !value.is_empty() && !voices.contains(&value.to_string()) {
                    voices.push(value.to_string());
                }
            }
        }
    }
    voices
}

/// One utterance for the apartment thread.
///
/// SAPI is the only engine that both speaks locally and renders to the mix, so
/// unlike the network engines it needs `to_stream` as a separate flag: local
/// playback happens either way.
#[derive(Debug, Clone)]
pub struct SapiRequest {
    pub synth: SynthRequest,
    /// Name of the TTS source in the audio engine to feed, when streaming.
    pub source_name: String,
    /// Also synthesize into the outgoing stream mix.
    pub to_stream: bool,
}

/// Starts the apartment thread and returns the queue that feeds it.
pub fn start(feeds: ExternalFeeds) -> Queue<SapiRequest> {
    let queue = Queue::new(super::speaker::QUEUE_DEPTH);
    let worker = queue.clone();
    let spawned = std::thread::Builder::new()
        .name("sapi-speech".into())
        .spawn(move || speech_thread(worker, feeds));
    if let Err(error) = spawned {
        log::error!("Could not start the SAPI speech thread: {error}");
    }
    queue
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn speech_thread(queue: Queue<SapiRequest>, feeds: ExternalFeeds) {
    unsafe {
        // SAPI wants an apartment; errors here leave TTS silently disabled.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let voice: ISpVoice = match CoCreateInstance(&SpVoice, None, CLSCTX_ALL) {
            Ok(v) => v,
            Err(e) => {
                log::error!("SAPI unavailable: {e}");
                return;
            }
        };

        let mut current_voice = String::new();
        // The resolved token is kept, not just the name it was resolved from.
        // `find_voice_token` enumerates every installed voice and compares
        // description strings, and `synth_to_pcm` needs the token for every
        // message — so returning `None` here once the voice had settled (i.e.
        // for every message after the first) sent it back to a full
        // enumeration per chat message, which is exactly what this was written
        // to avoid.
        let mut current_token: Option<ISpObjectToken> = None;
        while let Some(request) = queue.pop() {
            let synth = &request.synth;
            if synth.voice != current_voice {
                current_token = find_voice_token(&synth.voice);
                match &current_token {
                    Some(t) => {
                        if let Err(e) = voice.SetVoice(t) {
                            log::warn!("Could not select voice {:?}: {e}", synth.voice);
                        }
                    }
                    None if !synth.voice.is_empty() => {
                        log::warn!("Voice {:?} not found; keeping current voice", synth.voice);
                    }
                    None => {}
                }
                current_voice = synth.voice.clone();
            }
            let token = current_token.clone();
            let _ = voice.SetRate(synth.rate.clamp(-10, 10));
            let _ = voice.SetVolume(synth.volume.clamp(0, 100) as u16);

            // Local playback (async so queued messages stay responsive).
            let text = wide(&synth.text);
            if let Err(e) = voice.Speak(PCWSTR(text.as_ptr()), SPF_ASYNC.0 as u32, None) {
                log::error!("SAPI speak failed: {e}");
            }

            // Stream synthesis: render the same text to PCM and feed the
            // engine while the local speech plays.
            if request.to_stream {
                match synth_to_pcm(synth, token.as_ref()) {
                    Ok(samples) => feeds.feed_all(&request.source_name, &samples, "TTS"),
                    Err(e) => log::error!("TTS stream synthesis failed: {e}"),
                }
            }
        }
    }
}

/// Renders text to 48 kHz stereo f32 samples using a dedicated voice bound
/// to a memory stream.
unsafe fn synth_to_pcm(
    request: &SynthRequest,
    token: Option<&ISpObjectToken>,
) -> windows::core::Result<Vec<f32>> {
    unsafe {
        let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)?;
        if let Some(token) = token {
            let _ = voice.SetVoice(token);
        } else if !request.voice.is_empty() {
            if let Some(token) = find_voice_token(&request.voice) {
                let _ = voice.SetVoice(&token);
            }
        }
        let _ = voice.SetRate(request.rate.clamp(-10, 10));
        let _ = voice.SetVolume(request.volume.clamp(0, 100) as u16);

        let base: IStream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let stream: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL)?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS as u32 * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        stream.SetBaseStream(&base, &SPDFID_WAVEFORMATEX, &format)?;
        voice.SetOutput(&stream, false)?;

        let text = wide(&request.text);
        // Synchronous: returns when synthesis is complete.
        voice.Speak(PCWSTR(text.as_ptr()), 0, None)?;
        stream.Close()?;

        // Size the buffer up front. Growing from zero meant ~20 reallocations
        // and hundreds of kilobytes of memcpy for a few seconds of speech.
        let mut stat = STATSTG::default();
        let size = match base.Stat(&mut stat, STATFLAG_NONAME) {
            Ok(()) => stat.cbSize as usize,
            Err(_) => 0,
        };
        base.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = Vec::with_capacity(size.min(16 * 1024 * 1024));
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            let result = base.Read(
                chunk.as_mut_ptr() as *mut _,
                chunk.len() as u32,
                Some(&mut read),
            );
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read as usize]);
            if result.is_err() {
                break;
            }
        }

        Ok(bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audible smoke test; run with `cargo test sapi_speaks -- --ignored`.
    #[test]
    #[ignore]
    fn sapi_speaks() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let voice: ISpVoice =
                CoCreateInstance(&SpVoice, None, CLSCTX_ALL).expect("SpVoice creation");
            voice.SetRate(0).expect("set rate");
            voice.SetVolume(100).expect("set volume");
            let text = wide("Pubsplash text to speech is working.");
            // Synchronous so the test waits for playback to finish.
            voice.Speak(PCWSTR(text.as_ptr()), 0, None).expect("speak");
        }
    }

    /// Verifies memory synthesis yields plausible 48 kHz audio.
    #[test]
    #[ignore]
    fn sapi_synthesizes_pcm() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let request = SynthRequest {
                text: "Testing stream synthesis.".into(),
                voice: String::new(),
                rate: 0,
                volume: 100,
                pitch: 0,
                provider_settings: None,
            };
            let samples = synth_to_pcm(&request, None).expect("synthesis");
            // A couple of words should be at least half a second of audio.
            assert!(
                samples.len() > (SAMPLE_RATE as usize * CHANNELS) / 2,
                "unexpectedly short synthesis: {} samples",
                samples.len()
            );
            let peak = samples.iter().fold(0f32, |a, &s| a.max(s.abs()));
            assert!(peak > 0.01, "synthesized audio is silent (peak {peak})");
        }
    }

    #[test]
    fn voice_enumeration_finds_installed_voices() {
        let voices = voice_names();
        assert!(
            !voices.is_empty(),
            "expected at least one installed SAPI voice"
        );
    }
}

/// Finds a voice token whose description matches `name` (case-insensitive).
/// Returns `None` for the empty string (meaning: keep the default voice).
unsafe fn find_voice_token(name: &str) -> Option<ISpObjectToken> {
    if name.is_empty() {
        return None;
    }
    unsafe {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL).ok()?;
        let id = wide(SPCAT_VOICES);
        category.SetId(PCWSTR(id.as_ptr()), false).ok()?;
        let tokens = category.EnumTokens(PCWSTR::null(), PCWSTR::null()).ok()?;
        loop {
            let mut token: Option<ISpObjectToken> = None;
            if tokens.Next(1, &mut token, None).is_err() {
                return None;
            }
            let token = token?;
            // The token's default string value is the voice description.
            if let Ok(description) = token.GetStringValue(PCWSTR::null()) {
                let text = description.to_string().unwrap_or_default();
                CoTaskMemFree(Some(description.as_ptr() as *const _));
                if text.eq_ignore_ascii_case(name) {
                    return Some(token);
                }
            }
        }
    }
}
