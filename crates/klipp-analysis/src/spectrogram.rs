//! A file's sound as a picture of frequency against time, and the
//! numbers behind it.
//!
//! Klipp's scrolling analyser shows what is playing. That is the right
//! instrument for setting a level and the wrong one for the question
//! people actually ask of a long recording: *where* did the hum start,
//! *when* did the radio mic begin to hiss, *which* two minutes have the
//! air conditioning in them. Those are answered by looking at the whole
//! file at once, and that is what a spectrogram is for.
//!
//! Two things come out of here. The picture, for a person, rendered by
//! the same code the live scope uses so the two agree. And the band
//! tracks with their findings, for an assistant, because a model
//! staring at a spectrogram image will say it looks a bit noisy and be
//! unable to say at what frequency or between which seconds — the same
//! reason the colour tools hand out numbers rather than asking anyone
//! to eyeball a frame.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The rate the file is decoded at. 32 kHz puts the Nyquist limit at
/// 16 kHz, which covers everything anyone reaches for a tool over:
/// rumble and mains hum at the bottom, sibilance and tape hiss at the
/// top. Decoding at 48 kHz would double the work to show an octave
/// almost nobody is grading.
pub const RATE: u32 = 32_000;

/// The window. At 32 kHz, 4096 samples is 128 ms — long enough to
/// resolve 50 Hz from 60 Hz, which is the single most useful
/// distinction down there, since it says which side of the Atlantic
/// the hum came from and therefore which notch to reach for.
pub const FFT: usize = 4096;

/// One band of the sound, followed over the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandTrack {
    /// The band's name and the range it covers, in Hz.
    pub name: String,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Level per column, dBFS.
    pub db: Vec<f32>,
}

/// A file's sound over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spectrogram {
    /// Seconds per column.
    pub hop_secs: f64,
    pub duration_secs: f64,
    /// Columns × bins, oldest first, dBFS.
    #[serde(skip)]
    pub columns: Vec<Vec<f32>>,
    pub bins: usize,
    pub rate: u32,
    pub bands: Vec<BandTrack>,
}

impl Spectrogram {
    /// The second a column stands for.
    pub fn secs_at(&self, column: usize) -> f64 {
        (column as f64 + 0.5) * self.hop_secs
    }

    fn band(&self, name: &str) -> Option<&BandTrack> {
        self.bands.iter().find(|b| b.name == name)
    }
}

/// The bands worth following, and why each one.
///
/// They do not overlap, and that is the point: a 50 Hz mains hum sat
/// inside both "rumble" and "hum" in the first version of this and was
/// reported twice, as though a recording had two faults. A sound editor
/// names one fault and reaches for one tool. So rumble stops where the
/// mains begins.
const BANDS: [(&str, f32, f32); 5] = [
    // Below the mains: wind, traffic, a hand on the tripod. Nothing
    // anybody wants is down here.
    ("rumble", 20.0, 45.0),
    // Mains hum and its first harmonics, both standards.
    ("hum", 45.0, 125.0),
    // Where speech actually lives, for the level track.
    ("voice", 200.0, 4000.0),
    // Sibilance: a de-esser's target.
    ("sibilance", 4000.0, 9000.0),
    // Hiss, air, a noisy preamp.
    ("hiss", 9000.0, 16000.0),
];

/// Decode a file's sound to mono and take its spectrogram.
///
/// `columns` is what the caller has room for; the cost does not depend
/// on how long the file is. `from`/`to` limit it to a stretch.
pub fn of_file(
    ffmpeg: &Path,
    path: &Path,
    from: f64,
    to: Option<f64>,
    columns: usize,
    bins: usize,
) -> Result<Spectrogram, String> {
    let columns = columns.clamp(16, 4096);
    let bins = bins.clamp(16, 512);
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"]);
    if from > 0.0 {
        cmd.args(["-ss", &format!("{from}")]);
    }
    cmd.args(klipp_media::input_args(path)?);
    if let Some(to) = to {
        let len = (to - from).max(0.01);
        cmd.args(["-t", &format!("{len}")]);
    }
    cmd.args(["-vn", "-sn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "f32le", "-"]);
    let out = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).output().map_err(|e| format!("ffmpeg would not run: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("ffmpeg could not read the sound of {}: {}", path.display(), err.lines().last().unwrap_or("").trim()));
    }
    let mono: Vec<f32> = out.stdout.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if mono.is_empty() {
        return Err(format!("{} has no sound in it", path.display()));
    }
    Ok(of_samples(&mono, columns, bins))
}

/// The spectrogram of samples already in hand, at [`RATE`].
pub fn of_samples(mono: &[f32], columns: usize, bins: usize) -> Spectrogram {
    let (cols, hop) = klipp_audio::spectrum::spectrogram(mono, RATE, FFT, columns, bins);
    let duration = mono.len() as f64 / RATE as f64;
    // Each named band is the loudest bin inside it, per column: a hum
    // is one narrow line, and averaging it with its quiet neighbours
    // would hide exactly what is being looked for.
    let bands = BANDS
        .iter()
        .map(|(name, lo, hi)| {
            let in_band: Vec<usize> = (0..bins)
                .filter(|b| {
                    let hz = klipp_audio::spectrum::bin_hz(*b, bins, RATE);
                    hz >= *lo && hz < *hi
                })
                .collect();
            let db = cols
                .iter()
                .map(|col| in_band.iter().map(|b| col[*b]).fold(-96.0f32, f32::max))
                .collect();
            BandTrack { name: (*name).to_string(), low_hz: *lo, high_hz: *hi, db }
        })
        .collect();
    Spectrogram { hop_secs: hop, duration_secs: duration, columns: cols, bins, rate: RATE, bands }
}

/// A stretch of the file where something is wrong.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub from_secs: f64,
    pub to_secs: f64,
}

/// What the bands say, in the terms a sound editor would use.
///
/// Every threshold here is a level *relative to the voice*, never an
/// absolute dBFS, because a quiet recording of a clean room and a loud
/// recording of a noisy one have the same absolute noise floor and
/// completely different problems. What matters is whether the noise is
/// close enough to the speech to be heard under it.
pub fn findings(s: &Spectrogram) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let Some(voice) = s.band("voice") else { return out };
    if voice.db.is_empty() {
        return out;
    }
    // The voice's own level where it is actually speaking: the loudest
    // quarter of the columns, so pauses do not drag the reference down.
    let mut sorted: Vec<f32> = voice.db.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let speech = sorted[sorted.len() * 3 / 4];
    if speech <= -80.0 {
        out.push((
            "warning".into(),
            "there is almost no sound in the speech range in this file".into(),
            "check the right track is being read, and that the recording is not silent".into(),
        ));
        return out;
    }
    // Under about 30 dB below the voice a steady noise stops being
    // audible under speech; at 20 dB down it is plainly there. Those
    // are the numbers a dialogue editor works to.
    struct Said {
        band: &'static str,
        worst: f32,
        text: String,
        fix: &'static str,
        error: bool,
    }
    let mut said: Vec<Said> = Vec::new();
    for (band, warn_at, fix) in [
        ("hum", 30.0f32, "a notch at the mains frequency — clean_audio hum"),
        ("rumble", 26.0, "a high-pass around 80 Hz — clean_audio wind"),
        ("hiss", 28.0, "a gentle broadband reduction — clean_audio hiss"),
        ("sibilance", 12.0, "a de-esser — clean_audio deess"),
    ] {
        let Some(t) = s.band(band) else { continue };
        let loud: Vec<usize> = t.db.iter().enumerate().filter(|(_, db)| **db > speech - warn_at).map(|(i, _)| i).collect();
        if loud.is_empty() {
            continue;
        }
        let spans = gather(&loud, s.hop_secs);
        // Every run too short to be a fault: a door closing, a chair.
        if spans.is_empty() {
            continue;
        }
        let worst = t.db.iter().fold(-96.0f32, |a, b| a.max(*b));
        let at_hz = (band == "hum")
            .then(|| loudest_hz(s, t.low_hz, t.high_hz))
            .flatten()
            .map(|hz| {
                // The bins are log-spaced and coarse this low, so the
                // measured centre lands a few Hz out. What matters is
                // which mains standard it is, and there are only two.
                let mains = [50.0f32, 60.0].into_iter().find(|m| (hz - m).abs() / m < 0.12);
                match mains {
                    Some(m) => format!(" at {m:.0} Hz"),
                    None => format!(" at {hz:.0} Hz"),
                }
            })
            .unwrap_or_default();
        let whole = loud.len() * 4 > t.db.len() * 3;
        let where_ = if whole {
            "throughout".to_string()
        } else {
            let listed: Vec<String> = spans.iter().take(3).map(|sp| format!("{:.1}–{:.1} s", sp.from_secs, sp.to_secs)).collect();
            format!("{}{}", listed.join(", "), if spans.len() > 3 { format!(" and {} more", spans.len() - 3) } else { String::new() })
        };
        said.push(Said {
            band,
            worst,
            text: format!("{band}{at_hz} ({:.0}–{:.0} Hz) reaches {:.0} dB under the voice, {where_}", t.low_hz, t.high_hz, speech - worst),
            fix,
            error: worst > speech - warn_at / 2.0,
        });
    }
    // One fault, one tool. A mains hum has skirts that reach into the
    // band below it, and reporting that as rumble as well would send
    // someone after a high-pass for a problem a notch fixes. So rumble
    // only stands on its own when it is the louder of the two.
    if let Some(hum) = said.iter().find(|x| x.band == "hum").map(|x| x.worst) {
        said.retain(|x| x.band != "rumble" || x.worst > hum);
    }
    for x in said {
        out.push((if x.error { "error".into() } else { "warning".into() }, x.text, x.fix.to_string()));
    }
    out
}

/// The frequency of the loudest bin between `low` and `high`, over the
/// whole file — what is actually there, rather than the band it is in.
fn loudest_hz(s: &Spectrogram, low: f32, high: f32) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for b in 0..s.bins {
        let hz = klipp_audio::spectrum::bin_hz(b, s.bins, s.rate);
        if hz < low || hz >= high {
            continue;
        }
        let db = s.columns.iter().map(|c| c[b]).fold(-96.0f32, f32::max);
        if best.is_none_or(|(_, d)| db > d) {
            best = Some((hz, db));
        }
    }
    best.map(|(hz, _)| hz)
}

/// Runs of neighbouring columns as spans in seconds. A single column
/// on its own is not a finding — it is a door closing.
fn gather(columns: &[usize], hop: f64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for &c in columns {
        match out.last_mut() {
            Some(last) if c as f64 * hop <= last.to_secs + hop * 2.0 => last.to_secs = (c + 1) as f64 * hop,
            _ => out.push(Span { from_secs: c as f64 * hop, to_secs: (c + 1) as f64 * hop }),
        }
    }
    out.retain(|s| s.to_secs - s.from_secs >= hop * 1.5);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin()).collect()
    }

    fn mixed(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    #[test]
    fn a_clean_voice_has_nothing_to_report() {
        // A 1 kHz tone standing in for speech, and nothing else.
        let s = of_samples(&sine(1000.0, 4.0, 0.3), 64, 96);
        assert!(findings(&s).is_empty(), "{:?}", findings(&s));
    }

    #[test]
    fn mains_hum_under_a_voice_is_named_with_its_frequency() {
        // 50 Hz or 60 Hz says which side of the Atlantic the recording
        // came from, and therefore which notch to reach for.
        for mains in [50.0f32, 60.0] {
            let s = of_samples(&mixed(&sine(1000.0, 4.0, 0.3), &sine(mains, 4.0, 0.05)), 64, 96);
            let f = findings(&s);
            let said = f.iter().find(|(_, w, _)| w.starts_with("hum")).unwrap_or_else(|| panic!("{mains} Hz hum found: {f:?}"));
            assert!(said.1.contains(&format!("at {mains:.0} Hz")), "{}", said.1);
            assert!(said.2.contains("notch"));
        }
    }

    #[test]
    fn one_fault_is_reported_once() {
        // A 50 Hz hum sat inside both the rumble band and the hum band
        // in the first version of this and was reported as two faults.
        // A sound editor names one fault and reaches for one tool.
        let s = of_samples(&mixed(&sine(1000.0, 4.0, 0.3), &sine(50.0, 4.0, 0.06)), 64, 96);
        let f = findings(&s);
        assert!(f.iter().any(|(_, w, _)| w.starts_with("hum")), "{f:?}");
        assert!(!f.iter().any(|(_, w, _)| w.starts_with("rumble")), "the mains is not also rumble: {f:?}");
        // …and something genuinely below the mains still is.
        let low = of_samples(&mixed(&sine(1000.0, 4.0, 0.3), &sine(28.0, 4.0, 0.08)), 64, 96);
        assert!(findings(&low).iter().any(|(_, w, _)| w.starts_with("rumble")), "{:?}", findings(&low));
    }

    #[test]
    fn a_hiss_that_is_far_enough_under_the_voice_is_left_alone() {
        // The threshold is relative to the voice, not absolute: a quiet
        // recording of a clean room must not be reported as noisy.
        let loud = mixed(&sine(1000.0, 3.0, 0.5), &sine(12000.0, 3.0, 0.0005));
        assert!(!findings(&of_samples(&loud, 48, 96)).iter().any(|(_, w, _)| w.contains("hiss")));
        // The same ratio at a tenth the level is the same recording.
        let quiet: Vec<f32> = loud.iter().map(|v| v * 0.1).collect();
        assert!(!findings(&of_samples(&quiet, 48, 96)).iter().any(|(_, w, _)| w.contains("hiss")));
    }

    #[test]
    fn a_fault_in_one_stretch_is_reported_with_its_seconds() {
        // Four seconds of voice; hum only in the middle two.
        let mut sig = sine(1000.0, 4.0, 0.3);
        let hum = sine(50.0, 2.0, 0.06);
        let start = RATE as usize;
        for (i, h) in hum.iter().enumerate() {
            sig[start + i] += h;
        }
        let s = of_samples(&sig, 64, 96);
        let f = findings(&s);
        let hum_said = f.iter().find(|(_, w, _)| w.contains("hum")).expect("the hum is found");
        assert!(!hum_said.1.contains("throughout"), "it is not throughout: {}", hum_said.1);
        assert!(hum_said.1.contains(" s"), "it says when: {}", hum_said.1);
    }

    #[test]
    fn a_silent_file_says_so_rather_than_reporting_noise() {
        let s = of_samples(&vec![0.0f32; RATE as usize], 32, 64);
        let f = findings(&s);
        assert_eq!(f.len(), 1);
        assert!(f[0].1.contains("almost no sound"), "{f:?}");
    }

    #[test]
    fn a_column_says_which_second_it_stands_for() {
        let s = of_samples(&sine(440.0, 10.0, 0.2), 20, 64);
        assert!((s.duration_secs - 10.0).abs() < 0.1);
        assert!((s.secs_at(0) - 0.25).abs() < 0.05, "{}", s.secs_at(0));
        assert!((s.secs_at(19) - 9.75).abs() < 0.05, "{}", s.secs_at(19));
    }
}
