//! Pure mixing DSP: gain strips with click-free fades, and block mixing.
//!
//! All audio is interleaved stereo f32 at [`SAMPLE_RATE`] Hz.

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Frames per mix block (10 ms at 48 kHz).
pub const BLOCK_FRAMES: usize = 480;
/// Samples (not frames) per mix block.
pub const BLOCK_SAMPLES: usize = BLOCK_FRAMES * CHANNELS;

/// Duration of the mute/unmute fade, in seconds.
pub const FADE_SECONDS: f32 = 0.05;

/// Pulls up to `dest.len()` samples out of `ring`, zero-filling whatever the
/// ring could not supply. Returns how many real samples were copied, so the
/// caller can tell a full block from a padded one — a source that is padded
/// block after block is one whose audio is not keeping up, and the padding is
/// what makes that inaudible until it is too late to catch.
///
/// One bulk read rather than a `pop()` per sample. Popping individually costs
/// an atomic index store per sample — 96,000 a second per source per
/// direction — so a six-source scene was spending over a million ring
/// operations a second on plumbing, which is headroom the FX chains compete
/// for on the same thread.
pub fn pull_block(ring: &mut rtrb::Consumer<f32>, dest: &mut [f32]) -> usize {
    let take = ring.slots().min(dest.len());
    let mut filled = 0;
    if take > 0
        && let Ok(chunk) = ring.read_chunk(take)
    {
        // The requested range can straddle the end of the buffer.
        let (first, second) = chunk.as_slices();
        dest[..first.len()].copy_from_slice(first);
        dest[first.len()..first.len() + second.len()].copy_from_slice(second);
        filled = first.len() + second.len();
        // Every sample handed out was copied, so all of it is consumed.
        chunk.commit_all();
    }
    dest[filled..].fill(0.0);
    filled
}

/// Absolute ceiling for a strip's volume. 100 is unity gain; anything above it
/// is make-up gain for a source that is simply too quiet at the OS level, and
/// is only reachable when the strip's "volume boost" is enabled in the UI (the
/// UI holds un-boosted strips at 100). The strip itself only enforces the
/// absolute ceiling.
pub const MAX_VOLUME: u32 = 500;

/// A volume/mute stage for one source (or the master bus). Gain changes are
/// ramped over [`FADE_SECONDS`] so mutes fade out and unmutes fade in.
#[derive(Debug, Clone)]
pub struct ChannelStrip {
    /// Slider volume 0-[`MAX_VOLUME`] (100 = unity), remembered across mute.
    volume: u32,
    muted: bool,
    /// The gain currently being applied (moves toward `target_gain`).
    current_gain: f32,
}

impl ChannelStrip {
    pub fn new(volume: u32, muted: bool) -> Self {
        let mut strip = Self {
            volume,
            muted,
            current_gain: 0.0,
        };
        strip.current_gain = strip.target_gain();
        strip
    }

    fn target_gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            (self.volume.min(MAX_VOLUME) as f32) / 100.0
        }
    }

    pub fn set_volume(&mut self, volume: u32) {
        self.volume = volume.min(MAX_VOLUME);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn volume(&self) -> u32 {
        self.volume
    }

    /// Mute keeps the volume so unmute restores the previous level.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    #[allow(dead_code)]
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Applies the strip's gain to `block` in place, ramping toward the
    /// target gain across the block.
    pub fn process(&mut self, block: &mut [f32]) {
        let target = self.target_gain();
        if (self.current_gain - target).abs() < f32::EPSILON {
            if target == 0.0 {
                block.fill(0.0);
            } else if (target - 1.0).abs() > f32::EPSILON {
                for s in block.iter_mut() {
                    *s *= target;
                }
            }
            return;
        }
        // Per-frame step so the fade takes FADE_SECONDS regardless of block size.
        let step = 1.0 / (FADE_SECONDS * SAMPLE_RATE as f32);
        let mut gain = self.current_gain;
        for frame in block.chunks_mut(CHANNELS) {
            if gain < target {
                gain = (gain + step).min(target);
            } else {
                gain = (gain - step).max(target);
            }
            for s in frame {
                *s *= gain;
            }
        }
        self.current_gain = gain;
    }
}

/// How loud another source has to be, post-fader, before it counts as somebody
/// talking, out of the box. See [`trigger_level`] for why the units matter more
/// than the number.
///
/// It has to sit in the gap between the two things it must tell apart, and that
/// gap is narrower in a real room than the numbers suggest. A headset
/// microphone idling in a quiet room measures about -55 dBFS RMS and speech at
/// an ordinary gain measures -25 to -15 — but a real microphone is not idling in
/// a quiet room. Breath across the capsule, a fan, a keyboard and a chair all
/// land tens of dB above that floor, and every one of them re-arms the full
/// [`DUCK_HOLD_SECONDS`], so a threshold set close to the floor gives music that
/// is turned down permanently for reasons the user cannot hear. This default is
/// deliberately well clear of that: it is the level of *deliberate speech*, and
/// somebody whose voice does not reach it lowers it themselves — with the slider
/// beside the ducking checkbox, or by pressing Calibrate and talking for five
/// seconds (see [`calibrated_threshold_db`]).
pub const DUCK_THRESHOLD_DB_DEFAULT: i32 = -30;

/// The range the threshold slider offers, in dBFS RMS. The bottom is under any
/// usable noise floor and the top is above ordinary speech, so both ends are
/// past the point of being useful — which is what a range should be.
pub const DUCK_THRESHOLD_DB_MIN: i32 = -60;
pub const DUCK_THRESHOLD_DB_MAX: i32 = -10;

/// How far below the loudest thing calibration heard the threshold is placed.
///
/// Calibration measures the *peak* block RMS over its window, which is the
/// loudest syllable of the loudest word. Speech within one sentence swings well
/// over 10 dB below that, so the leeway has to be generous enough that ordinary
/// words still cross the line — and [`DUCK_HOLD_SECONDS`] covers the rest.
pub const DUCK_CALIBRATION_LEEWAY_DB: f32 = 12.0;

/// The quietest peak calibration will accept as somebody having spoken. Below
/// this, whatever it heard is more likely to be a room than a voice, and
/// calibrating to it would produce a threshold that ducks on nothing at all.
pub const DUCK_CALIBRATION_MIN_SPEECH_DB: f32 = -45.0;

/// How long the music takes to get out of the way. Fast enough that the first
/// syllable is not lost under it, slow enough not to sound like a gate.
pub const DUCK_ATTACK_SECONDS: f32 = 0.15;

/// How long it waits, after the last block above the threshold, before starting
/// to come back. This is what keeps the gaps between words — and between two
/// sentences of a chat message — from pumping the music up and down.
pub const DUCK_HOLD_SECONDS: f32 = 1.0;

/// How long the music takes to come back afterwards.
pub const DUCK_RELEASE_SECONDS: f32 = 0.8;

/// An amplitude (an RMS, here) as dBFS. Silence is [`f32::NEG_INFINITY`] rather
/// than a very large negative number, so callers have to think about it.
pub fn amplitude_to_db(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return f32::NEG_INFINITY;
    }
    20.0 * amplitude.log10()
}

/// dBFS back to a linear amplitude, which is what [`Ducker`] compares against.
pub fn db_to_amplitude(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The threshold to set from a calibration run, given the loudest block RMS it
/// measured while the user was talking.
///
/// `None` means the run heard nothing worth calibrating to — a muted or missing
/// microphone, or one so quiet that the ducker could never work from it — and
/// the caller must say so rather than writing a threshold nobody can cross.
pub fn calibrated_threshold_db(peak_rms: f32) -> Option<i32> {
    let peak_db = amplitude_to_db(peak_rms);
    if !peak_db.is_finite() || peak_db < DUCK_CALIBRATION_MIN_SPEECH_DB {
        return None;
    }
    Some(
        (peak_db - DUCK_CALIBRATION_LEEWAY_DB)
            .round()
            .clamp(DUCK_THRESHOLD_DB_MIN as f32, DUCK_THRESHOLD_DB_MAX as f32) as i32,
    )
}

/// How loud a block is, as the RMS of its samples: what [`Ducker`] compares
/// against its threshold.
///
/// **RMS, not peak, and that is the whole of it.** A peak reading answers the
/// loudest single sample in ten milliseconds, and noise is spiky: a microphone's
/// idle noise floor has a crest factor around 10 dB, so a room 20 dB quieter
/// than speech still touches a peak threshold set below speech, in *every*
/// block. Each of those blocks re-arms the full [`DUCK_HOLD_SECONDS`], so the
/// music goes down when the source is opened and never comes back — ducking
/// that looks, from the outside, like it is ignoring how loud anything is.
/// An RMS is the block's actual level, and a noise floor measures as one.
pub fn trigger_level(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let sum: f32 = block.iter().map(|s| s * s).sum();
    (sum / block.len() as f32).sqrt()
}

/// Turns a media player down while anything else in the scene has signal.
///
/// One of these per ducking source, driven once per block from the
/// [`trigger_level`] of everything else that is playing. The gain it produces is applied *after* the
/// source's own strip, so it scales whatever the fader is set to rather than
/// replacing it, and it is applied before every tap — master, sends and the
/// local monitor alike — so what the broadcaster hears is what the listeners
/// hear.
///
/// Attack, hold and release are all here rather than being a plain gate because
/// speech is not continuous: the gaps between words fall below any threshold
/// worth using, and a ducker without a hold answers them by pumping the music
/// back up between syllables.
#[derive(Debug, Clone)]
pub struct Ducker {
    /// Gain applied while ducked, 0-1. 1 is a ducker that does nothing.
    ducked_gain: f32,
    /// The [`trigger_level`] at or above which something counts as talking,
    /// as a linear amplitude — the user's dB setting resolved once, here,
    /// rather than a `powf` per block.
    threshold: f32,
    /// The gain being applied right now, moving toward its target.
    current_gain: f32,
    /// Blocks left to hold the duck after the last one above the threshold.
    hold_blocks: u32,
}

impl Ducker {
    /// `percent` is the level to drop to, as a percentage of the source's own
    /// fader. Held to 0-100: this attenuates, it never boosts. `threshold_db`
    /// is how loud everything else has to be before it does — see
    /// [`DUCK_THRESHOLD_DB_DEFAULT`].
    pub fn new(percent: u32, threshold_db: i32) -> Self {
        Self {
            ducked_gain: percent.min(100) as f32 / 100.0,
            threshold: db_to_amplitude(clamp_threshold_db(threshold_db) as f32),
            current_gain: 1.0,
            hold_blocks: 0,
        }
    }

    /// Whether this ducker would behave exactly as one built from `percent` and
    /// `threshold_db`. Used to carry a duck in progress across a routing update
    /// rather than snapping the music back to full for a block.
    pub fn matches(&self, percent: u32, threshold_db: i32) -> bool {
        let other = Ducker::new(percent, threshold_db);
        (self.ducked_gain - other.ducked_gain).abs() < f32::EPSILON
            && (self.threshold - other.threshold).abs() < f32::EPSILON
    }

    /// Takes this ducker's state over from the one it replaces, so a scene edit
    /// mid-duck does not jump the music.
    pub fn adopt(&mut self, previous: &Ducker) {
        self.current_gain = previous.current_gain;
        self.hold_blocks = previous.hold_blocks;
    }

    /// Applies the duck to one block, given the loudest thing anything else in
    /// the scene is doing.
    pub fn process(&mut self, block: &mut [f32], trigger: f32) {
        let target = if trigger >= self.threshold {
            self.hold_blocks = hold_blocks();
            self.ducked_gain
        } else if self.hold_blocks > 0 {
            self.hold_blocks -= 1;
            self.ducked_gain
        } else {
            1.0
        };
        let seconds = if target < self.current_gain {
            DUCK_ATTACK_SECONDS
        } else {
            DUCK_RELEASE_SECONDS
        };
        let step = 1.0 / (seconds * SAMPLE_RATE as f32);
        let mut gain = self.current_gain;
        for frame in block.chunks_mut(CHANNELS) {
            if gain < target {
                gain = (gain + step).min(target);
            } else {
                gain = (gain - step).max(target);
            }
            if gain != 1.0 {
                for s in frame {
                    *s *= gain;
                }
            }
        }
        self.current_gain = gain;
    }

    /// The gain being applied right now, for tests.
    #[cfg(test)]
    pub fn gain(&self) -> f32 {
        self.current_gain
    }
}

fn hold_blocks() -> u32 {
    (DUCK_HOLD_SECONDS * SAMPLE_RATE as f32 / BLOCK_FRAMES as f32) as u32
}

/// Holds a threshold to the range the slider offers. Config is a file users can
/// edit, and a threshold of 0 dB is a ducker that never fires.
pub fn clamp_threshold_db(db: i32) -> i32 {
    db.clamp(DUCK_THRESHOLD_DB_MIN, DUCK_THRESHOLD_DB_MAX)
}

/// Adds `src` into `dst` (same length), saturating is not needed for f32;
/// the master stage clamps before conversion to integer PCM.
pub fn mix_into(dst: &mut [f32], src: &[f32]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += s;
    }
}

/// Converts a mixed f32 block to interleaved i16 with clamping, for the
/// MP3 encoder.
pub fn to_i16(block: &[f32], out: &mut Vec<i16>) {
    out.clear();
    out.extend(
        block
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partly_filled_ring_is_read_out_and_the_rest_zeroed() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(64);
        for i in 0..10 {
            producer.push(i as f32).unwrap();
        }
        let mut dest = vec![9.0f32; 16];
        assert_eq!(pull_block(&mut consumer, &mut dest), 10, "ten real samples");
        assert_eq!(&dest[..10], &[0., 1., 2., 3., 4., 5., 6., 7., 8., 9.]);
        assert!(dest[10..].iter().all(|&s| s == 0.0), "tail is silence");
        assert_eq!(consumer.slots(), 0, "exactly what was read is consumed");
    }

    #[test]
    fn a_read_that_wraps_the_ring_is_still_contiguous_in_the_block() {
        // Push past the halfway point, drain it, then push again so the next
        // read straddles the end of the backing buffer.
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        for i in 0..6 {
            producer.push(i as f32).unwrap();
        }
        let mut drain = vec![0f32; 6];
        pull_block(&mut consumer, &mut drain);
        for i in 6..12 {
            producer.push(i as f32).unwrap();
        }
        let mut dest = vec![9.0f32; 6];
        assert_eq!(pull_block(&mut consumer, &mut dest), 6, "a full block");
        assert_eq!(dest, vec![6., 7., 8., 9., 10., 11.]);
        assert_eq!(consumer.slots(), 0);
    }

    #[test]
    fn an_empty_ring_yields_silence() {
        let (_producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        let mut dest = vec![9.0f32; 4];
        assert_eq!(pull_block(&mut consumer, &mut dest), 0, "nothing was real");
        assert_eq!(dest, vec![0.0; 4]);
    }

    #[test]
    fn unity_gain_passes_through() {
        let mut strip = ChannelStrip::new(100, false);
        let mut block = vec![0.5f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(block.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn muted_strip_silences_after_fade() {
        let mut strip = ChannelStrip::new(100, false);
        strip.set_muted(true);
        let mut block = vec![0.5f32; BLOCK_SAMPLES];
        strip.process(&mut block); // 10ms of a 50ms fade: not yet silent
        assert!(block[0] > 0.4, "fade should start near previous gain");
        assert!(block[BLOCK_SAMPLES - 1] < 0.5, "fade should be descending");
        for _ in 0..6 {
            let mut b = vec![0.5f32; BLOCK_SAMPLES];
            strip.process(&mut b);
            block = b;
        }
        assert!(
            block.iter().all(|&s| s == 0.0),
            "silent after fade completes"
        );
    }

    #[test]
    fn unmute_restores_previous_volume() {
        let mut strip = ChannelStrip::new(60, false);
        strip.set_muted(true);
        for _ in 0..10 {
            strip.process(&mut vec![0.5f32; BLOCK_SAMPLES]);
        }
        strip.set_muted(false);
        assert_eq!(strip.volume(), 60);
        for _ in 0..10 {
            strip.process(&mut vec![0.5f32; BLOCK_SAMPLES]);
        }
        let mut block = vec![1.0f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.6).abs() < 1e-3),
            "gain should settle at volume 60 => 0.6"
        );
    }

    /// Runs enough blocks for the gain ramp to reach its target.
    fn settle(strip: &mut ChannelStrip) {
        for _ in 0..60 {
            strip.process(&mut vec![0.0f32; BLOCK_SAMPLES]);
        }
    }

    #[test]
    fn boosted_volume_amplifies() {
        let mut strip = ChannelStrip::new(200, false);
        settle(&mut strip);
        let mut block = vec![0.25f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.5).abs() < 1e-3),
            "volume 200 should double the signal"
        );
    }

    #[test]
    fn volume_is_capped_at_max() {
        let mut strip = ChannelStrip::new(100, false);
        strip.set_volume(9_999);
        assert_eq!(strip.volume(), MAX_VOLUME);
        settle(&mut strip);
        let mut block = vec![0.1f32; BLOCK_SAMPLES];
        strip.process(&mut block);
        assert!(
            block.iter().all(|&s| (s - 0.5).abs() < 1e-3),
            "gain should cap at MAX_VOLUME / 100 = 5.0"
        );
    }

    /// A ducker at the shipped threshold, which is what nearly every test here
    /// is about; the ones that are about the threshold itself spell it out.
    fn ducker(percent: u32) -> Ducker {
        Ducker::new(percent, DUCK_THRESHOLD_DB_DEFAULT)
    }

    /// Runs `blocks` blocks of a steady signal past a ducker and returns the
    /// last one.
    fn duck_blocks(ducker: &mut Ducker, blocks: usize, trigger: f32) -> Vec<f32> {
        let mut block = Vec::new();
        for _ in 0..blocks {
            block = vec![1.0f32; BLOCK_SAMPLES];
            ducker.process(&mut block, trigger);
        }
        block
    }

    #[test]
    fn a_ducker_with_nothing_talking_over_it_is_silent_about_it() {
        let mut ducker = ducker(25);
        let block = duck_blocks(&mut ducker, 10, 0.0);
        assert!(block.iter().all(|&s| s == 1.0), "untouched at full gain");
    }

    #[test]
    fn speech_pulls_the_music_down_to_the_configured_level() {
        let mut ducker = ducker(25);
        // Attack is 150 ms; 50 blocks is half a second.
        duck_blocks(&mut ducker, 50, 0.5);
        assert!(
            (ducker.gain() - 0.25).abs() < 1e-3,
            "settled at 25%: {}",
            ducker.gain()
        );
    }

    /// The attack is a ramp, not a step: the first block after speech starts is
    /// on its way down, not already there.
    #[test]
    fn the_duck_is_a_ramp_rather_than_a_jump() {
        let mut ducker = ducker(25);
        let block = duck_blocks(&mut ducker, 1, 0.5);
        assert!(block[0] > 0.9, "starts from where it was: {}", block[0]);
        assert!(
            block[BLOCK_SAMPLES - 1] < 1.0 && block[BLOCK_SAMPLES - 1] > 0.25,
            "descending, not arrived: {}",
            block[BLOCK_SAMPLES - 1]
        );
    }

    /// The property the hold exists for: a gap between two words must not let
    /// the music back up.
    #[test]
    fn a_gap_between_words_does_not_release_the_duck() {
        let mut ducker = ducker(25);
        duck_blocks(&mut ducker, 50, 0.5);
        // 300 ms of silence — longer than a syllable, shorter than the hold.
        duck_blocks(&mut ducker, 30, 0.0);
        assert!(
            (ducker.gain() - 0.25).abs() < 1e-3,
            "still ducked through the gap: {}",
            ducker.gain()
        );
    }

    #[test]
    fn the_music_comes_back_once_the_talking_really_stops() {
        let mut ducker = ducker(25);
        duck_blocks(&mut ducker, 50, 0.5);
        // The hold is a second, the release 800 ms; 300 blocks covers both.
        duck_blocks(&mut ducker, 300, 0.0);
        assert!(
            (ducker.gain() - 1.0).abs() < 1e-3,
            "back to full: {}",
            ducker.gain()
        );
    }

    /// A source below the threshold is a quiet room, not a person talking.
    #[test]
    fn a_signal_under_the_threshold_is_not_somebody_talking() {
        let mut ducker = ducker(25);
        let half = db_to_amplitude(DUCK_THRESHOLD_DB_DEFAULT as f32) / 2.0;
        duck_blocks(&mut ducker, 20, half);
        assert_eq!(ducker.gain(), 1.0);
    }

    #[test]
    fn a_duck_in_progress_survives_a_routing_update() {
        let mut ducker = ducker(25);
        duck_blocks(&mut ducker, 5, 0.5);
        let mid_duck = ducker.gain();
        assert!(mid_duck < 1.0 && mid_duck > 0.25);

        let mut replacement = Ducker::new(25, DUCK_THRESHOLD_DB_DEFAULT);
        assert!(replacement.matches(25, DUCK_THRESHOLD_DB_DEFAULT));
        replacement.adopt(&ducker);
        assert_eq!(replacement.gain(), mid_duck, "no jump back to full");
    }

    /// The other half of `matches`: a threshold edit must *not* be carried
    /// across, or the setting the user just changed would not take effect until
    /// the next scene switch.
    #[test]
    fn a_ducker_does_not_match_one_with_a_different_threshold() {
        let ducker = ducker(25);
        assert!(!ducker.matches(25, DUCK_THRESHOLD_DB_DEFAULT - 6));
        assert!(!ducker.matches(50, DUCK_THRESHOLD_DB_DEFAULT));
    }

    #[test]
    fn a_duck_level_above_100_cannot_amplify() {
        let mut ducker = ducker(400);
        let block = duck_blocks(&mut ducker, 50, 0.5);
        assert!(block.iter().all(|&s| s == 1.0), "held at unity");
    }

    #[test]
    fn the_trigger_level_is_the_blocks_rms() {
        // A square wave is the one signal whose RMS is its amplitude.
        assert!((trigger_level(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
        assert_eq!(trigger_level(&[]), 0.0);
        assert_eq!(trigger_level(&[0.0; 8]), 0.0);
    }

    /// A block of pseudo-random noise at a given RMS: the shape of a
    /// microphone's idle noise floor. Deterministic, so the test cannot flake,
    /// and built by summing four uniform draws so its crest factor is a
    /// realistic ~10 dB rather than the ~3 dB of a flat distribution — the
    /// crest factor is the entire point of the tests below.
    fn noise(rms: f32, seed: &mut u32) -> Vec<f32> {
        let scale = rms / (2.0 / 3f32.sqrt());
        (0..BLOCK_SAMPLES)
            .map(|_| {
                let mut sum = 0.0;
                for _ in 0..4 {
                    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    sum += (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                }
                sum * scale
            })
            .collect()
    }

    /// Why the trigger is an RMS. A noise floor 20 dB below speech has
    /// individual samples three times its own level, so a peak reading of it
    /// lands in the range a threshold set below speech has to occupy.
    #[test]
    fn a_noise_floors_peaks_are_not_its_level() {
        let mut seed = 12_345;
        let block = noise(0.002, &mut seed);
        let peak = block.iter().fold(0f32, |max, s| max.max(s.abs()));
        assert!(
            peak > 0.005,
            "a -54 dBFS room peaked at only {peak}; the test's noise is unrealistic"
        );
        assert!(
            trigger_level(&block) < db_to_amplitude(DUCK_THRESHOLD_DB_DEFAULT as f32),
            "but its level is well under the threshold"
        );
    }

    /// An open microphone in a quiet room is not somebody talking, and must
    /// never hold the music down. Two hundred blocks is two seconds — twice the
    /// hold — so a single triggered block anywhere in it would still show here.
    #[test]
    fn a_microphones_idle_noise_floor_does_not_duck_the_music() {
        let mut ducker = ducker(30);
        let mut seed = 12_345;
        for _ in 0..200 {
            let mut music = vec![1.0f32; BLOCK_SAMPLES];
            // -54 dBFS: a headset microphone in a quiet room.
            ducker.process(&mut music, trigger_level(&noise(0.002, &mut seed)));
        }
        assert_eq!(ducker.gain(), 1.0, "the noise floor ducked the music");
    }

    /// The other half of the same threshold: somebody actually talking over
    /// that same microphone still ducks it, all the way down.
    #[test]
    fn speech_over_the_same_microphone_still_ducks_it() {
        let mut ducker = ducker(30);
        let mut seed = 12_345;
        for _ in 0..100 {
            let mut music = vec![1.0f32; BLOCK_SAMPLES];
            // -20 dBFS: ordinary talking at an ordinary microphone gain.
            ducker.process(&mut music, trigger_level(&noise(0.1, &mut seed)));
        }
        assert!(
            (ducker.gain() - 0.30).abs() < 1e-3,
            "fully ducked, got {}",
            ducker.gain()
        );
    }

    /// What the default was moved for. A knock on the desk, a breath across the
    /// capsule or a fan spinning up lands around -38 dBFS — above the -40 the
    /// threshold used to be fixed at, and so a duck the user cannot account for.
    #[test]
    fn a_bump_on_the_microphone_no_longer_ducks_the_music() {
        let mut ducker = ducker(30);
        let mut seed = 999;
        for _ in 0..200 {
            let mut music = vec![1.0f32; BLOCK_SAMPLES];
            ducker.process(&mut music, trigger_level(&noise(0.0126, &mut seed)));
        }
        assert_eq!(ducker.gain(), 1.0, "a -38 dBFS bump ducked the music");
    }

    /// And what the slider is for: a quiet or distant voice that the default
    /// deliberately ignores ducks once the user lowers the threshold to it.
    #[test]
    fn a_quiet_voice_ducks_once_the_threshold_is_lowered_to_it() {
        let mut seed = 4_242;
        // -35 dBFS: quiet, distant speech.
        let quiet = |seed: &mut u32| trigger_level(&noise(0.018, seed));

        let mut default = ducker(30);
        for _ in 0..200 {
            let mut music = vec![1.0f32; BLOCK_SAMPLES];
            default.process(&mut music, quiet(&mut seed));
        }
        assert_eq!(default.gain(), 1.0, "left alone at the default");

        let mut lowered = Ducker::new(30, -40);
        for _ in 0..100 {
            let mut music = vec![1.0f32; BLOCK_SAMPLES];
            lowered.process(&mut music, quiet(&mut seed));
        }
        assert!(
            (lowered.gain() - 0.30).abs() < 1e-3,
            "fully ducked at -40 dB, got {}",
            lowered.gain()
        );
    }

    /// Calibration puts the threshold below what it heard, not at it: the peak
    /// block of a sentence is its loudest syllable, and the rest of the words
    /// have to keep the duck held.
    #[test]
    fn calibration_leaves_room_under_the_voice_it_measured() {
        // -20 dBFS peak: an ordinary speaking level.
        let threshold = calibrated_threshold_db(db_to_amplitude(-20.0)).unwrap();
        assert_eq!(threshold, -32);
        // A loud voice or a hot microphone gets a higher threshold, and is
        // still nowhere near the top of the range.
        assert_eq!(calibrated_threshold_db(db_to_amplitude(-8.0)), Some(-20));
    }

    /// A calibration run that heard nothing must say so rather than writing a
    /// threshold. Silence, a muted microphone and a room tone all land here.
    #[test]
    fn calibration_refuses_to_guess_from_silence() {
        assert_eq!(calibrated_threshold_db(0.0), None);
        assert_eq!(calibrated_threshold_db(db_to_amplitude(-54.0)), None);
        // Just above the edge of what counts as a voice, it still answers.
        assert_eq!(
            calibrated_threshold_db(db_to_amplitude(DUCK_CALIBRATION_MIN_SPEECH_DB + 1.0)),
            Some(-56)
        );
    }

    /// The slider's ends are the only thing a threshold can be, wherever it
    /// came from — a hand-edited config, or a shout into a hot microphone.
    #[test]
    fn a_threshold_is_held_to_the_range_the_slider_offers() {
        assert_eq!(clamp_threshold_db(0), DUCK_THRESHOLD_DB_MAX);
        assert_eq!(clamp_threshold_db(-200), DUCK_THRESHOLD_DB_MIN);
        // Full scale, and past it: a microphone hot enough to clip still
        // calibrates to a threshold inside the range.
        assert_eq!(calibrated_threshold_db(1.0), Some(-12));
        assert_eq!(
            calibrated_threshold_db(db_to_amplitude(6.0)),
            Some(DUCK_THRESHOLD_DB_MAX)
        );
        // A threshold of 0 dBFS would be a ducker that never fires; clamping is
        // what keeps `Ducker` from being handed one.
        let mut never = Ducker::new(30, 0);
        duck_blocks(&mut never, 50, db_to_amplitude(-10.0));
        assert!(never.gain() < 1.0, "clamped to the top of the range");
    }

    #[test]
    fn decibels_round_trip_through_amplitudes() {
        assert!((amplitude_to_db(1.0)).abs() < 1e-4);
        assert!((amplitude_to_db(db_to_amplitude(-30.0)) + 30.0).abs() < 1e-3);
        assert_eq!(amplitude_to_db(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn mix_and_convert() {
        let mut dst = vec![0.25f32; 4];
        mix_into(&mut dst, &[0.25, 0.25, 1.0, -2.0]);
        let mut out = Vec::new();
        to_i16(&dst, &mut out);
        assert_eq!(out[0], (0.5 * i16::MAX as f32) as i16);
        assert_eq!(out[2], i16::MAX); // clamped from 1.25
        assert_eq!(out[3], -i16::MAX); // clamped from -1.75
    }
}
