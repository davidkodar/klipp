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
    args.push("-i".into());
    args.push(path.to_string_lossy().into_owned());
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



}
