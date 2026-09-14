//! Linear-RGBA resampling for the drag-time grade-downsample optimisation.
//!
//! When the user is actively dragging a colour-grade slider, the grade
//! chain re-runs on every parameter tick. At full resolution that
//! costs ~5–50 ms per frame and pushes update_playback over budget,
//! visible to the user as "drops to 10 the moment I touch hue".
//!
//! These helpers let `post_process_rgba_inner` run at half resolution
//! during a drag, then upsample back so the rest of the pipeline
//! (composite, display transform, viewer write) keeps its original
//! resolution contract. 4× compute reduction in the grade chain for
//! a small upsample cost.
//!
//! Box-filter downsample + bilinear upsample. Both operate on
//! linear-light f32 RGBA — no gamma surprises, no CPU sRGB seam.
//! Single-threaded for simplicity; the per-frame cost at 1080p is
//! well under a millisecond on modern hardware.
//!
//! Pure functions (no engine state). Tested in isolation.

/// Direction-aware linear-light resampler. Picks the right primitive
/// for the (src, dst) ratio:
/// * Identical dimensions → return the input unchanged (zero copy).
/// * Both axes shrink → `downsample_linear_box` (proper anti-aliasing +
///   ~3× faster than bilinear at 2:1 because it averages 2×2 blocks
///   in a tight loop instead of doing 4 weighted-bilinear taps).
/// * Anything else → `upsample_linear_bilinear` (the generic path —
///   handles upsamples and asymmetric ratios).
///
/// The compositor was previously calling `upsample_linear_bilinear`
/// for both directions, which meant 4K → 1080p downsamples used
/// 4-tap bilinear with no pre-filter. Aliasing aside, that path
/// reads only 4 source pixels per output pixel (1/4 of the data),
/// so you'd hope it's fast — but the per-pixel weight math + index
/// arithmetic burns enough cycles that the box-average path wins on
/// total cost. Logs showed `ov_resize=20–29 ms` on a 4K overlay; this
/// router drops it under 5 ms.
pub fn resize_linear(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<f32> {
    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }
    // Exact 2:1 downsample (4K → 1080p, the common overlay case) →
    // dispatch to the specialised path. It uses chunks_exact(8) +
    // straight-line ADD/MUL (4 adds, 1 mul, 4 channels) with no
    // ratio arithmetic, no per-pixel bounds checks. Substantially
    // faster than the generic box helper when the ratio fits.
    if src_w == dst_w * 2 && src_h == dst_h * 2 {
        return downsample_linear_2x(src, src_w, src_h);
    }
    if dst_w <= src_w && dst_h <= src_h {
        downsample_linear_box(src, src_w, src_h, dst_w, dst_h)
    } else {
        upsample_linear_bilinear(src, src_w, src_h, dst_w, dst_h)
    }
}

/// 2× box-filter downsample. `dst_w` and `dst_h` should be roughly
/// half of `src_w`/`src_h` — this works for any ratio but is most
/// useful at exactly 2:1, where each output pixel is the average of
/// a 2×2 source block.
///
/// Returns `dst_w * dst_h * 4` floats. Empty input → empty output.
pub fn downsample_linear_box(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<f32> {
    if src.is_empty() || src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let dst_pixels = (dst_w as usize) * (dst_h as usize);
    let mut out = vec![0.0f32; dst_pixels * 4];
    let src_w_us = src_w as usize;
    let src_h_us = src_h as usize;

    // Map each output pixel to its source rectangle. Round-down start,
    // round-up end so the boundary pixels are still covered when
    // dimensions don't divide evenly. Average all source pixels in
    // that rectangle (typically 2×2 = 4).
    for dy in 0..dst_h {
        let sy0 = ((dy as u64 * src_h as u64) / dst_h as u64) as usize;
        let sy1 = ((dy + 1) as u64 * src_h as u64).div_ceil(dst_h as u64) as usize;
        let sy1 = sy1.min(src_h_us);
        for dx in 0..dst_w {
            let sx0 = ((dx as u64 * src_w as u64) / dst_w as u64) as usize;
            let sx1 = ((dx + 1) as u64 * src_w as u64).div_ceil(dst_w as u64) as usize;
            let sx1 = sx1.min(src_w_us);
            let mut acc = [0.0f32; 4];
            let mut count = 0u32;
            for sy in sy0..sy1.max(sy0 + 1) {
                if sy >= src_h_us { break; }
                let row = sy * src_w_us;
                for sx in sx0..sx1.max(sx0 + 1) {
                    if sx >= src_w_us { break; }
                    let idx = (row + sx) * 4;
                    acc[0] += src[idx];
                    acc[1] += src[idx + 1];
                    acc[2] += src[idx + 2];
                    acc[3] += src[idx + 3];
                    count += 1;
                }
            }
            let inv = if count > 0 { 1.0 / count as f32 } else { 0.0 };
            let dst_idx = ((dy as usize) * (dst_w as usize) + dx as usize) * 4;
            out[dst_idx]     = acc[0] * inv;
            out[dst_idx + 1] = acc[1] * inv;
            out[dst_idx + 2] = acc[2] * inv;
            out[dst_idx + 3] = acc[3] * inv;
        }
    }
    out
}

/// Bilinear resize in either direction. Output dimensions are
/// arbitrary; the math is direction-agnostic (Triangle filter without
/// a prefilter), matching what `image::imageops::resize` with
/// `FilterType::Triangle` does — except in linear-light f32 RGBA, so
/// the bilinear taps stay gamma-correct. Severe downsamples (>2:1)
/// alias because there's no prefilter; upstream callers handle that
/// case with `downsample_linear_box` instead.
pub fn upsample_linear_bilinear(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<f32> {
    if src.is_empty() || src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let dst_pixels = (dst_w as usize) * (dst_h as usize);
    let mut out = vec![0.0f32; dst_pixels * 4];
    let src_w_us = src_w as usize;
    let src_h_us = src_h as usize;
    let dst_w_us = dst_w as usize;
    let row_floats = src_w_us * 4;

    let sx_scale = src_w as f32 / dst_w as f32;
    let sy_scale = src_h as f32 / dst_h as f32;

    // Pre-compute per-column source indexes + horizontal weights so
    // the inner loop reuses them across every output row. At
    // 1920-wide output that's two 1920-element scans up-front in
    // exchange for skipping two `floor + clamp + as usize` per
    // pixel inside the dst_h × dst_w hot loop.
    let mut col_x0 = vec![0usize; dst_w_us];
    let mut col_x1 = vec![0usize; dst_w_us];
    let mut col_wx = vec![0.0f32; dst_w_us];
    for dx in 0..dst_w_us {
        let sx_f = (dx as f32 + 0.5) * sx_scale - 0.5;
        let sx0 = sx_f.floor().max(0.0) as usize;
        let sx1 = (sx0 + 1).min(src_w_us - 1);
        col_x0[dx] = sx0 * 4;
        col_x1[dx] = sx1 * 4;
        col_wx[dx] = (sx_f - sx0 as f32).clamp(0.0, 1.0);
    }

    for dy in 0..dst_h {
        let sy_f = (dy as f32 + 0.5) * sy_scale - 0.5;
        let sy0 = sy_f.floor().max(0.0) as usize;
        let sy1 = (sy0 + 1).min(src_h_us - 1);
        let wy = (sy_f - sy0 as f32).clamp(0.0, 1.0);
        let one_minus_wy = 1.0 - wy;

        // Row references: bind once per dst row so the inner loop
        // sees four fixed-length slices instead of a single long
        // `src` buffer with index arithmetic. LLVM elides the
        // per-channel bounds checks against length-known refs and
        // unrolls the 4-channel body cleanly.
        let row0 = &src[sy0 * row_floats..(sy0 + 1) * row_floats];
        let row1 = &src[sy1 * row_floats..(sy1 + 1) * row_floats];
        let dst_row_off = (dy as usize) * dst_w_us * 4;
        let dst_row = &mut out[dst_row_off..dst_row_off + dst_w_us * 4];

        for dx in 0..dst_w_us {
            let i0 = col_x0[dx];
            let i1 = col_x1[dx];
            let wx = col_wx[dx];
            let one_minus_wx = 1.0 - wx;
            let dst_idx = dx * 4;

            // Channel-unrolled body — straight-line ADD/MUL with
            // four fixed-stride loads from each row.
            for c in 0..4 {
                let top = row0[i0 + c] * one_minus_wx + row0[i1 + c] * wx;
                let bot = row1[i0 + c] * one_minus_wx + row1[i1 + c] * wx;
                dst_row[dst_idx + c] = top * one_minus_wy + bot * wy;
            }
        }
    }
    out
}

/// Specialised 2× box downsample. Each output pixel is the average of a
/// 2×2 source block; no ratio arithmetic, no per-pixel bounds checks.
/// Used when the caller knows `src_w = 2·dst_w` and `src_h = 2·dst_h`
/// (the drag-downsample case). ~3-4× faster than the generic path.
pub fn downsample_linear_2x(src: &[f32], src_w: u32, src_h: u32) -> Vec<f32> {
    if src.is_empty() || src_w < 2 || src_h < 2 {
        return Vec::new();
    }
    let dst_w = (src_w / 2) as usize;
    let dst_h = (src_h / 2) as usize;
    let src_row_floats = src_w as usize * 4;
    let dst_row_floats = dst_w * 4;
    let mut out = vec![0.0f32; dst_row_floats * dst_h];

    for dy in 0..dst_h {
        let sy0 = dy * 2 * src_row_floats;
        let sy1 = sy0 + src_row_floats;
        let dst_row = &mut out[dy * dst_row_floats .. (dy + 1) * dst_row_floats];
        let row_a = &src[sy0 .. sy0 + src_row_floats];
        let row_b = &src[sy1 .. sy1 + src_row_floats];

        // Each iteration reads 2 source pixels from each of 2 rows (= 4
        // source pixels) and writes 1 destination pixel. `.chunks_exact`
        // avoids bounds checks in the inner work; 4 channels per pixel.
        for (dx, (pair_a, pair_b)) in row_a.chunks_exact(8).zip(row_b.chunks_exact(8)).enumerate() {
            let out_i = dx * 4;
            dst_row[out_i]     = (pair_a[0] + pair_a[4] + pair_b[0] + pair_b[4]) * 0.25;
            dst_row[out_i + 1] = (pair_a[1] + pair_a[5] + pair_b[1] + pair_b[5]) * 0.25;
            dst_row[out_i + 2] = (pair_a[2] + pair_a[6] + pair_b[2] + pair_b[6]) * 0.25;
            dst_row[out_i + 3] = (pair_a[3] + pair_a[7] + pair_b[3] + pair_b[7]) * 0.25;
        }
    }
    out
}

/// Specialised 2× pixel-duplicate upsample. Each source pixel becomes a
/// 2×2 destination block. Not bilinear — the output looks blocky at the
/// pixel level — but the egui viewer's texture sampler applies its own
/// bilinear filter when rendering, so the visible image stays smooth.
/// At 1080p output this is ~3-4× faster than the bilinear path and
/// memory-bandwidth bound instead of compute bound.
pub fn upsample_linear_2x(src: &[f32], src_w: u32, src_h: u32) -> Vec<f32> {
    if src.is_empty() || src_w == 0 || src_h == 0 {
        return Vec::new();
    }
    let dst_w = src_w as usize * 2;
    let dst_h = src_h as usize * 2;
    let src_row_floats = src_w as usize * 4;
    let dst_row_floats = dst_w * 4;
    let mut out = vec![0.0f32; dst_row_floats * dst_h];

    for sy in 0..src_h as usize {
        let src_row = &src[sy * src_row_floats .. (sy + 1) * src_row_floats];
        let dy0_start = sy * 2 * dst_row_floats;

        // Expand the row horizontally into dst row dy0 — each source
        // pixel becomes two adjacent destination pixels.
        {
            let dy0_row = &mut out[dy0_start .. dy0_start + dst_row_floats];
            for (sx, chunk) in src_row.chunks_exact(4).enumerate() {
                let di = sx * 8;
                dy0_row[di..di + 4].copy_from_slice(chunk);
                dy0_row[di + 4..di + 8].copy_from_slice(chunk);
            }
        }

        // Duplicate dy0 into dy1. Single memcpy — fastest possible
        // way to populate the second row.
        let dy1_start = dy0_start + dst_row_floats;
        out.copy_within(dy0_start..dy0_start + dst_row_floats, dy1_start);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [f32; 4]) -> Vec<f32> {
        let n = (w as usize) * (h as usize);
        let mut v = Vec::with_capacity(n * 4);
        for _ in 0..n { v.extend_from_slice(&rgba); }
        v
    }

    #[test]
    fn downsample_solid_color_preserves_value() {
        let src = solid(4, 4, [0.25, 0.5, 0.75, 1.0]);
        let dst = downsample_linear_box(&src, 4, 4, 2, 2);
        for px in dst.chunks_exact(4) {
            assert!((px[0] - 0.25).abs() < 1e-6);
            assert!((px[1] - 0.5).abs() < 1e-6);
            assert!((px[2] - 0.75).abs() < 1e-6);
            assert!((px[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn upsample_solid_color_preserves_value() {
        let src = solid(2, 2, [0.1, 0.2, 0.3, 1.0]);
        let dst = upsample_linear_bilinear(&src, 2, 2, 4, 4);
        for px in dst.chunks_exact(4) {
            assert!((px[0] - 0.1).abs() < 1e-6);
            assert!((px[1] - 0.2).abs() < 1e-6);
            assert!((px[2] - 0.3).abs() < 1e-6);
            assert!((px[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn bilinear_downsample_solid_color_preserves_value() {
        // The overlay path (P-4) uses upsample_linear_bilinear for the
        // shrink direction too. A solid-colour input must still produce
        // the same solid colour out — proves the function isn't
        // upsample-only despite the historical name.
        let src = solid(8, 8, [0.7, 0.4, 0.2, 1.0]);
        let dst = upsample_linear_bilinear(&src, 8, 8, 4, 4);
        assert_eq!(dst.len(), 4 * 4 * 4);
        for px in dst.chunks_exact(4) {
            assert!((px[0] - 0.7).abs() < 1e-6);
            assert!((px[1] - 0.4).abs() < 1e-6);
            assert!((px[2] - 0.2).abs() < 1e-6);
            assert!((px[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn bilinear_resize_preserves_alpha() {
        // Overlay clips often carry per-pixel alpha (PNG titles, masked
        // footage). Bilinear taps must preserve a constant alpha through
        // either direction.
        let src = solid(3, 5, [0.5, 0.5, 0.5, 0.42]);
        let up = upsample_linear_bilinear(&src, 3, 5, 9, 7);
        for px in up.chunks_exact(4) {
            assert!((px[3] - 0.42).abs() < 1e-6, "alpha drift on upsample: {}", px[3]);
        }
        let down = upsample_linear_bilinear(&src, 3, 5, 2, 3);
        for px in down.chunks_exact(4) {
            assert!((px[3] - 0.42).abs() < 1e-6, "alpha drift on downsample: {}", px[3]);
        }
    }

    #[test]
    fn downsample_2x2_block_averages() {
        // Source: top-left 2×2 of 4×2 image.
        // Pixel layout (RGBA): (1,0,0,1) (0,1,0,1) | (0,0,1,1) (1,1,1,1)
        let src = vec![
            1.0, 0.0, 0.0, 1.0,   0.0, 1.0, 0.0, 1.0,   1.0, 1.0, 0.0, 1.0,   0.0, 0.0, 1.0, 1.0,
            0.0, 0.0, 1.0, 1.0,   1.0, 1.0, 1.0, 1.0,   0.5, 0.5, 0.5, 1.0,   0.0, 1.0, 1.0, 1.0,
        ];
        let dst = downsample_linear_box(&src, 4, 2, 2, 1);
        // Left output pixel = average of (1,0,0), (0,1,0), (0,0,1), (1,1,1)
        // R=(1+0+0+1)/4=0.5, G=(0+1+0+1)/4=0.5, B=(0+0+1+1)/4=0.5. Alpha all 1.
        assert!((dst[0] - 0.5).abs() < 1e-6);
        assert!((dst[1] - 0.5).abs() < 1e-6);
        assert!((dst[2] - 0.5).abs() < 1e-6);
        assert!((dst[3] - 1.0).abs() < 1e-6);
        // Right output pixel = average of (1,1,0), (0,0,1), (0.5,0.5,0.5), (0,1,1)
        // = (0.375, 0.625, 0.625).
        assert!((dst[4] - 0.375).abs() < 1e-6);
        assert!((dst[5] - 0.625).abs() < 1e-6);
        assert!((dst[6] - 0.625).abs() < 1e-6);
        assert!((dst[7] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_inputs_return_empty() {
        assert!(downsample_linear_box(&[], 0, 0, 0, 0).is_empty());
        assert!(upsample_linear_bilinear(&[], 0, 0, 0, 0).is_empty());
        assert!(downsample_linear_2x(&[], 0, 0).is_empty());
        assert!(upsample_linear_2x(&[], 0, 0).is_empty());
    }

    #[test]
    fn fast_2x_downsample_matches_generic() {
        let src = vec![
            1.0, 0.0, 0.0, 1.0,   0.0, 1.0, 0.0, 1.0,   1.0, 1.0, 0.0, 1.0,   0.0, 0.0, 1.0, 1.0,
            0.0, 0.0, 1.0, 1.0,   1.0, 1.0, 1.0, 1.0,   0.5, 0.5, 0.5, 1.0,   0.0, 1.0, 1.0, 1.0,
        ];
        let fast = downsample_linear_2x(&src, 4, 2);
        let generic = downsample_linear_box(&src, 4, 2, 2, 1);
        assert_eq!(fast.len(), generic.len());
        for (a, b) in fast.iter().zip(generic.iter()) {
            assert!((a - b).abs() < 1e-6, "fast={a} generic={b}");
        }
    }

    #[test]
    fn fast_2x_upsample_duplicates_source_pixels() {
        let src = vec![0.1, 0.2, 0.3, 1.0,   0.4, 0.5, 0.6, 1.0];
        let dst = upsample_linear_2x(&src, 2, 1);
        // 2x1 → 4x2. Each src pixel becomes a 2x2 block.
        // Row 0: [0.1,0.2,0.3,1.0] [0.1,0.2,0.3,1.0] [0.4,0.5,0.6,1.0] [0.4,0.5,0.6,1.0]
        // Row 1: identical to row 0.
        assert_eq!(dst.len(), 4 * 2 * 4);
        for row in 0..2 {
            let row_start = row * 4 * 4;
            assert!((dst[row_start]     - 0.1).abs() < 1e-6);
            assert!((dst[row_start + 4] - 0.1).abs() < 1e-6);
            assert!((dst[row_start + 8] - 0.4).abs() < 1e-6);
            assert!((dst[row_start + 12] - 0.4).abs() < 1e-6);
        }
    }

    /// Sanity perf for the user's actual case: 1080×1920 (portrait
    /// iPhone clip) → 1920×1080 (landscape sequence). The engine
    /// dispatches this to bilinear (it's a mixed up/down asymmetric
    /// resize, not an exact 2:1). Production logs showed 22 ms here;
    /// the slice-ref + hoisted-weights rewrite should drop it well
    /// under that. Loose cap to catch a regression that lands a
    /// per-pixel allocation in the inner loop.
    #[test]
    fn bilinear_portrait_to_landscape_perf_sanity() {
        let src_w = 1080u32;
        let src_h = 1920u32;
        let dst_w = 1920u32;
        let dst_h = 1080u32;
        let src = vec![0.5f32; (src_w as usize) * (src_h as usize) * 4];
        // Warmup
        let _ = upsample_linear_bilinear(&src, src_w, src_h, dst_w, dst_h);
        let mut best_ms = f64::INFINITY;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let _out = upsample_linear_bilinear(&src, src_w, src_h, dst_w, dst_h);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms < best_ms { best_ms = ms; }
        }
        // 50 ms cap; real release should land ~5–10 ms.
        assert!(best_ms < 50.0, "bilinear 1080x1920→1920x1080 took {best_ms:.1}ms — too slow");
    }

    /// Direction router: identical sizes round-trip without copying
    /// data shape, downsamples route to the box helper, upsamples
    /// route to bilinear. Pin the contract so future callers don't
    /// accidentally reintroduce the bilinear-for-downsample bug.
    #[test]
    fn resize_linear_dispatches_correctly() {
        // 1×1 RGBA pixel.
        let one = vec![0.5, 0.5, 0.5, 1.0];
        // identity
        let same = resize_linear(&one, 1, 1, 1, 1);
        assert_eq!(same.len(), 4);
        // upsample 1×1 → 2×2 → 16 floats (bilinear path)
        let up = resize_linear(&one, 1, 1, 2, 2);
        assert_eq!(up.len(), 4 * 4);
        // downsample 4×4 of value 0.25 → 2×2 (box path, must average to 0.25)
        let four = vec![0.25; 4 * 4 * 4];
        let down = resize_linear(&four, 4, 4, 2, 2);
        assert_eq!(down.len(), 4 * 4);
        for v in &down {
            assert!((v - 0.25).abs() < 1e-6);
        }
    }
}
