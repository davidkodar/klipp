/// Affine + 3D perspective transform via inverse mapping with
/// **bilinear** sampling.
///
/// Why bilinear: with nearest-neighbour, sub-pixel motion of the
/// underlying image (e.g. a credits scroll moving 7.5 px/frame at
/// 24 fps) snaps to integer source coordinates. Some frames advance
/// by 7 px, the next by 8 px, alternating — visible as a low-rate
/// stutter that's especially obvious on long, slow vertical scrolls.
/// Bilinear weights the four nearest source pixels by the fractional
/// part of the inverse-mapped position, so the output advances
/// continuously as `position_y` changes by less than a pixel.
///
/// Same transform code is used by the engine playback path
/// (`engine::update_overlay_decoders`) and the export path
/// (`klipp-render::stages::builtin_effects`), so this fix smooths
/// motion in both program-viewer playback and rendered files.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    position_x: f32,
    position_y: f32,
    scale_x: f32,
    scale_y: f32,
    rotation: f32,
    anchor_x: f32,
    anchor_y: f32,
    rotation_x: f32,
    rotation_y: f32,
    perspective: f32,
) -> Vec<u8> {
    // Identity check
    if position_x == 0.0
        && position_y == 0.0
        && (scale_x - 1.0).abs() < f32::EPSILON
        && (scale_y - 1.0).abs() < f32::EPSILON
        && rotation.abs() < f32::EPSILON
        && rotation_x.abs() < f32::EPSILON
        && rotation_y.abs() < f32::EPSILON
    {
        return rgba;
    }

    // Translation-only fast path: rotation, scale, and 3D rotations
    // are all no-ops — only `position_x` / `position_y` are non-zero.
    // This is overwhelmingly the common case for animated titles
    // (credits scroll, ticker crawl, slide-in), and the general
    // inverse-mapping loop pays for full per-pixel bilinear with
    // four bounds-checked closure samples each — measurably enough
    // cost to drop frames during playback at 1080p.
    //
    // The fast path computes the source row's fractional offset once
    // outside the inner loop and uses two row pointers (top + bottom)
    // for the vertical blend, then a single horizontal lerp inside
    // each pixel. Branching on integer-shift cases (no horizontal
    // and / or no vertical fractional component) lets the hottest
    // case — credits roll with sub-pixel-y, integer-x — collapse to
    // two reads + one lerp per channel, no per-pixel bounds
    // arithmetic.
    let is_translation_only = (scale_x - 1.0).abs() < f32::EPSILON
        && (scale_y - 1.0).abs() < f32::EPSILON
        && rotation.abs() < f32::EPSILON
        && rotation_x.abs() < f32::EPSILON
        && rotation_y.abs() < f32::EPSILON;
    if is_translation_only {
        return apply_translation(rgba, width, height, position_x, position_y);
    }

    let w = width as f32;
    let h = height as f32;
    let ax = anchor_x * w;
    let ay = anchor_y * h;

    let rot_rad = rotation.to_radians();
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();

    // 3D rotation around X and Y axes
    let rx_rad = rotation_x.to_radians();
    let ry_rad = rotation_y.to_radians();
    let cos_rx = rx_rad.cos();
    let sin_rx = rx_rad.sin();
    let cos_ry = ry_rad.cos();
    let sin_ry = ry_rad.sin();

    // Inverse scale
    let inv_sx = if scale_x.abs() > f32::EPSILON { 1.0 / scale_x } else { 1.0 };
    let inv_sy = if scale_y.abs() > f32::EPSILON { 1.0 / scale_y } else { 1.0 };

    // Perspective focal length (higher = less perspective distortion)
    let focal = if perspective > 0.001 { w / perspective } else { 0.0 };
    let has_3d = rotation_x.abs() > 0.01 || rotation_y.abs() > 0.01;

    let mut output = vec![0u8; rgba.len()];

    for oy in 0..height {
        for ox in 0..width {
            let dx = ox as f32 - ax - position_x;
            let dy = oy as f32 - ay - position_y;

            // Inverse Z-rotation (2D)
            let rx = dx * cos_r + dy * sin_r;
            let ry = -dx * sin_r + dy * cos_r;

            // Inverse scale
            let mut sx = rx * inv_sx;
            let mut sy = ry * inv_sy;

            // 3D projection: apply inverse 3D rotations
            if has_3d {
                if focal > 0.0 {
                    // Perspective projection: unproject from screen to 3D, then inverse rotate
                    // Forward: p3d = Ry * Rx * [sx, sy, 0], then project: px = focal*x/z, py = focal*y/z
                    // Inverse: we solve for source coords given output coords
                    // Simplified: apply forward 3D rotation to get the source sampling point
                    let z = 0.0f32;
                    // Rotate around X (tilt)
                    let y1 = sy * cos_rx - z * sin_rx;
                    let z1 = sy * sin_rx + z * cos_rx;
                    // Rotate around Y (pan)
                    let x2 = sx * cos_ry + z1 * sin_ry;
                    let z2 = -sx * sin_ry + z1 * cos_ry;
                    // Perspective divide
                    let w_factor = 1.0 + z2 / focal;
                    if w_factor > 0.01 {
                        sx = x2 / w_factor;
                        sy = y1 / w_factor;
                    }
                } else {
                    // Orthographic 3D rotation (no perspective)
                    let y1 = sy * cos_rx;
                    let x2 = sx * cos_ry;
                    sx = x2;
                    sy = y1;
                }
            }

            sx += ax;
            sy += ay;

            // Bilinear sample: read the four pixels surrounding the
            // fractional source position and blend by the fractional
            // offset. Out-of-bounds neighbours read as transparent
            // (0,0,0,0) so the edges fade into the background instead
            // of clamping a hard line of source-edge colour.
            //
            // Premultiplication caveat: blending unpremultiplied RGBA
            // (the input shape from the title/render path) at the
            // alpha boundary fades RGB toward (0,0,0) when alpha
            // drops, since transparent neighbours contribute their
            // RGB=0 to the blend. For the common case — anti-aliased
            // text on transparent bg — this matches the visual
            // expectation (the soft edge stays the colour of the text
            // because all opaque neighbours share that colour). The
            // sRGB-vs-linear approximation is left in place: a 1-pixel
            // motion boundary in sRGB space is below perceptual
            // threshold; converting in the hot loop would dominate
            // the cost.
            let x0 = sx.floor() as i32;
            let y0 = sy.floor() as i32;
            let x1 = x0 + 1;
            let y1 = y0 + 1;
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;

            let sample = |x: i32, y: i32| -> [f32; 4] {
                if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                    return [0.0; 4];
                }
                let idx = (y as u32 * width + x as u32) as usize * 4;
                [
                    rgba[idx] as f32,
                    rgba[idx + 1] as f32,
                    rgba[idx + 2] as f32,
                    rgba[idx + 3] as f32,
                ]
            };
            let p00 = sample(x0, y0);
            let p10 = sample(x1, y0);
            let p01 = sample(x0, y1);
            let p11 = sample(x1, y1);

            let dst_idx = (oy * width + ox) as usize * 4;
            for c in 0..4 {
                let top = p00[c] * (1.0 - fx) + p10[c] * fx;
                let bot = p01[c] * (1.0 - fx) + p11[c] * fx;
                let v = top * (1.0 - fy) + bot * fy;
                output[dst_idx + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    output
}

/// Translation-only transform: shift the whole image by
/// `(position_x, position_y)` with bilinear sub-pixel interpolation
/// at fractional offsets, falling out to a row-copy when the offset
/// is integer. Replaces the per-pixel inverse-mapping inner loop
/// with two row pointers and one lerp per channel — measurably
/// faster, exact same result.
fn apply_translation(rgba: Vec<u8>, width: u32, height: u32, position_x: f32, position_y: f32) -> Vec<u8> {
    let w = width as i32;
    let h = height as i32;
    let stride = (width as usize) * 4;
    let mut output = vec![0u8; rgba.len()];

    // Source position for output pixel (ox, oy) is (ox - position_x,
    // oy - position_y). The fractional part is constant across the
    // whole frame, so pre-compute it.
    let sx_for = |ox: i32| ox as f32 - position_x;
    let sy_for = |oy: i32| oy as f32 - position_y;

    // Vertical fractional component (constant for the frame).
    let any_y = sy_for(0);
    let fy = any_y - any_y.floor();
    let fy_int = fy.abs() < f32::EPSILON;

    // Horizontal fractional component (constant for the frame).
    let any_x = sx_for(0);
    let fx = any_x - any_x.floor();
    let fx_int = fx.abs() < f32::EPSILON;

    // Pure integer shift — collapse to row-range memcpy. This is the
    // ideal case (no per-pixel arithmetic at all) and naturally
    // covers any keyframe combination that lands on integer-pixel
    // motion per frame — for a credits clip whose travel divides the
    // frame count evenly, every render frame falls here.
    if fx_int && fy_int {
        let shift_x = position_x.round() as i32;
        let shift_y = position_y.round() as i32;
        for oy in 0..h {
            let src_y = oy - shift_y;
            if src_y < 0 || src_y >= h {
                continue;
            }
            let dst_row_off = (oy as usize) * stride;
            let src_row_off = (src_y as usize) * stride;
            // Copy the overlapping x-range only. shift_x > 0 → image
            // shifts right; shift_x < 0 → shifts left. Compute the
            // intersection of [0, w) and [shift_x, shift_x + w) on
            // the destination axis.
            let dst_lo = shift_x.max(0);
            let dst_hi = (shift_x + w).min(w);
            if dst_lo >= dst_hi {
                continue;
            }
            let src_lo = dst_lo - shift_x;
            let copy_len = ((dst_hi - dst_lo) as usize) * 4;
            let dst_off = dst_row_off + (dst_lo as usize) * 4;
            let src_off = src_row_off + (src_lo as usize) * 4;
            output[dst_off..dst_off + copy_len]
                .copy_from_slice(&rgba[src_off..src_off + copy_len]);
        }
        return output;
    }

    for oy in 0..h {
        let src_y_f = sy_for(oy);
        let y0 = src_y_f.floor() as i32;
        let y1 = if fy_int { y0 } else { y0 + 1 };
        let y0_in = y0 >= 0 && y0 < h;
        let y1_in = y1 >= 0 && y1 < h;
        let dst_row = (oy as usize) * stride;

        if !y0_in && !y1_in {
            // Whole row is out of bounds — already zero-initialized.
            continue;
        }

        let row0_off = if y0_in { (y0 as usize) * stride } else { usize::MAX };
        let row1_off = if y1_in { (y1 as usize) * stride } else { usize::MAX };

        for ox in 0..w {
            let src_x_f = sx_for(ox);
            let x0 = src_x_f.floor() as i32;
            let x1 = if fx_int { x0 } else { x0 + 1 };
            let x0_in = x0 >= 0 && x0 < w;
            let x1_in = x1 >= 0 && x1 < w;
            if !x0_in && !x1_in {
                // Out of bounds — leave zero.
                continue;
            }

            let dst = dst_row + (ox as usize) * 4;

            // Read whole pixel (4 bytes) per corner — better cache
            // line use than per-channel reads, and the compiler can
            // typically promote the 4-element array to a register.
            #[inline(always)]
            fn read_pixel(rgba: &[u8], row_off: usize, x: i32, x_in: bool) -> [u8; 4] {
                if !x_in || row_off == usize::MAX {
                    return [0, 0, 0, 0];
                }
                let i = row_off + (x as usize) * 4;
                [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
            }

            let p00 = read_pixel(&rgba, row0_off, x0, x0_in);
            let p10 = read_pixel(&rgba, row0_off, x1, x1_in);
            let p01 = read_pixel(&rgba, row1_off, x0, x0_in);
            let p11 = read_pixel(&rgba, row1_off, x1, x1_in);

            for ch in 0..4 {
                let top = (p00[ch] as f32) * (1.0 - fx) + (p10[ch] as f32) * fx;
                let bot = (p01[ch] as f32) * (1.0 - fx) + (p11[ch] as f32) * fx;
                let v = top * (1.0 - fy) + bot * fy;
                output[dst + ch] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_passthrough() {
        let input = vec![255u8; 16]; // 2x2 white
        let result = apply(input.clone(), 2, 2, 0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0);
        assert_eq!(result, input);
    }

    /// At a half-pixel position offset, a sharp black/white edge in
    /// the source should produce a 50%-grey blended output pixel —
    /// the canonical bilinear sub-pixel result. With the old
    /// nearest-neighbour code this returned either pure black or
    /// pure white depending on rounding direction, which is what
    /// produced the credits-scroll micro-stutter.
    #[test]
    fn half_pixel_shift_blends_edge() {
        // 4x1 strip: black, black, white, white.
        let input = vec![
            0, 0, 0, 255,
            0, 0, 0, 255,
            255, 255, 255, 255,
            255, 255, 255, 255,
        ];
        // Shift by +0.5 pixels in X — the boundary between pixel 1
        // and pixel 2 lands on the centre of output pixel 2.
        let out = apply(
            input, 4, 1,
            0.5, 0.0,         // position
            1.0, 1.0,         // scale
            0.0,              // rotation
            0.0, 0.0,         // anchor (0,0 → no anchor offset)
            0.0, 0.0, 0.0,    // 3D / perspective
        );
        // Output pixel index 2: source position = 2 - 0.5 = 1.5
        // → 50% blend of pixel 1 (black) and pixel 2 (white) = grey.
        let p2 = &out[2 * 4..2 * 4 + 4];
        assert!((p2[0] as i16 - 128).abs() <= 1, "R={}", p2[0]);
        assert!((p2[1] as i16 - 128).abs() <= 1, "G={}", p2[1]);
        assert!((p2[2] as i16 - 128).abs() <= 1, "B={}", p2[2]);
    }

    /// Integer translation should hit the row-range memcpy fast
    /// path: pixels are shifted exactly without any blend. Verifies
    /// the per-row intersection arithmetic is correct.
    #[test]
    fn integer_shift_uses_memcpy_path() {
        // 4×1 strip: distinct per-pixel colours so we can check
        // exactly which source pixel ends up in each output slot.
        let input = vec![
            10, 20, 30, 255,
            40, 50, 60, 255,
            70, 80, 90, 255,
            100, 110, 120, 255,
        ];
        // Shift right by 1 px (anchor 0) → output[1..] holds
        // input[0..3], output[0] stays zero.
        let out = apply(
            input.clone(), 4, 1,
            1.0, 0.0,         // integer translation
            1.0, 1.0,         // scale
            0.0,              // rotation
            0.0, 0.0,         // anchor (0,0)
            0.0, 0.0, 0.0,    // 3D / perspective
        );
        assert_eq!(&out[0..4], &[0, 0, 0, 0], "output pixel 0 must be zero");
        assert_eq!(&out[4..8], &[10, 20, 30, 255], "output pixel 1 = input pixel 0");
        assert_eq!(&out[8..12], &[40, 50, 60, 255], "output pixel 2 = input pixel 1");
        assert_eq!(&out[12..16], &[70, 80, 90, 255], "output pixel 3 = input pixel 2");
    }

    /// Sub-pixel motion should produce a *continuous* sequence of
    /// output values across small position changes. Pin the
    /// regression: shifting by 0.0, 0.25, 0.5, 0.75 px must produce
    /// monotonically interpolated edge intensity at the boundary
    /// pixel, never the same value twice in a row (which is exactly
    /// what nearest-neighbour did, and what the user reported as
    /// stuttery scroll).
    #[test]
    fn sub_pixel_motion_is_continuous() {
        // Sharp edge between pixel 1 (black) and pixel 2 (white).
        let input = vec![
            0, 0, 0, 255,
            0, 0, 0, 255,
            255, 255, 255, 255,
            255, 255, 255, 255,
        ];
        let mut last = -1i16;
        for &shift in &[0.0_f32, 0.25, 0.5, 0.75] {
            let out = apply(
                input.clone(), 4, 1,
                shift, 0.0,
                1.0, 1.0,
                0.0,
                0.0, 0.0,
                0.0, 0.0, 0.0,
            );
            // Output pixel 2 at shift=0.0 reads src 2.0 (white=255);
            // at shift=0.75 reads src 1.25 (≈25% black + 75% white).
            let v = out[2 * 4] as i16;
            // Strictly decreasing as shift grows — no plateaus.
            if last >= 0 {
                assert!(v < last, "shift={shift}: v={v} >= last={last}");
            }
            last = v;
        }
    }
}
