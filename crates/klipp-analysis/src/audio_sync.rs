//! Audio synchronisation via cross-correlation (#61).
//!
//! Given two clips' mono `f32` samples, find the sample offset that best
//! aligns them, so a caller can shift the target clip's timeline
//! position. The samples come either from the app's conformed PCM cache
//! (via [`load_pcm_mono`]) or straight off the media through ffmpeg (via
//! [`decode_mono_pcm`]) — the UI uses the first, the agent's align_audio
//! command the second.
//!
//! The search is intentionally simple: time-domain cross-correlation
//! with a coarse/refine strategy. A coarse pass walks the full search
//! window at stride 8 to locate the rough peak; a fine pass then scans
//! ±8 samples around the coarse peak for exact alignment. Both passes
//! cap the inner correlation window at `CORR_WINDOW` samples so CPU cost
//! stays bounded even for 60 s search ranges. For the usual cases (clap
//! tracks, dual-system audio, a few seconds of offset) this runs in well
//! under a second even on multi-minute clips.

use std::path::Path;

/// Inner correlation window cap. Keeps each evaluation O(4096) regardless
/// of clip length — important for 60 s search ranges where `n_a` could be
/// ~3 M samples.
const CORR_WINDOW: i64 = 4096;

/// Load a PCM file (f32 LE interleaved stereo, 48 kHz) from disk and mix
/// it to mono by averaging the two channels. Returns a vec of f32 samples.
pub fn load_pcm_mono(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() % 8 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PCM file length is not a multiple of 8 bytes (stereo f32 frame size)",
        ));
    }
    let frame_count = bytes.len() / 8;
    let mut mono = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let o = i * 8;
        let l = f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let r = f32::from_le_bytes([bytes[o + 4], bytes[o + 5], bytes[o + 6], bytes[o + 7]]);
        mono.push(0.5 * (l + r));
    }
    Ok(mono)
}

/// Compute a confidence-weighted cross-correlation score at a single offset.
///
/// `offset` is the position of `b` relative to `a`: when `offset >= 0` we
/// compare `a[offset..]` with `b[0..]`; when negative, `a[0..]` with
/// `b[-offset..]`. Returns `f64::NEG_INFINITY` for empty inputs so it
/// always loses vs. any real correlation.
///
/// The returned value is the normalised (Pearson-style) correlation scaled
/// by `overlap / target_window`. The scale is necessary because a
/// single-sample overlap of two identically-signed samples produces a
/// normalised score of 1.0 — without the scale, any short alignment at
/// the edge of the search range could beat a real, long alignment in the
/// middle. Scaling makes "many matching samples" beat "few matching
/// samples" reliably.
pub(crate) fn correlation_at_offset(a: &[f32], b: &[f32], offset: i64, n_a: i64, n_b: i64) -> f64 {
    if n_a == 0 || n_b == 0 {
        return f64::NEG_INFINITY;
    }

    let (a_start, b_start) = if offset >= 0 {
        (offset, 0)
    } else {
        (0, -offset)
    };
    if a_start >= n_a || b_start >= n_b {
        return f64::NEG_INFINITY;
    }
    let overlap = (n_a - a_start).min(n_b - b_start);
    if overlap <= 0 {
        return f64::NEG_INFINITY;
    }
    let n = overlap.min(CORR_WINDOW) as usize;

    let a_slice = &a[a_start as usize..a_start as usize + n];
    let b_slice = &b[b_start as usize..b_start as usize + n];

    // Pearson-ish normalised cross-correlation — numerator/sqrt(energy_a*energy_b).
    // Normalising makes the peak robust to amplitude differences between the
    // reference and the target (different mic gain, compression, etc.).
    let mut num = 0.0_f64;
    let mut ea = 0.0_f64;
    let mut eb = 0.0_f64;
    for i in 0..n {
        let x = a_slice[i] as f64;
        let y = b_slice[i] as f64;
        num += x * y;
        ea += x * x;
        eb += y * y;
    }
    let denom = (ea * eb).sqrt();
    if denom <= f64::EPSILON {
        // Silent window — can't be a confident match.
        return f64::NEG_INFINITY;
    }
    let pearson = num / denom;
    // Weight by the fraction of the target window actually compared, so
    // tiny high-correlation tails can't outrank a long, equally-correlated
    // alignment. We use the full-buffer length (capped to CORR_WINDOW) as
    // the denominator so identical-length inputs never hit the cap.
    let max_possible = n_a.min(n_b).clamp(1, CORR_WINDOW) as f64;
    let weight = (n as f64 / max_possible).min(1.0);
    pearson * weight
}

/// Find the sample offset that aligns `clip_pcm` to `ref_pcm`.
///
/// Returns a signed offset in samples at `sample_rate`. **Positive** means
/// the target clip starts _later_ than the reference (its audio content
/// begins further into the buffer); **negative** means earlier. Searches
/// ±`max_offset_secs * sample_rate` samples.
///
/// Strategy: coarse scan at stride 8 locates the rough peak, then a ±8
/// sample fine scan around it pins the exact offset. This cuts the
/// number of correlation evaluations by ~8× without losing sample
/// accuracy.
///
/// Internally we evaluate `correlation_at_offset(a, b, off)` where off
/// is the position of `b` relative to `a`. "Clip starts later" (positive
/// return value) corresponds to matching `a[0..]` against `b[k..]`, i.e.
/// the internal offset is `-k`. We flip the sign at return.
pub fn find_sync_offset(
    ref_pcm: &[f32],
    clip_pcm: &[f32],
    sample_rate: u32,
    max_offset_secs: f32,
) -> i64 {
    find_sync_offset_scored(ref_pcm, clip_pcm, sample_rate, max_offset_secs).0
}

/// Like [`find_sync_offset`], and also returns the confidence at the peak:
/// the normalised cross-correlation there, roughly 0 (no match) to 1
/// (identical). Below about 0.3 the two recordings likely share no common
/// sound and the offset should not be trusted.
pub fn find_sync_offset_scored(
    ref_pcm: &[f32],
    clip_pcm: &[f32],
    sample_rate: u32,
    max_offset_secs: f32,
) -> (i64, f64) {
    let n_a = ref_pcm.len() as i64;
    let n_b = clip_pcm.len() as i64;
    if n_a == 0 || n_b == 0 {
        return (0, 0.0);
    }
    let max_off = (max_offset_secs as f64 * sample_rate as f64) as i64;
    // Clamp search range to what the buffers can actually support.
    let lo = (-max_off).max(-(n_b - 1));
    let hi = max_off.min(n_a - 1);

    // Coarse pass.
    let mut best_coarse_off = 0_i64;
    let mut best_coarse_score = f64::NEG_INFINITY;
    let mut off = lo;
    while off <= hi {
        let s = correlation_at_offset(ref_pcm, clip_pcm, off, n_a, n_b);
        if s > best_coarse_score {
            best_coarse_score = s;
            best_coarse_off = off;
        }
        off += 8;
    }

    // Also evaluate offset 0 explicitly — with stride 8 starting at `lo`
    // the coarse grid frequently misses it entirely for short buffers,
    // which is precisely where zero offset is the correct answer.
    {
        let s = correlation_at_offset(ref_pcm, clip_pcm, 0, n_a, n_b);
        if s > best_coarse_score {
            best_coarse_score = s;
            best_coarse_off = 0;
        }
    }

    // Fine pass: ±8 around the coarse winner.
    let fine_lo = (best_coarse_off - 8).max(lo);
    let fine_hi = (best_coarse_off + 8).min(hi);
    let mut best_off = best_coarse_off;
    let mut best_score = best_coarse_score;
    let mut off = fine_lo;
    while off <= fine_hi {
        let s = correlation_at_offset(ref_pcm, clip_pcm, off, n_a, n_b);
        if s > best_score {
            best_score = s;
            best_off = off;
        }
        off += 1;
    }

    // Flip to the public convention: positive = clip starts later than ref.
    // The score is weighted (see correlation_at_offset); divide the weight
    // back out for a plain 0..1 confidence, since for equal-length windows
    // the weight is 1 anyway.
    let confidence = if best_score.is_finite() { best_score.clamp(0.0, 1.0) } else { 0.0 };
    (-best_off, confidence)
}

/// Decode a media file's sound to mono `f32` samples at `sample_rate`,
/// starting `from_secs` in and taking at most `max_secs`, via ffmpeg. The
/// buffers this makes feed [`find_sync_offset_scored`] straight off the
/// media, without the app's PCM cache.
pub fn decode_mono_pcm(ffmpeg: &std::path::Path, path: &std::path::Path, sample_rate: u32, from_secs: f64, max_secs: f64) -> Result<Vec<f32>, String> {
    let mut args: Vec<String> = Vec::new();
    if from_secs > 0.0 {
        args.push("-ss".into());
        args.push(format!("{from_secs}"));
    }
    // A stream read off the network stalls rather than ends when the
    // server stops answering, and ffmpeg's HTTP input has no read
    // timeout of its own: fifteen seconds without a byte gives up (#767
    // review).
    let input = path.to_string_lossy().into_owned();
    if input.starts_with("http://") || input.starts_with("https://") {
        // A preview Klipp fetches on purpose: the network protocols it
        // needs and no others.
        args.extend(["-rw_timeout".into(), "15000000".into(), "-protocol_whitelist".into(), "https,http,tls,tcp".into(), "-i".into(), input]);
    } else {
        args.extend(klipp_media::input_args_lossy(path)?);
    }
    if max_secs > 0.0 {
        args.push("-t".into());
        args.push(format!("{max_secs}"));
    }
    args.extend(["-vn", "-sn", "-ac", "1"].iter().map(|s| s.to_string()));
    args.push("-ar".into());
    args.push(format!("{sample_rate}"));
    args.extend(["-f", "f32le", "-"].iter().map(|s| s.to_string()));
    let out = std::process::Command::new(ffmpeg)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !out.status.success() {
        return Err(format!("ffmpeg failed to decode {} ({})", path.display(), out.status));
    }
    let bytes = out.stdout;
    if bytes.len() < 4 {
        return Err(format!("{} produced no audio", path.display()));
    }
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

/// Where `clip` sits against `reference` when the two recordings may be
/// minutes apart and overlap only in part — two cameras on one event,
/// started at different times. `Some((offset_secs, confidence))`, where a
/// moment at `t` seconds into `reference` is at `t + offset_secs` into
/// `clip`, the convention [`find_sync_offset`] uses. `None` when they
/// share less than [`MIN_OVERLAP_SECS`] of sound.
///
/// [`find_sync_offset`] compares a few thousand samples at each lag,
/// which is right for a clap a second or two away and too slow and too
/// short-sighted for an offset of twenty minutes. This correlates the two
/// loudness envelopes (100 per second) over their whole overlap with an
/// FFT, which finds the lag to within 10 ms across any distance, then
/// refines it to the sample on the waveforms over two seconds of the
/// overlap. Confidence is the normalised correlation of the envelopes at
/// that lag: roughly 0 for unrelated sound, near 1 for the same room.
pub fn find_long_offset(reference: &[f32], clip: &[f32], sample_rate: u32) -> Option<(f64, f64)> {
    let coarse = coarse_offset(&SoundPrint::of(reference, sample_rate), &SoundPrint::of(clip, sample_rate))?;
    let sr = f64::from(sample_rate);
    let (ref_from, clip_from, len) = coarse.refine_windows();
    let slice = |pcm: &'_ [f32], from: f64, secs: f64| {
        let a = ((from * sr).round().max(0.0) as usize).min(pcm.len());
        let b = (a + (secs * sr).round() as usize).min(pcm.len());
        pcm[a..b].to_vec()
    };
    let r = slice(reference, ref_from, len);
    let c = slice(clip, clip_from, len + 2.0 * REFINE_SLACK_SECS);
    Some((refine_offset(&r, &c, sample_rate, &coarse), coarse.confidence))
}

/// A recording's level envelope, 100 frames a second: all
/// the coarse search needs of a whole file. An hour is about 1.4 MB,
/// where the sound it came from is over a hundred MB at 8 kHz, so a
/// dozen angles can be compared without holding a dozen files' sound.
#[derive(Debug, Clone)]
pub struct SoundPrint {
    env: Vec<f32>,
}

impl SoundPrint {
    pub fn of(pcm: &[f32], sample_rate: u32) -> Self {
        let hop = (sample_rate / ENVELOPE_RATE).max(1) as usize;
        Self { env: onset_envelope(pcm, hop) }
    }

    pub fn secs(&self) -> f64 {
        self.env.len() as f64 / f64::from(ENVELOPE_RATE)
    }
}

/// Where a clip sits against a reference, to within an envelope frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoarseMatch {
    /// A moment `t` into the reference is `t + offset_secs` into the clip.
    pub offset_secs: f64,
    /// 0 (chance) to 1 (unmistakable). See [`find_long_offset`].
    pub confidence: f64,
    /// Where in the reference to refine: the start of a window in the
    /// middle of the overlap.
    pub probe_at_secs: f64,
}

impl CoarseMatch {
    /// The stretches to decode for [`refine_offset`]: the reference from
    /// the first value, the clip from the second, for the third's length
    /// (the clip's plus twice [`REFINE_SLACK_SECS`]).
    pub fn refine_windows(&self) -> (f64, f64, f64) {
        (self.probe_at_secs, self.probe_at_secs + self.offset_secs - REFINE_SLACK_SECS, REFINE_WINDOW_SECS)
    }
}

/// The waveform compared to refine a coarse match.
pub const REFINE_WINDOW_SECS: f64 = 2.0;
/// How far either side of the coarse match the refinement looks: two
/// envelope frames.
pub const REFINE_SLACK_SECS: f64 = 2.0 / ENVELOPE_RATE as f64;

/// The waveform correlation a refinement must reach to be used over the
/// coarse answer. The same sound through two microphones scores well
/// above it; unrelated stretches of sound, far below.
const REFINE_MIN_CORRELATION: f64 = 0.3;

/// The coarse half of [`find_long_offset`], on prints.
pub fn coarse_offset(reference: &SoundPrint, clip: &SoundPrint) -> Option<CoarseMatch> {
    let min_overlap = (MIN_OVERLAP_SECS * f64::from(ENVELOPE_RATE)) as usize;
    if reference.env.len() < min_overlap || clip.env.len() < min_overlap {
        return None;
    }
    let (lag, confidence) = best_envelope_lag(&reference.env, &clip.env, min_overlap)?;
    let offset_secs = lag as f64 / f64::from(ENVELOPE_RATE);
    // The overlap in reference time, and a window in its middle that
    // leaves the slack inside both recordings.
    let from = (-offset_secs).max(0.0) + REFINE_SLACK_SECS;
    let to = reference.secs().min(clip.secs() - offset_secs) - REFINE_SLACK_SECS;
    let probe_at_secs = (from + (to - from - REFINE_WINDOW_SECS) / 2.0).max(from);
    Some(CoarseMatch { offset_secs, confidence, probe_at_secs })
}

/// Refine `coarse` to the sample: `reference` and `clip` are the windows
/// [`CoarseMatch::refine_windows`] names. Falls back to the coarse offset
/// where there is nothing to compare — digital silence would otherwise
/// pull the answer to the edge of the search.
pub fn refine_offset(reference: &[f32], clip: &[f32], sample_rate: u32, coarse: &CoarseMatch) -> f64 {
    let sr = f64::from(sample_rate);
    let slack = (REFINE_SLACK_SECS * sr).round() as usize;
    let w = reference.len().min(clip.len().saturating_sub(2 * slack));
    let energy = |x: &[f32]| x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>();
    if w < 16 || energy(&reference[..w]) <= 1e-9 {
        return coarse.offset_secs;
    }
    let seg = &reference[..w];
    let mut best: Option<(usize, f64)> = None;
    for k in 0..=2 * slack {
        let other = &clip[k..k + w];
        let eb = energy(other);
        if eb <= 1e-9 {
            continue;
        }
        let num: f64 = seg.iter().zip(other).map(|(&x, &y)| f64::from(x) * f64::from(y)).sum();
        let score = num / (energy(seg) * eb).sqrt();
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((k, score));
        }
    }
    match best {
        // At the edge of the search the true peak may lie beyond it — a
        // seek that landed off by more than the slack (VBR MP3, some
        // containers) — and the edge is worse than the coarse answer.
        // A weak peak is noise lining up, which is what a missed window
        // gives too.
        Some((k, score)) if k > 0 && k < 2 * slack && score >= REFINE_MIN_CORRELATION => {
            coarse.offset_secs - REFINE_SLACK_SECS + k as f64 / sr
        }
        _ => coarse.offset_secs,
    }
}

/// Envelope frames per second for [`find_long_offset`].
const ENVELOPE_RATE: u32 = 100;

/// The least shared sound [`find_long_offset`] will call a match.
pub const MIN_OVERLAP_SECS: f64 = 3.0;

/// Level per hop in log terms, less its average over the surrounding
/// second: how the sound moves (speech, a door, a clap) rather than how
/// loud it is, which is what two microphones at different distances and
/// gains disagree on. The log turns a gain difference into a constant,
/// and taking the local average away removes it.
fn onset_envelope(pcm: &[f32], hop: usize) -> Vec<f32> {
    let log: Vec<f32> = pcm
        .chunks(hop)
        .map(|c| ((c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt() + 1e-4).ln())
        .collect();
    let half = ENVELOPE_RATE as usize / 2;
    let mut prefix = Vec::with_capacity(log.len() + 1);
    prefix.push(0.0f64);
    for &v in &log {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + f64::from(v));
    }
    (0..log.len())
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(log.len()));
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (f64::from(log[i]) - mean) as f32
        })
        .collect()
}

/// Significance (correlation × √overlap) at or below which a peak is
/// what chance produces over tens of thousands of lags of a smooth
/// envelope, and at or above which it is unmistakable. Confidence runs
/// from 0 to 1 between them, so it measures the same thing the lag was
/// chosen by.
const CHANCE_Z: f64 = 5.0;
const SURE_Z: f64 = 12.0;

/// The lag `k` at which `a[i]` and `b[i + k]` correlate most
/// significantly over at least `min_overlap` frames, and a confidence.
///
/// Significance, not the raw correlation: a few seconds at the edge of
/// the search can line up by chance as well as minutes in the middle
/// line up for real, and correlation × √overlap (a z-score for
/// independent frames) tells them apart. The confidence is the
/// peak's significance mapped onto 0..1 between chance and certainty.
fn best_envelope_lag(a: &[f32], b: &[f32], min_overlap: usize) -> Option<(i64, f64)> {
    use realfft::RealFftPlanner;
    let n = (a.len() + b.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let spectrum = |x: &[f32]| {
        let mut buf = vec![0.0f64; n];
        let mean = x.iter().map(|&v| f64::from(v)).sum::<f64>() / x.len() as f64;
        for (d, &v) in buf.iter_mut().zip(x) {
            *d = f64::from(v) - mean;
        }
        let mut out = fwd.make_output_vec();
        fwd.process(&mut buf, &mut out).ok()?;
        Some((out, mean))
    };
    let (fa, mean_a) = spectrum(a)?;
    let (fb, mean_b) = spectrum(b)?;
    let mut prod: Vec<_> = fa.iter().zip(&fb).map(|(x, y)| x.conj() * y).collect();
    let mut corr = inv.make_output_vec();
    inv.process(&mut prod, &mut corr).ok()?;

    // Energy over each overlap, for normalising, from prefix sums of
    // squares of the same mean-removed signals.
    let prefix = |x: &[f32], mean: f64| {
        let mut p = Vec::with_capacity(x.len() + 1);
        p.push(0.0f64);
        for &v in x {
            let d = f64::from(v) - mean;
            p.push(p.last().copied().unwrap_or(0.0) + d * d);
        }
        p
    };
    let (pa, pb) = (prefix(a, mean_a), prefix(b, mean_b));
    // Neighbouring envelope frames are far from independent (a level
    // holds for a while, and the one-second average is shared), so a
    // count of frames overstates the evidence. Bartlett's correction
    // for two autocorrelated series gives the independent count.
    let lag1 = |x: &[f32], mean: f64| {
        let d: Vec<f64> = x.iter().map(|&v| f64::from(v) - mean).collect();
        let var: f64 = d.iter().map(|v| v * v).sum();
        if var <= f64::EPSILON {
            return 0.0;
        }
        (d.windows(2).map(|w| w[0] * w[1]).sum::<f64>() / var).clamp(0.0, 0.99)
    };
    let rho = lag1(a, mean_a) * lag1(b, mean_b);
    let independent = (1.0 - rho) / (1.0 + rho);
    let (na, nb) = (a.len() as i64, b.len() as i64);
    let mut best: Option<(i64, f64, f64)> = None;
    for k in -(na - 1)..nb {
        // a[i] meets b[i + k] for i in max(0, -k) .. min(na, nb - k).
        let i0 = (-k).max(0);
        let i1 = na.min(nb - k);
        if i1 - i0 < min_overlap as i64 {
            continue;
        }
        let ea = pa[i1 as usize] - pa[i0 as usize];
        let eb = pb[(i1 + k) as usize] - pb[(i0 + k) as usize];
        let denom = (ea * eb).sqrt();
        if denom <= f64::EPSILON {
            continue;
        }
        let raw = corr[k.rem_euclid(n as i64) as usize] / n as f64;
        let r = raw / denom;
        let z = r * ((i1 - i0) as f64 * independent).sqrt();
        if best.is_none_or(|(_, bz, _)| z > bz) {
            best = Some((k, z, r));
        }
    }
    best.map(|(k, z, _)| (k, ((z - CHANCE_Z) / (SURE_Z - CHANCE_Z)).clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_perfect_match_at_zero() {
        let signal = vec![1.0f32, -1.0, 0.5, -0.5, 0.8, -0.8, 0.3, -0.3];
        let offset = find_sync_offset(&signal, &signal, 48000, 1.0);
        assert_eq!(offset, 0);
    }

    #[test]
    fn correlation_detects_positive_offset() {
        // `clip_pcm` contains the same pattern as `ref_pcm` but shifted 3
        // samples later. The returned offset should place the patterns on
        // top of each other; with our convention (positive = clip later)
        // that is +3.
        let ref_pcm = vec![1.0f32, -1.0, 0.5, -0.5, 0.8, -0.8, 0.3, -0.3, 0.0, 0.0, 0.0, 0.0];
        let clip_pcm = vec![0.0f32, 0.0, 0.0, 1.0, -1.0, 0.5, -0.5, 0.8, -0.8, 0.3, -0.3, 0.0];
        let offset = find_sync_offset(&ref_pcm, &clip_pcm, 48000, 1.0);
        assert!((offset - 3).abs() <= 1, "Expected ~3, got {}", offset);
    }

    #[test]
    fn correlation_at_offset_empty_returns_neg_inf() {
        let a: Vec<f32> = Vec::new();
        let b: Vec<f32> = Vec::new();
        assert_eq!(correlation_at_offset(&a, &b, 0, 0, 0), f64::NEG_INFINITY);
    }




    /// Deterministic noise with a changing level, like a room with
    /// people in it: bursts of sound at irregular intervals.
    fn room(len: usize, seed: u64) -> Vec<f32> {
        let mut x = seed;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 10_000) as f32 / 10_000.0
        };
        let mut level = 0.1f32;
        (0..len)
            .map(|i| {
                if i % 800 == 0 && rnd() < 0.3 {
                    level = 0.02 + rnd() * 0.9;
                }
                (rnd() * 2.0 - 1.0) * level
            })
            .collect()
    }

    #[test]
    fn two_cameras_minutes_apart_are_placed_to_the_sample() {
        const SR: u32 = 8_000;
        let event = room(SR as usize * 400, 7);
        // Camera A rolled from 0 s to 240 s; camera B from 185.25 s to
        // 400 s, with its own hiss and at a lower level.
        let a = event[..SR as usize * 240].to_vec();
        let start_b = SR as usize * 185 + SR as usize / 4;
        let hiss = room(event.len() - start_b, 99);
        let b: Vec<f32> = event[start_b..].iter().zip(&hiss).map(|(s, h)| s * 0.4 + h * 0.02).collect();
        let (offset, confidence) = find_long_offset(&a, &b, SR).expect("they overlap by 55 s");
        // A moment t into A is at t - 185.25 into B.
        assert!((offset + 185.25).abs() < 1.0 / SR as f64 * 2.0, "offset {offset}");
        assert!(confidence > 0.5, "confidence {confidence}");
    }

    #[test]
    fn unrelated_recordings_score_low() {
        const SR: u32 = 8_000;
        let a = room(SR as usize * 60, 1);
        let b = room(SR as usize * 60, 2);
        let (_, confidence) = find_long_offset(&a, &b, SR).unwrap();
        assert!(confidence < 0.3, "confidence {confidence}");
    }

    #[test]
    fn a_short_take_inside_a_long_one_and_a_brief_overlap_both_sync() {
        const SR: u32 = 8_000;
        let sec = SR as usize;
        let event = room(sec * 300, 11);
        // A handheld take from 97.5 s to 130 s, inside the wide shot.
        let wide = event.clone();
        let take: Vec<f32> = event[sec * 97 + sec / 2..sec * 130].iter().map(|s| s * 2.5).collect();
        let (offset, confidence) = find_long_offset(&wide, &take, SR).unwrap();
        assert!((offset + 97.5).abs() < 0.001, "offset {offset}");
        assert!(confidence > 0.5, "confidence {confidence}");
        // Two cameras that share only the last and first 12 s.
        let a = event[..sec * 150].to_vec();
        let b = event[sec * 138..].to_vec();
        let (offset, confidence) = find_long_offset(&a, &b, SR).unwrap();
        assert!((offset + 138.0).abs() < 0.001, "offset {offset}");
        assert!(confidence > 0.3, "confidence {confidence}");
    }

    /// A crude second microphone: the room further away (duller, with an
    /// early reflection) under its own hiss.
    fn other_mic(room_sound: &[f32], sr: usize, hiss_seed: u64) -> Vec<f32> {
        let hiss = room(room_sound.len(), hiss_seed);
        let echo = sr * 23 / 1000;
        let mut dull = 0.0f32;
        room_sound
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                dull = dull * 0.3 + s * 0.7;
                let reflection = if i >= echo { room_sound[i - echo] * 0.25 } else { 0.0 };
                dull * 0.8 + reflection + hiss[i] * 0.15
            })
            .collect()
    }

    #[test]
    fn two_different_microphones_in_one_room_sync() {
        const SR: u32 = 8_000;
        let sec = SR as usize;
        let event = room(sec * 240, 23);
        let a = event[..sec * 150].to_vec();
        let b = other_mic(&event[sec * 61..], sec, 77);
        let (offset, confidence) = find_long_offset(&a, &b, SR).expect("89 s of overlap");
        // Within 5 ms, which is well inside a frame.
        assert!((offset + 61.0).abs() < 0.005, "offset {offset}");
        assert!(confidence > 0.5, "confidence {confidence}");
        let unrelated = other_mic(&room(sec * 120, 5), sec, 6);
        let (_, low) = find_long_offset(&a, &unrelated, SR).unwrap();
        assert!(low < 0.3, "unrelated rooms scored {low}");
    }

    #[test]
    fn refining_over_digital_silence_keeps_the_coarse_answer() {
        let coarse = CoarseMatch { offset_secs: -12.34, confidence: 1.0, probe_at_secs: 20.0 };
        assert_eq!(refine_offset(&[0.0; 16_000], &[0.0; 16_400], 8_000, &coarse), -12.34);
    }

    #[test]
    fn a_refinement_pinned_to_the_edge_keeps_the_coarse_answer() {
        const SR: u32 = 8_000;
        let sound = room(SR as usize * 4, 3);
        let coarse = CoarseMatch { offset_secs: 5.0, confidence: 1.0, probe_at_secs: 0.0 };
        // The clip window starts 0.1 s late, far past the 20 ms slack.
        let slack = (REFINE_SLACK_SECS * f64::from(SR)) as usize;
        let reference = sound[..SR as usize * 2].to_vec();
        let late = sound[SR as usize / 10..SR as usize / 10 + SR as usize * 2 + 2 * slack].to_vec();
        assert_eq!(refine_offset(&reference, &late, SR, &coarse), 5.0);
    }
}
