//! A music track's shape, for cutting to it (#503): its loudness bar by
//! bar as an energy curve, the sections that curve falls into (intro,
//! build, peak, breakdown, outro, and the body between), and a way to fit
//! the track to a length by taking out or repeating whole bars where the
//! join is cleanest.
//!
//! It stands on the beat grid ([`crate::beats`]): a bar is four beats
//! from the first, a phrase four bars. Sections change only on a phrase,
//! where the energy steps; a fit cuts or repeats a whole number of bars,
//! from one bar line to another, choosing the pair of bar lines whose
//! sound on either side is most alike, so the join falls between two bars
//! that could have followed each other.

use crate::beats::BeatData;

/// What a section of the track does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    Intro,
    Body,
    Build,
    Peak,
    Breakdown,
    Outro,
}

/// One section: where it runs and how loud it is, 0 to 1 against the
/// track's own range.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Section {
    pub start_secs: f64,
    pub end_secs: f64,
    pub kind: SectionKind,
    pub energy: f32,
}

/// The track's shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Structure {
    /// Bar lines, in seconds: bar `n` runs from `bars[n]` to `bars[n + 1]`.
    pub bars: Vec<f64>,
    /// Each bar's energy, 0 to 1 against the track's quietest and
    /// loudest bars.
    pub energy: Vec<f32>,
    /// Each bar's brightness, 0 to 1 the same way: how much of its sound
    /// is in the highs (cymbals, a lead, distortion) against the lows. A
    /// mastered track can keep one loudness through verse and chorus;
    /// what changes is this.
    pub brightness: Vec<f32>,
    pub sections: Vec<Section>,
    /// The track's length, seconds.
    pub duration_secs: f64,
}

/// Beats to a bar, bars to a phrase.
const BEATS_PER_BAR: usize = 4;
const BARS_PER_PHRASE: usize = 4;

/// The shape of `pcm` (mono, `sample_rate`) on the beat grid `beats`.
/// `None` when there is no beat to stand on.
pub fn analyse(pcm: &[f32], sample_rate: u32, beats: &BeatData) -> Option<Structure> {
    let duration = pcm.len() as f64 / sample_rate as f64;
    let bars = bar_lines(beats, duration)?;
    let (loudness, bright): (Vec<f64>, Vec<f64>) = bars
        .windows(2)
        .map(|w| {
            let (a, b) = ((w[0] * sample_rate as f64) as usize, ((w[1] * sample_rate as f64) as usize).min(pcm.len()));
            let bar = &pcm[a.min(b)..b];
            let n = bar.len().max(1);
            let ms = bar.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / n as f64;
            // The first difference weighs the highs: its share of the
            // energy rises with the sound's brightness.
            let diff = bar.windows(2).map(|d| ((d[1] - d[0]) as f64).powi(2)).sum::<f64>() / n as f64;
            (10.0 * (ms + 1e-10).log10(), 10.0 * ((diff + 1e-12) / (ms + 1e-10)).log10())
        })
        .unzip();
    let energy = normalise(&loudness);
    let brightness = normalise(&bright);
    let sections = sections(&bars, &energy, &brightness);
    Some(Structure { bars, energy, brightness, sections, duration_secs: duration })
}

/// The bar lines: every fourth beat from the first, carried on at the
/// beat interval past the beats read, to the end of the track.
fn bar_lines(beats: &BeatData, duration: f64) -> Option<Vec<f64>> {
    if beats.bpm <= 0.0 || beats.confidence < 0.3 || beats.beat_times.len() < BEATS_PER_BAR * 2 {
        return None;
    }
    let beat = 60.0 / beats.bpm as f64;
    let mut lines: Vec<f64> = beats.beat_times.iter().step_by(BEATS_PER_BAR).copied().collect();
    let bar = beat * BEATS_PER_BAR as f64;
    while let Some(&last) = lines.last() {
        if last + bar > duration {
            break;
        }
        lines.push(last + bar);
    }
    // The music before the first bar line is a pickup into bar 0.
    if lines[0] > 0.05 {
        lines.insert(0, 0.0);
    }
    if duration - lines[lines.len() - 1] > 0.05 {
        lines.push(duration);
    }
    Some(lines)
}

/// Loudness in dB to 0..1 between the 5th and 95th percentile bars.
fn normalise(db: &[f64]) -> Vec<f32> {
    let mut sorted = db.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pick = |q: f64| sorted.get(((sorted.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
    let (lo, hi) = (pick(0.05), pick(0.95));
    let span = (hi - lo).max(1.0);
    db.iter().map(|d| ((d - lo) / span).clamp(0.0, 1.0) as f32).collect()
}

/// Sections: phrases joined while their energy and brightness stay
/// near, split where either steps; then named by where they fall and how
/// loud they are.
fn sections(bars: &[f64], energy: &[f32], brightness: &[f32]) -> Vec<Section> {
    let n = energy.len();
    if n == 0 {
        return Vec::new();
    }
    let mean = |a: usize, b: usize| energy[a..b].iter().sum::<f32>() / (b - a).max(1) as f32;
    let bright = |a: usize, b: usize| brightness[a..b].iter().sum::<f32>() / (b - a).max(1) as f32;
    // Phrase boundaries, in bars.
    let mut cuts: Vec<usize> = (0..n).step_by(BARS_PER_PHRASE).collect();
    cuts.push(n);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        match spans.last_mut() {
            Some(last) if (mean(last.0, last.1) - mean(a, b)).abs() < 0.18 && (bright(last.0, last.1) - bright(a, b)).abs() < 0.25 => last.1 = b,
            _ => spans.push((a, b)),
        }
    }
    let peak = spans.iter().map(|&(a, b)| mean(a, b)).fold(0.0f32, f32::max);
    let count = spans.len();
    spans
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let e = mean(a, b);
            let next = spans.get(i + 1).map(|&(c, d)| mean(c, d));
            let prev = i.checked_sub(1).map(|j| mean(spans[j].0, spans[j].1));
            let kind = if count > 1 && i == 0 && e < peak - 0.15 {
                SectionKind::Intro
            } else if count > 1 && i == count - 1 && e < peak - 0.15 {
                SectionKind::Outro
            } else if e >= peak - 0.1 && e > 0.55 {
                SectionKind::Peak
            } else if prev.is_some_and(|p| p > e + 0.2) {
                // A drop from what came before, even on the way back up.
                SectionKind::Breakdown
            } else if next.is_some_and(|n| n > e + 0.15) {
                SectionKind::Build
            } else {
                SectionKind::Body
            };
            Section { start_secs: bars[a], end_secs: bars[b.min(bars.len() - 1)], kind, energy: e }
        })
        .collect()
}

/// A fitted track: the stretches of the source to play, in order, back
/// to back, and how long they come to.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fit {
    pub segments: Vec<(f64, f64)>,
    pub length_secs: f64,
    /// Where the join falls in the result, seconds.
    pub join_secs: Option<f64>,
    /// Bars taken out (negative) or repeated (positive).
    pub bars_changed: i64,
}

/// The track fitted to about `target_secs`: a whole number of bars taken
/// out (or repeated, to lengthen), from one bar line to another, the
/// first and last phrase left alone so the track still starts and ends as
/// it does. Phrases are preferred to odd bars; among the cuts that come
/// nearest the target, the one whose join is between the most alike bars.
/// `None` when no bar-line cut can fit.
pub fn fit(s: &Structure, target_secs: f64) -> Option<Fit> {
    let n = s.energy.len();
    if n < 2 * BARS_PER_PHRASE + 1 || target_secs <= 0.0 {
        return None;
    }
    let change = target_secs - s.duration_secs;
    if change.abs() < 0.25 {
        return Some(Fit { segments: vec![(0.0, s.duration_secs)], length_secs: s.duration_secs, join_secs: None, bars_changed: 0 });
    }
    // The cut runs from bar line `a` to bar line `b` (a < b): taken out
    // when shortening, played again when lengthening.
    let lo = BARS_PER_PHRASE.min(n);
    let hi = n.saturating_sub(BARS_PER_PHRASE);
    let mut best: Option<(f64, usize, usize)> = None;
    for a in lo..hi {
        for b in a + 1..=hi {
            let span = s.bars[b] - s.bars[a];
            if change < 0.0 && span > s.duration_secs - 1.0 {
                continue;
            }
            let length = if change < 0.0 { s.duration_secs - span } else { s.duration_secs + span };
            let miss = (length - target_secs).abs();
            // The join plays bar a-1 then bar b (shortening) or bar b-1
            // then bar a (lengthening): those two should sound alike.
            let (before, after) = if change < 0.0 { (a - 1, b) } else { (b - 1, a) };
            let seam = (s.energy[before] - s.energy.get(after).copied().unwrap_or(0.0)).abs() as f64;
            let phrase = if (b - a) % BARS_PER_PHRASE == 0 && a % BARS_PER_PHRASE == 0 { 0.0 } else { 0.5 };
            // A bar's length of miss weighs as much as an unlike join.
            let bar = s.bars[1] - s.bars[0];
            let score = miss / bar.max(0.1) + seam * 2.0 + phrase;
            if best.is_none_or(|(sc, ..)| score < sc) {
                best = Some((score, a, b));
            }
        }
    }
    let (_, a, b) = best?;
    let (ta, tb) = (s.bars[a], s.bars[b]);
    let (segments, join, bars) = if change < 0.0 {
        (vec![(0.0, ta), (tb, s.duration_secs)], ta, -((b - a) as i64))
    } else {
        (vec![(0.0, tb), (ta, s.duration_secs)], tb, (b - a) as i64)
    };
    let length = segments.iter().map(|(x, y)| y - x).sum();
    Some(Fit { segments, length_secs: length, join_secs: Some(join), bars_changed: bars })
}

/// Where the track's intro ends: the end of its first section when that
/// section is an intro, for a fit that starts on the music rather than
/// on its quiet opening (#1576).
pub fn intro_end(s: &Structure) -> Option<f64> {
    s.sections.first().filter(|x| x.kind == SectionKind::Intro).map(|x| x.end_secs)
}

/// [`fit`] of the track from `start_secs` on: what comes before the bar
/// line at or after it is left out, and the rest is fitted to about
/// `target_secs` as `fit` does. The segments are in the source's own
/// seconds.
pub fn fit_from(s: &Structure, target_secs: f64, start_secs: f64) -> Option<Fit> {
    let first = s.bars.iter().position(|&b| b >= start_secs - 1e-6)?;
    if first == 0 {
        return fit(s, target_secs);
    }
    let start = s.bars[first];
    let trimmed = Structure {
        bars: s.bars[first..].iter().map(|b| b - start).collect(),
        energy: s.energy.get(first..).unwrap_or(&[]).to_vec(),
        brightness: s.brightness.get(first..).unwrap_or(&[]).to_vec(),
        sections: s.sections.iter().filter(|x| x.end_secs > start).map(|x| Section {
            start_secs: (x.start_secs - start).max(0.0), end_secs: x.end_secs - start, ..x.clone()
        }).collect(),
        duration_secs: s.duration_secs - start,
    };
    let mut f = fit(&trimmed, target_secs)?;
    for seg in &mut f.segments {
        seg.0 += start;
        seg.1 += start;
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 8_000;

    /// A track at 120 BPM (a bar is 2 s) whose loudness by phrase (8 s)
    /// is `levels`: a click on every beat over a tone at that level.
    fn track(levels: &[f32]) -> (Vec<f32>, BeatData) {
        let secs = levels.len() as f64 * 8.0;
        let n = (secs * SR as f64) as usize;
        let mut pcm = vec![0.0f32; n];
        for (i, s) in pcm.iter_mut().enumerate() {
            let t = i as f64 / SR as f64;
            let level = levels[((t / 8.0) as usize).min(levels.len() - 1)];
            let click = if (t % 0.5) < 0.01 { 1.0 } else { 0.0 };
            *s = level * ((2.0 * std::f64::consts::PI * 220.0 * t).sin() as f32 * 0.5 + click * 0.5);
        }
        let beats = BeatData { bpm: 120.0, confidence: 0.9, beat_times: (0..(secs / 0.5) as usize).map(|k| k as f64 * 0.5).collect() };
        (pcm, beats)
    }

    #[test]
    fn sections_follow_the_energy_and_are_named() {
        // Quiet intro, a body, a rise, the peak, a drop, the peak again, a quiet end.
        let (pcm, beats) = track(&[0.05, 0.2, 0.35, 1.0, 0.15, 1.0, 0.05]);
        let s = analyse(&pcm, SR, &beats).expect("a beat to stand on");
        assert_eq!(s.bars.len() - 1, 28, "28 bars of 2 s");
        assert!((s.bars[1] - 2.0).abs() < 1e-9);
        let kinds: Vec<SectionKind> = s.sections.iter().map(|x| x.kind).collect();
        assert_eq!(kinds.first(), Some(&SectionKind::Intro), "{kinds:?}");
        assert_eq!(kinds.last(), Some(&SectionKind::Outro), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| **k == SectionKind::Peak).count(), 2, "{kinds:?}");
        assert!(kinds.contains(&SectionKind::Breakdown), "{kinds:?}");
        assert!(kinds.contains(&SectionKind::Build), "{kinds:?}");
        // Every section starts on a phrase.
        for sec in &s.sections {
            assert!((sec.start_secs / 8.0 - (sec.start_secs / 8.0).round()).abs() < 1e-9, "{sec:?}");
        }
        let peak = s.sections.iter().find(|x| x.kind == SectionKind::Peak).unwrap();
        assert!(peak.energy > 0.9 && (peak.start_secs - 24.0).abs() < 1e-9, "{peak:?}");
    }

    /// A track as loud all through, its sound turning bright halfway (a
    /// chorus with cymbals), has two sections: loudness alone would see one.
    #[test]
    fn a_change_of_sound_at_one_loudness_splits_a_section() {
        let secs = 32.0;
        let pcm: Vec<f32> = (0..(secs * SR as f64) as usize)
            .map(|i| {
                let t = i as f64 / SR as f64;
                let hz = if t < 16.0 { 110.0 } else { 2500.0 };
                (2.0 * std::f64::consts::PI * hz * t).sin() as f32 * 0.5
            })
            .collect();
        let beats = BeatData { bpm: 120.0, confidence: 0.9, beat_times: (0..64).map(|k| k as f64 * 0.5).collect() };
        let s = analyse(&pcm, SR, &beats).unwrap();
        assert_eq!(s.sections.len(), 2, "{:?}", s.sections);
        assert!((s.sections[1].start_secs - 16.0).abs() < 1e-9);
    }

    /// #1576: skipping the intro starts the fit on the bar line where the
    /// intro ends, and the result still comes near the target.
    #[test]
    fn a_fit_can_skip_the_intro() {
        let (pcm, beats) = track(&[0.05, 0.2, 0.35, 1.0, 0.15, 1.0, 0.05]);
        let s = analyse(&pcm, SR, &beats).unwrap();
        let end = intro_end(&s).expect("the quiet first phrase is an intro");
        assert!((end - 8.0).abs() < 1e-9, "{end}");
        let f = fit_from(&s, 32.0, end).expect("a fit from the intro's end");
        assert!((f.segments[0].0 - 8.0).abs() < 1e-9, "{:?}", f.segments);
        assert!((f.length_secs - 32.0).abs() <= 2.0, "{f:?}");
        assert!(f.segments.iter().all(|(a, b)| *a >= 8.0 - 1e-9 && b > a), "{:?}", f.segments);
    }

    #[test]
    fn no_beat_no_structure() {
        let (pcm, mut beats) = track(&[0.5, 0.5]);
        beats.confidence = 0.1;
        assert!(analyse(&pcm, SR, &beats).is_none());
    }

    #[test]
    fn a_track_is_shortened_by_whole_bars_at_a_like_join() {
        let (pcm, beats) = track(&[0.05, 0.4, 0.4, 1.0, 0.4, 0.4, 0.05]);
        let s = analyse(&pcm, SR, &beats).unwrap();
        // 56 s to 40 s: 8 bars out.
        let f = fit(&s, 40.0).expect("a fit");
        assert!((f.length_secs - 40.0).abs() < 1e-6, "{f:?}");
        assert_eq!(f.bars_changed, -8);
        assert_eq!(f.segments.len(), 2);
        let (cut_from, cut_to) = (f.segments[0].1, f.segments[1].0);
        assert!((cut_from / 2.0).fract().abs() < 1e-9 && (cut_to / 2.0).fract().abs() < 1e-9, "bar lines: {f:?}");
        assert!(cut_from >= 8.0 && cut_to <= 48.0, "the first and last phrase are kept: {f:?}");
        // The bars either side of the join are alike in energy.
        let bar = |t: f64| s.energy[(t / 2.0) as usize];
        assert!((bar(cut_from - 2.0) - bar(cut_to)).abs() < 0.1, "{f:?}");
        assert_eq!(f.segments[1].1, s.duration_secs, "it ends as it did");
    }

    #[test]
    fn a_track_is_lengthened_by_repeating_bars() {
        let (pcm, beats) = track(&[0.05, 0.4, 1.0, 0.4, 0.05]);
        let s = analyse(&pcm, SR, &beats).unwrap();
        let f = fit(&s, 48.0).expect("a fit");
        assert!((f.length_secs - 48.0).abs() < 1e-6, "{f:?}");
        assert_eq!(f.bars_changed, 4);
        assert!(f.segments[1].0 < f.segments[0].1, "the second stretch goes back: {f:?}");
        // A target already met changes nothing.
        let same = fit(&s, s.duration_secs).unwrap();
        assert_eq!(same.segments, vec![(0.0, s.duration_secs)]);
    }
}

/// A media file's music fitted to about `target_secs` (#2210): its beats
/// (read once and cached), its structure over the decoded sound, and
/// [`fit_from`] from `start_secs` on, or from the end of its intro with
/// `skip_intro`. The fit and where it started.
pub fn fit_media(ffmpeg: &std::path::Path, path: &std::path::Path, duration_secs: f64, target_secs: f64, start_secs: f64, skip_intro: bool) -> Result<(Fit, f64), String> {
    const SR: u32 = 22_050;
    if !target_secs.is_finite() || target_secs <= 0.0 {
        return Err("the length to fit to must be above 0 s".into());
    }
    let beats = crate::beats::read_and_cache(ffmpeg, path)?;
    if beats.confidence < 0.3 {
        return Err("the track has no clear beat to fit by bars".into());
    }
    let pcm = crate::audio_sync::decode_mono_pcm(ffmpeg, path, SR, 0.0, duration_secs.clamp(1.0, 1800.0))?;
    let shape = analyse(&pcm, SR, &beats).ok_or("the track's bars could not be read")?;
    let start = if skip_intro { intro_end(&shape).unwrap_or(start_secs).max(start_secs) } else { start_secs };
    let fitted = if start > 1e-6 { fit_from(&shape, target_secs, start) } else { fit(&shape, target_secs) };
    let fit = fitted.ok_or_else(|| format!("the track ({:.1} s) cannot be fitted to {target_secs:.1} s by whole bars: it needs a first and last phrase and room between", shape.duration_secs))?;
    Ok((fit, start))
}

