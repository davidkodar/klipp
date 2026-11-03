//! Is a sound-only file speech or music, with no model to ask: a small check
//! of how its level moves, for the role its clips start with. Speech stops
//! and starts a few times a second and leaves gaps; music holds and repeats. The
//! sound tagger ([`crate::audio_role`]), when it is downloaded, hears more and
//! is taken over this, and a role set by hand is taken over both.

use serde::{Deserialize, Serialize};

pub const ID: &str = "speech_check";
pub const VERSION: u32 = 1;

/// The rate it is decoded at: speech's band is well inside it.
const RATE: u32 = 8_000;
/// The most of a file read, from its start.
const MAX_SECS: f64 = 600.0;
/// A level is taken every this many samples, 20 ms.
const HOP: usize = 160;
/// Quieter than this at its loudest (dBFS), it has nothing to say.
const QUIET_DB: f32 = -50.0;
/// A gap: this far under the loud part's level.
const GAP_DB: f32 = 20.0;
/// Speech: this much of the file is gaps, which music and a noisy place
/// don't have.
const SPEECH_GAPS: f32 = 0.15;
/// Speech under a bed that fills its gaps: this much of the level's movement
/// is at syllables' rate, with no strong beat. Measured on few files (see the
/// tests): a voice over a song quieter than it passes, and a louder song
/// doesn't, so that is left to the role it starts with.
const SPEECH_SYLLABLES: f32 = 0.6;
const BEAT: f32 = 0.4;

/// What the check measured, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpeechCheck {
    /// How much of the file is gaps.
    pub gaps: f32,
    /// How much of the level's movement is at the rate of syllables.
    pub syllables: f32,
    /// How regularly the level repeats, as a beat does.
    pub beat: f32,
}

impl SpeechCheck {
    /// The role it proposes, when it has something to say: this is a guess
    /// from how the level moves, so it says how sure and a role set by hand
    /// stays. With neither, the clip keeps the role it starts with.
    pub fn proposal(&self) -> Option<klipp_core::RoleProposal> {
        let (role, confidence) = if self.gaps >= SPEECH_GAPS {
            (klipp_core::ClipRole::Dialogue, 0.8)
        } else if self.syllables >= SPEECH_SYLLABLES && self.beat < BEAT {
            (klipp_core::ClipRole::Dialogue, 0.6)
        } else if self.beat >= BEAT {
            (klipp_core::ClipRole::Music, 0.6)
        } else {
            return None;
        };
        Some(klipp_core::RoleProposal { role, confidence, confident: true })
    }
}

/// `pcm` at [`RATE`], checked; `None` for one too short or too quiet to say.
pub fn check(pcm: &[f32]) -> Option<SpeechCheck> {
    let rate = RATE as f32;
    // The band a voice carries what it says in: a kick and a bass are not
    // the movement of speech. A first-order high-pass near 300 Hz.
    let k = 1.0 / (1.0 + std::f32::consts::TAU * 300.0 / rate);
    let (mut last_in, mut last_out) = (0.0f32, 0.0f32);
    let band: Vec<f32> = pcm.iter().map(|&x| {
        last_out = k * (last_out + x - last_in);
        last_in = x;
        last_out
    }).collect();
    let levels: Vec<f32> = band.chunks_exact(HOP).map(|f| 10.0 * (f.iter().map(|s| s * s).sum::<f32>() / HOP as f32).max(1e-12).log10()).collect();
    // Two seconds, to have a rate to read.
    if levels.len() < 100 {
        return None;
    }
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[sorted.len() * 95 / 100];
    if loud < QUIET_DB {
        return None;
    }
    let gaps = levels.iter().filter(|l| **l < loud - GAP_DB).count() as f32 / levels.len() as f32;
    let syllables = syllable_share(&levels, loud);
    let beat = beat_strength(&levels, loud);
    Some(SpeechCheck { gaps, syllables, beat })
}

/// How strongly the level repeats at a beat's distance, 0.3 to 1.5 s: 1 for
/// a level that repeats exactly. Rhythm repeats; talk doesn't.
fn beat_strength(levels: &[f32], loud: f32) -> f32 {
    let floor = loud - GAP_DB - 10.0;
    let held: Vec<f32> = levels.iter().take(3000).map(|l| l.max(floor)).collect();
    let mean = held.iter().sum::<f32>() / held.len() as f32;
    let moved: Vec<f32> = held.iter().map(|l| l - mean).collect();
    let power: f32 = moved.iter().map(|v| v * v).sum();
    if power <= 0.0 || moved.len() < 150 {
        return 0.0;
    }
    (15..=75usize.min(moved.len() / 2))
        .map(|lag| moved.iter().zip(&moved[lag..]).map(|(a, b)| a * b).sum::<f32>() / power * moved.len() as f32 / (moved.len() - lag) as f32)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

/// The share of the level's movement between 2 and 8 times a second, among
/// all from 0.5 to 20.
fn syllable_share(levels: &[f32], loud: f32) -> f32 {
    use realfft::RealFftPlanner;
    // A window of 40 s at a time, as much of them as the file has.
    const WINDOW: usize = 2048;
    let per_sec = RATE as f32 / HOP as f32;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let (mut syllables, mut all) = (0.0f32, 0.0f32);
    let mut buffer = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let pieces: Vec<&[f32]> = if levels.len() >= WINDOW { levels.chunks_exact(WINDOW).collect() } else { vec![levels] };
    for piece in pieces {
        let floor = loud - GAP_DB - 10.0;
        let held: Vec<f32> = piece.iter().map(|l| l.max(floor)).collect();
        let mean = held.iter().sum::<f32>() / held.len() as f32;
        buffer.iter_mut().for_each(|b| *b = 0.0);
        for (i, (b, l)) in buffer.iter_mut().zip(&held).enumerate() {
            let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / held.len() as f32).cos();
            *b = (l - mean) * hann;
        }
        if fft.process(&mut buffer, &mut spectrum).is_err() {
            continue;
        }
        for (i, bin) in spectrum.iter().enumerate() {
            let hz = i as f32 * per_sec / WINDOW as f32;
            let power = bin.norm_sqr();
            if (0.5..=20.0).contains(&hz) {
                all += power;
                if (2.0..=8.0).contains(&hz) {
                    syllables += power;
                }
            }
        }
    }
    if all > 0.0 { syllables / all } else { 0.0 }
}

pub struct SpeechCheckAnalyser;

impl crate::Analyser for SpeechCheckAnalyser {
    fn id(&self) -> &'static str {
        ID
    }

    fn schema_version(&self) -> u32 {
        VERSION
    }

    /// Sound-only files, which are the ones given a role by their sound.
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        item.sound_only()
    }

    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let secs = item.duration.to_secs_f64().clamp(1.0, MAX_SECS);
        let pcm = crate::audio_sync::decode_mono_pcm(&ctx.ffmpeg, &item.path, RATE, 0.0, secs)?;
        serde_json::to_value(check(&pcm)).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A noise of its own, so a test reads the same every run.
    fn noise(i: usize) -> f32 {
        let x = (i as f32 * 12.9898).sin() * 43758.547;
        2.0 * (x - x.floor()) - 1.0
    }

    /// A stand-in voice: syllables of uneven length, about 4 a second, of
    /// harmonics under a moving pitch, some ending in a hiss, with a short
    /// gap after each and a pause every so often.
    fn speech(secs: usize) -> Vec<f32> {
        let rate = RATE as f32;
        let mut out = Vec::with_capacity(secs * RATE as usize);
        let mut syllable = 0usize;
        while out.len() < secs * RATE as usize {
            let length = 0.10 + 0.20 * (noise(syllable * 31 + 5) + 1.0) / 2.0;
            let hissed = noise(syllable * 17 + 3) > 0.3;
            let pitch = 110.0 + 40.0 * (noise(syllable * 7 + 1) + 1.0) / 2.0;
            let start = out.len();
            for i in 0..(length * rate) as usize {
                let t = (start + i) as f32 / rate;
                let at = i as f32 / (length * rate);
                let voiced: f32 = (1..=12).map(|h| (std::f32::consts::TAU * pitch * h as f32 * t).sin() / h as f32).sum();
                out.push(if hissed && at > 0.7 { 0.08 * noise(start + i) } else { 0.15 * voiced * (std::f32::consts::PI * at).sin() });
            }
            let gap = if noise(syllable * 13 + 9) > 0.8 { 0.45 } else { 0.05 };
            out.extend(std::iter::repeat_n(0.0, (gap * rate) as usize));
            syllable += 1;
        }
        out.truncate(secs * RATE as usize);
        out
    }

    /// A stand-in song: held chords changing every 2 s over a soft kick
    /// twice a second.
    fn music(secs: usize) -> Vec<f32> {
        let rate = RATE as f32;
        (0..secs * RATE as usize).map(|i| {
            let t = i as f32 / rate;
            let root = [220.0, 261.6, 196.0, 293.7][((t / 2.0) as usize) % 4];
            let chord: f32 = [1.0, 1.26, 1.5, 2.0, 3.0].iter().map(|m| (std::f32::consts::TAU * root * m * t).sin()).sum();
            let beat = (t * 2.0).fract();
            0.08 * chord + 0.3 * (std::f32::consts::TAU * 70.0 * t).sin() * (-beat * 12.0).exp()
        }).collect()
    }

    fn role(pcm: &[f32]) -> Option<klipp_core::ClipRole> {
        check(pcm).and_then(|c| c.proposal()).map(|p| p.role)
    }

    #[test]
    fn clear_speech_is_dialogue_and_music_is_music() {
        assert_eq!(role(&speech(30)), Some(klipp_core::ClipRole::Dialogue), "{:?}", check(&speech(30)));
        assert_eq!(role(&music(30)), Some(klipp_core::ClipRole::Music), "{:?}", check(&music(30)));
    }

    /// A voice over a soft sustained bed, with no beat, is still someone
    /// talking, its gaps filled in.
    #[test]
    fn speech_over_a_bed_is_dialogue() {
        let rate = RATE as f32;
        let mixed: Vec<f32> = speech(30).iter().enumerate().map(|(i, s)| {
            let t = i as f32 / rate;
            s + 0.015 * [440.0, 660.0, 880.0].iter().map(|f| (std::f32::consts::TAU * f * t).sin()).sum::<f32>()
        }).collect();
        assert_eq!(role(&mixed), Some(klipp_core::ClipRole::Dialogue), "{:?}", check(&mixed));
    }

    /// Silence and a moment of sound say nothing.
    #[test]
    fn too_quiet_or_too_short_says_nothing() {
        assert_eq!(check(&vec![0.0; RATE as usize * 10]), None);
        assert_eq!(check(&vec![1e-4; RATE as usize * 10]), None);
        assert_eq!(check(&speech(1)), None);
    }

    /// Water or a crowd is neither: it doesn't guess, and the clip keeps the
    /// role it starts with.
    #[test]
    fn a_steady_noise_proposes_nothing() {
        let unsure = SpeechCheck { gaps: 0.0, syllables: 0.33, beat: 0.1 };
        assert_eq!(unsure.proposal(), None);
    }

    /// The check speaks only with no tagger's answer, and a tagger that heard
    /// nothing leaves it to the check.
    #[cfg(feature = "audio_role")]
    #[test]
    fn the_tagger_is_taken_over_the_check() {
        use crate::audio_role::{AudioRoles, Stretch};
        use klipp_core::ClipRole::{Dialogue, Music};
        let role = |report: &crate::report::MediaAnalysis| report.role_proposal().map(|p| p.role);
        let mut report = crate::report::MediaAnalysis::default();
        assert_eq!(role(&report), None);
        report.speech_check = Some(SpeechCheck { gaps: 0.4, syllables: 0.5, beat: 0.1 });
        assert_eq!(role(&report), Some(Dialogue));
        report.audio_role = Some(AudioRoles::default());
        assert_eq!(role(&report), Some(Dialogue));
        report.audio_role = Some(AudioRoles { stretches: vec![Stretch { start_secs: 0.0, end_secs: 10.0, scores: [0.05, 0.9, 0.01, 0.01] }] });
        assert_eq!(role(&report), Some(Music));
    }

    /// The rule against the values measured on real files: spoken lines (two
    /// voices), a voice over a quieter song, two songs at three places, a
    /// held tone and flowing water. A voice over a louder song reads as the
    /// song, which is the role it starts with.
    #[test]
    fn measured_files_get_their_roles() {
        use klipp_core::ClipRole::{Dialogue, Music};
        let role = |gaps, syllables, beat| SpeechCheck { gaps, syllables, beat }.proposal().map(|p| p.role);
        for (name, g, s, b, want) in [
            ("spoken", 0.432, 0.524, 0.132, Some(Dialogue)),
            ("spoken, long", 0.339, 0.584, 0.022, Some(Dialogue)),
            ("spoken, another voice", 0.391, 0.816, 0.201, Some(Dialogue)),
            ("over a quiet song", 0.046, 0.668, 0.263, Some(Dialogue)),
            ("over another quiet song", 0.026, 0.644, 0.047, Some(Dialogue)),
            ("over a loud song", 0.034, 0.654, 0.468, Some(Music)),
            ("a song", 0.065, 0.663, 0.484, Some(Music)),
            ("a song, later", 0.061, 0.544, 0.490, Some(Music)),
            ("a song, later still", 0.0, 0.367, 0.462, Some(Music)),
            ("another song", 0.055, 0.361, 0.466, Some(Music)),
            ("another song, later", 0.068, 0.392, 0.658, Some(Music)),
            ("another song, later still", 0.0, 0.481, 0.575, Some(Music)),
            ("a held tone", 0.0, 0.0, 0.993, Some(Music)),
            ("water", 0.003, 0.349, 0.044, None),
            ("a spillway", 0.0, 0.321, 0.120, None),
        ] {
            assert_eq!(role(g, s, b), want, "{name}");
        }
    }
}
