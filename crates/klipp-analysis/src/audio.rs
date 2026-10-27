//! The sound as numbers: integrated loudness, loudness range and true
//! peak per EBU R128, and the spans of silence — the facts most quiet
//! suggestions and every loudness decision start from.

use serde::{Deserialize, Serialize};

pub const ID: &str = "audio";
// v2 adds the spectral bands (noise floor, low-frequency, mains-hum and
// high-frequency levels) that tell wind, rumble, hum and hiss apart.
// v3 measures the 50/60 Hz tones against their neighbours, so hum is a
// peak and not just a lot of low end: wind was reported as mains hum.
// v4 measures the band under 60 Hz, below any voice, for the rumble rule
// (#505).
pub const VERSION: u32 = 4;

/// Below this the signal is silence, and it has to stay there this long.
const SILENCE_DB: f32 = -35.0;
const SILENCE_MIN_SECS: f32 = 0.4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AudioEnvelope {
    pub duration_secs: f64,
    /// EBU R128 integrated loudness, LUFS.
    pub integrated_lufs: Option<f32>,
    /// Loudness range, LU.
    pub loudness_range_lu: Option<f32>,
    /// True peak, dBTP.
    pub true_peak_dbtp: Option<f32>,
    pub silence_threshold_db: f32,
    pub silences: Vec<Span>,
    /// Overall RMS level, dB (full-band). The reference the bands sit under.
    #[serde(default)]
    pub rms_db: Option<f32>,
    /// The noise floor, dB, as astats measures it.
    #[serde(default)]
    pub noise_floor_db: Option<f32>,
    /// RMS below 120 Hz — the low end (wind, rumble, hum) live here.
    #[serde(default)]
    pub low_db: Option<f32>,
    /// RMS below 60 Hz: under a voice's fundamental, so rumble and wind
    /// and not speech (#505).
    #[serde(default)]
    pub sub_db: Option<f32>,
    /// RMS in a narrow band around 50/60 Hz — mains hum sits here.
    #[serde(default)]
    pub hum_db: Option<f32>,
    /// RMS above 7 kHz — hiss and air live here.
    #[serde(default)]
    pub high_db: Option<f32>,
    /// The louder of two narrow bands at 50 and 60 Hz: a mains tone.
    #[serde(default)]
    pub hum_tone_db: Option<f32>,
    /// The louder of two narrow bands beside them, at 42 and 70 Hz. A
    /// hum stands above these; wind and rumble fill them as well.
    #[serde(default)]
    pub hum_neighbour_db: Option<f32>,
    /// When the sound is loudest, in seconds of the file (the peak of
    /// the momentary loudness): where a riser hits, where a stinger
    /// lands. Place that on the cut.
    #[serde(default)]
    pub loudest_secs: Option<f64>,
}

impl AudioEnvelope {
    /// Seconds of silence within `from..to`.
    pub fn silence_within(&self, from: f64, to: f64) -> f64 {
        self.silences
            .iter()
            .map(|s| (s.end_secs.min(to) - s.start_secs.max(from)).max(0.0))
            .sum()
    }

    /// The silences overlapping `from..to`.
    pub fn silences_within(&self, from: f64, to: f64) -> Vec<&Span> {
        self.silences.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }

    /// The audible sound problems the spectral bands reveal, each as
    /// `(what is wrong, what would put it right)`. Empty when the bands
    /// were not measured (an older analysis) or the sound is clean. A
    /// band sitting close under the full-band level means that part of
    /// the spectrum carries as much energy as the programme — a fault.
    pub fn faults(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let (Some(full), Some(low), Some(high)) = (self.rms_db, self.low_db, self.high_db) else {
            return out;
        };
        // Rumble is energy under the voice. Below 120 Hz a male voice's
        // own fundamental counts, and clean narration read as rumble;
        // below 60 Hz, measured, clean speech sits 39 to 40 dB under the
        // programme, a quiet room 30, and wind, traffic and a film score
        // 6 to 14 (#505). An analysis from before the band falls back.
        let lf_strong = match self.sub_db {
            Some(sub) => sub - full > RUMBLE_DB,
            None => low - full > -12.0,
        };
        // A mains hum is a tone: 50 or 60 Hz standing above the bands
        // beside it, loud enough to be heard. Broadband wind and rumble
        // fill the neighbours as well. The wide hum band alone could not
        // tell them apart, and wind on outdoor clips read as hum.
        if self.hum_is_a_tone(full) {
            out.push(("a mains hum near 50/60 Hz runs under the sound".to_string(), "clean_audio with hum on this clip".to_string()));
        } else if lf_strong {
            out.push(("a strong low-frequency rumble — wind or handling — sits under the sound".to_string(), "clean_audio with voice on this clip if someone speaks, else wind".to_string()));
        }
        if high - full > -12.0 {
            out.push(("high-frequency hiss or air is loud against the sound".to_string(), "clean_audio with voice on this clip if someone speaks, else hiss".to_string()));
        }
        if let Some(floor) = self.noise_floor_db {
            if floor > -40.0 && !lf_strong && high - full <= -12.0 {
                out.push((format!("the noise floor is high at {floor:.0} dB — the background is audible"), "clean_audio with hiss, or capture room tone to record over".to_string()));
            }
        }
        out
    }

    /// Whether a 50/60 Hz tone stands at least [`HUM_PEAK_DB`] above its
    /// neighbours and within 40 dB of the programme. An analysis from
    /// before the tones were measured says no.
    fn hum_is_a_tone(&self, full: f32) -> bool {
        match (self.hum_tone_db, self.hum_neighbour_db) {
            (Some(tone), Some(beside)) => tone - beside >= HUM_PEAK_DB && tone - full > -40.0,
            _ => false,
        }
    }

    /// The clean_audio flags the bands suggest: `(wind, hiss, hum)`.
    pub fn suggested_cleanup(&self) -> (bool, bool, bool) {
        let mut wind = false;
        let mut hiss = false;
        let mut hum = false;
        for (what, _) in self.faults() {
            if what.contains("hum") { hum = true; }
            else if what.contains("rumble") || what.contains("wind") { wind = true; }
            else if what.contains("hiss") || what.contains("noise floor") { hiss = true; }
        }
        (wind, hiss, hum)
    }
}

fn number_after(line: &str, key: &str) -> Option<f64> {
    let i = line.find(key)?;
    let rest = line[i + key.len()..].trim_start();
    let token: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    token.parse().ok()
}

/// What `silencedetect` and `ebur128` printed.
pub fn parse(stderr: &str, duration_secs: f64) -> AudioEnvelope {
    let mut env = AudioEnvelope { duration_secs, silence_threshold_db: SILENCE_DB, ..Default::default() };
    let mut open: Option<f64> = None;
    let mut in_summary = false;
    let mut loudest: Option<(f64, f64)> = None;
    for line in stderr.lines() {
        // The momentary loudness ebur128 prints ten times a second:
        // "t: 12.3 TARGET:-23 LUFS M: -18.4 S: ..." — the loudest tenth.
        if line.contains("ebur128") && line.contains(" M:") {
            if let (Some(t), Some(m)) = (number_after(line, "t:"), number_after(line, "M:")) {
                if m.is_finite() && loudest.is_none_or(|(_, best)| m > best) {
                    loudest = Some((t, m));
                }
            }
        }
        if line.contains("silence_start:") {
            open = number_after(line, "silence_start:");
        } else if line.contains("silence_end:") {
            if let (Some(start), Some(end)) = (open.take(), number_after(line, "silence_end:")) {
                env.silences.push(Span { start_secs: start, end_secs: end });
            }
        } else if line.contains("Summary:") {
            in_summary = true;
        } else if in_summary {
            let t = line.trim_start();
            if let Some(v) = t.strip_prefix("I:") {
                env.integrated_lufs = number_after(v, "").map(|x| x as f32);
            } else if let Some(v) = t.strip_prefix("LRA:") {
                env.loudness_range_lu = number_after(v, "").map(|x| x as f32);
            } else if let Some(v) = t.strip_prefix("Peak:") {
                env.true_peak_dbtp = number_after(v, "").map(|x| x as f32);
            }
        }
    }
    // A silence still open at the end runs to the end.
    if let Some(start) = open {
        if duration_secs > start {
            env.silences.push(Span { start_secs: start, end_secs: duration_secs });
        }
    }
    env.loudest_secs = loudest.map(|(t, _)| t);
    env
}

/// How far a mains tone must stand above the bands beside it. Measured:
/// a 50 Hz hum under brown noise stood 11 dB above, a quiet 60 Hz hum
/// under pink noise 4.5 dB; wind and traffic on three outdoor phone
/// clips between -1 and +2 dB.
const HUM_PEAK_DB: f32 = 3.5;

/// The narrow tone bands, measured after the four of [`BAND_FILTER`]:
/// 50, 60, 42 and 70 Hz, each two 2 Hz band-passes in a row, which is
/// sharp enough for a tone to stand out of broadband low end.
const TONE_FILTER: &str = "asplit=4[e][f][g][h];\
[e]bandpass=f=50:width_type=h:w=2,bandpass=f=50:width_type=h:w=2,astats=metadata=0:measure_perchannel=none[t1];\
[f]bandpass=f=60:width_type=h:w=2,bandpass=f=60:width_type=h:w=2,astats=metadata=0:measure_perchannel=none[t2];\
[g]bandpass=f=42:width_type=h:w=2,bandpass=f=42:width_type=h:w=2,astats=metadata=0:measure_perchannel=none[t3];\
[h]bandpass=f=70:width_type=h:w=2,bandpass=f=70:width_type=h:w=2,astats=metadata=0:measure_perchannel=none[t4];\
[t1][t2][t3][t4]amix=inputs=4";

/// Read `(tone, neighbour)` from an `astats` run of the tone filter: the
/// louder of 50/60 Hz and the louder of 42/70 Hz, in dB.
pub fn parse_tones(stderr: &str) -> (Option<f32>, Option<f32>) {
    let level = |n| parse_nth_level(stderr, n);
    let louder = |x: Option<f32>, y: Option<f32>| match (x, y) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    };
    (louder(level(0), level(1)), louder(level(2), level(3)))
}

/// The RMS level of the `n`th `astats` instance, counting from 0.
fn parse_nth_level(stderr: &str, n: usize) -> Option<f32> {
    let mut by_index: std::collections::BTreeMap<u32, Option<f32>> = std::collections::BTreeMap::new();
    for line in stderr.lines() {
        let Some(rest) = line.split("Parsed_astats_").nth(1) else { continue };
        let idx: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(idx) = idx.parse::<u32>() else { continue };
        if let Some(v) = line.split("RMS level dB:").nth(1) {
            if !line.contains("RMS peak") && !line.contains("RMS trough") {
                by_index.insert(idx, v.trim().parse().ok());
            }
        }
    }
    by_index.into_values().nth(n).flatten()
}

/// The filtergraph that splits the sound into full, low (<120 Hz),
/// mains-hum (~50/60 Hz) and high (>7 kHz) bands and measures each. The
/// `astats` instances come out numbered in this order, so sorting by the
/// instance index recovers full, low, hum, high.
const BAND_FILTER: &str = "asplit=5[a][b][c][d][e];\
[a]astats=metadata=0:measure_perchannel=none[o1];\
[b]lowpass=f=120,astats=metadata=0:measure_perchannel=none[o2];\
[c]bandpass=f=55:width_type=h:w=25,astats=metadata=0:measure_perchannel=none[o3];\
[d]highpass=f=7000,astats=metadata=0:measure_perchannel=none[o4];\
[e]lowpass=f=60,lowpass=f=60,astats=metadata=0:measure_perchannel=none[o5];\
[o1][o2][o3][o4][o5]amix=inputs=5";

/// How close under the programme the band below 60 Hz may sit before it
/// is rumble, dB.
const RUMBLE_DB: f32 = -20.0;

/// Read the per-band RMS and the noise floor from an `astats` run of
/// `BAND_FILTER`. Returns `(rms, noise_floor, low, hum, high, sub)` in dB.
pub fn parse_bands(stderr: &str) -> (Option<f32>, Option<f32>, Option<f32>, Option<f32>, Option<f32>, Option<f32>) {
    // instance index -> (rms level dB, noise floor dB)
    let mut by_index: std::collections::BTreeMap<u32, (Option<f32>, Option<f32>)> = std::collections::BTreeMap::new();
    for line in stderr.lines() {
        let Some(rest) = line.split("Parsed_astats_").nth(1) else { continue };
        let idx: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(idx) = idx.parse::<u32>() else { continue };
        let entry = by_index.entry(idx).or_insert((None, None));
        if let Some(v) = line.split("RMS level dB:").nth(1) {
            if !line.contains("RMS peak") && !line.contains("RMS trough") {
                entry.0 = v.trim().parse().ok();
            }
        } else if let Some(v) = line.split("Noise floor dB:").nth(1) {
            entry.1 = v.trim().parse().ok();
        }
    }
    let ordered: Vec<(Option<f32>, Option<f32>)> = by_index.into_values().collect();
    let rms = ordered.first().and_then(|x| x.0);
    let noise = ordered.first().and_then(|x| x.1);
    let low = ordered.get(1).and_then(|x| x.0);
    let hum = ordered.get(2).and_then(|x| x.0);
    let high = ordered.get(3).and_then(|x| x.0);
    let sub = ordered.get(4).and_then(|x| x.0);
    (rms, noise, low, hum, high, sub)
}

pub struct AudioAnalyser;

impl crate::Analyser for AudioAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        item.has_audio && item.duration.to_secs_f64() > 0.0
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let input = klipp_media::input_args_lossy(&item.path)?;
        let args: Vec<String> = [
            "-hide_banner".to_string(),
            "-nostats".to_string(),
        ].into_iter().chain(input.iter().cloned()).chain([
            "-vn".to_string(),
            "-sn".to_string(),
            "-af".to_string(),
            format!("silencedetect=noise={SILENCE_DB}dB:d={SILENCE_MIN_SECS},ebur128=peak=true"),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ]).collect();
        let stderr = crate::ffmpeg_stderr(&ctx.ffmpeg, &args)?;
        let mut env = parse(&stderr, item.duration.to_secs_f64());
        // A second, quick pass measures the spectral bands, so wind,
        // rumble, hum and hiss can be told apart from the numbers.
        let band_args: Vec<String> = [
            "-hide_banner".to_string(),
            "-nostats".to_string(),
        ].into_iter().chain(input.iter().cloned()).chain([
            "-vn".to_string(),
            "-sn".to_string(),
            "-filter_complex".to_string(),
            BAND_FILTER.to_string(),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ]).collect();
        if let Ok(bs) = crate::ffmpeg_stderr(&ctx.ffmpeg, &band_args) {
            let (rms, noise, low, hum, high, sub) = parse_bands(&bs);
            env.sub_db = sub;
            env.rms_db = rms;
            env.noise_floor_db = noise;
            env.low_db = low;
            env.hum_db = hum;
            env.high_db = high;
        }
        // The mains tones against their neighbours: hum is a peak.
        let tone_args: Vec<String> = band_args
            .iter()
            .map(|a| if a == BAND_FILTER { TONE_FILTER.to_string() } else { a.clone() })
            .collect();
        if let Ok(ts) = crate::ffmpeg_stderr(&ctx.ffmpeg, &tone_args) {
            (env.hum_tone_db, env.hum_neighbour_db) = parse_tones(&ts);
        }
        serde_json::to_value(env).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_stderr(full: f32, low: f32, hum: f32, high: f32) -> String {
        // astats instances come out numbered 1,3,5,7 for full/low/hum/high.
        let mut s = String::new();
        for (idx, v) in [(1, full), (3, low), (5, hum), (7, high)] {
            s.push_str(&format!("[Parsed_astats_{idx} @ 0x1] RMS level dB: {v:.3}\n"));
            s.push_str(&format!("[Parsed_astats_{idx} @ 0x1] RMS peak dB: {:.3}\n", v + 3.0));
            s.push_str(&format!("[Parsed_astats_{idx} @ 0x1] Noise floor dB: {:.3}\n", v - 6.0));
        }
        s
    }

    /// astats output for the tone filter: 50, 60, 42, 70 Hz levels, with
    /// the instance numbers ffmpeg gives them.
    fn tone_stderr(f50: f32, f60: f32, f42: f32, f70: f32) -> String {
        let mut s = String::new();
        for (idx, v) in [(3, f50), (6, f60), (9, f42), (12, f70)] {
            s.push_str(&format!("[Parsed_astats_{idx} @ 0x1] RMS level dB: {v:.3}\n"));
            s.push_str(&format!("[Parsed_astats_{idx} @ 0x1] RMS peak dB: {:.3}\n", v + 3.0));
        }
        s
    }

    /// #505: rumble is the band under 60 Hz, where a voice has nothing.
    /// Clean narration fills the band under 120 Hz with its own
    /// fundamental and read as rumble; wind and traffic fill both.
    #[test]
    fn narration_is_not_rumble_and_wind_still_is() {
        let with = |full: f32, low: f32, sub: f32| AudioEnvelope { rms_db: Some(full), low_db: Some(low), high_db: Some(full - 30.0), sub_db: Some(sub), ..Default::default() };
        // A deep narrating voice: its fundamental 11 dB under the whole,
        // nothing below 60 Hz (a Piper voice measured -40 there).
        let narration = with(-18.0, -29.0, -58.0);
        assert!(!narration.faults().iter().any(|(w, _)| w.contains("rumble")), "{:?}", narration.faults());
        // The wind clip, measured: 60 Hz band 14 dB under.
        let wind = with(-19.6, -25.3, -33.8);
        assert!(wind.faults().iter().any(|(w, _)| w.contains("rumble")), "{:?}", wind.faults());
        // An analysis from before the band reads as it did.
        let old = AudioEnvelope { sub_db: None, ..narration.clone() };
        assert!(old.faults().iter().any(|(w, _)| w.contains("rumble")));
    }

    #[test]
    fn bands_tell_hum_hiss_and_clean_apart() {
        // The three real measured signatures (from ffmpeg astats).
        let clean = { let (r, n, l, h, hi, _) = parse_bands(&band_stderr(-34.3, -54.1, -59.8, -54.4)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
        assert!(clean.faults().is_empty(), "clean has no faults: {:?}", clean.faults());
        assert_eq!(clean.rms_db, Some(-34.3));

        let mut hum = { let (r, n, l, h, hi, _) = parse_bands(&band_stderr(-28.4, -29.2, -29.15, -57.1)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
        // A 50 Hz tone standing 11 dB over its neighbours (measured).
        (hum.hum_tone_db, hum.hum_neighbour_db) = parse_tones(&tone_stderr(-46.9, -58.4, -58.2, -58.5));
        assert!(hum.faults().iter().any(|(w, _)| w.contains("hum")), "{:?}", hum.faults());
        assert_eq!(hum.suggested_cleanup(), (false, false, true));

        // #330: the same strong low end with no tone in it — wind and
        // traffic on an outdoor phone clip, measured — is rumble, not hum.
        let mut wind = hum.clone();
        (wind.hum_tone_db, wind.hum_neighbour_db) = parse_tones(&tone_stderr(-48.2, -44.2, -49.2, -46.2));
        let faults = wind.faults();
        assert!(!faults.iter().any(|(w, _)| w.contains("hum")), "wind read as hum: {faults:?}");
        assert!(faults.iter().any(|(w, _)| w.contains("rumble")), "{faults:?}");

        // An analysis from before the tones were measured claims no hum.
        let mut old = hum.clone();
        (old.hum_tone_db, old.hum_neighbour_db) = (None, None);
        assert!(!old.faults().iter().any(|(w, _)| w.contains("hum")));

        let hiss = { let (r, n, l, h, hi, _) = parse_bands(&band_stderr(-24.3, -46.4, -51.8, -26.3)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
        assert!(hiss.faults().iter().any(|(w, _)| w.contains("hiss")), "{:?}", hiss.faults());
        assert_eq!(hiss.suggested_cleanup(), (false, true, false));

        // An older analysis with no bands measured reports nothing.
        assert!(AudioEnvelope::default().faults().is_empty());
    }

    #[test]
    fn silences_and_the_r128_summary_are_read() {
        let text = "\
[silencedetect @ 0x1] silence_start: 1.02\n\
[silencedetect @ 0x1] silence_end: 2.5 | silence_duration: 1.48\n\
[silencedetect @ 0x1] silence_start: 9.0\n\
[Parsed_ebur128_1 @ 0x2] Summary:\n\
\n\
  Integrated loudness:\n\
    I:         -23.4 LUFS\n\
    Threshold: -33.5 LUFS\n\
\n\
  Loudness range:\n\
    LRA:         6.2 LU\n\
\n\
  True peak:\n\
    Peak:       -1.3 dBFS\n";
        let e = parse(text, 10.0);
        assert_eq!(e.silences, vec![Span { start_secs: 1.02, end_secs: 2.5 }, Span { start_secs: 9.0, end_secs: 10.0 }]);
        assert_eq!(e.integrated_lufs, Some(-23.4));
        assert_eq!(e.loudness_range_lu, Some(6.2));
        assert_eq!(e.true_peak_dbtp, Some(-1.3));
        assert!((e.silence_within(0.0, 2.0) - 0.98).abs() < 1e-6);
    }
}
