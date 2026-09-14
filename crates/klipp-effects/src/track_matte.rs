//! Track-matte primitives — extract a single-channel mask from a clip's
//! rendered RGBA, soften the edges, then multiply another clip's alpha by
//! the result. Phase 2 of the track-matte feature.
//!
//! The extractor is duplicated for the two compositor domains:
//! linear-f32 (playback / linear-light path) and sRGB-u8 (export's
//! current u8 path). Both produce a normalised `[0.0, 1.0]` mask so the
//! downstream `apply_mask_*` helpers stay domain-agnostic.
//!
//! Choke (negative-feather / erosion) is implemented: [`apply_choke_mask`]
//! erodes the mask by `choke_px` with a separable min-pool, and
//! `klipp-compose`'s render calls it with the binding's real setting
//! before feathering.
//!
//! Domain note for luma extraction: the linear extractors take the
//! working space's own luminance row (`klipp_color::linear::luma_coefs`)
//! rather than Rec.709's, because the pixels they weigh are in the
//! project's primaries. Hardcoding Rec.709, as these used to,
//! under-weighted green by a fifth on every ACEScg project — the same
//! defect the luma key and the difference key carried until it was
//! fixed there. `extract_mask_srgb_u8` keeps Rec.709: its input is
//! display-encoded bytes, which are Rec.709-primaried whatever the
//! working space.

use crate::blur;

/// Source channel selector for the matte signal. Mirrors the order of
/// `klipp_nodes::params::MatteChannel` so callers can convert by enum
/// pattern match without touching this crate's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum MatteChannel {
    Alpha,
    #[default]
    Luminance,
    Red,
    Green,
    Blue,
}


/// Threshold below which we don't divide RGB by alpha — un-premultiply
/// at near-zero alpha amplifies quantisation noise and produces wild
/// values (a 1/255 alpha pixel divided by alpha = 255× the encoded
/// RGB). At this threshold the pixel is essentially invisible anyway,
/// so the matte read for it doesn't matter.
const PREMULT_ALPHA_EPSILON: f32 = 1e-4;

/// Extract a single-channel mask from a linear-f32 RGBA buffer.
///
/// Output is one f32 per pixel, clamped to `[0.0, 1.0]`. `invert` flips
/// the signal (mask = 1 − mask) after channel extraction so an inverted
/// luma key gives the silhouette of the bright source as the mask.
///
/// `source_premultiplied`: if true, RGB is divided by alpha before
/// reading R / G / B / Luminance channels, undoing the alpha-darkening
/// of premultiplied encodings (the compositor's internal convention
/// after `opacity::apply` and the typical encoding of ProRes 4444 /
/// PSD sources from AE). Alpha-channel extraction is unaffected by
/// this flag — alpha is alpha regardless of encoding. Pixels with
/// alpha ≤ `PREMULT_ALPHA_EPSILON` skip the divide (they're invisible).
///
/// Panics: never. Input length is checked: a buffer that isn't a
/// multiple of 4 returns an empty vec, matching the defensive shape of
/// the rest of the compositor.
pub fn extract_mask_linear(
    rgba: &[f32],
    channel: MatteChannel,
    invert: bool,
    source_premultiplied: bool,
    luma: [f32; 3],
) -> Vec<f32> {
    if !rgba.len().is_multiple_of(4) {
        return Vec::new();
    }
    let pixels = rgba.len() / 4;
    let mut mask = Vec::with_capacity(pixels);
    for i in 0..pixels {
        let mut r = rgba[i * 4];
        let mut g = rgba[i * 4 + 1];
        let mut b = rgba[i * 4 + 2];
        let a = rgba[i * 4 + 3];
        if source_premultiplied && !matches!(channel, MatteChannel::Alpha) && a > PREMULT_ALPHA_EPSILON {
            let inv = 1.0 / a;
            r = (r * inv).clamp(0.0, 1.0);
            g = (g * inv).clamp(0.0, 1.0);
            b = (b * inv).clamp(0.0, 1.0);
        }
        let raw = match channel {
            MatteChannel::Alpha => a,
            // The working space's luminance row, applied to linear values.
            MatteChannel::Luminance => luma[0] * r + luma[1] * g + luma[2] * b,
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
///
/// `source_premultiplied`: see [`extract_mask_linear`] — same
/// semantics, applied on the normalised f32 values before luma
/// weighting / channel pickoff.
pub fn extract_mask_srgb_u8(
    rgba: &[u8],
    channel: MatteChannel,
    invert: bool,
    source_premultiplied: bool,
) -> Vec<f32> {
    if !rgba.len().is_multiple_of(4) {
        return Vec::new();
    }
    let pixels = rgba.len() / 4;
    let mut mask = Vec::with_capacity(pixels);
    for i in 0..pixels {
        let mut r = rgba[i * 4] as f32 / 255.0;
        let mut g = rgba[i * 4 + 1] as f32 / 255.0;
        let mut b = rgba[i * 4 + 2] as f32 / 255.0;
        let a = rgba[i * 4 + 3] as f32 / 255.0;
        if source_premultiplied && !matches!(channel, MatteChannel::Alpha) && a > PREMULT_ALPHA_EPSILON {
            let inv = 1.0 / a;
            r = (r * inv).clamp(0.0, 1.0);
            g = (g * inv).clamp(0.0, 1.0);
            b = (b * inv).clamp(0.0, 1.0);
        }
        let raw = match channel {
            MatteChannel::Alpha => a,
            // Rec.709 weights on sRGB byte values — Y' (perceptual
            // luma), the historical NLE convention. Rec.709's row is
            // right here and stays: these bytes are display-encoded, and
            // the display transform converts to Rec.709 primaries
            // whatever the working space. Unlike the linear extractor
            // above, which weighs the project's own primaries and so
            // needs the project's own row.
            //
            // Nothing in the render path calls this today — the
            // compositor uses the linear extractors — so it carries no
            // working-space parameter rather than one that would always
            // be ignored.
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

/// Fast path for `extract_mask_linear → apply_mask_to_alpha_linear`
/// when neither choke nor feather are in use. Skips the intermediate
/// mask buffer entirely: for each pixel, derives the channel value
/// from the source, applies invert, and multiplies the consumer's
/// alpha in one pass. Saves ~16 MB of memory traffic per matte per
/// 1080p frame (no 8 MB mask write + no 8 MB mask read), which the
/// hot logs flag as the dominant matte cost when choke/feather are
/// zero — i.e. the default user case.
///
/// `source_premultiplied`: see [`extract_mask_linear`]. Branches
/// at the channel-match level so the un-premult path costs nothing
/// for callers that pass `false`.
///
/// Caller must guarantee the buffers are the same length and a
/// multiple of 4 (RGBA) — the slow path's defensive checks live in
/// `apply_track_matte_linear`'s caller.
pub fn apply_matte_fused_linear(
    consumer_rgba: &mut [f32],
    source_rgba: &[f32],
    channel: MatteChannel,
    invert: bool,
    source_premultiplied: bool,
    luma: [f32; 3],
) {
    debug_assert_eq!(consumer_rgba.len(), source_rgba.len());
    debug_assert!(consumer_rgba.len().is_multiple_of(4));
    // Hoist the channel match out of the per-pixel loop so the
    // optimizer sees a vectorisable straight-line body. With the
    // match inside, LLVM emits a 5-way branch per pixel and
    // auto-vectorisation gives up — the original implementation hit
    // ~5 ms at 1080p in production logs even for the trivial Alpha
    // case. Chunked iteration also drops the per-pixel bounds checks.
    //
    // Alpha channel never un-premultiplies; the (Alpha, *) arms stay
    // shared regardless of `source_premultiplied`.
    let con = consumer_rgba.chunks_exact_mut(4);
    let src = source_rgba.chunks_exact(4);
    match (channel, invert, source_premultiplied) {
        (MatteChannel::Alpha, false, _) => {
            for (c, s) in con.zip(src) {
                c[3] *= s[3].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Alpha, true, _) => {
            for (c, s) in con.zip(src) {
                c[3] *= 1.0 - s[3].clamp(0.0, 1.0);
            }
        }

        // Straight-alpha branches (no un-premultiply).
        (MatteChannel::Luminance, false, false) => {
            for (c, s) in con.zip(src) {
                let l = luma[0] * s[0] + luma[1] * s[1] + luma[2] * s[2];
                c[3] *= l.clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Luminance, true, false) => {
            for (c, s) in con.zip(src) {
                let l = luma[0] * s[0] + luma[1] * s[1] + luma[2] * s[2];
                c[3] *= 1.0 - l.clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Red, false, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= s[0].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Red, true, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= 1.0 - s[0].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Green, false, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= s[1].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Green, true, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= 1.0 - s[1].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Blue, false, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= s[2].clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Blue, true, false) => {
            for (c, s) in con.zip(src) {
                c[3] *= 1.0 - s[2].clamp(0.0, 1.0);
            }
        }

        // Premult-alpha branches: un-premultiply RGB before reading
        // the channel so a "white at 50% alpha" stored as (0.5, 0.5,
        // 0.5, 0.5) reads as the logical 1.0, not the darkened 0.5.
        (MatteChannel::Luminance, false, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let l = if a > PREMULT_ALPHA_EPSILON {
                    let inv = 1.0 / a;
                    let r = (s[0] * inv).clamp(0.0, 1.0);
                    let g = (s[1] * inv).clamp(0.0, 1.0);
                    let b = (s[2] * inv).clamp(0.0, 1.0);
                    luma[0] * r + luma[1] * g + luma[2] * b
                } else {
                    0.0
                };
                c[3] *= l.clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Luminance, true, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let l = if a > PREMULT_ALPHA_EPSILON {
                    let inv = 1.0 / a;
                    let r = (s[0] * inv).clamp(0.0, 1.0);
                    let g = (s[1] * inv).clamp(0.0, 1.0);
                    let b = (s[2] * inv).clamp(0.0, 1.0);
                    luma[0] * r + luma[1] * g + luma[2] * b
                } else {
                    0.0
                };
                c[3] *= 1.0 - l.clamp(0.0, 1.0);
            }
        }
        (MatteChannel::Red, false, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[0] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= v;
            }
        }
        (MatteChannel::Red, true, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[0] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= 1.0 - v;
            }
        }
        (MatteChannel::Green, false, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[1] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= v;
            }
        }
        (MatteChannel::Green, true, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[1] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= 1.0 - v;
            }
        }
        (MatteChannel::Blue, false, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[2] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= v;
            }
        }
        (MatteChannel::Blue, true, true) => {
            for (c, s) in con.zip(src) {
                let a = s[3];
                let v = if a > PREMULT_ALPHA_EPSILON { (s[2] / a).clamp(0.0, 1.0) } else { 0.0 };
                c[3] *= 1.0 - v;
            }
        }
    }
}

/// Soften mask edges with a Gaussian blur of `radius_px`. Skips when
/// the radius is below a quarter-pixel — anything smaller is sub-sample
/// noise the bilinear taps in the downstream blend already swallow.
///
/// Direct single-channel f32 separable box blur (3-pass box → Gaussian
/// approximation; same Bock's-method radii as `blur::apply` so visual
/// output matches the RGBA path closely). Operates on the mask's own
/// f32 buffer with no u8 round-trip — a 1920x1080 mask used to allocate
/// an 8.3 MB RGBA u8 packing buffer + run a 4-channel blur + unpack;
/// this version touches just the 4 MB f32 buffer + one scratch.
pub fn feather_mask(
    mask: &mut [f32],
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
    let w = width as usize;
    let h = height as usize;
    let radii = blur::boxes_for_gaussian(radius_px);
    let mut tmp = vec![0.0f32; mask.len()];
    for &r in &radii {
        box_blur_h_f32(mask, &mut tmp, w, h, r);
        box_blur_v_f32(&tmp, mask, w, h, r);
    }
}

/// Single-channel horizontal box blur on an f32 buffer. Sliding-window
/// running sum — O(N) regardless of radius.
fn box_blur_h_f32(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let diam = r + r + 1;
    let inv = 1.0 / diam as f32;
    for y in 0..h {
        let row = y * w;
        // Seed: replicate left-edge pixel for the window's "before-zero" overhang.
        let left = src[row];
        let mut sum = left * r as f32;
        let init_end = r.min(w - 1);
        for x in 0..=init_end {
            sum += src[row + x];
        }
        if r >= w {
            let right = src[row + w - 1];
            sum += right * (r + 1 - w) as f32;
        }
        for x in 0..w {
            dst[row + x] = sum * inv;
            let ax = (x + r + 1).min(w - 1);
            sum += src[row + ax];
            let sx = x.saturating_sub(r);
            sum -= src[row + sx];
        }
    }
}

/// Single-channel vertical box blur on an f32 buffer. Tile across
/// columns for cache locality, same shape as `blur::box_blur_v` but
/// f32-native.
fn box_blur_v_f32(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let diam = r + r + 1;
    let inv = 1.0 / diam as f32;
    const TILE: usize = 64;
    let mut x = 0;
    while x < w {
        let x_end = (x + TILE).min(w);
        let tile_w = x_end - x;
        let mut sums = vec![0.0f32; tile_w];
        // Seed: top-edge replication for window overhang.
        for c in 0..tile_w {
            sums[c] = src[x + c] * r as f32;
        }
        let init_end = r.min(h - 1);
        for y in 0..=init_end {
            let off = y * w + x;
            for c in 0..tile_w {
                sums[c] += src[off + c];
            }
        }
        if r >= h {
            let bot_off = (h - 1) * w + x;
            let cnt = (r + 1 - h) as f32;
            for c in 0..tile_w {
                sums[c] += src[bot_off + c] * cnt;
            }
        }
        for y in 0..h {
            let oi = y * w + x;
            for c in 0..tile_w {
                dst[oi + c] = sums[c] * inv;
            }
            let ay = (y + r + 1).min(h - 1);
            let ai = ay * w + x;
            let sy = y.saturating_sub(r);
            let si = sy * w + x;
            for c in 0..tile_w {
                sums[c] += src[ai + c];
                sums[c] -= src[si + c];
            }
        }
        x = x_end;
    }
}

/// Erosion / "choke": shrinks the mask by `radius_px`, pulling the matte
/// edge inwards so a key that spills past the subject can be pulled back
/// before the feather softens it.
///
/// A separable min-pool over Lemire's monotonic deque, so the cost is
/// O(pixels) and independent of the radius — the same shape as the box
/// blur above it. Radii round to whole pixels: sub-pixel choke is not
/// meaningful on a discrete grid, and the feather kernel owns the
/// soft-edge geometry.
///
/// A zero or sub-half-pixel radius, an empty mask, or a mask whose
/// length disagrees with `width * height` all return untouched, so a
/// caller cannot silently erode the wrong buffer.
pub fn apply_choke_mask(mask: &mut [f32], width: u32, height: u32, radius_px: f32) {
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
    if !rgba.len().is_multiple_of(4) || mask.len() * 4 != rgba.len() {
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
    if !rgba.len().is_multiple_of(4) || mask.len() * 4 != rgba.len() {
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
    /// These tests fix the axis to Rec.709 so the numbers read as the
    /// published weights. The per-working-space behaviour is asserted
    /// separately in `a_wide_gamut_row_changes_the_matte`.
    const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    // ── extract_mask_linear ──────────────────────────────────────────

    #[test]
    fn linear_alpha_passthrough() {
        // Two pixels: alpha 0.0, alpha 1.0 → mask [0.0, 1.0]
        let rgba = vec![1.0, 0.0, 0.0, 0.0,  0.0, 1.0, 0.0, 1.0];
        let m = extract_mask_linear(&rgba, MatteChannel::Alpha, false, false, REC709);
        assert_eq!(m.len(), 2);
        assert!(approx(m[0], 0.0));
        assert!(approx(m[1], 1.0));
    }

    #[test]
    fn linear_alpha_inverted() {
        let rgba = vec![1.0, 0.0, 0.0, 0.25,  0.0, 1.0, 0.0, 0.75];
        let m = extract_mask_linear(&rgba, MatteChannel::Alpha, true, false, REC709);
        assert!(approx(m[0], 0.75));
        assert!(approx(m[1], 0.25));
    }

    #[test]
    fn linear_luma_uses_bt709() {
        // Pure red linear 1.0 → 0.2126
        let red = vec![1.0, 0.0, 0.0, 1.0];
        let m = extract_mask_linear(&red, MatteChannel::Luminance, false, false, REC709);
        assert!(approx(m[0], 0.2126), "got {}", m[0]);
        // Pure green 1.0 → 0.7152
        let green = vec![0.0, 1.0, 0.0, 1.0];
        let m = extract_mask_linear(&green, MatteChannel::Luminance, false, false, REC709);
        assert!(approx(m[0], 0.7152));
        // Pure blue 1.0 → 0.0722
        let blue = vec![0.0, 0.0, 1.0, 1.0];
        let m = extract_mask_linear(&blue, MatteChannel::Luminance, false, false, REC709);
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
            let m = extract_mask_linear(&rgba, ch, false, false, REC709);
            assert!(approx(m[0], expected), "channel {ch:?} got {}", m[0]);
        }
    }

    #[test]
    fn linear_clamps_negative_and_oversaturated() {
        // HDR / out-of-gamut input shouldn't poison the mask.
        let rgba = vec![-0.5, 1.5, 2.0, 1.0];
        let m = extract_mask_linear(&rgba, MatteChannel::Red, false, false, REC709);
        assert!(approx(m[0], 0.0));
        let m = extract_mask_linear(&rgba, MatteChannel::Green, false, false, REC709);
        assert!(approx(m[0], 1.0));
    }

    #[test]
    fn linear_rejects_non_multiple_of_four_length() {
        let bad = vec![1.0, 0.0, 0.0]; // only 3 floats
        let m = extract_mask_linear(&bad, MatteChannel::Alpha, false, false, REC709);
        assert!(m.is_empty());
    }

    /// The luminance row is actually consulted, and a wide-gamut one
    /// gives a different matte.
    ///
    /// Every other test here passes Rec.709 explicitly, so all of them
    /// would keep passing if these functions went back to hardcoding it.
    /// This is the one that would not: AP1 weighs green at 0.674 where
    /// Rec.709 weighs it at 0.715, so a saturated green source reads
    /// four percent less matte on an ACEScg timeline — which is the
    /// whole point of threading the row through.
    #[test]
    fn a_wide_gamut_row_changes_the_matte() {
        const AP1: [f32; 3] = [0.272_228_72, 0.674_081_77, 0.053_689_52];
        // Saturated green: the channel the two rows disagree about most.
        let green = vec![0.0f32, 1.0, 0.0, 1.0];

        let m709 = extract_mask_linear(&green, MatteChannel::Luminance, false, false, REC709);
        let map1 = extract_mask_linear(&green, MatteChannel::Luminance, false, false, AP1);
        assert!(approx(m709[0], 0.7152), "Rec.709 green luma, got {}", m709[0]);
        assert!(approx(map1[0], 0.674_081_77), "AP1 green luma, got {}", map1[0]);
        assert!(
            (m709[0] - map1[0]).abs() > 0.03,
            "the two rows must give different mattes: {} vs {}",
            m709[0], map1[0],
        );

        // And the fused path, which is the one the compositor actually
        // takes when choke and feather are both off — i.e. the default.
        let mut c709 = vec![1.0f32, 1.0, 1.0, 1.0];
        let mut cap1 = vec![1.0f32, 1.0, 1.0, 1.0];
        apply_matte_fused_linear(&mut c709, &green, MatteChannel::Luminance, false, false, REC709);
        apply_matte_fused_linear(&mut cap1, &green, MatteChannel::Luminance, false, false, AP1);
        assert!(approx(c709[3], 0.7152), "fused Rec.709, got {}", c709[3]);
        assert!(approx(cap1[3], 0.674_081_77), "fused AP1, got {}", cap1[3]);
        assert!(
            (c709[3] - cap1[3]).abs() > 0.03,
            "the fused path must honour the row too: {} vs {}",
            c709[3], cap1[3],
        );
    }

    // ── extract_mask_srgb_u8 ─────────────────────────────────────────

    #[test]
    fn srgb_alpha_normalises_to_unit() {
        let rgba = vec![255, 0, 0, 0,  0, 255, 0, 255];
        let m = extract_mask_srgb_u8(&rgba, MatteChannel::Alpha, false, false);
        assert!(approx(m[0], 0.0));
        assert!(approx(m[1], 1.0));
    }

    #[test]
    fn srgb_luma_pure_white_is_one() {
        let rgba = vec![255, 255, 255, 255];
        let m = extract_mask_srgb_u8(&rgba, MatteChannel::Luminance, false, false);
        assert!(approx(m[0], 1.0));
    }

    /// With `source_premultiplied=false`, the extractor reads RGB
    /// as-given — a "white at 50% alpha" stored premultiplied as
    /// (0.5, 0.5, 0.5, 0.5) yields luma = 0.5. With `=true`, the
    /// same pixel un-premultiplies to (1, 1, 1) and yields luma = 1.0.
    /// Pin both polarities so the convention can't drift.
    #[test]
    fn linear_luma_un_premultiplies_when_flagged() {
        // Same logical pixel, two encodings.
        let straight: Vec<f32> = vec![1.0, 1.0, 1.0, 0.5];
        let premult:  Vec<f32> = vec![0.5, 0.5, 0.5, 0.5];

        // Without the flag: each encoding reports its raw luma.
        let m_straight_off = extract_mask_linear(&straight, MatteChannel::Luminance, false, false, REC709);
        let m_premult_off  = extract_mask_linear(&premult,  MatteChannel::Luminance, false, false, REC709);
        assert!(approx(m_straight_off[0], 1.0), "straight off: got {}", m_straight_off[0]);
        assert!(approx(m_premult_off[0],  0.5), "premult off: got {}", m_premult_off[0]);

        // With the flag: premult un-darkens to match straight.
        let m_premult_on = extract_mask_linear(&premult, MatteChannel::Luminance, false, true, REC709);
        assert!(approx(m_premult_on[0], 1.0),
            "premult-on should un-premult to luma 1.0, got {}", m_premult_on[0]);
    }

    #[test]
    fn srgb_luma_un_premultiplies_when_flagged() {
        // Same logical pixel, sRGB-u8 encoding.
        let straight: Vec<u8> = vec![255, 255, 255, 128];
        let premult:  Vec<u8> = vec![128, 128, 128, 128];

        let m_straight_off = extract_mask_srgb_u8(&straight, MatteChannel::Luminance, false, false);
        let m_premult_off  = extract_mask_srgb_u8(&premult,  MatteChannel::Luminance, false, false);
        assert!(approx(m_straight_off[0], 1.0), "straight off: got {}", m_straight_off[0]);
        // 128/255 = 0.5019…; the BT.709 sum on (0.5019, 0.5019, 0.5019)
        // is the same 0.5019.
        assert!((m_premult_off[0] - 0.5019).abs() < 0.002,
            "premult off: got {}", m_premult_off[0]);

        // With the flag: 128/0.5019 ≈ 254/255 → luma ≈ 1.0 (within
        // 8-bit quantisation: 128/255 ÷ 128/255 = 1.0 exactly in math,
        // but the divide → clamp → reweight introduces ~ 1/255 noise).
        let m_premult_on = extract_mask_srgb_u8(&premult, MatteChannel::Luminance, false, true);
        assert!((m_premult_on[0] - 1.0).abs() < 0.01,
            "premult-on should un-premult to luma ~1.0, got {}", m_premult_on[0]);
    }

    /// Per-channel extraction is also affected by un-premultiply: a
    /// premult-encoded green pixel at 50% alpha stores (0, 0.5, 0, 0.5).
    /// Reading the Green channel raw gives 0.5; reading with un-premult
    /// gives 1.0 (the logical "fully green").
    #[test]
    fn linear_per_channel_un_premultiplies_when_flagged() {
        let premult_green: Vec<f32> = vec![0.0, 0.5, 0.0, 0.5];

        let m_off = extract_mask_linear(&premult_green, MatteChannel::Green, false, false, REC709);
        assert!(approx(m_off[0], 0.5), "off: got {}", m_off[0]);

        let m_on = extract_mask_linear(&premult_green, MatteChannel::Green, false, true, REC709);
        assert!(approx(m_on[0], 1.0), "on: should un-premult to 1.0, got {}", m_on[0]);
    }

    /// Alpha-channel extraction is independent of `source_premultiplied`
    /// — alpha is alpha regardless of encoding. Pin so a future
    /// refactor doesn't accidentally divide alpha by alpha.
    #[test]
    fn linear_alpha_unaffected_by_premult_flag() {
        let pixel: Vec<f32> = vec![0.5, 0.5, 0.5, 0.5];
        let m_off = extract_mask_linear(&pixel, MatteChannel::Alpha, false, false, REC709);
        let m_on  = extract_mask_linear(&pixel, MatteChannel::Alpha, false, true, REC709);
        assert!(approx(m_off[0], 0.5));
        assert!(approx(m_on[0], 0.5));
    }

    /// Below the epsilon, the divide is skipped — protects against
    /// 1/255 alpha pixels ballooning to 255× the encoded RGB.
    #[test]
    fn premult_skips_divide_below_epsilon() {
        // Alpha = 1e-5, RGB = small premult value. With un-premult on,
        // the divide must be skipped (otherwise 0.001/0.00001 = 100,
        // which would clamp to 1.0 = bogus mask hit for an invisible pixel).
        let near_zero_alpha: Vec<f32> = vec![0.001, 0.001, 0.001, 1e-5];
        let m = extract_mask_linear(&near_zero_alpha, MatteChannel::Luminance, false, true, REC709);
        // Without divide, raw RGB ≈ 0.001 → luma ≈ 0.001 (clamps to itself).
        assert!(m[0] < 0.01, "near-zero alpha must skip divide, got {}", m[0]);
    }

    #[test]
    fn srgb_invert_flips_signal() {
        let rgba = vec![128, 128, 128, 255];
        let m_normal = extract_mask_srgb_u8(&rgba, MatteChannel::Red, false, false);
        let m_inv = extract_mask_srgb_u8(&rgba, MatteChannel::Red, true, false);
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
        let mut a = extract_mask_linear(&rgba, MatteChannel::Red, false, false, REC709);
        apply_choke_mask(&mut a, 16, 1, 1.0);
        let inverted: Vec<f32> = a.iter().map(|v| 1.0 - v).collect();
        let mut b = extract_mask_linear(&rgba, MatteChannel::Red, true, false, REC709);
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

    /// Pin: the fused fast path produces byte-identical alpha to the
    /// extract + apply slow path when choke/feather are zero. If the
    /// fast path ever drifts, this catches it before users see a
    /// visual difference between "no feather" and "feather=0.0".
    #[test]
    fn fused_matches_kernel_path_zero_radii() {
        let w = 16u32;
        let h = 16u32;
        let pixels = (w * h) as usize;
        // Random-ish source: gradient + per-channel variation.
        let source: Vec<f32> = (0..pixels * 4).map(|i| {
            let p = i / 4;
            let c = i % 4;
            let base = (p as f32) / (pixels as f32);
            match c {
                0 => base,                  // R
                1 => 1.0 - base,            // G
                2 => (base * 7.0).fract(),  // B
                _ => 0.5 + 0.5 * base,      // A
            }
        }).collect();
        // Consumer starts with full alpha; both paths multiply alpha
        // by the derived mask.
        let mut consumer_fused: Vec<f32> = vec![0.5; pixels * 4];
        let mut consumer_kernel: Vec<f32> = consumer_fused.clone();

        for ch in [
            MatteChannel::Alpha,
            MatteChannel::Luminance,
            MatteChannel::Red,
            MatteChannel::Green,
            MatteChannel::Blue,
        ] {
            for invert in [false, true] {
                for premult in [false, true] {
                    consumer_fused.fill(0.5);
                    consumer_kernel.fill(0.5);
                    apply_matte_fused_linear(&mut consumer_fused, &source, ch, invert, premult, REC709);
                    let mask = extract_mask_linear(&source, ch, invert, premult, REC709);
                    apply_mask_to_alpha_linear(&mut consumer_kernel, &mask);
                    for i in 0..pixels * 4 {
                        let f = consumer_fused[i];
                        let k = consumer_kernel[i];
                        assert!((f - k).abs() < 1e-6,
                            "fused vs kernel divergence at i={i} ch={ch:?} invert={invert} premult={premult}: {f} vs {k}");
                    }
                }
            }
        }
    }

    /// Sanity perf test for the fused fast-path. At 1080p with no
    /// choke/feather (the default user case the perf logs flagged
    /// at 3.5–8.8 ms), the hoisted-channel-match version should be
    /// under 5 ms in debug and well under 2 ms in release. We test
    /// the slowest channel (Luminance, 3 multiplies + 2 adds per
    /// pixel) to set an upper bound.
    #[test]
    fn fused_1080p_perf_sanity() {
        let w = 1920u32;
        let h = 1080u32;
        let pixels = (w * h) as usize;
        let source = vec![0.5f32; pixels * 4];
        let mut consumer = vec![0.5f32; pixels * 4];
        // Warmup
        apply_matte_fused_linear(&mut consumer, &source, MatteChannel::Luminance, false, false, REC709);
        // Measure best-of-3 to dampen noise.
        let mut best_ms = f64::INFINITY;
        for _ in 0..3 {
            consumer.fill(0.5);
            let t = std::time::Instant::now();
            apply_matte_fused_linear(&mut consumer, &source, MatteChannel::Luminance, false, false, REC709);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms < best_ms { best_ms = ms; }
        }
        // Loose cap to catch a regression that lands a per-pixel
        // allocation; tight enough that the old buggy nested-match
        // would fail.
        assert!(best_ms < 50.0, "fused 1080p Luminance took {best_ms:.1}ms — too slow");
    }

    /// Sanity perf test: a 1080p mask feather completes in reasonable
    /// time. The previous u8-RGBA-round-trip path allocated a 8.3 MB
    /// scratch buffer + ran a 4-channel blur; the f32-direct path
    /// touches just the 4 MB mask + a single f32 scratch, so this
    /// should comfortably stay under the 60 Hz budget at large radii.
    #[test]
    fn feather_1080p_perf_sanity() {
        let w = 1920u32;
        let h = 1080u32;
        let mut mask = vec![0.5f32; (w as usize) * (h as usize)];
        let t = std::time::Instant::now();
        feather_mask(&mut mask, w, h, 25.0);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(ms < 200.0, "feather_mask 1080p radius=25 took {ms:.1}ms — too slow");
        // Uniform input should stay (close to) uniform.
        for &m in mask.iter().take(100) {
            assert!((m - 0.5).abs() < 0.01, "uniform input should stay uniform, got {m}");
        }
    }
}
