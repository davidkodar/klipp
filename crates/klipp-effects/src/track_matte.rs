//! Track-matte primitives — extract a single-channel mask from a clip's
//! rendered RGBA, soften the edges, then multiply another clip's alpha by
//! the result. Phase 2 of the track-matte feature.
//!
//! The extractor is duplicated for the two compositor domains:
//! linear-f32 (playback / linear-light path) and sRGB-u8 (export's
//! current u8 path). Both produce a normalised `[0.0, 1.0]` mask so the
//! downstream `apply_mask_*` helpers stay domain-agnostic.
//!
//! Choke (negative-feather / erosion) is intentionally deferred to a
//! later slice — the wire format already carries `choke_px`, but the
//! Phase 2 mask pipeline applies feather only. A no-op on `choke_px > 0`
//! is fine for ship: the user's edge stays where it was rather than
//! moving the wrong direction. The `apply_choke_mask` stub below is a
//! visible TODO so future work has an obvious entry point.
//!
//! Domain note for luma extraction: BT.709 weights `(0.2126, 0.7152,
//! 0.0722)` come from the linear-light spec, but in practice both NLE
//! conventions exist. We apply the same weights in both code paths so
//! a luma matte derived in playback and the same matte exported to file
//! agree to within sRGB-encode rounding (sub-1 byte at 8-bit).

use crate::blur;

/// Source channel selector for the matte signal. Mirrors the order of
/// `klipp_nodes::params::MatteChannel` so callers can convert by enum
/// pattern match without touching this crate's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatteChannel {
    Alpha,
    Luminance,
    Red,
    Green,
    Blue,
}

impl Default for MatteChannel {
    fn default() -> Self {
        MatteChannel::Luminance
    }
}

/// Extract a single-channel mask from a linear-f32 RGBA buffer.
///
/// Output is one f32 per pixel, clamped to `[0.0, 1.0]`. `invert` flips
/// the signal (mask = 1 − mask) after channel extraction so an inverted
/// luma key gives the silhouette of the bright source as the mask.
///
/// Panics: never. Input length is checked: a buffer that isn't a
/// multiple of 4 returns an empty vec, matching the defensive shape of
/// the rest of the compositor.
pub fn extract_mask_linear(
    rgba: &[f32],
    channel: MatteChannel,
    invert: bool,
) -> Vec<f32> {
    if rgba.len() % 4 != 0 {
        return Vec::new();
    }
    let pixels = rgba.len() / 4;
    let mut mask = Vec::with_capacity(pixels);
    for i in 0..pixels {
        let r = rgba[i * 4];
        let g = rgba[i * 4 + 1];
        let b = rgba[i * 4 + 2];
        let a = rgba[i * 4 + 3];
        let raw = match channel {
            MatteChannel::Alpha => a,
            // BT.709 luminance weights — applied to linear values.
            MatteChannel::Luminance => 0.2126 * r + 0.7152 * g + 0.0722 * b,
            MatteChannel::Red => r,
            MatteChannel::Green => g,
            MatteChannel::Blue => b,
        };
        let clamped = raw.clamp(0.0, 1.0);
        mask.push(if invert { 1.0 - clamped } else { clamped });
    }
    mask
}

/// Extract a single-channel mask from an sRGB-u8 RGBA buffer. Output is
/// f32 in `[0.0, 1.0]`, normalised by `1/255`. Used by the export
/// compositor while it still operates in u8 sRGB.
pub fn extract_mask_srgb_u8(
    rgba: &[u8],
    channel: MatteChannel,
    invert: bool,
) -> Vec<f32> {
    if rgba.len() % 4 != 0 {
        return Vec::new();
    }
    let pixels = rgba.len() / 4;
    let mut mask = Vec::with_capacity(pixels);
    for i in 0..pixels {
        let r = rgba[i * 4] as f32 / 255.0;
        let g = rgba[i * 4 + 1] as f32 / 255.0;
        let b = rgba[i * 4 + 2] as f32 / 255.0;
        let a = rgba[i * 4 + 3] as f32 / 255.0;
        let raw = match channel {
            MatteChannel::Alpha => a,
            // BT.709 weights on sRGB byte values — gives Y' (perceptual
            // luma), the historical NLE convention. Slight delta against
            // the linear extractor but invisible at 8-bit.
            MatteChannel::Luminance => 0.2126 * r + 0.7152 * g + 0.0722 * b,
            MatteChannel::Red => r,
            MatteChannel::Green => g,
            MatteChannel::Blue => b,
        };
        let clamped = raw.clamp(0.0, 1.0);
        mask.push(if invert { 1.0 - clamped } else { clamped });
    }
    mask
}

/// Soften mask edges with a Gaussian blur of `radius_px`. Skips when
/// the radius is below a quarter-pixel — anything smaller is sub-sample
/// noise the bilinear taps in the downstream blend already swallow.
///
/// Implemented as: pack the f32 mask into an RGBA u8 buffer (R = mask,
/// G/B/A unused), reuse the existing `blur::apply` (CPU 2-pass
/// separable Gaussian), unpack the R channel back to f32. Slightly
/// wasteful at 4×, but the mask is one-channel-equivalent traffic and
/// avoids a second blur kernel. Phase 4 can drop in a single-channel
/// GPU compute pass if the perf budget needs it.
pub fn feather_mask(
    mask: &mut Vec<f32>,
    width: u32,
    height: u32,
    radius_px: f32,
) {
    if radius_px < 0.25 || mask.is_empty() {
        return;
    }
    if mask.len() != (width as usize) * (height as usize) {
        return;
    }
    let pixels = mask.len();
    let mut packed = Vec::with_capacity(pixels * 4);
    for &m in mask.iter() {
        let v = (m.clamp(0.0, 1.0) * 255.0) as u8;
        packed.extend_from_slice(&[v, 0, 0, 0]);
    }
    let blurred = blur::apply(packed, width, height, radius_px);
    if blurred.len() != pixels * 4 {
        // Blur returned a different size — the kernel rejected the input.
        // Leave the original mask in place rather than corrupt it.
        return;
    }
    for i in 0..pixels {
        mask[i] = blurred[i * 4] as f32 / 255.0;
    }
}

/// Erosion / "choke" — intentionally a no-op for Phase 2. The wire
/// format already carries `choke_px` so user-saved bindings round-trip
/// cleanly; once a per-pixel min-pool kernel lands here in Phase 4 the
/// existing data renders correctly without any project-format change.
pub fn apply_choke_mask(mask: &mut Vec<f32>, width: u32, height: u32, radius_px: f32) {
    if radius_px < 0.5 || mask.is_empty() {
        return;
    }
    if mask.len() != (width as usize) * (height as usize) {
        return;
    }
    // Round to nearest pixel-radius. Sub-pixel choke isn't meaningful
    // on a discrete grid — let the matching feather kernel handle the
    // soft-edge geometry. Cap each axis independently so a thin strip
    // (e.g. a 1-row mask in a unit test) doesn't degenerate the cap to
    // zero on the other axis.
    let r = (radius_px.round() as usize).max(1);

    let w = width as usize;
    let h = height as usize;
    let mut tmp = vec![0.0f32; mask.len()];
    min_pool_h(mask, &mut tmp, w, h, r);
    min_pool_v(&tmp, mask, w, h, r);
}

/// Single-axis sliding-window minimum filter. Implements Lemire's
/// O(N) deque algorithm: each pixel is pushed and popped from the
/// monotonic deque exactly once across the row, so total work is
/// linear in pixels — independent of `r`. Critical at the wider end
/// of the user-facing 0–50 px range, where a naive O(N·R) scan
/// would touch ~50× more memory per frame.
fn min_pool_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let mut deque: std::collections::VecDeque<usize> = std::collections::VecDeque::with_capacity(2 * r + 1);
    for y in 0..h {
        deque.clear();
        let row = y * w;
        // Prime the window with the first `r` pixels (the right half
        // of the kernel that's already inside the image at x=0).
        for x in 0..r.min(w) {
            while let Some(&back) = deque.back() {
                if src[row + back] >= src[row + x] {
                    deque.pop_back();
                } else { break; }
            }
            deque.push_back(x);
        }
        for x in 0..w {
            // Slide the right edge in by one (if there's a new pixel
            // to admit), maintaining the monotonic-min property.
            let next = x + r;
            if next < w {
                while let Some(&back) = deque.back() {
                    if src[row + back] >= src[row + next] {
                        deque.pop_back();
                    } else { break; }
                }
                deque.push_back(next);
            }
            // Slide the left edge in by one (drop pixels that left
            // the window).
            if let Some(&front) = deque.front() {
                if x > r && front + r < x {
                    deque.pop_front();
                }
            }
            // Front of the deque is the index of the minimum value.
            // Edge guard: at x < r the deque is built only from the
            // right half of the window — which is correct (clamping
            // the kernel against the image boundary).
            let min_idx = *deque.front().unwrap_or(&x);
            dst[row + x] = src[row + min_idx];
        }
    }
}

fn min_pool_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let mut deque: std::collections::VecDeque<usize> = std::collections::VecDeque::with_capacity(2 * r + 1);
    for x in 0..w {
        deque.clear();
        for y in 0..r.min(h) {
            while let Some(&back) = deque.back() {
                if src[back * w + x] >= src[y * w + x] {
                    deque.pop_back();
                } else { break; }
            }
            deque.push_back(y);
        }
        for y in 0..h {
            let next = y + r;
            if next < h {
                while let Some(&back) = deque.back() {
                    if src[back * w + x] >= src[next * w + x] {
                        deque.pop_back();
                    } else { break; }
                }
                deque.push_back(next);
            }
            if let Some(&front) = deque.front() {
                if y > r && front + r < y {
                    deque.pop_front();
                }
            }
            let min_idx = *deque.front().unwrap_or(&y);
            dst[y * w + x] = src[min_idx * w + x];
        }
    }
}

/// Multiply a linear-f32 RGBA buffer's alpha by a single-channel mask
/// in place. Mask must have one entry per pixel (length = `rgba.len()
/// / 4`). Length mismatch is a no-op rather than a panic so the
/// composite path stays robust against frame-shape drift between the
/// matte source and the consumer (in practice we always resize the
/// matte to the consumer's resolution before calling, but the guard
/// is cheap).
pub fn apply_mask_to_alpha_linear(rgba: &mut [f32], mask: &[f32]) {
    if rgba.len() % 4 != 0 || mask.len() * 4 != rgba.len() {
        return;
    }
    for (i, &m) in mask.iter().enumerate() {
        let a_idx = i * 4 + 3;
        rgba[a_idx] *= m.clamp(0.0, 1.0);
    }
}

/// Multiply an sRGB-u8 RGBA buffer's alpha by a single-channel mask in
/// place. Mask is f32 `[0.0, 1.0]`; we round to nearest u8 for the
/// modified alpha. Same length-mismatch policy as the linear variant.
pub fn apply_mask_to_alpha_srgb_u8(rgba: &mut [u8], mask: &[f32]) {
    if rgba.len() % 4 != 0 || mask.len() * 4 != rgba.len() {
        return;
    }
    for (i, &m) in mask.iter().enumerate() {
        let a_idx = i * 4 + 3;
        let scaled = (rgba[a_idx] as f32) * m.clamp(0.0, 1.0);
        rgba[a_idx] = scaled.round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    // ── extract_mask_linear ──────────────────────────────────────────

    #[test]
    fn linear_alpha_passthrough() {
        // Two pixels: alpha 0.0, alpha 1.0 → mask [0.0, 1.0]
        let rgba = vec![1.0, 0.0, 0.0, 0.0,  0.0, 1.0, 0.0, 1.0];
        let m = extract_mask_linear(&rgba, MatteChannel::Alpha, false);
        assert_eq!(m.len(), 2);
        assert!(approx(m[0], 0.0));
        assert!(approx(m[1], 1.0));
    }

    #[test]
    fn linear_alpha_inverted() {
        let rgba = vec![1.0, 0.0, 0.0, 0.25,  0.0, 1.0, 0.0, 0.75];
        let m = extract_mask_linear(&rgba, MatteChannel::Alpha, true);
        assert!(approx(m[0], 0.75));
        assert!(approx(m[1], 0.25));
    }

    #[test]
    fn linear_luma_uses_bt709() {
        // Pure red linear 1.0 → 0.2126
        let red = vec![1.0, 0.0, 0.0, 1.0];
        let m = extract_mask_linear(&red, MatteChannel::Luminance, false);
        assert!(approx(m[0], 0.2126), "got {}", m[0]);
        // Pure green 1.0 → 0.7152
        let green = vec![0.0, 1.0, 0.0, 1.0];
        let m = extract_mask_linear(&green, MatteChannel::Luminance, false);
        assert!(approx(m[0], 0.7152));
        // Pure blue 1.0 → 0.0722
        let blue = vec![0.0, 0.0, 1.0, 1.0];
        let m = extract_mask_linear(&blue, MatteChannel::Luminance, false);
        assert!(approx(m[0], 0.0722));
    }

    #[test]
    fn linear_per_channel_extraction() {
        let rgba = vec![0.4, 0.6, 0.8, 1.0];
        for (ch, expected) in [
            (MatteChannel::Red, 0.4),
            (MatteChannel::Green, 0.6),
            (MatteChannel::Blue, 0.8),
            (MatteChannel::Alpha, 1.0),
        ] {
            let m = extract_mask_linear(&rgba, ch, false);
            assert!(approx(m[0], expected), "channel {ch:?} got {}", m[0]);
        }
    }

    #[test]
    fn linear_clamps_negative_and_oversaturated() {
        // HDR / out-of-gamut input shouldn't poison the mask.
        let rgba = vec![-0.5, 1.5, 2.0, 1.0];
        let m = extract_mask_linear(&rgba, MatteChannel::Red, false);
        assert!(approx(m[0], 0.0));
        let m = extract_mask_linear(&rgba, MatteChannel::Green, false);
        assert!(approx(m[0], 1.0));
    }

    #[test]
    fn linear_rejects_non_multiple_of_four_length() {
        let bad = vec![1.0, 0.0, 0.0]; // only 3 floats
        let m = extract_mask_linear(&bad, MatteChannel::Alpha, false);
        assert!(m.is_empty());
    }

    // ── extract_mask_srgb_u8 ─────────────────────────────────────────

    #[test]
    fn srgb_alpha_normalises_to_unit() {
        let rgba = vec![255, 0, 0, 0,  0, 255, 0, 255];
        let m = extract_mask_srgb_u8(&rgba, MatteChannel::Alpha, false);
        assert!(approx(m[0], 0.0));
        assert!(approx(m[1], 1.0));
    }

    #[test]
    fn srgb_luma_pure_white_is_one() {
        let rgba = vec![255, 255, 255, 255];
        let m = extract_mask_srgb_u8(&rgba, MatteChannel::Luminance, false);
        assert!(approx(m[0], 1.0));
    }

    /// Pin the contract: luma extraction reads RGB as-given, with no
    /// un-premultiply step. A "white-at-50%-alpha" pixel encoded
    /// premultiplied (RGB pre-darkened to 0.5) yields luma = 0.5.
    /// Encoded straight (RGB = 1.0, alpha = 0.5) yields luma = 1.0.
    ///
    /// The implication is real: clips coming through `opacity::apply`
    /// (which premultiplies) or premult-encoded sources (typical
    /// ProRes 4444 from AE) feed dim luma into the matte extractor.
    /// Out-of-band, this means a luma-keyed matte over a title with
    /// antialiased semi-transparent edges renders softer than the
    /// user expects. Documented here so a future un-premultiply pass
    /// (or a decoder-side mode flag) doesn't regress this behaviour
    /// without us noticing.
    #[test]
    fn linear_luma_does_not_un_premultiply() {
        // Same logical pixel, two encodings.
        let straight: Vec<f32>   = vec![1.0, 1.0, 1.0, 0.5];
        let premult:  Vec<f32>   = vec![0.5, 0.5, 0.5, 0.5];

        let m_straight = extract_mask_linear(&straight, MatteChannel::Luminance, false);
        let m_premult  = extract_mask_linear(&premult,  MatteChannel::Luminance, false);

        // Straight: BT.709 weights on (1,1,1) sum to 1.0.
        assert!(approx(m_straight[0], 1.0),
            "straight-alpha luma should be 1.0 for (1,1,1,0.5), got {}", m_straight[0]);
        // Premult: RGB is already alpha-darkened, so luma = 0.5.
        assert!(approx(m_premult[0], 0.5),
            "premult luma reflects the darkened RGB — got {} (expected 0.5 to confirm \
             current behaviour; if a fix lands this should become 1.0 and the test \
             should be updated alongside the fix)", m_premult[0]);
    }

    #[test]
    fn srgb_luma_does_not_un_premultiply() {
        // Same logical pixel as above, encoded as 8-bit sRGB.
        // 128/255 ≈ 0.502; the BT.709 sum lands at ~0.502.
        let straight: Vec<u8> = vec![255, 255, 255, 128];
        let premult:  Vec<u8> = vec![128, 128, 128, 128];

        let m_straight = extract_mask_srgb_u8(&straight, MatteChannel::Luminance, false);
        let m_premult  = extract_mask_srgb_u8(&premult,  MatteChannel::Luminance, false);

        assert!(approx(m_straight[0], 1.0),
            "straight-alpha luma = 1.0, got {}", m_straight[0]);
        // 128/255 = 0.5019…; the BT.709 sum on (0.5019, 0.5019, 0.5019)
        // is the same 0.5019. Tolerance must allow ~0.002 slack vs 0.5
        // — the existing `approx` helper uses 1e-3 which is too tight.
        assert!((m_premult[0] - 0.5019).abs() < 0.002,
            "premult luma reflects the darkened RGB — got {}", m_premult[0]);
    }

    #[test]
    fn srgb_invert_flips_signal() {
        let rgba = vec![128, 128, 128, 255];
        let m_normal = extract_mask_srgb_u8(&rgba, MatteChannel::Red, false);
        let m_inv = extract_mask_srgb_u8(&rgba, MatteChannel::Red, true);
        assert!(approx(m_normal[0] + m_inv[0], 1.0));
    }

    // ── apply_mask_to_alpha_* ────────────────────────────────────────

    #[test]
    fn apply_mask_linear_zero_makes_transparent() {
        let mut rgba = vec![1.0, 0.5, 0.25, 1.0];
        let mask = vec![0.0];
        apply_mask_to_alpha_linear(&mut rgba, &mask);
        assert!(approx(rgba[3], 0.0));
        // RGB unchanged.
        assert!(approx(rgba[0], 1.0));
    }

    #[test]
    fn apply_mask_linear_half_halves_alpha() {
        let mut rgba = vec![0.0, 0.0, 0.0, 1.0,  0.0, 0.0, 0.0, 0.5];
        let mask = vec![0.5, 0.5];
        apply_mask_to_alpha_linear(&mut rgba, &mask);
        assert!(approx(rgba[3], 0.5));
        assert!(approx(rgba[7], 0.25));
    }

    #[test]
    fn apply_mask_linear_length_mismatch_is_noop() {
        let original = vec![1.0, 1.0, 1.0, 1.0];
        let mut rgba = original.clone();
        let mask = vec![0.0, 0.0, 0.0]; // length doesn't match
        apply_mask_to_alpha_linear(&mut rgba, &mask);
        assert_eq!(rgba, original, "mismatched mask must not corrupt RGBA");
    }

    #[test]
    fn apply_mask_srgb_u8_zero_makes_transparent() {
        let mut rgba = vec![255u8, 128, 64, 255];
        let mask = vec![0.0];
        apply_mask_to_alpha_srgb_u8(&mut rgba, &mask);
        assert_eq!(rgba[3], 0);
    }

    #[test]
    fn apply_mask_srgb_u8_half_halves_alpha() {
        let mut rgba = vec![0u8, 0, 0, 200];
        let mask = vec![0.5];
        apply_mask_to_alpha_srgb_u8(&mut rgba, &mask);
        assert_eq!(rgba[3], 100);
    }

    // ── feather_mask ─────────────────────────────────────────────────

    #[test]
    fn feather_mask_zero_radius_is_noop() {
        let original: Vec<f32> = (0..16).map(|i| (i as f32) / 16.0).collect();
        let mut mask = original.clone();
        feather_mask(&mut mask, 4, 4, 0.0);
        assert_eq!(mask, original);
    }

    #[test]
    fn feather_mask_softens_hard_edge() {
        // 8x1 mask: left half 0, right half 1. After feather, the centre
        // pair should land near 0.5 — ish. We just check monotonic and
        // not-binary, since the exact values depend on blur kernel.
        let mut mask = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        feather_mask(&mut mask, 8, 1, 1.5);
        // Leftmost should still be near 0, rightmost near 1.
        assert!(mask[0] < 0.1, "left edge should stay dark, got {}", mask[0]);
        assert!(mask[7] > 0.9, "right edge should stay bright, got {}", mask[7]);
        // Some intermediate pixel should be neither 0 nor 1.
        let mid = mask[3];
        assert!(mid > 0.0 && mid < 1.0, "feather should produce intermediate value, got {mid}");
    }

    #[test]
    fn feather_mask_size_mismatch_is_noop() {
        let original = vec![1.0; 16];
        let mut mask = original.clone();
        // declare 5x5 but pass a 16-pixel buffer
        feather_mask(&mut mask, 5, 5, 1.0);
        assert_eq!(mask, original, "size mismatch must not corrupt mask");
    }

    // ── apply_choke_mask ─────────────────────────────────────────────

    #[test]
    fn choke_mask_zero_radius_is_noop() {
        let original: Vec<f32> = (0..16).map(|i| (i as f32) / 16.0).collect();
        let mut mask = original.clone();
        apply_choke_mask(&mut mask, 4, 4, 0.0);
        assert_eq!(mask, original);
    }

    #[test]
    fn choke_mask_size_mismatch_is_noop() {
        let original = vec![1.0; 16];
        let mut mask = original.clone();
        apply_choke_mask(&mut mask, 5, 5, 2.0); // wrong dims
        assert_eq!(mask, original, "size mismatch must not corrupt mask");
    }

    #[test]
    fn choke_shrinks_hard_edge_by_radius_pixels() {
        // 11×1 mask: pixels 4..7 are bright (3-pixel block of 1.0),
        // rest are 0.0. Choke radius 1 should shrink the bright block
        // from 3 pixels to 1 pixel (removing one from each side via
        // the min-pool window). Choke radius 2 should erase it entirely.
        let mut mask: Vec<f32> = (0..11).map(|x| if (4..=6).contains(&x) { 1.0 } else { 0.0 }).collect();
        apply_choke_mask(&mut mask, 11, 1, 1.0);
        // After radius=1: the block centred at 5 should be the only
        // surviving 1.0 (its neighbours 4 and 6 had a 0 within their
        // window via the OUT pixel).
        assert!((mask[4] - 0.0).abs() < 1e-5, "left edge of block should be eroded, got {}", mask[4]);
        assert!((mask[5] - 1.0).abs() < 1e-5, "centre should survive, got {}", mask[5]);
        assert!((mask[6] - 0.0).abs() < 1e-5, "right edge of block should be eroded, got {}", mask[6]);

        let mut mask2: Vec<f32> = (0..11).map(|x| if (4..=6).contains(&x) { 1.0 } else { 0.0 }).collect();
        apply_choke_mask(&mut mask2, 11, 1, 2.0);
        // After radius=2: nothing survives — every pixel's window
        // touches the surrounding zeros.
        for (i, &v) in mask2.iter().enumerate() {
            assert!(v.abs() < 1e-5, "pixel {i} should be eroded to 0, got {v}");
        }
    }

    #[test]
    fn choke_then_feather_round_trip_keeps_solid_interior() {
        // A 9×9 fully-bright mask should survive choke(1) + feather(1)
        // entirely in its interior; only the border pixels can dim
        // because the choke pulls the boundary one pixel inward and
        // feather softens it back.
        let mut mask = vec![1.0f32; 81];
        apply_choke_mask(&mut mask, 9, 9, 1.0);
        feather_mask(&mut mask, 9, 9, 1.0);
        // Centre 3×3 should still read as ~1.0 — well inside both
        // kernels' reach of the (still-bright) original interior.
        for y in 3..=5 {
            for x in 3..=5 {
                let v = mask[y * 9 + x];
                assert!(v > 0.95, "interior ({x},{y}) should be ≈1, got {v}");
            }
        }
    }

    #[test]
    fn choke_is_independent_of_invert_call_order() {
        // Pin: extracting an INVERTED mask then choking it should
        // produce the same shape as extracting a normal mask, choking
        // it, then inverting AT THE EXTRACT STEP — i.e. choke is a
        // pure shape op on whatever signal we feed it. This stops any
        // future inverter accidentally swapping erode/dilate.
        let rgba: Vec<f32> = (0..16).flat_map(|i| {
            let v = if (4..=6).contains(&i) { 1.0 } else { 0.0 };
            [v, v, v, 1.0_f32]
        }).collect();
        let mut a = extract_mask_linear(&rgba, MatteChannel::Red, false);
        apply_choke_mask(&mut a, 16, 1, 1.0);
        let inverted: Vec<f32> = a.iter().map(|v| 1.0 - v).collect();
        let mut b = extract_mask_linear(&rgba, MatteChannel::Red, true);
        apply_choke_mask(&mut b, 16, 1, 1.0);
        // `b` choked the inverted signal — that's ERODING zeros, i.e.
        // dilating the bright-area, the opposite of `inverted`. They
        // should NOT match. This pins the asymmetry: the user's
        // intuition "choke == shrink the visible bit" is what we
        // promise, regardless of which direction the signal points.
        let agree = a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(!agree, "choke is shape-erosion, not signal-erosion — invert must change result");
        // And `inverted` (post-choke flip) is what the user would
        // expect of "invert + choke" semantically — but we don't
        // promise that either, so just sanity-check the lengths line up.
        assert_eq!(inverted.len(), b.len());
    }
}
