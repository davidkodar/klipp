//! The sound as numbers: integrated loudness, loudness range and true
//! peak per EBU R128, and the spans of silence — the facts most quiet
//! suggestions and every loudness decision start from.

use serde::{Deserialize, Serialize};

pub const ID: &str = "audio";
// v2 adds the spectral bands (noise floor, low-frequency, mains-hum and
// high-frequency levels) that tell wind, rumble, hum and hiss apart.
pub const VERSION: u32 = 2;

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
    /// RMS in a narrow band around 50/60 Hz — mains hum sits here.
    #[serde(default)]
    pub hum_db: Option<f32>,
    /// RMS above 7 kHz — hiss and air live here.
    #[serde(default)]
    pub high_db: Option<f32>,
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
        let (Some(full), Some(low), Some(hum), Some(high)) = (self.rms_db, self.low_db, self.hum_db, self.high_db) else {
            return out;
        };
        let lf_strong = low - full > -12.0;
        // Low end within 12 dB of the whole: either a mains-hum tone
        // (the energy concentrates in the 50/60 Hz band) or broadband
        // wind and rumble (it does not).
        if lf_strong && hum - low > -5.0 {
            out.push(("a mains hum near 50/60 Hz runs under the sound".to_string(), "clean_audio with hum on this clip".to_string()));
        } else if lf_strong {
            out.push(("a strong low-frequency rumble — wind or handling — sits under the sound".to_string(), "clean_audio with wind on this clip".to_string()));
        }
        if high - full > -12.0 {
            out.push(("high-frequency hiss or air is loud against the sound".to_string(), "clean_audio with hiss on this clip".to_string()));
        }
        if let Some(floor) = self.noise_floor_db {
            if floor > -40.0 && !lf_strong && high - full <= -12.0 {
                out.push((format!("the noise floor is high at {floor:.0} dB — the background is audible"), "clean_audio with hiss, or capture room tone to record over".to_string()));
            }
        }
        out
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

/// The filtergraph that splits the sound into full, low (<120 Hz),
/// mains-hum (~50/60 Hz) and high (>7 kHz) bands and measures each. The
/// `astats` instances come out numbered in this order, so sorting by the
/// instance index recovers full, low, hum, high.
const BAND_FILTER: &str = "asplit=4[a][b][c][d];\
[a]astats=metadata=0:measure_perchannel=none[o1];\
[b]lowpass=f=120,astats=metadata=0:measure_perchannel=none[o2];\
[c]bandpass=f=55:width_type=h:w=25,astats=metadata=0:measure_perchannel=none[o3];\
[d]highpass=f=7000,astats=metadata=0:measure_perchannel=none[o4];\
[o1][o2][o3][o4]amix=inputs=4";

/// Read the per-band RMS and the noise floor from an `astats` run of
/// `BAND_FILTER`. Returns `(rms, noise_floor, low, hum, high)` in dB.
pub fn parse_bands(stderr: &str) -> (Option<f32>, Option<f32>, Option<f32>, Option<f32>, Option<f32>) {
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
    (rms, noise, low, hum, high)
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
        let args = vec![
            "-hide_banner".to_string(),
            "-nostats".to_string(),
            "-i".to_string(),
            item.path.to_string_lossy().into_owned(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-af".to_string(),
            format!("silencedetect=noise={SILENCE_DB}dB:d={SILENCE_MIN_SECS},ebur128=peak=true"),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ];
        let stderr = crate::ffmpeg_stderr(&ctx.ffmpeg, &args)?;
        let mut env = parse(&stderr, item.duration.to_secs_f64());
        // A second, quick pass measures the spectral bands, so wind,
        // rumble, hum and hiss can be told apart from the numbers.
        let band_args = vec![
            "-hide_banner".to_string(),
            "-nostats".to_string(),
            "-i".to_string(),
            item.path.to_string_lossy().into_owned(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-filter_complex".to_string(),
            BAND_FILTER.to_string(),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ];
        if let Ok(bs) = crate::ffmpeg_stderr(&ctx.ffmpeg, &band_args) {
            let (rms, noise, low, hum, high) = parse_bands(&bs);
            env.rms_db = rms;
            env.noise_floor_db = noise;
            env.low_db = low;
            env.hum_db = hum;
            env.high_db = high;
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

    #[test]
    fn bands_tell_hum_hiss_and_clean_apart() {
        // The three real measured signatures (from ffmpeg astats).
        let clean = { let (r, n, l, h, hi) = parse_bands(&band_stderr(-34.3, -54.1, -59.8, -54.4)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
        assert!(clean.faults().is_empty(), "clean has no faults: {:?}", clean.faults());
        assert_eq!(clean.rms_db, Some(-34.3));

        let hum = { let (r, n, l, h, hi) = parse_bands(&band_stderr(-28.4, -29.2, -29.15, -57.1)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
        assert!(hum.faults().iter().any(|(w, _)| w.contains("hum")), "{:?}", hum.faults());
        assert_eq!(hum.suggested_cleanup(), (false, false, true));

        let hiss = { let (r, n, l, h, hi) = parse_bands(&band_stderr(-24.3, -46.4, -51.8, -26.3)); let mut e = AudioEnvelope::default(); (e.rms_db, e.noise_floor_db, e.low_db, e.hum_db, e.high_db) = (r, n, l, h, hi); e };
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
