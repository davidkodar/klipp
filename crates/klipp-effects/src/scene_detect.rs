//! Shot-boundary (scene) detection via luma-histogram L1 distance.
//!
//! CPU-only. Operates on RGBA8 frames provided by the caller. For each frame
//! we build a 64-bin normalised luminance histogram (Rec.601 weights), then
//! compare each frame to the previous one using L1 distance. A cut is
//! declared when the distance exceeds `sensitivity * 0.4`, subject to a
//! minimum-gap debounce and the caller's source-range clamp.
//!
//! Returned cut times are in source-media seconds (i.e. `frame_index / fps`).

const BINS: usize = 64;

/// Detect scene cuts in a stream of RGBA8 frames.
///
/// - `fps`: source media frame rate.
/// - `sensitivity`: 0.0 (detect nothing) .. 1.0 (detect everything).
///   Threshold = `sensitivity * 0.4`.
/// - `min_gap_secs`: debounce — no two cuts closer than this.
/// - `source_in_secs` / `source_out_secs`: cuts are only emitted in
///   `[source_in, source_out)`. If the range is empty or inverted, no cuts.
/// - `frame_count`: total frame hint for progress reporting (0 = unknown,
///   progress will be reported as 0.0 until done).
/// - `frame_provider`: returns `(rgba, w, h)` per frame, `None` at EOF.
/// - `progress`: called once per frame with `0.0..=1.0`; return `false` to
///   cancel. On cancel, cuts found so far are returned.
pub fn detect_scenes(
    fps: f64,
    sensitivity: f32,
    min_gap_secs: f32,
    source_in_secs: f64,
    source_out_secs: f64,
    frame_count: usize,
    frame_provider: &mut dyn FnMut() -> Option<(Vec<u8>, u32, u32)>,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<f64> {
    let mut cuts: Vec<f64> = Vec::new();
    if fps <= 0.0 {
        return cuts;
    }
    let threshold = (sensitivity.clamp(0.0, 1.0) as f64) * 0.4;
    let min_gap = min_gap_secs.max(0.0) as f64;

    let mut prev: Option<[f32; BINS]> = None;
    let mut last_cut_secs: f64 = f64::NEG_INFINITY;
    let mut frame_index: usize = 0;

    while let Some((rgba, w, h)) = frame_provider() {
        let pixel_count = (w as usize) * (h as usize);
        if pixel_count == 0 {
            frame_index += 1;
            continue;
        }
        let hist = histogram(&rgba, pixel_count);
        let current_time_secs = frame_index as f64 / fps;

        if let Some(prev_h) = prev.as_ref() {
            let dist = l1_distance(prev_h, &hist);
            if dist > threshold
                && (current_time_secs - last_cut_secs) >= min_gap
                && current_time_secs >= source_in_secs
                && current_time_secs < source_out_secs
            {
                cuts.push(current_time_secs);
                last_cut_secs = current_time_secs;
            }
        }
        prev = Some(hist);

        if frame_count > 0 {
            let p = (frame_index + 1) as f32 / frame_count as f32;
            if !progress(p.clamp(0.0, 1.0)) {
                return cuts;
            }
        } else if !progress(0.0) {
            return cuts;
        }

        frame_index += 1;
    }
    cuts
}

fn histogram(rgba: &[u8], pixel_count: usize) -> [f32; BINS] {
    let mut counts = [0u32; BINS];
    let n = pixel_count.min(rgba.len() / 4);
    for i in 0..n {
        let r = rgba[i * 4] as f32;
        let g = rgba[i * 4 + 1] as f32;
        let b = rgba[i * 4 + 2] as f32;
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        let mut bin = (luma / 256.0 * BINS as f32) as usize;
        if bin >= BINS {
            bin = BINS - 1;
        }
        counts[bin] += 1;
    }
    let mut out = [0f32; BINS];
    let total = n.max(1) as f32;
    for i in 0..BINS {
        out[i] = counts[i] as f32 / total;
    }
    out
}

fn l1_distance(a: &[f32; BINS], b: &[f32; BINS]) -> f64 {
    let mut sum = 0f64;
    for i in 0..BINS {
        sum += (a[i] - b[i]).abs() as f64;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black_frame(w: u32, h: u32) -> (Vec<u8>, u32, u32) {
        (vec![0u8; (w * h * 4) as usize], w, h)
    }

    fn white_frame(w: u32, h: u32) -> (Vec<u8>, u32, u32) {
        (vec![255u8; (w * h * 4) as usize], w, h)
    }

    #[test]
    fn identical_frames_produce_no_cuts() {
        let mut frames: Vec<(Vec<u8>, u32, u32)> =
            (0..30).map(|_| black_frame(16, 16)).collect();
        frames.reverse();
        let mut provider = || frames.pop();
        let cuts = detect_scenes(
            24.0, 0.5, 0.0,
            0.0, 100.0,
            30,
            &mut provider,
            &|_| true,
        );
        assert!(cuts.is_empty(), "identical frames should not produce cuts, got {:?}", cuts);
    }

    #[test]
    fn hard_cut_detected() {
        let mut frames: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        for _ in 0..10 { frames.push(black_frame(16, 16)); }
        for _ in 0..10 { frames.push(white_frame(16, 16)); }
        frames.reverse();
        let mut provider = || frames.pop();
        let cuts = detect_scenes(
            24.0, 0.3, 0.0,
            0.0, 100.0,
            20,
            &mut provider,
            &|_| true,
        );
        assert_eq!(cuts.len(), 1, "expected exactly 1 cut, got {:?}", cuts);
        let expected = 10.0 / 24.0;
        assert!((cuts[0] - expected).abs() < 1e-6, "cut at {} expected ~{}", cuts[0], expected);
    }

    #[test]
    fn min_gap_debounce_filters_rapid_cuts() {
        let mut frames: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        for i in 0..30 {
            frames.push(if i % 2 == 0 { black_frame(16, 16) } else { white_frame(16, 16) });
        }
        frames.reverse();
        let mut provider = || frames.pop();
        let cuts = detect_scenes(
            24.0, 0.3, 1.0,
            0.0, 100.0,
            30,
            &mut provider,
            &|_| true,
        );
        let first_second: Vec<_> = cuts.iter().filter(|&&t| t < 1.0).collect();
        assert!(first_second.len() <= 1, "min_gap=1.0 should allow ≤1 cut in first second, got {:?}", cuts);
    }

    #[test]
    fn source_range_clamps_cuts() {
        // Inverted range → 0 cuts.
        let mut frames: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        for _ in 0..10 { frames.push(black_frame(16, 16)); }
        for _ in 0..10 { frames.push(white_frame(16, 16)); }
        frames.reverse();
        let mut provider = || frames.pop();
        let cuts = detect_scenes(
            24.0, 0.3, 0.0,
            0.5, 0.3,
            20,
            &mut provider,
            &|_| true,
        );
        assert!(cuts.is_empty(), "inverted source range should produce 0 cuts, got {:?}", cuts);

        // Range ends before cut → 0 cuts.
        let mut frames: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        for _ in 0..10 { frames.push(black_frame(16, 16)); }
        for _ in 0..10 { frames.push(white_frame(16, 16)); }
        frames.reverse();
        let mut provider = || frames.pop();
        let cuts = detect_scenes(
            24.0, 0.3, 0.0,
            0.0, 0.2,
            20,
            &mut provider,
            &|_| true,
        );
        assert!(cuts.is_empty(), "range ending before cut should produce 0 cuts, got {:?}", cuts);
    }

    #[test]
    fn cancellation_stops_early() {
        use std::cell::Cell;
        let mut frames: Vec<(Vec<u8>, u32, u32)> = (0..100)
            .map(|i| if i % 2 == 0 { black_frame(16, 16) } else { white_frame(16, 16) })
            .collect();
        frames.reverse();
        let seen = Cell::new(0usize);
        let mut provider = || {
            seen.set(seen.get() + 1);
            frames.pop()
        };
        let call_count = Cell::new(0usize);
        let progress = |_p: f32| -> bool {
            call_count.set(call_count.get() + 1);
            call_count.get() < 10
        };
        let _ = detect_scenes(
            24.0, 0.3, 0.0,
            0.0, 100.0,
            100,
            &mut provider,
            &progress,
        );
        assert!(seen.get() < 100, "should stop early, but consumer drained {} frames", seen.get());
    }
}
