//! SAPI 5 engine: voice enumeration and synthesis into the mixer.
//!
//! Speech runs on a dedicated COM (STA) thread owning the SAPI voices;
//! requests arrive over a channel so callers never block. Every request is
//! synthesized to memory at the mixer format (48 kHz stereo 16-bit) and
//! trickle-fed into the named source's ring in the audio engine.
//!
//! SAPI used to also speak each message through an `ISpVoice` of its own, out
//! of the default playback device and around the mixer entirely, because it was
//! the only engine that *could*. That made it the only engine the broadcaster
//! could hear without monitoring its strip, and it meant the source's fader and
//! mute governed what listeners heard but not what the broadcaster did. All nine
//! engines now take the same route: the strip is heard locally on its own (see
//! `SourceSpec::local` in [`crate::audio`]), so a second voice here would simply
//! be the same words twice.

use super::engine::SynthRequest;
use super::queue::Queue;
use crate::audio::ExternalFeeds;
use crate::audio::mixer::{CHANNELS, SAMPLE_RATE};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::{
    ISpObjectToken, ISpObjectTokenCategory, ISpStream, ISpVoice, SpObjectTokenCategory, SpStream,
    SpVoice,
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
#[derive(Debug, Clone)]
pub struct SapiRequest {
    pub synth: SynthRequest,
    /// Name of the TTS source in the audio engine to feed.
    pub source_name: String,
}

/// Starts the apartment thread and returns the queue that feeds it.
pub(super) fn start(
    feeds: ExternalFeeds,
    problems: super::speaker::Problems,
) -> Queue<SapiRequest> {
    let queue = Queue::new(super::speaker::QUEUE_DEPTH);
    let worker = queue.clone();
    let spawned = std::thread::Builder::new()
        .name("sapi-speech".into())
        .spawn(move || speech_thread(worker, feeds, problems));
    if let Err(error) = spawned {
        log::error!("Could not start the SAPI speech thread: {error}");
    }
    queue
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn speech_thread(
    queue: Queue<SapiRequest>,
    feeds: ExternalFeeds,
    problems: super::speaker::Problems,
) {
    unsafe {
        // SAPI wants an apartment; errors here leave TTS silently disabled.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let mut current_voice = String::new();
        // The resolved token is kept, not just the name it was resolved from.
        // `find_voice_token` enumerates every installed voice and compares
        // description strings, and `synth_to_pcm` needs the token for every
        // message — so returning `None` here once the voice had settled (i.e.
        // for every message after the first) sent it back to a full
        // enumeration per chat message, which is exactly what this was written
        // to avoid.
        let mut current_token: Option<ISpObjectToken> = None;
        // Whether the *name* we were asked for came back with nothing, as
        // opposed to no name having been asked for. The miss is remembered for
        // the same reason the hit is — see below.
        let mut voice_missing = false;
        while let Some(request) = queue.pop() {
            let synth = &request.synth;
            if synth.voice != current_voice {
                current_token = find_voice_token(&synth.voice);
                voice_missing = current_token.is_none() && !synth.voice.is_empty();
                if voice_missing {
                    log::warn!("Voice {:?} not found; using the default", synth.voice);
                }
                current_voice = synth.voice.clone();
            }

            // `synth_to_pcm` applies the token, rate and volume to the voice it
            // renders with, so there is nothing to set up out here. It falls
            // back to resolving the name itself when handed no token, which is
            // what the preview wants and what a chat flood must not have: the
            // name is blanked on a miss so the enumeration above is not repeated
            // once a message for a voice already known not to be installed.
            let rendered = if voice_missing {
                let mut default_voice = synth.clone();
                default_voice.voice.clear();
                synth_to_pcm(&default_voice, None)
            } else {
                synth_to_pcm(synth, current_token.as_ref())
            };
            // The API tab's tally. SAPI is local and free, so there is nothing
            // to bill and no model to name — but the request and character
            // counts are still what tell a user which source is doing the
            // talking. A voice that was asked for and missing is recorded as
            // none, because the default is what actually spoke.
            super::usage::record(
                super::engines::SAPI,
                synth.text.chars().count(),
                None,
                (!voice_missing && !synth.voice.is_empty()).then(|| synth.voice.clone()),
                rendered.is_err(),
            );
            match rendered {
                // `feed_all` sleeps while the ring drains, which is exactly why
                // this is its own thread.
                Ok(samples) => feeds.feed_all(&request.source_name, &samples, "TTS"),
                Err(e) => {
                    // SAPI has no other way out now, so a failure here is
                    // silence rather than a degraded stream — worth telling the
                    // user about, at the same one-a-minute rate as the network
                    // engines.
                    log::error!("SAPI synthesis failed: {e}");
                    problems.report(
                        super::engines::SAPI,
                        &super::engine::TtsError::Other(e.message()),
                    );
                }
            }
        }
    }
}

/// Synthesizes one phrase on the calling thread, for the voice preview.
///
/// The apartment thread's queue feeds a mixer source, which a preview has no
/// business touching — the source may not exist yet, and previewing must never
/// reach the stream. So this initializes an apartment of its own and hands the
/// samples back for [`crate::audio::cue`] to play, exactly as the network
/// engines' preview does.
pub fn synth_preview(request: &SynthRequest) -> Result<Vec<f32>, super::engine::TtsError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        synth_to_pcm(request, None).map_err(|e| super::engine::TtsError::Other(e.message()))
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

    /// Verifies memory synthesis yields plausible 48 kHz audio.
    ///
    /// This is now the whole of SAPI's output path — there is no local voice
    /// left to smoke-test separately.
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
