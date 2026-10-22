//! Transition easing, and the original 8-bit CPU pixel math.
//!
//! [`apply_easing`] is what the render path uses: `klipp-compose` calls
//! it for every transition, on the GPU path and the CPU one alike.
//!
//! The byte-buffer renderers here are the earlier CPU fallback. The one
//! that runs today is `klipp_effects::render_transition_linear`, which
//! works in linear light and is generated from the same shader body the
//! GPU runs, so the two agree. Nothing in the render path calls
//! [`render_transition_with_params`] any more.

use klipp_timeline::TransitionKind;
use rayon::prelude::*;

/// Parallel per-pixel CPU transition helper.
///
/// Replaces the single-threaded `for y in 0..height` pattern that
/// every CPU transition arm used to write. Each row is processed on
/// rayon's thread pool, so an 8-core CPU completes a 1080p barn-door
/// in ~3 ms instead of the 20-30 ms the serial path took. The
/// per-pixel closure receives `(x, y)` and returns the blend factor
/// `t` (0.0 = from, 1.0 = to); the helper handles the buffer math
/// and `blend_pixel` for both u8 and linear-f32 outputs.
///
/// `t_for` must be `Sync` so the row chunks can fan out across
/// threads. The closure is captured by reference; per-arm state
/// (softness, threshold, centre coords) is captured by value or
/// borrow — all the existing arms' state is `Copy` or `&str`,
/// which satisfies `Sync` trivially.
fn parallel_blend_u8_rows<F>(
    from: &[u8],
    to: &[u8],
    width: u32,
    height: u32,
    t_for: F,
) -> Vec<u8>
where
    F: Fn(u32, u32) -> f32 + Sync,
{
    let stride = (width as usize) * 4;
    let total = stride * (height as usize);
    let mut output = vec![0u8; total];
    output
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            let from_row = &from[(y as usize) * stride..(y as usize) * stride + stride];
            let to_row = &to[(y as usize) * stride..(y as usize) * stride + stride];
            for x in 0..width {
                let i = (x as usize) * 4;
                let t = t_for(x, y);
                blend_pixel(&from_row[i..i + 4], &to_row[i..i + 4], t, &mut row[i..i + 4]);
            }
        });
    output
}

fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear blend of two RGBA pixels into `out`. `t=0` → from, `t=1` → to.
fn blend_pixel(from: &[u8], to: &[u8], t: f32, out: &mut [u8]) {
    let inv = 1.0 - t;
    out[0] = (from[0] as f32 * inv + to[0] as f32 * t + 0.5) as u8;
    out[1] = (from[1] as f32 * inv + to[1] as f32 * t + 0.5) as u8;
    out[2] = (from[2] as f32 * inv + to[2] as f32 * t + 0.5) as u8;
    out[3] = (from[3] as f32 * inv + to[3] as f32 * t + 0.5) as u8;
}

/// Remap a linear `[0,1]` `t` through an easing curve selected by `curve` (0..=4).
/// Used by CrossDissolve for user-selectable easing. Exposed so the engine's
/// GPU path can reshape progress before dispatching the shader.
pub fn apply_easing(t: f32, curve: f32) -> f32 {
    match curve as u32 {
        1 => t * t,                                                 // Ease In
        2 => t * (2.0 - t),                                         // Ease Out
        3 => if t < 0.5 { 2.0 * t * t } else { -1.0 + (4.0 - 2.0 * t) * t }, // In-Out
        4 => (-(t * std::f32::consts::PI).cos() + 1.0) * 0.5,       // Sine
        _ => t,                                                     // Linear
    }
}

pub fn render_transition_with_params(
    kind: TransitionKind,
    from: &[u8],
    to: &[u8],
    progress: f32,
    width: u32,
    height: u32,
    user_params: &klipp_core::TransitionUserParams,
) -> Vec<u8> {
    // CrossDissolve: reshape progress via user-selected easing curve.
    let progress = if matches!(kind, TransitionKind::CrossDissolve) {
        apply_easing(progress, user_params.param0)
    } else {
        progress
    };
    let pixel_count = (width * height) as usize;
    let byte_count = pixel_count * 4;
    debug_assert_eq!(from.len(), byte_count);
    debug_assert_eq!(to.len(), byte_count);

    let mut output = vec![0u8; byte_count];
    let p = progress.clamp(0.0, 1.0);

    match kind {
        TransitionKind::CrossDissolve => {
            // Constant t per pixel — embarrassingly parallel byte-wise.
            // par_chunks_mut on bytes (1024-byte chunks) gets clean
            // SIMD inside the closure and even thread-saturation on
            // multi-core machines for 1080p+.
            let inv = 1.0 - p;
            output
                .par_chunks_mut(1024)
                .enumerate()
                .for_each(|(chunk_idx, chunk)| {
                    let base = chunk_idx * 1024;
                    for (i, out_b) in chunk.iter_mut().enumerate() {
                        let src_i = base + i;
                        if src_i >= byte_count { break; }
                        *out_b = (from[src_i] as f32 * inv + to[src_i] as f32 * p + 0.5) as u8;
                    }
                });
        }
        TransitionKind::WipeLeft => {
            let feather = user_params.param0.max(0.001);
            let threshold = p * (width as f32 + 2.0 * feather) - feather;
            output = parallel_blend_u8_rows(from, to, width, height, |x, _y| {
                1.0 - smoothstep_f32(threshold - feather, threshold + feather, x as f32)
            });
        }
        TransitionKind::WipeRight => {
            let feather = user_params.param0.max(0.001);
            let threshold = (1.0 - p) * (width as f32 + 2.0 * feather) - feather;
            output = parallel_blend_u8_rows(from, to, width, height, |x, _y| {
                smoothstep_f32(threshold - feather, threshold + feather, x as f32)
            });
        }
        TransitionKind::WipeUp => {
            let feather = user_params.param0.max(0.001);
            let threshold = p * (height as f32 + 2.0 * feather) - feather;
            output = parallel_blend_u8_rows(from, to, width, height, |_x, y| {
                1.0 - smoothstep_f32(threshold - feather, threshold + feather, y as f32)
            });
        }
        TransitionKind::WipeDown => {
            let feather = user_params.param0.max(0.001);
            let threshold = (1.0 - p) * (height as f32 + 2.0 * feather) - feather;
            output = parallel_blend_u8_rows(from, to, width, height, |_x, y| {
                smoothstep_f32(threshold - feather, threshold + feather, y as f32)
            });
        }
        TransitionKind::DipToBlack | TransitionKind::DipToWhite => {
            // Unified dip-to-colour. param0 = hold duration at midpoint,
            // color = dip colour (default black for DipToBlack, white for DipToWhite).
            let dip_r = (user_params.color[0] * 255.0).clamp(0.0, 255.0);
            let dip_g = (user_params.color[1] * 255.0).clamp(0.0, 255.0);
            let dip_b = (user_params.color[2] * 255.0).clamp(0.0, 255.0);
            let dip_pixel = [dip_r as u8, dip_g as u8, dip_b as u8, 255u8];
            let hold = user_params.param0.clamp(0.0, 0.45);
            let mid_start = 0.5 - hold * 0.5;
            let mid_end = 0.5 + hold * 0.5;
            for i in (0..byte_count).step_by(4) {
                if p <= mid_start {
                    let t = p / mid_start.max(0.001);
                    blend_pixel(&from[i..i + 4], &dip_pixel, t, &mut output[i..i + 4]);
                } else if p >= mid_end {
                    let t = (p - mid_end) / (1.0 - mid_end).max(0.001);
                    blend_pixel(&dip_pixel, &to[i..i + 4], t, &mut output[i..i + 4]);
                } else {
                    output[i..i + 4].copy_from_slice(&dip_pixel);
                }
            }
        }
        TransitionKind::SmoothCut => {
            // The morph follows the motion between the two shots' frames,
            // which the renderer holds (#2219); with only two pixels to go
            // on, this 8-bit renderer draws it as the dissolve it falls
            // back to.
            output = render_transition_with_params(TransitionKind::CrossDissolve, from, to, p, width, height, &klipp_core::TransitionUserParams::default());
        }
        TransitionKind::SlideStrips | TransitionKind::SlidePanels | TransitionKind::PartHalves | TransitionKind::Shatter => {
            // Nor are the piece transitions (#985): here they draw as the
            // push their pieces travel like, eased.
            output = render_transition_with_params(TransitionKind::PushLeft, from, to, p * p * (3.0 - 2.0 * p), width, height, &klipp_core::TransitionUserParams::default());
        }
        TransitionKind::WhipPan => {
            // This 8-bit renderer is not the render path (see the crate
            // docs): a whip draws here as the push it is built on, eased,
            // without the blur the shader adds.
            output = render_transition_with_params(TransitionKind::PushLeft, from, to, p * p * (3.0 - 2.0 * p), width, height, user_params);
        }
        TransitionKind::PushLeft => {
            // Push: hard slide, no blending. param0 = direction.
            // 0 = Left (default), 1 = Right, 2 = Up, 3 = Down
            let dir = user_params.param0.round() as i32;
            let w = width as i32;
            let h = height as i32;
            match dir {
                1 => {
                    // Push Right
                    let offset = (p * w as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_x = x - offset;
                            let (src, sx) = if src_x >= 0 {
                                (from, src_x)
                            } else {
                                (to, src_x + w)
                            };
                            let src_idx = (y * w + sx) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                2 => {
                    // Push Up
                    let offset = (p * h as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_y = y + offset;
                            let (src, sy) = if src_y < h {
                                (from, src_y)
                            } else {
                                (to, src_y - h)
                            };
                            let src_idx = (sy * w + x) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                3 => {
                    // Push Down
                    let offset = (p * h as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_y = y - offset;
                            let (src, sy) = if src_y >= 0 {
                                (from, src_y)
                            } else {
                                (to, src_y + h)
                            };
                            let src_idx = (sy * w + x) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                _ => {
                    // Push Left (default — matches the original PushLeft behaviour)
                    let offset = (p * w as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_x = x + offset;
                            let (src, sx) = if src_x < w {
                                (from, src_x)
                            } else {
                                (to, src_x - w)
                            };
                            let src_idx = (y * w + sx) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
            }
        }
        TransitionKind::FilmDissolve => {
            // Dissolve with perceptual gamma curve (looks more natural than linear)
            // Uses sqrt blending for a film-like optical dissolve
            let t_sqrt = p.sqrt();
            let inv_sqrt = (1.0 - p).sqrt();
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let f = from[i + c] as f32 / 255.0;
                    let t = to[i + c] as f32 / 255.0;
                    let blended = (f * f * (1.0 - p) + t * t * p).sqrt();
                    output[i + c] = (blended * 255.0 + 0.5).min(255.0) as u8;
                }
                output[i + 3] = (from[i + 3] as f32 * inv_sqrt + to[i + 3] as f32 * t_sqrt + 0.5) as u8;
            }
        }
        TransitionKind::BlurDissolve => {
            // Cross dissolve with simulated blur: blend + average with neighbors
            // CPU approximation: box-blur-like blend weighted by progress
            let inv = 1.0 - p;
            // First pass: standard dissolve
            for i in 0..byte_count {
                output[i] = (from[i] as f32 * inv + to[i] as f32 * p + 0.5) as u8;
            }
            // Second pass: horizontal box blur (radius proportional to progress)
            // param0 = max blur radius in pixels
            let max_radius = user_params.param0.max(1.0);
            let radius = (p * (1.0 - p) * 4.0 * max_radius) as i32; // peaks at p=0.5
            if radius > 0 {
                let temp = output.clone();
                for y in 0..height as i32 {
                    for x in 0..width as i32 {
                        let idx = (y * width as i32 + x) as usize * 4;
                        let mut r = 0u32;
                        let mut g = 0u32;
                        let mut b = 0u32;
                        let mut count = 0u32;
                        for dx in -radius..=radius {
                            let sx = (x + dx).clamp(0, width as i32 - 1);
                            let si = (y * width as i32 + sx) as usize * 4;
                            r += temp[si] as u32;
                            g += temp[si + 1] as u32;
                            b += temp[si + 2] as u32;
                            count += 1;
                        }
                        output[idx] = (r / count) as u8;
                        output[idx + 1] = (g / count) as u8;
                        output[idx + 2] = (b / count) as u8;
                        output[idx + 3] = temp[idx + 3];
                    }
                }
            }
        }
        TransitionKind::Flash => {
            // Bright colour flash with parameterized peak.
            // param0 = intensity, param1 = peak position, color = flash colour.
            let flash_r = (user_params.color[0] * 255.0).clamp(0.0, 255.0);
            let flash_g = (user_params.color[1] * 255.0).clamp(0.0, 255.0);
            let flash_b = (user_params.color[2] * 255.0).clamp(0.0, 255.0);
            let intensity = user_params.param0.clamp(0.0, 2.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let dissolve_start = peak * 0.8;

            let flash_t = if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            };
            let flash_t = (flash_t * intensity).clamp(0.0, 1.0);
            let dissolve_t = ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001))
                .clamp(0.0, 1.0);

            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * (1.0 - dissolve_t) + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * (1.0 - dissolve_t) + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * (1.0 - dissolve_t) + to[i + 2] as f32 * dissolve_t;
                let base_a = from[i + 3] as f32 * (1.0 - dissolve_t) + to[i + 3] as f32 * dissolve_t;
                output[i] = (base_r * (1.0 - flash_t) + flash_r * flash_t + 0.5).min(255.0) as u8;
                output[i + 1] = (base_g * (1.0 - flash_t) + flash_g * flash_t + 0.5).min(255.0) as u8;
                output[i + 2] = (base_b * (1.0 - flash_t) + flash_b * flash_t + 0.5).min(255.0) as u8;
                output[i + 3] = base_a as u8;
            }
        }
        TransitionKind::Spin => {
            // Rotation dissolve: sample pixels with rotation transform.
            // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount.
            let rotations = user_params.param0.max(0.05);
            let zoom_amount = user_params.param1.clamp(0.0, 1.0);
            let angle = p * rotations * std::f32::consts::PI * 2.0;
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let cos_a = angle.cos();
            let sin_a = angle.sin();
            let scale = 1.0 + (p * (1.0 - p) * 4.0) * zoom_amount;
            let inv_scale = 1.0 / scale;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    // Rotate around center
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let rx = (dx * cos_a + dy * sin_a) * inv_scale + cx;
                    let ry = (-dx * sin_a + dy * cos_a) * inv_scale + cy;
                    let sx = rx.round().clamp(0.0, width as f32 - 1.0) as u32;
                    let sy = ry.round().clamp(0.0, height as f32 - 1.0) as u32;
                    let src_idx = (sy * width + sx) as usize * 4;
                    // Blend rotated from with to
                    let inv = 1.0 - p;
                    for c in 0..4 {
                        output[idx + c] = (from[src_idx + c] as f32 * inv + to[idx + c] as f32 * p + 0.5) as u8;
                    }
                }
            }
        }
        TransitionKind::LightLeak => {
            // Additive coloured glow that peaks at midpoint.
            // param0 = intensity, color = leak colour (default warm amber).
            let leak_r = user_params.color[0] * 255.0;
            let leak_g = user_params.color[1] * 255.0;
            let leak_b = user_params.color[2] * 255.0;
            let intensity = user_params.param0.clamp(0.0, 2.0);
            let leak_t = (p * (1.0 - p) * 4.0).powf(0.7) * intensity;
            let inv = 1.0 - p;
            // Pure additive — leak colour added uniformly so non-amber leaks look right.
            let add_r = leak_r * leak_t;
            let add_g = leak_g * leak_t;
            let add_b = leak_b * leak_t;
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 * inv + to[i] as f32 * p;
                let g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                output[i] = (r + add_r + 0.5).min(255.0) as u8;
                output[i + 1] = (g + add_g + 0.5).min(255.0) as u8;
                output[i + 2] = (b + add_b + 0.5).min(255.0) as u8;
                output[i + 3] = (from[i + 3] as f32 * inv + to[i + 3] as f32 * p + 0.5) as u8;
            }
        }
        TransitionKind::LuminanceDissolve => {
            // Bright pixels of `from` dissolve to `to` first, dark pixels last.
            // param0 = softness (default 1.0, range 0.0-3.0)
            let softness = (user_params.param0 * 0.15).max(0.01);
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 / 255.0;
                let g = from[i + 1] as f32 / 255.0;
                let b = from[i + 2] as f32 / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let t = smoothstep_f32(p - softness, p + softness, luma);
                output[i] = (from[i] as f32 * (1.0 - t) + to[i] as f32 * t + 0.5) as u8;
                output[i + 1] = (from[i + 1] as f32 * (1.0 - t) + to[i + 1] as f32 * t + 0.5) as u8;
                output[i + 2] = (from[i + 2] as f32 * (1.0 - t) + to[i + 2] as f32 * t + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::CircleWipeIn | TransitionKind::CircleWipeOut => {
            // Circle expanding (In) or contracting (Out) from a normalized center.
            // param0 = softness, param1 = cx (0..1), param2 = cy (0..1)
            let cx = user_params.param1 * width as f32;
            let cy = user_params.param2 * height as f32;
            let max_r = ((cx.max(width as f32 - cx)).powi(2)
                + (cy.max(height as f32 - cy)).powi(2))
                .sqrt()
                .max(1.0);
            let softness = (user_params.param0 * 0.04).max(0.001);
            let is_in = matches!(kind, TransitionKind::CircleWipeIn);
            // Extend threshold past [0,1] so endpoints fully cover through the soft edge.
            let radius = if is_in {
                p * (1.0 + 2.0 * softness) - softness
            } else {
                (1.0 - p) * (1.0 + 2.0 * softness) - softness
            };
            output = parallel_blend_u8_rows(from, to, width, height, |x, y| {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt() / max_r;
                let s_val = smoothstep_f32(radius - softness, radius + softness, dist);
                if is_in { 1.0 - s_val } else { s_val }
            });
        }
        TransitionKind::ZoomInDissolve
        | TransitionKind::ZoomOutDissolve
        | TransitionKind::CrossZoom => {
            // Zoom sampling family: scale from/to around frame center.
            let zoom_amount = user_params.param0.max(1.001);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let eased = if matches!(kind, TransitionKind::CrossZoom)
                && user_params.param1 > 0.5
            {
                p * p * (3.0 - 2.0 * p)
            } else {
                p
            };
            let inv_e = 1.0 - eased;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let (fsx, fsy, tsx, tsy) = match kind {
                        TransitionKind::ZoomInDissolve => {
                            let scale = 1.0 + (zoom_amount - 1.0) * eased;
                            let fsx =
                                (dx / scale + cx).clamp(0.0, width as f32 - 1.0);
                            let fsy =
                                (dy / scale + cy).clamp(0.0, height as f32 - 1.0);
                            (fsx, fsy, x as f32, y as f32)
                        }
                        TransitionKind::ZoomOutDissolve => {
                            let scale = zoom_amount - (zoom_amount - 1.0) * eased;
                            let fsx =
                                (dx / scale + cx).clamp(0.0, width as f32 - 1.0);
                            let fsy =
                                (dy / scale + cy).clamp(0.0, height as f32 - 1.0);
                            (fsx, fsy, x as f32, y as f32)
                        }
                        _ => {
                            let scale_a = 1.0 + (zoom_amount - 1.0) * eased;
                            let scale_b = zoom_amount - (zoom_amount - 1.0) * eased;
                            let fsx = (dx / scale_a + cx)
                                .clamp(0.0, width as f32 - 1.0);
                            let fsy = (dy / scale_a + cy)
                                .clamp(0.0, height as f32 - 1.0);
                            let tsx = (dx / scale_b + cx)
                                .clamp(0.0, width as f32 - 1.0);
                            let tsy = (dy / scale_b + cy)
                                .clamp(0.0, height as f32 - 1.0);
                            (fsx, fsy, tsx, tsy)
                        }
                    };
                    let fsi = (fsy as usize * w + fsx as usize) * 4;
                    let tsi = (tsy as usize * w + tsx as usize) * 4;
                    for c in 0..3 {
                        output[fi + c] = (from[fsi + c] as f32 * inv_e
                            + to[tsi + c] as f32 * eased
                            + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::BokehDissolve => {
            // Out-of-focus bokeh bloom with chromatic fringe.
            // param0 = bokeh size, param1 = brightness boost
            let bokeh_size = user_params.param0;
            let brightness_boost = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let base_r = from[fi] as f32 * inv + to[fi] as f32 * p;
                    let base_g = from[fi + 1] as f32 * inv + to[fi + 1] as f32 * p;
                    let base_b = from[fi + 2] as f32 * inv + to[fi + 2] as f32 * p;
                    let luma =
                        (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                    let bloom_strength =
                        luma * bokeh_size * envelope * brightness_boost * 2.0;
                    let bloom_val = bloom_strength * 255.0;
                    // Screen blend
                    let screen_r =
                        255.0 - (255.0 - base_r) * (255.0 - bloom_val) / 255.0;
                    let screen_g =
                        255.0 - (255.0 - base_g) * (255.0 - bloom_val) / 255.0;
                    let screen_b =
                        255.0 - (255.0 - base_b) * (255.0 - bloom_val) / 255.0;
                    // Chromatic fringe
                    let dx = (x as f32 - cx) / cx.max(1.0);
                    let dy = (y as f32 - cy) / cy.max(1.0);
                    let radial = (dx * dx + dy * dy).sqrt();
                    let fringe = radial * bokeh_size * envelope * 8.0;
                    output[fi] = (screen_r + fringe).clamp(0.0, 255.0) as u8;
                    output[fi + 1] = screen_g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = (screen_b - fringe).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::AnamorphicFlare => {
            // Horizontal lens flare streak sweeping across at the cut.
            // param0 = intensity, param1 = streak width, color = flare color
            let intensity = user_params.param0;
            let streak_width = user_params.param1;
            let flare_r = user_params.color[0] * 255.0;
            let flare_g = user_params.color[1] * 255.0;
            let flare_b = user_params.color[2] * 255.0;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let base_r = from[fi] as f32 * inv + to[fi] as f32 * p;
                    let base_g = from[fi + 1] as f32 * inv + to[fi + 1] as f32 * p;
                    let base_b = from[fi + 2] as f32 * inv + to[fi + 2] as f32 * p;
                    let ny = (y as f32 / height as f32 - 0.5).abs() * 2.0;
                    let streak_t =
                        (1.0 - ny / streak_width.max(0.001)).max(0.0);
                    let streak_t2 = streak_t * streak_t;
                    let shimmer = 0.7
                        + 0.3
                            * (x as f32 / width as f32 * std::f32::consts::PI * 8.0)
                                .sin();
                    let flare_strength = streak_t2 * envelope * shimmer;
                    output[fi] =
                        (base_r + flare_r * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 1] =
                        (base_g + flare_g * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 2] =
                        (base_b + flare_b * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::DreamSequence => {
            // Heavy bloom + warm glow + overexposure — dreamy look.
            // param0 = bloom intensity, param1 = warmth
            let bloom_intensity = user_params.param0;
            let warmth = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma =
                    (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                let bloom = luma * bloom_intensity * envelope * 60.0;
                let warm_r = warmth * envelope * 30.0;
                let warm_g = warmth * envelope * 15.0;
                let warm_b = -warmth * envelope * 10.0;
                let lift = envelope * bloom_intensity * 15.0;
                output[i] =
                    (base_r + bloom + warm_r + lift).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + bloom + warm_g + lift).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + bloom + warm_b + lift).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::ProjectorFlicker => {
            // Old film projector: brightness flicker, gate weave, frame jump.
            // param0 = flicker intensity, param1 = gate weave px
            let flicker_intensity = user_params.param0;
            let weave_amount = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin() * flicker_intensity;
            let frame_hash =
                ((p * 127.1).sin() * 43758.545).fract().abs();
            let frame_hash2 =
                ((p * 311.7).sin() * 12345.678).fract().abs();
            let frame_hash3 =
                ((p * 543.2).sin() * 98765.432).fract().abs();
            let brightness = 1.0 + (frame_hash - 0.5) * envelope * 0.6;
            let weave = ((frame_hash2 - 0.5) * weave_amount * envelope) as isize;
            let jump = if frame_hash3 < envelope * 0.1 {
                ((frame_hash3 - 0.5) * 8.0) as isize
            } else {
                0
            };
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let sx = (x as isize + weave).clamp(0, w as isize - 1) as usize;
                    let sy = (y as isize + jump).clamp(0, h as isize - 1) as usize;
                    let si = (sy * w + sx) * 4;
                    let r = (from[si] as f32 * inv + to[si] as f32 * p) * brightness;
                    let g =
                        (from[si + 1] as f32 * inv + to[si + 1] as f32 * p) * brightness;
                    let b =
                        (from[si + 2] as f32 * inv + to[si + 2] as f32 * p) * brightness;
                    output[fi] = r.clamp(0.0, 255.0) as u8;
                    output[fi + 1] = g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = b.clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Halation => {
            // Film halation: per-pixel brightness-threshold additive glow. O(1).
            // param0 = threshold, param1 = spread, color = halo color
            let threshold = user_params.param0;
            let spread = user_params.param1;
            let halo_r = user_params.color[0] * 255.0;
            let halo_g = user_params.color[1] * 255.0;
            let halo_b = user_params.color[2] * 255.0;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma =
                    (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                let excess =
                    (luma - threshold).max(0.0) / (1.0 - threshold).max(0.001);
                let halo_strength = excess * spread * envelope * 3.0;
                output[i] =
                    (base_r + halo_r * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + halo_g * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + halo_b * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::GlitchRGBSplit => {
            // Hard digital glitch: per-block RGB channel separation with blackouts.
            // param0 = intensity, param1 = block size
            let intensity = user_params.param0;
            let block_size = user_params.param1.max(1.0) as usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let block_x = x / block_size.max(1);
                    let block_y = y / block_size.max(1);
                    let hash_r = ((block_x as f32 * 127.1
                        + block_y as f32 * 311.7
                        + p * 999.9)
                        .sin()
                        * 43758.545)
                        .fract()
                        .abs();
                    let hash_g = ((block_x as f32 * 269.5
                        + block_y as f32 * 183.3
                        + p * 543.2)
                        .sin()
                        * 12345.678)
                        .fract()
                        .abs();
                    let hash_b = ((block_x as f32 * 419.2
                        + block_y as f32 * 371.9
                        + p * 777.3)
                        .sin()
                        * 98765.432)
                        .fract()
                        .abs();
                    let r_shift = ((hash_r - 0.5) * envelope * 80.0) as isize;
                    let g_shift = ((hash_g - 0.5) * envelope * 60.0) as isize;
                    let b_shift = ((hash_b - 0.5) * envelope * 80.0) as isize;
                    let rx = (x as isize + r_shift).clamp(0, w as isize - 1) as usize;
                    let gx = (x as isize + g_shift).clamp(0, w as isize - 1) as usize;
                    let bx = (x as isize + b_shift).clamp(0, w as isize - 1) as usize;
                    let ri = (y * w + rx) * 4;
                    let gi = (y * w + gx) * 4;
                    let bi = (y * w + bx) * 4;
                    let blackout: f32 =
                        if hash_r < envelope * 0.15 { 0.0 } else { 1.0 };
                    let r = (from[ri] as f32 * inv + to[ri] as f32 * p) * blackout;
                    let g =
                        (from[gi + 1] as f32 * inv + to[gi + 1] as f32 * p) * blackout;
                    let b =
                        (from[bi + 2] as f32 * inv + to[bi + 2] as f32 * p) * blackout;
                    output[fi] = r.clamp(0.0, 255.0) as u8;
                    output[fi + 1] = g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = b.clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::StaticTV => {
            // Television static noise overtakes the frame at midpoint.
            // param0 = intensity, param1 = scan line height
            let intensity = user_params.param0;
            let line_h = user_params.param1.max(1.0) as usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let hash = ((x as f32 * 127.1 + y as f32 * 311.7 + p * 999.9).sin()
                        * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((x as f32 * 269.5 + y as f32 * 183.3 + p * 543.2).sin()
                        * 12345.678)
                        .fract()
                        .abs();
                    let scan_line: f32 =
                        if (y / line_h.max(1)).is_multiple_of(2) { 1.0 } else { 0.75 };
                    let static_val = hash * 255.0 * envelope * scan_line;
                    let band = y / line_h.max(1);
                    let band_hash = ((band as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let shift = ((band_hash - 0.5) * envelope * 20.0) as isize;
                    let sx = (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let base_r = from[si] as f32 * inv + to[si] as f32 * p;
                    let base_g = from[si + 1] as f32 * inv + to[si + 1] as f32 * p;
                    let base_b = from[si + 2] as f32 * inv + to[si + 2] as f32 * p;
                    let static_mix = envelope * hash2 * 0.8;
                    output[fi] =
                        (base_r + (static_val - base_r) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 1] =
                        (base_g + (static_val - base_g) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 2] =
                        (base_b + (static_val - base_b) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::VHSTracking => {
            // VHS tape tracking error with band shifts and color bleed.
            // param0 = intensity, param1 = band count
            let intensity = user_params.param0;
            let band_count = user_params.param1.max(1.0);
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let band = (y as f32 / height as f32 * band_count) as usize;
                    let band_hash = ((band as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let band_hash2 = ((band as f32 * 269.5 + p * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let shift = ((band_hash - 0.5) * envelope * 40.0) as isize;
                    let cshift = ((band_hash2 - 0.5) * envelope * 15.0) as isize;
                    let sx = (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let csx = (x as isize + cshift).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let csi = (y * w + csx) * 4;
                    let base_r = from[si] as f32 * inv + to[si] as f32 * p;
                    let base_g = from[si + 1] as f32 * inv + to[si + 1] as f32 * p;
                    let base_b = from[si + 2] as f32 * inv + to[si + 2] as f32 * p;
                    let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                    let chr = from[csi] as f32 * inv + to[csi] as f32 * p;
                    let chrb = from[csi + 2] as f32 * inv + to[csi + 2] as f32 * p;
                    let brightness = 1.0 - band_hash * envelope * 0.4;
                    let vhs_r = base_r + (chr - base_r) * envelope * 0.5;
                    let vhs_g = base_g + (luma - base_g) * envelope * 0.5;
                    let vhs_b = base_b + (chrb - base_b) * envelope * 0.5;
                    output[fi] = (vhs_r * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 1] = (vhs_g * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 2] = (vhs_b * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Swirl => {
            // Rotational distortion spiraling from center, peaking at midpoint.
            // param0 = max angle (degrees), param1 = radius falloff
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
            let max_angle = user_params.param0 * std::f32::consts::PI / 180.0;
            let falloff = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let norm_dist = dist / max_r;
                    let swirl_angle = max_angle * envelope * (-norm_dist * falloff).exp();
                    let cos_a = swirl_angle.cos();
                    let sin_a = swirl_angle.sin();
                    let sx = (cos_a * dx - sin_a * dy + cx)
                        .clamp(0.0, width as f32 - 1.0) as usize;
                    let sy = (sin_a * dx + cos_a * dy + cy)
                        .clamp(0.0, height as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] =
                            (from[si + c] as f32 * inv + to[si + c] as f32 * p + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::WaveDistortion => {
            // Sinusoidal displacement ripples across the frame.
            // param0 = amplitude (px), param1 = frequency, param2 = direction
            let amplitude = user_params.param0;
            let frequency = user_params.param1;
            let dir = user_params.param2.round() as u32;
            let envelope = (p * std::f32::consts::PI).sin() * amplitude;
            let two_pi = std::f32::consts::TAU;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let (off_x, off_y) = match dir {
                        1 => (
                            0.0,
                            (x as f32 / width as f32 * frequency * two_pi).sin() * envelope,
                        ),
                        2 => (
                            (y as f32 / height as f32 * frequency * two_pi).sin()
                                * envelope
                                * 0.7,
                            (x as f32 / width as f32 * frequency * two_pi).sin()
                                * envelope
                                * 0.7,
                        ),
                        _ => (
                            (y as f32 / height as f32 * frequency * two_pi).sin() * envelope,
                            0.0,
                        ),
                    };
                    let sx = (x as f32 + off_x).clamp(0.0, width as f32 - 1.0) as usize;
                    let sy = (y as f32 + off_y).clamp(0.0, height as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] =
                            (from[si + c] as f32 * inv + to[si + c] as f32 * p + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::ColorizeFlash => {
            // User-defined color washes over the frame at the cut.
            // param0 = intensity, param1 = peak position, color = flash color
            let flash_r = user_params.color[0] * 255.0;
            let flash_g = user_params.color[1] * 255.0;
            let flash_b = user_params.color[2] * 255.0;
            let intensity = user_params.param0.clamp(0.0, 1.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let flash_t = (if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            }) * intensity;
            let dissolve_start = peak * 0.8;
            let dissolve_t =
                ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001)).clamp(0.0, 1.0);
            let inv_d = 1.0 - dissolve_t;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv_d + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * inv_d + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * inv_d + to[i + 2] as f32 * dissolve_t;
                output[i] =
                    (base_r + (flash_r - base_r) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + (flash_g - base_g) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + (flash_b - base_b) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::HueRotate => {
            // Hue spins through the color wheel at midpoint while dissolving.
            // param0 = rotations
            let rotations = user_params.param0;
            let envelope = (p * std::f32::consts::PI).sin() * rotations;
            let hue_shift = envelope * 360.0;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r =
                    (from[i] as f32 * inv + to[i] as f32 * p) / 255.0;
                let base_g =
                    (from[i + 1] as f32 * inv + to[i + 1] as f32 * p) / 255.0;
                let base_b =
                    (from[i + 2] as f32 * inv + to[i + 2] as f32 * p) / 255.0;
                // RGB → HSV
                let cmax = base_r.max(base_g).max(base_b);
                let cmin = base_r.min(base_g).min(base_b);
                let delta = cmax - cmin;
                let h = if delta < 0.001 {
                    0.0
                } else if (cmax - base_r).abs() < 0.001 {
                    60.0 * (((base_g - base_b) / delta) % 6.0)
                } else if (cmax - base_g).abs() < 0.001 {
                    60.0 * ((base_b - base_r) / delta + 2.0)
                } else {
                    60.0 * ((base_r - base_g) / delta + 4.0)
                };
                let s = if cmax < 0.001 { 0.0 } else { delta / cmax };
                let v = cmax;
                // Shift hue
                let h2 = ((h + hue_shift) % 360.0 + 360.0) % 360.0;
                // HSV → RGB
                let c = v * s;
                let x2 = c * (1.0 - ((h2 / 60.0) % 2.0 - 1.0).abs());
                let m = v - c;
                let hi = (h2 / 60.0) as u32 % 6;
                let (rr, gg, bb) = match hi {
                    0 => (c, x2, 0.0),
                    1 => (x2, c, 0.0),
                    2 => (0.0, c, x2),
                    3 => (0.0, x2, c),
                    4 => (x2, 0.0, c),
                    _ => (c, 0.0, x2),
                };
                output[i] = ((rr + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 1] = ((gg + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 2] = ((bb + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::NegativeFlash => {
            // Brief photographic negative inversion at the cut point.
            // param0 = intensity, param1 = peak position
            let intensity = user_params.param0.clamp(0.0, 1.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let flash_t = if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            } * intensity;
            let dissolve_start = peak * 0.8;
            let dissolve_t =
                ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001)).clamp(0.0, 1.0);
            let inv_d = 1.0 - dissolve_t;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv_d + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * inv_d + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * inv_d + to[i + 2] as f32 * dissolve_t;
                // mix(base, 255-base, flash_t) = base + (255 - 2*base)*flash_t
                output[i] =
                    (base_r + (255.0 - 2.0 * base_r) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + (255.0 - 2.0 * base_g) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + (255.0 - 2.0 * base_b) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::Sepia => {
            // Warm sepia tone through the cut, peaking at midpoint.
            // param0 = strength, param1 = warmth
            let strength = user_params.param0.clamp(0.0, 1.0);
            let warmth = user_params.param1.clamp(0.5, 2.0);
            let envelope = (p * std::f32::consts::PI).sin() * strength;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                let sepia_r = (luma * warmth * 1.2).clamp(0.0, 255.0);
                let sepia_g = (luma * warmth * 1.0).clamp(0.0, 255.0);
                let sepia_b = (luma * warmth * 0.75).clamp(0.0, 255.0);
                output[i] = (base_r + (sepia_r - base_r) * envelope + 0.5) as u8;
                output[i + 1] = (base_g + (sepia_g - base_g) * envelope + 0.5) as u8;
                output[i + 2] = (base_b + (sepia_b - base_b) * envelope + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::GlitchDissolve => {
            // Digital corruption: horizontal block shifts + RGB split + noise.
            // param0 = intensity, param1 = block size
            let intensity = user_params.param0;
            let block_size = user_params.param1.max(1.0) as usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let w = width as usize;
            let h = height as usize;
            let inv = 1.0 - p;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let block_row = y / block_size.max(1);
                    let hash = ((block_row as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((block_row as f32 * 269.5 + p * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let shift = ((hash - 0.5) * envelope * 40.0) as isize;
                    let sx =
                        (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let r_sx =
                        (x as isize + shift / 2).clamp(0, w as isize - 1) as usize;
                    let b_sx =
                        (x as isize - shift / 2).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let r_si = (y * w + r_sx) * 4;
                    let b_si = (y * w + b_sx) * 4;
                    let noise = ((hash2 - 0.5) * envelope * 60.0) as i16;
                    let r = ((from[r_si] as f32 * inv + to[r_si] as f32 * p) as i16 + noise)
                        .clamp(0, 255) as u8;
                    let g = ((from[si + 1] as f32 * inv + to[si + 1] as f32 * p) as i16
                        + noise / 2)
                        .clamp(0, 255) as u8;
                    let b = ((from[b_si + 2] as f32 * inv + to[b_si + 2] as f32 * p) as i16
                        + noise)
                        .clamp(0, 255) as u8;
                    output[fi] = r;
                    output[fi + 1] = g;
                    output[fi + 2] = b;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Desaturate => {
            // Drain to greyscale at midpoint then resaturate into clip B.
            // param0 = depth (how fully it desaturates)
            let depth = user_params.param0.clamp(0.0, 1.0);
            let envelope = (p * std::f32::consts::PI).sin() * depth;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                output[i] = (base_r + (luma - base_r) * envelope + 0.5) as u8;
                output[i + 1] = (base_g + (luma - base_g) * envelope + 0.5) as u8;
                output[i + 2] = (base_b + (luma - base_b) * envelope + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::AdditiveDissolve => {
            // Cross-dissolve with additive brightness boost peaking at midpoint.
            let intensity = user_params.param0;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let add = envelope * 0.5;
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let base = from[i + c] as f32 * (1.0 - p) + to[i + c] as f32 * p;
                    output[i + c] =
                        ((base / 255.0 + add).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
                output[i + 3] = 255;
            }
        }
        TransitionKind::FadeToColor => {
            // Fade out to a solid color, then fade in from it.
            let fade_r = (user_params.color[0] * 255.0 + 0.5) as u8;
            let fade_g = (user_params.color[1] * 255.0 + 0.5) as u8;
            let fade_b = (user_params.color[2] * 255.0 + 0.5) as u8;
            let hold = user_params.param0.clamp(0.0, 0.45);
            let mid_start = 0.5 - hold * 0.5;
            let mid_end = 0.5 + hold * 0.5;
            for i in (0..byte_count).step_by(4) {
                if p <= mid_start {
                    let t = p / mid_start.max(0.001);
                    let inv = 1.0 - t;
                    output[i] = (from[i] as f32 * inv + fade_r as f32 * t + 0.5) as u8;
                    output[i + 1] =
                        (from[i + 1] as f32 * inv + fade_g as f32 * t + 0.5) as u8;
                    output[i + 2] =
                        (from[i + 2] as f32 * inv + fade_b as f32 * t + 0.5) as u8;
                } else if p >= mid_end {
                    let t = (p - mid_end) / (1.0 - mid_end).max(0.001);
                    let inv = 1.0 - t;
                    output[i] = (fade_r as f32 * inv + to[i] as f32 * t + 0.5) as u8;
                    output[i + 1] =
                        (fade_g as f32 * inv + to[i + 1] as f32 * t + 0.5) as u8;
                    output[i + 2] =
                        (fade_b as f32 * inv + to[i + 2] as f32 * t + 0.5) as u8;
                } else {
                    output[i] = fade_r;
                    output[i + 1] = fade_g;
                    output[i + 2] = fade_b;
                }
                output[i + 3] = 255;
            }
        }
        TransitionKind::SqueezeH | TransitionKind::SqueezeV => {
            // Proper squeeze: from collapses to a 1-pixel center line at p=0.5,
            // then to expands from that line to full frame. Black outside.
            let w = width as usize;
            let h = height as usize;
            let is_h = matches!(kind, TransitionKind::SqueezeH);
            let dim = if is_h { w } else { h } as f32;
            let half = dim * 0.5;
            let one_px = 1.0 / dim;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let pos = if is_h { x as f32 } else { y as f32 };
                    if p <= 0.5 {
                        let scale = 1.0 - 2.0 * p;
                        if scale < one_px {
                            // Collapsed below one pixel — all black.
                            output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                        } else {
                            let vis_lo = half - half * scale;
                            let vis_hi = half + half * scale;
                            if pos < vis_lo || pos >= vis_hi {
                                output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                            } else {
                                let src = ((pos - half) / scale + half)
                                    .clamp(0.0, dim - 1.0)
                                    as usize;
                                let si =
                                    if is_h { (y * w + src) * 4 } else { (src * w + x) * 4 };
                                output[fi..fi + 4].copy_from_slice(&from[si..si + 4]);
                            }
                        }
                    } else {
                        let scale = 2.0 * p - 1.0;
                        if scale < one_px {
                            output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                        } else {
                            let vis_lo = half - half * scale;
                            let vis_hi = half + half * scale;
                            if pos < vis_lo || pos >= vis_hi {
                                output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                            } else {
                                let src = ((pos - half) / scale + half)
                                    .clamp(0.0, dim - 1.0)
                                    as usize;
                                let si =
                                    if is_h { (y * w + src) * 4 } else { (src * w + x) * 4 };
                                output[fi..fi + 4].copy_from_slice(&to[si..si + 4]);
                            }
                        }
                    }
                }
            }
        }
        TransitionKind::SlideLeft
        | TransitionKind::SlideRight
        | TransitionKind::SlideUp
        | TransitionKind::SlideDown
        | TransitionKind::CoverLeft
        | TransitionKind::CoverRight
        | TransitionKind::UncoverLeft
        | TransitionKind::UncoverRight => {
            // Pixel-offset sampling family.
            let w = width as usize;
            let h = height as usize;
            let offset_w = ((p * width as f32) as usize).min(w);
            let offset_h = ((p * height as f32) as usize).min(h);
            let wi = w as isize;
            let hi = h as isize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let (use_to, sx, sy): (bool, usize, usize) = match kind {
                        TransitionKind::SlideLeft => {
                            let to_x = x as isize + wi - offset_w as isize;
                            if to_x < wi {
                                (true, to_x.max(0) as usize, y)
                            } else {
                                let fx = (x as isize - offset_w as isize)
                                    .clamp(0, wi - 1) as usize;
                                (false, fx, y)
                            }
                        }
                        TransitionKind::SlideRight => {
                            let to_x = x as isize - wi + offset_w as isize;
                            if to_x >= 0 {
                                (true, (to_x as usize).min(w - 1), y)
                            } else {
                                let fx = (x + offset_w).min(w - 1);
                                (false, fx, y)
                            }
                        }
                        TransitionKind::SlideUp => {
                            let to_y = y as isize + hi - offset_h as isize;
                            if to_y < hi {
                                (true, x, to_y.max(0) as usize)
                            } else {
                                let fy = (y as isize - offset_h as isize)
                                    .clamp(0, hi - 1) as usize;
                                (false, x, fy)
                            }
                        }
                        TransitionKind::SlideDown => {
                            let to_y = y as isize - hi + offset_h as isize;
                            if to_y >= 0 {
                                (true, x, (to_y as usize).min(h - 1))
                            } else {
                                let fy = (y + offset_h).min(h - 1);
                                (false, x, fy)
                            }
                        }
                        TransitionKind::CoverLeft => {
                            let to_x = x as isize + wi - offset_w as isize;
                            if to_x < wi {
                                (true, to_x.max(0) as usize, y)
                            } else {
                                (false, x, y)
                            }
                        }
                        TransitionKind::CoverRight => {
                            let to_x = x as isize - wi + offset_w as isize;
                            if to_x >= 0 {
                                (true, (to_x as usize).min(w - 1), y)
                            } else {
                                (false, x, y)
                            }
                        }
                        TransitionKind::UncoverLeft => {
                            let from_x = x + offset_w;
                            if from_x < w {
                                (false, from_x, y)
                            } else {
                                (true, x, y)
                            }
                        }
                        TransitionKind::UncoverRight => {
                            let from_x = x as isize - offset_w as isize;
                            if from_x >= 0 {
                                (false, from_x as usize, y)
                            } else {
                                (true, x, y)
                            }
                        }
                        _ => unreachable!(),
                    };
                    let si = (sy * w + sx) * 4;
                    let src = if use_to { to } else { from };
                    output[fi..fi + 4].copy_from_slice(&src[si..si + 4]);
                }
            }
        }
        TransitionKind::BoxWipeIn | TransitionKind::BoxWipeOut => {
            // Rectangle expanding from center (In) or contracting to center (Out).
            // param0 = softness, param1 = square blend (0 = frame aspect, 1 = square)
            let softness = (user_params.param0 * 0.03).max(0.001);
            let square = user_params.param1.clamp(0.0, 1.0);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let is_in = matches!(kind, TransitionKind::BoxWipeIn);
            // In square mode, landscape frames produce norm_dist up to cx/cy>1.
            // Scale threshold to max possible distance so corners are fully revealed.
            let aspect = (cx / cy.max(1.0)).max(1.0);
            let max_norm = 1.0 + (aspect - 1.0) * square;
            let threshold = if is_in {
                p * (max_norm + 2.0 * softness) - softness
            } else {
                (1.0 - p) * (max_norm + 2.0 * softness) - softness
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dx = (x as f32 - cx).abs() / cx.max(1.0);
                    let dy = (y as f32 - cy).abs() / cy.max(1.0);
                    let box_dist = dx.max(dy);
                    let sq_dist = (dx * cx / cy.max(1.0)).max(dy);
                    let dist = box_dist + (sq_dist - box_dist) * square;
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        dist,
                    );
                    // In: reveal where dist < threshold → invert.
                    // Out: reveal where dist > threshold → use directly (with flipped threshold).
                    let t = if is_in { 1.0 - s_val } else { s_val };
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::DiagonalWipeTLBR | TransitionKind::DiagonalWipeTRBL => {
            // Diagonal sweep from one corner to its opposite.
            // param0 = softness
            let softness = (user_params.param0 * 0.03).max(0.001);
            let tlbr = matches!(kind, TransitionKind::DiagonalWipeTLBR);
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let nx = x as f32 / width as f32;
                    let ny = y as f32 / height as f32;
                    let diag = if tlbr {
                        (nx + ny) * 0.5
                    } else {
                        ((1.0 - nx) + ny) * 0.5
                    };
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        diag,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::CheckerboardWipe => {
            // Alternating squares wipe left-to-right or right-to-left.
            // param0 = softness, param1 = grid size (squares per axis)
            let softness = (user_params.param0 * 0.04).max(0.001);
            let grid = user_params.param1.max(1.0);
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let fx = x as f32 / width as f32 * grid;
                    let fy = y as f32 / height as f32 * grid;
                    let col = fx as usize;
                    let row = fy as usize;
                    let local_x = fx.fract();
                    let is_even = (col + row).is_multiple_of(2);
                    let local_pos = if is_even { local_x } else { 1.0 - local_x };
                    // Reveal `to` where local_pos < threshold (same pattern as BarnDoorOpen).
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        local_pos,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::VenetianBlindHorizontal | TransitionKind::VenetianBlindVertical => {
            // N parallel strips wipe simultaneously.
            // param0 = softness, param1 = blind count
            let softness = (user_params.param0 * 0.04).max(0.001);
            let blinds = user_params.param1.max(1.0);
            let horizontal = matches!(kind, TransitionKind::VenetianBlindHorizontal);
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let strip_pos = if horizontal {
                        (y as f32 / height as f32 * blinds).fract()
                    } else {
                        (x as f32 / width as f32 * blinds).fract()
                    };
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        strip_pos,
                    );
                    // Blinds reveal `to` where strip_pos < threshold.
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::SplitWipeHorizontal | TransitionKind::SplitWipeVertical => {
            // Split from center outward — `to` reveals in the center first.
            // param0 = softness
            let softness = (user_params.param0 * 0.03).max(0.001);
            let horizontal = matches!(kind, TransitionKind::SplitWipeHorizontal);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dist = if horizontal {
                        (x as f32 - cx).abs() / cx.max(1.0)
                    } else {
                        (y as f32 - cy).abs() / cy.max(1.0)
                    };
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        dist,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::StarWipe => {
            // N-pointed star expanding from center.
            // param0 = softness, param1 = point count, param2 = inner radius ratio
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let points = user_params.param1.max(3.0);
            let inner_ratio = user_params.param2.clamp(0.2, 0.8);
            let softness = (user_params.param0 * 0.03).max(0.001);
            let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
            let sector = std::f32::consts::PI / points;
            let two_sector = 2.0 * sector;
            // Corners of the frame can reach norm_dist = 1/inner_ratio in the star's
            // concave sectors, so scale the threshold to that range for full coverage.
            let max_norm = 1.0 / inner_ratio;
            let threshold = p * (max_norm + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let angle = dy.atan2(dx);
                    let sector_angle =
                        ((angle % two_sector) + two_sector) % two_sector;
                    let star_r_norm = inner_ratio
                        + (1.0 - inner_ratio)
                            * smoothstep_f32(0.0, sector, (sector_angle - sector).abs());
                    let norm_dist = dist / (max_r * star_r_norm.max(0.01));
                    let t = 1.0
                        - smoothstep_f32(
                            threshold - softness,
                            threshold + softness,
                            norm_dist,
                        );
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::FilmBurn => {
            // Organic hot-spot bleach/burn with grain, peaks at midpoint.
            // param0 = temperature (0=white, 1=amber), param1 = speed, param2 = grain
            let temp = user_params.param0;
            let speed = user_params.param1;
            let grain = user_params.param2;
            let burn_r: f32 = 1.0;
            let burn_g: f32 = 1.0 - temp * 0.35;
            let burn_b: f32 = 1.0 - temp * 0.75;
            let shaped_p = (p * speed).powf(0.6).min(1.0);
            let envelope = (shaped_p * std::f32::consts::PI).sin();
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    // Pseudo-random per-pixel variation.
                    let hash = ((x as f32 * 127.1 + y as f32 * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((x as f32 * 269.5 + y as f32 * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let pixel_thresh = hash * 0.6 + 0.2;
                    // burn_t: active only while envelope exceeds per-pixel threshold.
                    // No additive bias, so at envelope=0 (p=0 and p=1) burn_t=0.
                    let burn_t = ((envelope - pixel_thresh) / 0.3).clamp(0.0, 1.0);
                    let grain_val = (hash2 - 0.5) * grain * envelope;
                    let base_r =
                        (from[fi] as f32 * (1.0 - p) + to[fi] as f32 * p) / 255.0;
                    let base_g =
                        (from[fi + 1] as f32 * (1.0 - p) + to[fi + 1] as f32 * p) / 255.0;
                    let base_b =
                        (from[fi + 2] as f32 * (1.0 - p) + to[fi + 2] as f32 * p) / 255.0;
                    let r = (base_r + (burn_r - base_r) * burn_t + grain_val).clamp(0.0, 1.0);
                    let g = (base_g + (burn_g - base_g) * burn_t + grain_val).clamp(0.0, 1.0);
                    let b = (base_b + (burn_b - base_b) * burn_t + grain_val).clamp(0.0, 1.0);
                    output[fi] = (r * 255.0 + 0.5) as u8;
                    output[fi + 1] = (g * 255.0 + 0.5) as u8;
                    output[fi + 2] = (b * 255.0 + 0.5) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::ChromaticAberration => {
            // RGB channel separation peaking at midpoint.
            // param0 = max offset (px), param1 = direction (0=H, 1=V, 2=diag)
            let max_offset = user_params.param0;
            let dir = user_params.param1.round() as u32;
            let envelope = (p * std::f32::consts::PI).sin() * max_offset;
            let (r_ox, r_oy, b_ox, b_oy) = match dir {
                1 => (0.0, -envelope, 0.0, envelope),
                2 => (
                    -envelope * 0.707,
                    -envelope * 0.707,
                    envelope * 0.707,
                    envelope * 0.707,
                ),
                _ => (-envelope, 0.0, envelope, 0.0),
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    // Green channel: base blend at pixel position.
                    let base_g = (from[fi + 1] as f32 * (1.0 - p)
                        + to[fi + 1] as f32 * p
                        + 0.5) as u8;
                    // Red channel from r_offset sample.
                    let rx = (x as f32 + r_ox).clamp(0.0, w as f32 - 1.0) as usize;
                    let ry = (y as f32 + r_oy).clamp(0.0, h as f32 - 1.0) as usize;
                    let ri = (ry * w + rx) * 4;
                    let r_val = (from[ri] as f32 * (1.0 - p)
                        + to[ri] as f32 * p
                        + 0.5) as u8;
                    // Blue channel from b_offset sample.
                    let bx = (x as f32 + b_ox).clamp(0.0, w as f32 - 1.0) as usize;
                    let by = (y as f32 + b_oy).clamp(0.0, h as f32 - 1.0) as usize;
                    let bi = (by * w + bx) * 4;
                    let b_val = (from[bi + 2] as f32 * (1.0 - p)
                        + to[bi + 2] as f32 * p
                        + 0.5) as u8;
                    output[fi] = r_val;
                    output[fi + 1] = base_g;
                    output[fi + 2] = b_val;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::PageTurn | TransitionKind::PageTurnReverse => {
            // Page peels from one side revealing `to` underneath, with fold shadow.
            let shadow_intensity = user_params.param0;
            let flip = matches!(kind, TransitionKind::PageTurnReverse) || user_params.param1 > 0.5;
            let fold_x = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            let denom = (p * 0.3).max(0.01);
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let fx = if flip { w - 1 - x } else { x };
                    let norm_x = fx as f32 / w as f32;
                    if norm_x > fold_x {
                        let shadow_dist = (norm_x - fold_x) / denom;
                        let shadow = shadow_intensity * (-shadow_dist * 3.0).exp();
                        let t = (1.0 - shadow).clamp(0.0, 1.0);
                        for c in 0..3 {
                            output[fi + c] = (to[fi + c] as f32 * t) as u8;
                        }
                    } else {
                        let shadow_dist = (fold_x - norm_x) / denom;
                        let shadow = shadow_intensity * 0.5 * (-shadow_dist * 2.0).exp();
                        let t = (1.0 - shadow).clamp(0.0, 1.0);
                        for c in 0..3 {
                            output[fi + c] = (from[fi + c] as f32 * t) as u8;
                        }
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Ripple => {
            // Water ripple: radial sine displacement peaking at midpoint.
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let amplitude = user_params.param0;
            let frequency = user_params.param1;
            let speed = user_params.param2;
            let envelope = (p * std::f32::consts::PI).sin() * amplitude;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let phase = dist * frequency * 0.1 - p * speed * 10.0;
                    let ripple = phase.sin() * envelope;
                    let (nx, ny) = if dist > 0.1 {
                        (dx / dist, dy / dist)
                    } else {
                        (0.0, 0.0)
                    };
                    let sx = (x as f32 + nx * ripple).clamp(0.0, w as f32 - 1.0) as usize;
                    let sy = (y as f32 + ny * ripple).clamp(0.0, h as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] = (from[si + c] as f32 * (1.0 - p)
                            + to[fi + c] as f32 * p
                            + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::WaterDrop => {
            // Water drop: a ring spreads from the centre; the incoming
            // picture is revealed inside it, refracted at the crest, with
            // ripples trailing behind the front. Same maths as the shader.
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let half_diag = (cx * cx + cy * cy).sqrt();
            let band = user_params.param0.max(0.02) * half_diag;
            let front = -band * 0.5 + p * (half_diag * 1.05 + band);
            let env = (p * std::f32::consts::PI).sin();
            let smoothstep = |lo: f32, hi: f32, v: f32| {
                let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let s = dist - front;
                    let crest = (-(s * s) / (band * band * 0.18)).exp();
                    let trail = if s < 0.0 {
                        (s / band * std::f32::consts::TAU * user_params.param2.max(0.0)).sin() * (s / band).exp()
                    } else {
                        0.0
                    };
                    let disp = (crest + trail * 0.5) * user_params.param1 * env;
                    let (nx, ny) = if dist > 0.1 { (dx / dist, dy / dist) } else { (0.0, 0.0) };
                    let sx = (x as f32 + nx * disp).clamp(0.0, w as f32 - 1.0) as usize;
                    let sy = (y as f32 + ny * disp).clamp(0.0, h as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    let reveal = 1.0 - smoothstep(-band * 0.5, band * 0.5, s);
                    let glint = crest * user_params.param3 * env * 255.0;
                    for c in 0..3 {
                        let v = from[si + c] as f32 * (1.0 - reveal) + to[si + c] as f32 * reveal + glint;
                        output[fi + c] = (v.min(255.0) + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Pixelate => {
            // Blocks grow to `max_block` at p=0.5, shrink to 1 at endpoints.
            // param0 = max block size (px), param1 = mode (0=pixelate then dissolve, 1=cross)
            let max_block = (user_params.param0.max(2.0)) as usize;
            let cross = user_params.param1 > 0.5;
            let triangle = 1.0 - (p * 2.0 - 1.0).abs();
            let block = ((max_block as f32 * triangle) as usize).max(1);
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let bx = ((x / block) * block + block / 2).min(w - 1);
                    let by = ((y / block) * block + block / 2).min(h - 1);
                    let bi = (by * w + bx) * 4;
                    if cross {
                        // Cross mode: pixelate both and cross-dissolve through p.
                        for c in 0..3 {
                            output[fi + c] = (from[bi + c] as f32 * (1.0 - p)
                                + to[bi + c] as f32 * p
                                + 0.5) as u8;
                        }
                        output[fi + 3] = 255;
                    } else if p < 0.5 {
                        // First half: show pixelated `from` only.
                        output[fi..fi + 4].copy_from_slice(&from[bi..bi + 4]);
                    } else {
                        // Second half: dissolve from pixelated-from to crisp `to`.
                        let t = (p - 0.5) * 2.0;
                        for c in 0..3 {
                            output[fi + c] = (from[bi + c] as f32 * (1.0 - t)
                                + to[fi + c] as f32 * t
                                + 0.5) as u8;
                        }
                        output[fi + 3] = 255;
                    }
                }
            }
        }
        TransitionKind::BarnDoorOpen | TransitionKind::BarnDoorClose => {
            // Two panels split from center outward (Open) or close from edges inward (Close).
            // param0 = softness, param1 = orientation (0=horizontal, 1=vertical)
            let vertical = user_params.param1 > 0.5;
            let softness = (user_params.param0 * 0.03).max(0.001);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let is_open = matches!(kind, TransitionKind::BarnDoorOpen);
            let open_threshold = p * (1.0 + 2.0 * softness) - softness;
            let close_threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
            output = parallel_blend_u8_rows(from, to, width, height, |x, y| {
                let dist = if vertical {
                    (y as f32 - cy).abs() / cy.max(1.0)
                } else {
                    (x as f32 - cx).abs() / cx.max(1.0)
                };
                if is_open {
                    1.0 - smoothstep_f32(open_threshold - softness, open_threshold + softness, dist)
                } else {
                    smoothstep_f32(close_threshold - softness, close_threshold + softness, dist)
                }
            });
        }
        TransitionKind::ClockWipe => {
            // Angular sweep from a starting angle around the frame center.
            // param0 = softness, param1 = start angle deg, param2 = direction (0=CW, 1=CCW)
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let start_rad = user_params.param1 * std::f32::consts::PI / 180.0;
            let ccw = user_params.param2 > 0.5;
            let softness = (user_params.param0 * 0.02).max(0.001);
            let two_pi = std::f32::consts::TAU;
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            output = parallel_blend_u8_rows(from, to, width, height, |x, y| {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let mut angle = dx.atan2(-dy) - start_rad;
                angle = ((angle % two_pi) + two_pi) % two_pi;
                if ccw {
                    angle = two_pi - angle;
                }
                let norm_angle = angle / two_pi;
                let s_val = smoothstep_f32(threshold - softness, threshold + softness, norm_angle);
                1.0 - s_val
            });
        }
    }

    output
}

#[cfg(test)]
mod tests {
    //! Smoke coverage for the transition renderer. Each TransitionKind
    //! variant is exercised at progress 0.0, 0.5, and 1.0 on a tiny
    //! 4×4 RGBA buffer; the harness catches panics, bad output sizes,
    //! and a match-arm silently renaming itself out of existence.
    //! Extra focused checks on CrossDissolve (easing + boundary
    //! blend) and apply_easing's shape invariants.
    use super::*;
    use klipp_core::{TransitionUserParams, TRANSITION_SPECS};

    fn rgba(byte: u8) -> Vec<u8> {
        vec![byte; 4 * 4 * 4] // 4×4 RGBA
    }

    #[test]
    fn apply_easing_stays_finite_and_in_unit_range() {
        // Linear + all 4 easing curves at boundary and interior points.
        for curve in 0..=5 {
            for &t in &[0.0_f32, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
                let r = apply_easing(t, curve as f32);
                assert!(r.is_finite(), "easing(t={t}, curve={curve}) = {r} not finite");
                assert!(
                    (0.0..=1.0).contains(&r),
                    "easing(t={t}, curve={curve}) = {r} escaped [0, 1]"
                );
            }
        }
    }

    #[test]
    fn apply_easing_linear_is_identity() {
        // curve=0 (and anything not matching 1..=4) falls through to
        // linear — output equals input.
        for &t in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(apply_easing(t, 0.0), t);
            assert_eq!(apply_easing(t, 99.0), t, "unmatched curve must fall to linear");
        }
    }

    #[test]
    fn apply_easing_hits_boundaries() {
        // Every curve must pass (0,0) and (1,1) exactly — a rescaling
        // that doesn't hit the endpoints would produce visible jumps
        // at the start/end of a CrossDissolve.
        for curve in 0..=4 {
            assert_eq!(apply_easing(0.0, curve as f32), 0.0, "curve {curve}: f(0) != 0");
            let at1 = apply_easing(1.0, curve as f32);
            assert!(
                (at1 - 1.0).abs() < 1e-5,
                "curve {curve}: f(1) = {at1}, expected 1.0"
            );
        }
    }

    #[test]
    fn render_all_kinds_produce_correct_size_and_finite_bytes() {
        // The registry holds all variants — using it instead of a
        // hand-written list means adding a new TransitionKind + a
        // TRANSITION_SPECS row is enough to get smoke coverage, no
        // test-list edit required.
        let width = 4;
        let height = 4;
        let expected = (width * height * 4) as usize;
        let from = rgba(50);
        let to = rgba(200);
        let params = TransitionUserParams::default();

        for spec in TRANSITION_SPECS.iter() {
            for &p in &[0.0_f32, 0.5, 1.0] {
                let out = render_transition_with_params(
                    spec.kind, &from, &to, p, width, height, &params,
                );
                assert_eq!(
                    out.len(),
                    expected,
                    "{:?} @ p={} produced {} bytes, want {}",
                    spec.kind,
                    p,
                    out.len(),
                    expected
                );
                // No NaN escaping as a u8 is trivially true, but we
                // guard against an all-zero or all-255 buffer that
                // signals "kernel wrote nothing meaningful". Allow
                // pure black/white at the endpoints (some wipes leave
                // regions fully covered) but require SOME byte != 0
                // AND SOME != 255 at p=0.5 where both halves should
                // contribute somewhere.
                if (p - 0.5).abs() < 1e-6 {
                    let has_nonzero = out.iter().any(|&b| b != 0);
                    assert!(
                        has_nonzero,
                        "{:?} @ p=0.5 produced all-zero output (kernel likely unimplemented)",
                        spec.kind,
                    );
                }
            }
        }
    }

    #[test]
    fn cross_dissolve_at_boundaries_matches_input_buffers() {
        // p=0 → output ≈ from, p=1 → output ≈ to, within 1 byte of
        // integer rounding. This is the load-bearing invariant for
        // seamless transition IN/OUT.
        let width = 4;
        let height = 4;
        let from = vec![100u8; (width * height * 4) as usize];
        let to = vec![200u8; (width * height * 4) as usize];
        let params = TransitionUserParams::default();

        let out0 = render_transition_with_params(
            TransitionKind::CrossDissolve, &from, &to, 0.0, width, height, &params,
        );
        let out1 = render_transition_with_params(
            TransitionKind::CrossDissolve, &from, &to, 1.0, width, height, &params,
        );

        for i in 0..out0.len() {
            let d0 = (out0[i] as i32 - from[i] as i32).abs();
            let d1 = (out1[i] as i32 - to[i] as i32).abs();
            assert!(d0 <= 1, "p=0 byte {i}: {} vs {}, drift {}", out0[i], from[i], d0);
            assert!(d1 <= 1, "p=1 byte {i}: {} vs {}, drift {}", out1[i], to[i], d1);
        }
    }

    #[test]
    fn cross_dissolve_at_half_is_average_of_inputs() {
        // Linear dissolve at p=0.5 should produce ≈ (from + to) / 2.
        let width = 4;
        let height = 4;
        let from = vec![50u8; (width * height * 4) as usize];
        let to = vec![150u8; (width * height * 4) as usize];
        let params = TransitionUserParams::default();

        let out = render_transition_with_params(
            TransitionKind::CrossDissolve, &from, &to, 0.5, width, height, &params,
        );

        let expected = 100u8; // (50 + 150) / 2
        for &b in &out {
            assert!(
                (b as i32 - expected as i32).abs() <= 1,
                "expected ~{expected}, got {b}"
            );
        }
    }

    #[test]
    fn progress_clamps_out_of_range_inputs() {
        // render_transition_with_params clamps progress to [0,1] —
        // callers don't need to pre-clamp.
        let width = 4;
        let height = 4;
        let from = vec![30u8; (width * height * 4) as usize];
        let to = vec![210u8; (width * height * 4) as usize];
        let params = TransitionUserParams::default();

        let out_neg = render_transition_with_params(
            TransitionKind::CrossDissolve, &from, &to, -10.0, width, height, &params,
        );
        let out_huge = render_transition_with_params(
            TransitionKind::CrossDissolve, &from, &to, 42.0, width, height, &params,
        );

        // Negative progress → behaves like p=0 (output ≈ from).
        for i in 0..out_neg.len() {
            assert!((out_neg[i] as i32 - from[i] as i32).abs() <= 1);
        }
        // Large progress → behaves like p=1 (output ≈ to).
        for i in 0..out_huge.len() {
            assert!((out_huge[i] as i32 - to[i] as i32).abs() <= 1);
        }
    }
}
