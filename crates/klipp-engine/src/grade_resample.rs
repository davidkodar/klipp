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
        let sy1 = ((((dy + 1) as u64 * src_h as u64) + dst_h as u64 - 1) / dst_h as u64) as usize;
        let sy1 = sy1.min(src_h_us);
        for dx in 0..dst_w {
            let sx0 = ((dx as u64 * src_w as u64) / dst_w as u64) as usize;
            let sx1 = ((((dx + 1) as u64 * src_w as u64) + dst_w as u64 - 1) / dst_w as u64) as usize;
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

    // Sample at pixel centres. Map dst centre → src centre, then
    // blend the four nearest source pixels by sub-pixel distance.
    let sx_scale = src_w as f32 / dst_w as f32;
    let sy_scale = src_h as f32 / dst_h as f32;

    for dy in 0..dst_h {
        let sy_f = (dy as f32 + 0.5) * sy_scale - 0.5;
        let sy0 = sy_f.floor().max(0.0) as usize;
        let sy1 = (sy0 + 1).min(src_h_us - 1);
        let wy = (sy_f - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..dst_w {
            let sx_f = (dx as f32 + 0.5) * sx_scale - 0.5;
            let sx0 = sx_f.floor().max(0.0) as usize;
            let sx1 = (sx0 + 1).min(src_w_us - 1);
            let wx = (sx_f - sx0 as f32).clamp(0.0, 1.0);

            let i00 = (sy0 * src_w_us + sx0) * 4;
            let i01 = (sy0 * src_w_us + sx1) * 4;
            let i10 = (sy1 * src_w_us + sx0) * 4;
            let i11 = (sy1 * src_w_us + sx1) * 4;
            let dst_idx = ((dy as usize) * (dst_w as usize) + dx as usize) * 4;

            for c in 0..4 {
                let top = src[i00 + c] * (1.0 - wx) + src[i01 + c] * wx;
                let bot = src[i10 + c] * (1.0 - wx) + src[i11 + c] * wx;
                out[dst_idx + c] = top * (1.0 - wy) + bot * wy;
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
}
