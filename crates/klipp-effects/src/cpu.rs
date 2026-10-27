//! The built-in effects on the CPU, each the same computation as its GPU
//! shader in `gpu.rs`, on the same linear-light f32 frame.
//!
//! The CPU runs an effect when there is no usable GPU, and for thumbnails
//! in the effect browser. It used to run every effect on 8-bit sRGB bytes,
//! so the same effect looked different depending on which path ran — a
//! blur averaged gamma-encoded bytes, levels read "0.1" on a different
//! scale. Each function here transcribes its shader, down to edge
//! sampling and rounding (WGSL `round` is ties-to-even; float → int
//! casts saturate), and `tests/cpu_gpu_effect_parity.rs` holds the two
//! together.
//!
//! Most effects act on light: blurs, glows, sharpening, geometry. The
//! perceptual adjustments — invert, posterize, levels, gamma, HSV,
//! equalize — act on the display-encoded picture (sRGB), as they do in
//! other editors; chroma key keys on it too.

use rayon::prelude::*;

use crate::BuiltinEffect;

type Px = [f32; 4];

/// sRGB encode, as the shaders' `linear_to_srgb_component`.
pub(crate) fn encode(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB decode, as the shaders' `srgb_to_linear_component`.
pub(crate) fn decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The picture's brightness, by the *working space's* luminance weights.
///
/// Not BT.709's, which is what every caller here used to assume. An
/// effect that gates on brightness — a bright pass, a denoise threshold,
/// an edge measure — mis-estimates it in any other basis, and the weights
/// differ materially: BT.2020's green is 0.678 against BT.709's 0.715,
/// and AP1's is 0.674. The GPU twins read the same numbers out of their
/// params, which `cpu_gpu_effect_parity` holds them to.
fn luma(p: Px, yw: [f32; 3]) -> f32 {
    yw[0] * p[0] + yw[1] * p[1] + yw[2] * p[2]
}

/// How much of a pixel a Luma Key keeps (#1638), from its encoded colour
/// `e`: 0 inside `[low, high]`, back to 1 across `soft` past either end;
/// `invert` keeps the range instead. It is the grade's LumaKey matte
/// (`klipp_color::luma_key::luma_key_value`, which this crate cannot call:
/// klipp-color sits above it) with its range selecting what goes, each
/// channel clamped to `[0, 1]` before it is weighted, as there;
/// `tests/luma_key_effect.rs` holds the two equal.
pub(crate) fn luma_key_keep(e: [f32; 3], low: f32, high: f32, soft: f32, invert: bool, yw: [f32; 3]) -> f32 {
    let e = e.map(|c| c.clamp(0.0, 1.0));
    let y = yw[0] * e[0] + yw[1] * e[1] + yw[2] * e[2];
    let inside = if y >= low && y <= high {
        1.0
    } else if soft <= 0.0001 {
        0.0
    } else if y < low {
        (1.0 - (low - y) / soft).clamp(0.0, 1.0)
    } else {
        (1.0 - (y - high) / soft).clamp(0.0, 1.0)
    };
    if invert { inside } else { 1.0 - inside }
}

fn mix(a: Px, b: Px, t: f32) -> Px {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// A read-only frame.
pub(crate) struct Frame<'a> {
    p: &'a [f32],
    w: usize,
    h: usize,
}

impl Frame<'_> {
    pub(crate) fn at(&self, x: usize, y: usize) -> Px {
        let i = (y * self.w + x) * 4;
        [self.p[i], self.p[i + 1], self.p[i + 2], self.p[i + 3]]
    }

    /// Clamp-to-edge sample.
    fn clamped(&self, x: i32, y: i32) -> Px {
        self.at(x.clamp(0, self.w as i32 - 1) as usize, y.clamp(0, self.h as i32 - 1) as usize)
    }
}

/// A new frame, each pixel computed from its coordinates and the input.
pub(crate) fn each_pixel(input: &[f32], w: u32, h: u32, f: impl Fn(usize, usize, &Frame) -> Px + Sync) -> Vec<f32> {
    let src = Frame { p: input, w: w as usize, h: h as usize };
    let mut out = vec![0.0f32; input.len()];
    out.par_chunks_mut(w as usize * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w as usize {
            row[x * 4..x * 4 + 4].copy_from_slice(&f(x, y, &src));
        }
    });
    out
}

/// Each pixel from itself alone, in place.
fn each_value(mut p: Vec<f32>, f: impl Fn(Px) -> Px + Sync) -> Vec<f32> {
    p.par_chunks_exact_mut(4).for_each(|c| {
        let o = f([c[0], c[1], c[2], c[3]]);
        c.copy_from_slice(&o);
    });
    p
}

/// A per-pixel adjustment on the display-encoded RGB (alpha untouched).
fn each_encoded(p: Vec<f32>, f: impl Fn([f32; 3]) -> [f32; 3] + Sync) -> Vec<f32> {
    each_value(p, |c| {
        let o = f([encode(c[0]), encode(c[1]), encode(c[2])]);
        [decode(o[0]), decode(o[1]), decode(o[2]), c[3]]
    })
}

/// The shaders' `round`: ties to even.
fn round(v: f32) -> i32 {
    v.round_ties_even() as i32
}

/// One horizontal then one vertical box pass of radius `r`.
fn box_blur(p: &[f32], w: u32, h: u32, r: i32) -> Vec<f32> {
    let inv = 1.0 / (2 * r + 1) as f32;
    let horizontal = each_pixel(p, w, h, |x, y, f| {
        let mut s = [0.0f32; 4];
        for dx in -r..=r {
            let q = f.clamped(x as i32 + dx, y as i32);
            (0..4).for_each(|c| s[c] += q[c]);
        }
        s.map(|v| v * inv)
    });
    each_pixel(&horizontal, w, h, |x, y, f| {
        let mut s = [0.0f32; 4];
        for dy in -r..=r {
            let q = f.clamped(x as i32, y as i32 + dy);
            (0..4).for_each(|c| s[c] += q[c]);
        }
        s.map(|v| v * inv)
    })
}

/// The Gaussian approximation every blur-based effect shares: three box
/// passes sized by `boxes_for_gaussian`.
pub(crate) fn gaussian(p: &[f32], w: u32, h: u32, sigma: f32, skip_zero: bool) -> Vec<f32> {
    let mut out = p.to_vec();
    for r in crate::blur::boxes_for_gaussian(sigma) {
        if r == 0 && skip_zero {
            continue;
        }
        out = box_blur(&out, w, h, r as i32);
    }
    out
}

/// Bright parts above `threshold`, scaled by how far above.
fn bright(p: &[f32], threshold: f32, yw: [f32; 3]) -> Vec<f32> {
    each_value(p.to_vec(), |c| {
        let l = luma(c, yw);
        if l > threshold {
            // A weight, not a ramp: normalising by `1 - threshold` gave a
            // factor of 34 to a 12x highlight, so bloom and halation grew
            // the light they spread instead of spreading it.
            let f = ((l - threshold) / (1.0 - threshold + 1e-6)).clamp(0.0, 1.0);
            [c[0] * f, c[1] * f, c[2] * f, c[3]]
        } else {
            [0.0; 4]
        }
    })
}

/// Screen `layer` × `intensity` over `base`, the layer first desaturated
/// by `desat`.
fn screen(base: &[f32], layer: &[f32], intensity: f32, desat: f32, yw: [f32; 3]) -> Vec<f32> {
    let mut out = base.to_vec();
    out.par_chunks_exact_mut(4).zip(layer.par_chunks_exact(4)).for_each(|(o, b)| {
        let l = luma([b[0], b[1], b[2], 0.0], yw);
        for c in 0..3 {
            let v = (b[c] + (l - b[c]) * desat) * intensity;
            o[c] = screen_at_ceiling(o[c], v);
        }
    });
    out
}

/// A glow: the picture premultiplied, blurred and screened over itself,
/// its light seen where the picture is clear, so it spreads past a
/// layer's edges (#2703); the GPU's glow blend with `lift_alpha`. An
/// opaque pixel is the plain screen.
fn glow(p: &[f32], w: u32, h: u32, radius: f32, intensity: f32, threshold: f32, yw: [f32; 3]) -> Vec<f32> {
    // Above a threshold, only the highlights give light, eased in over
    // the range above it (#2763): the bloom's weight.
    let weight = |c: &[f32]| {
        let l = luma([c[0], c[1], c[2], c[3]], yw);
        if threshold <= 0.0 { 1.0 } else if l > threshold { ((l - threshold) / (1.0 - threshold + 1e-6)).clamp(0.0, 1.0) } else { 0.0 }
    };
    let seen: Vec<f32> = p.chunks_exact(4).flat_map(|c| {
        let f = weight(c);
        [c[0] * c[3] * f, c[1] * c[3] * f, c[2] * c[3] * f, c[3]]
    }).collect();
    let light = gaussian(&seen, w, h, radius, false);
    let mut out = p.to_vec();
    out.par_chunks_exact_mut(4).zip(light.par_chunks_exact(4)).for_each(|(o, l)| {
        let b = [l[0] * intensity, l[1] * intensity, l[2] * intensity];
        let a = o[3] + b[0].max(b[1]).max(b[2]).clamp(0.0, 1.0) * (1.0 - o[3]);
        if a <= 0.0 {
            o.copy_from_slice(&[0.0; 4]);
            return;
        }
        for c in 0..3 {
            o[c] = screen_at_ceiling(o[c] * o[3], b[c]) / a;
        }
        o[3] = a;
    });
    out
}

/// `screen_at_ceiling` in the shaders' shared helpers: the screen blend
/// at the picture's own ceiling, so light lands on a highlight instead of
/// the blend coming back negative.
pub(crate) fn screen_at_ceiling(o: f32, v: f32) -> f32 {
    let w = o.max(v).max(1.0);
    o + v - o * v / w
}

/// Apply one built-in effect. Frei0r plugins are not here (they take
/// run as nodes of their own, M4 step 5); stabilisation, rolling-
/// shutter correction and speed ramps need data only the GPU path or the
/// renderer has, and pass through.
pub(crate) fn apply(
    effect: &BuiltinEffect,
    p: Vec<f32>,
    w: u32,
    h: u32,
    yw: [f32; 3],
) -> Vec<f32> {
    use BuiltinEffect::*;
    let (wf, hf) = (w as f32, h as f32);
    match *effect {
        Opacity { value } => {
            if (value - 1.0).abs() < f32::EPSILON {
                return p;
            }
            each_value(p, |c| c.map(|v| v * value))
        }
        ColorFade { color, progress, .. } => {
            let t = progress.clamp(0.0, 1.0);
            if t >= 0.9999 {
                return p;
            }
            let col = [decode(color[0] as f32 / 255.0), decode(color[1] as f32 / 255.0), decode(color[2] as f32 / 255.0), color[3] as f32 / 255.0];
            each_value(p, |c| mix(col, c, t))
        }
        Crop { left, right, top, bottom, feather } => {
            if left <= 0.0 && right <= 0.0 && top <= 0.0 && bottom <= 0.0 {
                return p;
            }
            let (l, r, t, b) = (left * wf, (1.0 - right) * wf, top * hf, (1.0 - bottom) * hf);
            each_pixel(&p, w, h, |x, y, f| {
                let (xf, yf) = (x as f32, y as f32);
                if xf < l || xf >= r || yf < t || yf >= b {
                    return [0.0; 4];
                }
                let mut px = f.at(x, y);
                if feather > 0.0 {
                    let dist = (xf - l).min(r - 1.0 - xf).min((yf - t).min(b - 1.0 - yf));
                    if dist < feather {
                        // Clamped: on the last row and column `dist` goes
                        // slightly negative, taking the alpha with it.
                        px[3] *= (dist / feather).clamp(0.0, 1.0);
                    }
                }
                px
            })
        }
        GaussianBlur { radius } => {
            if radius < 0.5 {
                return p;
            }
            gaussian(&p, w, h, radius, true)
        }
        RoundedCorners { radius, feather } => crate::layer::rounded_corners(p, w, h, radius, feather),
        DropShadow { offset_x, offset_y, blur, color, opacity } => {
            crate::layer::drop_shadow(p, w, h, offset_x, offset_y, blur, color, opacity)
        }
        OuterGlow { radius, intensity, color } => crate::layer::outer_glow(p, w, h, radius, intensity, color),
        FillBars { blur, zoom, darken } => crate::layer::fill_bars(p, w, h, blur, zoom, darken),
        Sharpen { amount } => {
            if amount.abs() < f32::EPSILON {
                return p;
            }
            each_pixel(&p, w, h, |x, y, f| {
                let o = f.at(x, y);
                let mut avg = [0.0f32; 4];
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let q = f.clamped(x as i32 + dx, y as i32 + dy);
                        (0..4).for_each(|c| avg[c] += q[c]);
                    }
                }
                let avg = avg.map(|v| v / 9.0);
                [o[0] + amount * (o[0] - avg[0]), o[1] + amount * (o[1] - avg[1]), o[2] + amount * (o[2] - avg[2]), o[3]]
            })
        }
        Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x, rotation_y, perspective, .. } => {
            if position_x == 0.0
                && position_y == 0.0
                && (scale_x - 1.0).abs() < f32::EPSILON
                && (scale_y - 1.0).abs() < f32::EPSILON
                && rotation.abs() < f32::EPSILON
                && rotation_x.abs() < f32::EPSILON
                && rotation_y.abs() < f32::EPSILON
            {
                return p;
            }
            let (sin_r, cos_r) = rotation.to_radians().sin_cos();
            let inv_sx = if scale_x.abs() > f32::EPSILON { 1.0 / scale_x } else { 1.0 };
            let inv_sy = if scale_y.abs() > f32::EPSILON { 1.0 / scale_y } else { 1.0 };
            let (sin_rx, cos_rx) = rotation_x.to_radians().sin_cos();
            let (sin_ry, cos_ry) = rotation_y.to_radians().sin_cos();
            let focal = if perspective > 0.001 { wf / perspective } else { 0.0 };
            let (ax, ay) = (anchor_x * wf, anchor_y * hf);
            each_pixel(&p, w, h, |x, y, f| {
                let dx = x as f32 - ax - position_x;
                let dy = y as f32 - ay - position_y;
                let rx = dx * cos_r + dy * sin_r;
                let ry = -dx * sin_r + dy * cos_r;
                let (mut sx, mut sy) = (rx * inv_sx, ry * inv_sy);
                if cos_rx != 1.0 || cos_ry != 1.0 {
                    (sx, sy) = crate::transform::unproject(sx, sy, (cos_rx, sin_rx), (cos_ry, sin_ry), focal).unwrap_or((f32::MIN, f32::MIN));
                }
                sx += ax;
                sy += ay;
                let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
                let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
                let s = |xx: i32, yy: i32| {
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 { [0.0; 4] } else { f.at(xx as usize, yy as usize) }
                };
                let top = mix(s(x0, y0), s(x0 + 1, y0), fx);
                let bot = mix(s(x0, y0 + 1), s(x0 + 1, y0 + 1), fx);
                mix(top, bot, fy)
            })
        }
        CornerPin { tl, tr, br, bl } => {
            let corners = [tl, tr, br, bl];
            if crate::corner_pin::is_still(&corners) {
                return p;
            }
            // No quadrilateral: nothing to pin the picture to, so
            // nothing is drawn.
            let Some(inv) = crate::corner_pin::inverse_map(w, h, &corners) else {
                return vec![0.0; p.len()];
            };
            each_pixel(&p, w, h, |x, y, f| {
                let Some((sx, sy)) = crate::corner_pin::sample_point(&inv, x as f32, y as f32) else {
                    return [0.0; 4];
                };
                let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
                let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
                let s = |xx: i32, yy: i32| {
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 { [0.0; 4] } else { f.at(xx as usize, yy as usize) }
                };
                let top = mix(s(x0, y0), s(x0 + 1, y0), fx);
                let bot = mix(s(x0, y0 + 1), s(x0 + 1, y0 + 1), fx);
                mix(top, bot, fy)
            })
        }
        ChromaKey { key_color, tolerance, soft_edge, spill_method, spill_strength } => {
            let (rf, gf, bf) = (key_color[0] as f32, key_color[1] as f32, key_color[2] as f32);
            let key_cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
            let key_cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);
            let key_idx = if key_color[1] >= key_color[0] && key_color[1] >= key_color[2] {
                1
            } else if key_color[2] > key_color[0] {
                2
            } else {
                0
            };
            let method = match spill_method {
                crate::SpillMethod::None => 0,
                crate::SpillMethod::ChannelClamp => 1,
                crate::SpillMethod::Hawk => 2,
            };
            each_value(p, |mut px| {
                let s = [0, 1, 2].map(|c| encode(px[c]).clamp(0.0, 1.0) * 255.0);
                let cb = 128.0 + (-0.168736 * s[0] - 0.331264 * s[1] + 0.5 * s[2]);
                let cr = 128.0 + (0.5 * s[0] - 0.418688 * s[1] - 0.081312 * s[2]);
                let (dcb, dcr) = ((cb - key_cb) / 255.0, (cr - key_cr) / 255.0);
                let dist = (dcb * dcb + dcr * dcr).sqrt();
                if dist < tolerance {
                    return [0.0; 4];
                } else if dist < tolerance + soft_edge {
                    px[3] *= (dist - tolerance) / soft_edge;
                }
                // The spill is taken out of the key channel alone, on the
                // pixel's own values: rebuilding all three from the clamped
                // copy the key is measured on flattened every channel above
                // white and every negative (#1020).
                if method != 0 && spill_strength > 0.0 && px[3] > 0.0 {
                    let e = [0, 1, 2].map(|c| encode(px[c]) * 255.0);
                    let key = e[key_idx];
                    let (a, b) = (e[(key_idx + 1) % 3], e[(key_idx + 2) % 3]);
                    let target = if method == 1 { a.max(b) } else { 0.5 * (a + b) };
                    if key > target {
                        px[key_idx] = decode((key + spill_strength * (target - key)) / 255.0);
                    }
                }
                px
            })
        }
        // The luma key's matte taken into alpha (#1638): the range keyed
        // out, the rest kept. The twin of `SHADER_LUMA_KEY` run into alpha.
        LumaKey { low, high, soft, invert } => each_value(p, |mut px| {
            px[3] *= luma_key_keep([encode(px[0]), encode(px[1]), encode(px[2])], low, high, soft, invert, yw);
            px
        }),
        Bloom { threshold, radius, intensity } => {
            let blurred = gaussian(&bright(&p, threshold, yw), w, h, radius, false);
            let mut out = p;
            out.par_chunks_exact_mut(4).zip(blurred.par_chunks_exact(4)).for_each(|(o, b)| {
                (0..3).for_each(|c| o[c] += b[c] * intensity);
            });
            out
        }
        Glow { radius, intensity, threshold } => glow(&p, w, h, radius, intensity, threshold, yw),
        LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            let samples = samples.clamp(4, 128);
            let (cx, cy) = (center_x * wf, center_y * hf);
            let step = decay / samples as f32;
            let per = 1.0 - step;
            let inv = 1.0 / samples as f32;
            each_pixel(&p, w, h, |x, y, f| {
                let (xf, yf) = (x as f32, y as f32);
                let (dx, dy) = (xf - cx, yf - cy);
                let mut ray = [0.0f32; 3];
                let mut weight = 1.0f32;
                for s in 0..samples {
                    let t = s as f32 * step;
                    let sx = ((xf - dx * t) as u32).min(w - 1);
                    let sy = ((yf - dy * t) as u32).min(h - 1);
                    let q = f.at(sx as usize, sy as usize);
                    if luma(q, yw) > threshold {
                        (0..3).for_each(|c| ray[c] += q[c] * weight);
                    }
                    weight *= per;
                }
                let o = f.at(x, y);
                [o[0] + ray[0] * inv * intensity, o[1] + ray[1] * inv * intensity, o[2] + ray[2] * inv * intensity, o[3]]
            })
        }
        Deflicker { strength, gain, .. } => {
            // `SHADER_DEFLICKER`. A scale on the light, which is what
            // an exposure change is. Unlike stabilization and rolling
            // shutter — which pass through on the CPU because their
            // per-frame data lives on the GPU — this one's gain rides
            // in the effect, so the twin is real and parity-testable.
            let g = 1.0 + (gain - 1.0) * strength.clamp(0.0, 1.0);
            let mut p = p;
            if (g - 1.0).abs() > f32::EPSILON {
                p.par_chunks_exact_mut(4).for_each(|px| {
                    px[0] *= g;
                    px[1] *= g;
                    px[2] *= g;
                });
            }
            p
        }
        // A surface pin is the corner pin it is in this frame.
        SurfacePin { .. } => match crate::corner_pin::surface_as_pin(effect, w, h) {
            Some(pin) => apply(&pin, p, w, h, yw),
            None => p,
        },
        // A flicker is the plain effect it is at its time (#989).
        Flicker { .. } => match crate::flicker::as_plain(effect) {
            Some(plain) => apply(&plain, p, w, h, yw),
            None => p,
        },
        // The twin of `SHADER_PARTICLES` (#1141).
        Particles { .. } => crate::particles::apply(effect, p, w, h),
        // The twin of the Warp's shader (#2758).
        Warp { .. } => match effect.warp_settings() {
            Some(s) if !crate::warp::is_identity(&s) => crate::warp::cpu(&p, w, h, &s),
            _ => p,
        },
        // The twin of `displace::SHADER` (#2702).
        Displace { amount, size, speed, direction, seed, time } => {
            if amount.abs() < f32::EPSILON {
                return p;
            }
            crate::displace::cpu(&p, w, h, amount, size, speed, direction, seed, time)
        }
        // The twin of `reflection::shader_wgsl` (#2707).
        Reflection { .. } => match effect.reflection_params(w, h) {
            Some(params) if !crate::is_identity_effect(effect) => crate::reflection::cpu(&p, &params),
            _ => p,
        },
        // The twin of `SHADER_BINOCULAR_VIGNETTE` (#989).
        BinocularVignette { amount, spacing, width: eye_w, height: eye_h, softness, fringe } => {
            if amount.abs() < f32::EPSILON && fringe.abs() < f32::EPSILON {
                return p;
            }
            let (rx, ry) = ((eye_w * hf * 0.5).max(1.0), (eye_h * hf * 0.5).max(1.0));
            let cy = hf * 0.5;
            let (c1, c2) = (wf * (0.5 - spacing * 0.5), wf * (0.5 + spacing * 0.5));
            let soft = softness.max(0.01);
            each_pixel(&p, w, h, |x, y, f| {
                let (x, y) = (x as f32, y as f32);
                let dist = |c: f32| (((x - c) / rx) * ((x - c) / rx) + ((y - cy) / ry) * ((y - cy) / ry)).sqrt();
                let (d1, d2) = (dist(c1), dist(c2));
                let (d, cx) = if d2 < d1 { (d2, c2) } else { (d1, c1) };
                let falloff = ((d - (1.0 - soft)) / soft).clamp(0.0, 1.0);
                let k = 1.0 + amount * falloff;
                let px = f.at(x as usize, y as usize);
                let (mut r, mut b) = (px[0], px[2]);
                let (vx, vy) = (x - cx, y - cy);
                let len = (vx * vx + vy * vy).sqrt();
                let reach = d.clamp(0.0, 1.0);
                let s = fringe * reach * reach;
                if len > 0.0 && s != 0.0 {
                    let (ux, uy) = (vx / len * s, vy / len * s);
                    r = bilinear(f, x + ux, y + uy)[0];
                    b = bilinear(f, x - ux, y - uy)[2];
                }
                [r * k, px[1] * k, b * k, px[3]]
            })
        }
        Vignette { amount, size, softness, roundness } => {
            let (cx, cy) = (wf * 0.5, hf * 0.5);
            each_pixel(&p, w, h, |x, y, f| {
                let (nx, ny) = ((x as f32 - cx) / cx, (y as f32 - cy) / cy);
                let rect = nx.abs().max(ny.abs());
                let circ = (nx * nx + ny * ny).sqrt();
                let dist = rect * (1.0 - roundness) + circ * roundness;
                let falloff = ((dist - size) / softness.max(0.01)).clamp(0.0, 1.0);
                let k = 1.0 + amount * falloff;
                let o = f.at(x, y);
                [o[0] * k, o[1] * k, o[2] * k, o[3]]
            })
        }
        Diffusion { amount, radius, threshold, desaturation } => {
            screen(&p, &gaussian(&bright(&p, threshold, yw), w, h, radius, false), amount, desaturation, yw)
        }
        LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
            let (fx, fy) = (position_x * wf, position_y * hf);
            let (cx, cy) = (wf * 0.5, hf * 0.5);
            let disc_r = flare_size * wf.max(hf);
            each_pixel(&p, w, h, |x, y, f| {
                let (px, py) = (x as f32, y as f32);
                let mut add = [0.0f32; 3];
                let dist = ((px - fx) * (px - fx) + (py - fy) * (py - fy)).sqrt();
                if dist < disc_r {
                    let fo = 1.0 - dist / disc_r;
                    let g = fo * fo * brightness;
                    add = [add[0] + g, add[1] + g * 0.9, add[2] + g * 0.7];
                }
                for gi in 0..ghost_count {
                    let t = (gi as f32 + 1.0) / (ghost_count as f32 + 1.0);
                    let gx = fx + (cx - fx) * t * 2.0;
                    let gy = fy + (cy - fy) * t * 2.0;
                    let gr = disc_r * (0.3 + t * 0.4);
                    let gd = ((px - gx) * (px - gx) + (py - gy) * (py - gy)).sqrt();
                    if gd < gr {
                        let gf = 1.0 - gd / gr;
                        let g = gf * gf * ghost_intensity * brightness;
                        let ca = chromatic_aberration * t;
                        add = [add[0] + g * (1.0 + ca), add[1] + g, add[2] + g * (1.0 - ca * 0.5)];
                    }
                }
                let o = f.at(x, y);
                [o[0] + add[0], o[1] + add[1], o[2] + add[2], o[3]]
            })
        }
        LightWrap { width: wrap, intensity, softness } => {
            screen(&p, &gaussian(&p, w, h, wrap * softness, false), intensity, 0.0, yw)
        }
        AnamorphicStreak { threshold, length, intensity, tint } => {
            let half = (length * 0.5) as u32;
            let (tint_r, tint_b) = (1.0 + (1.0 - tint) * 0.3, 1.0 + tint * 0.5);
            each_pixel(&p, w, h, |x, y, f| {
                let start = (x as i32 - half as i32).max(0) as u32;
                let end = (x as u32 + half + 1).min(w);
                let mut s = [0.0f32; 3];
                for sx in start..end {
                    let q = f.at(sx as usize, y);
                    let l = luma(q, yw);
                    if l > threshold {
                        let k = ((l - threshold) / (1.0 - threshold + 1e-6)).clamp(0.0, 1.0);
                        (0..3).for_each(|c| s[c] += q[c] * k);
                    }
                }
                let n = (end - start) as f32;
                let s = s.map(|v| v / n);
                let o = f.at(x, y);
                [o[0] + s[0] * tint_r * intensity, o[1] + s[1] * intensity, o[2] + s[2] * tint_b * intensity, o[3]]
            })
        }
        FilmHalation { threshold, radius, intensity, hue } => {
            // The spread light takes the halation's colour — deep red to
            // warm yellow — whatever colour the highlight was.
            let tint = [1.0, hue * 0.6, hue * hue * 0.2];
            let blurred = gaussian(&bright(&p, threshold, yw), w, h, radius, false);
            let mut out = p;
            out.par_chunks_exact_mut(4).zip(blurred.par_chunks_exact(4)).for_each(|(o, b)| {
                let l = luma([b[0], b[1], b[2], 0.0], yw);
                for c in 0..3 {
                    o[c] = screen_at_ceiling(o[c], l * tint[c] * intensity);
                }
            });
            out
        }
        Invert { mix: m } => {
            if m.abs() < f32::EPSILON {
                return p;
            }
            // The inverse of a highlight is black, not a negative: invert
            // what the display can show, and leave `v` as the mix's base
            // so mix 0 is identity.
            each_encoded(p, |s| s.map(|v| v + ((1.0 - v.clamp(0.0, 1.0)) - v) * m))
        }
        Posterize { levels } => {
            // The band each value falls in is found against the ladder's
            // edges, worked out once (`PosterizeLadder`), as the shader
            // finds it: bit for bit the same band on both routes.
            let ladder = PosterizeLadder::new(levels);
            each_value(p, |c| [ladder.band(c[0]), ladder.band(c[1]), ladder.band(c[2]), c[3]])
        }
        Mosaic { block_size } => {
            if block_size <= 1.0 {
                return p;
            }
            each_pixel(&p, w, h, |x, y, f| {
                let bx = ((x as f32 / block_size).floor() * block_size) as u32;
                let by = ((y as f32 / block_size).floor() * block_size) as u32;
                f.at(bx.min(w - 1) as usize, by.min(h - 1) as usize)
            })
        }
        EdgeDetect { strength, mix: m } => each_pixel(&p, w, h, |x, y, f| {
            let (x, y) = (x as i32, y as i32);
            let l = |dx: i32, dy: i32| luma(f.clamped(x + dx, y + dy), yw);
            let gx = -l(-1, -1) + l(1, -1) - 2.0 * l(-1, 0) + 2.0 * l(1, 0) - l(-1, 1) + l(1, 1);
            let gy = -l(-1, -1) - 2.0 * l(0, -1) - l(1, -1) + l(-1, 1) + 2.0 * l(0, 1) + l(1, 1);
            let edge = ((gx * gx + gy * gy).sqrt() * strength).clamp(0.0, 1.0);
            let o = f.at(x as usize, y as usize);
            [o[0] * (1.0 - m) + edge * m, o[1] * (1.0 - m) + edge * m, o[2] * (1.0 - m) + edge * m, o[3]]
        }),
        ChannelMixer { red_red, red_green, red_blue, green_red, green_green, green_blue, blue_red, blue_green, blue_blue } => {
            each_value(p, |c| {
                [
                    red_red * c[0] + red_green * c[1] + red_blue * c[2],
                    green_red * c[0] + green_green * c[1] + green_blue * c[2],
                    blue_red * c[0] + blue_green * c[1] + blue_blue * c[2],
                    c[3],
                ]
            })
        }
        DirectionalBlur { angle, distance, samples } => {
            if distance < 0.5 {
                return p;
            }
            let n = samples.clamp(3, 32);
            let (sin_a, cos_a) = angle.to_radians().sin_cos();
            let (dx, dy) = (cos_a * distance / n as f32, sin_a * distance / n as f32);
            each_pixel(&p, w, h, |x, y, f| {
                // Straight and premultiplied sums together: a tap's colour
                // counts by how much of it is there, so a picture over
                // transparency smears at its coverage instead of darkening
                // toward the clear pixels' colour (#2021), as the shutter's
                // samples are averaged (#2017).
                let (mut s, mut pre) = ([0.0f32; 4], [0.0f32; 3]);
                for i in 0..n {
                    let q = f.clamped(round(x as f32 + dx * i as f32), round(y as f32 + dy * i as f32));
                    (0..4).for_each(|c| s[c] += q[c]);
                    (0..3).for_each(|c| pre[c] += q[c] * q[3]);
                }
                // Every tap opaque: the plain mean, to the bit as before.
                if s[3] == n as f32 {
                    return s.map(|v| v / n as f32);
                }
                let colour = |c: usize| if s[3] > crate::UNPREMULTIPLY_MIN_ALPHA { pre[c] / s[3] } else { 0.0 };
                [colour(0), colour(1), colour(2), s[3] / n as f32]
            })
        }
        Emboss { angle, strength, mix: m } => {
            let (dy, dx) = angle.to_radians().sin_cos();
            each_pixel(&p, w, h, |x, y, f| {
                let o = f.at(x, y);
                if x == 0 || y == 0 || x >= w as usize - 1 || y >= h as usize - 1 {
                    return o;
                }
                let p1 = f.clamped(round(x as f32 + dx), round(y as f32 + dy));
                let p2 = f.clamped(round(x as f32 - dx), round(y as f32 - dy));
                let e = [0, 1, 2].map(|c| 0.18 + (p1[c] - p2[c]) * strength);
                [o[0] + (e[0] - o[0]) * m, o[1] + (e[1] - o[1]) * m, o[2] + (e[2] - o[2]) * m, o[3]]
            })
        }
        Mirror { horizontal, vertical } => {
            if horizontal == 0 && vertical == 0 {
                return p;
            }
            each_pixel(&p, w, h, |x, y, f| {
                let sx = if horizontal != 0 { w as usize - 1 - x } else { x };
                let sy = if vertical != 0 { h as usize - 1 - y } else { y };
                f.at(sx, sy)
            })
        }
        LensDistortion { distortion, scale } => {
            if distortion.abs() < f32::EPSILON && (scale - 1.0).abs() < f32::EPSILON {
                return p;
            }
            let (cx, cy) = (wf / 2.0, hf / 2.0);
            each_pixel(&p, w, h, |x, y, f| {
                let (nx, ny) = ((x as f32 - cx) / cx, (y as f32 - cy) / cy);
                let k = 1.0 + distortion * (nx * nx + ny * ny);
                let sx = (nx * k / scale) * cx + cx;
                let sy = (ny * k / scale) * cy + cy;
                let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
                let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
                let top = mix(f.clamped(x0, y0), f.clamped(x0 + 1, y0), fx);
                let bot = mix(f.clamped(x0, y0 + 1), f.clamped(x0 + 1, y0 + 1), fx);
                mix(top, bot, fy)
            })
        }
        Dilate { radius } | Erode { radius } => {
            let dilate = matches!(effect, Dilate { .. });
            let r = radius.clamp(1, 10) as i32;
            each_pixel(&p, w, h, |x, y, f| {
                // From the centre, not from black or white: starting at
                // 1.0 made every pixel of a picture brighter than white
                // erode to white, and 0.0 clipped negative channels.
                let mut m = f.at(x, y);
                for dy in -r..=r {
                    for dx in -r..=r {
                        let q = f.clamped(x as i32 + dx, y as i32 + dy);
                        for c in 0..4 {
                            m[c] = if dilate { m[c].max(q[c]) } else { m[c].min(q[c]) };
                        }
                    }
                }
                m
            })
        }
        Denoise { spatial_sigma, color_sigma, iterations } => {
            let k = ((spatial_sigma * 2.0).ceil() as i32).min(10);
            let sd = 2.0 * spatial_sigma * spatial_sigma;
            let cd = 2.0 * color_sigma * color_sigma;
            let mut out = p;
            for _ in 0..iterations.clamp(1, 3) {
                out = each_pixel(&out, w, h, |x, y, f| {
                    let c = f.at(x, y);
                    let cl = luma(c, yw);
                    let mut s = [0.0f32; 3];
                    let mut total = 0.0f32;
                    for dy in -k..=k {
                        for dx in -k..=k {
                            let n = f.clamped(x as i32 + dx, y as i32 + dy);
                            let d = cl - luma(n, yw);
                            let wgt = (-((dx * dx + dy * dy) as f32) / sd).exp() * (-d * d / cd).exp();
                            (0..3).for_each(|ch| s[ch] += n[ch] * wgt);
                            total += wgt;
                        }
                    }
                    let inv = 1.0 / total;
                    [s[0] * inv, s[1] * inv, s[2] * inv, c[3]]
                });
            }
            out
        }
        Gamma { gamma } => {
            if (gamma - 1.0).abs() < f32::EPSILON {
                return p;
            }
            let inv = 1.0 / gamma;
            each_encoded(p, |s| s.map(|v| v.max(0.0).powf(inv)))
        }
        HSVAdjust { hue_shift, saturation, value } => {
            if hue_shift.abs() < f32::EPSILON && (saturation - 1.0).abs() < f32::EPSILON && (value - 1.0).abs() < f32::EPSILON {
                return p;
            }
            each_encoded(p, |s| hsv_adjust(s, hue_shift, saturation, value))
        }
        Vibrance { amount } => each_value(p, |c| {
            let sat = c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
            let l = luma(c, yw);
            let boost = 1.0 + amount * (1.0 - sat.clamp(0.0, 1.0));
            [l + (c[0] - l) * boost, l + (c[1] - l) * boost, l + (c[2] - l) * boost, c[3]]
        }),
        NoiseGenerator { amount, monochrome, seed } => {
            if amount < f32::EPSILON {
                return p;
            }
            let mut out = p;
            out.par_chunks_exact_mut(4).enumerate().for_each(|(i, c)| {
                let n = (i as u32).wrapping_mul(7919).wrapping_add(seed);
                if monochrome != 0 {
                    let v = (noise_hash(n) - 0.5) * 2.0 * amount;
                    (0..3).for_each(|ch| c[ch] += v);
                } else {
                    (0..3).for_each(|ch| c[ch] += (noise_hash(n.wrapping_add(ch as u32)) - 0.5) * 2.0 * amount);
                }
            });
            out
        }
        MedianFilter { radius } => {
            let r = radius.clamp(1, 2) as i32;
            each_pixel(&p, w, h, |x, y, f| {
                let mut vals: [Vec<f32>; 3] = Default::default();
                for dy in -r..=r {
                    for dx in -r..=r {
                        let q = f.clamped(x as i32 + dx, y as i32 + dy);
                        (0..3).for_each(|c| vals[c].push(q[c]));
                    }
                }
                let mid = vals[0].len() / 2;
                let m = vals.map(|mut v| *v.select_nth_unstable_by(mid, f32::total_cmp).1);
                [m[0], m[1], m[2], f.at(x, y)[3]]
            })
        }
        RadialBlur { center_x, center_y, amount, samples, mode } => {
            if amount < 0.01 {
                return p;
            }
            let s = samples.clamp(4, 32);
            let (cx, cy) = (center_x * wf, center_y * hf);
            each_pixel(&p, w, h, |x, y, f| {
                let (xf, yf) = (x as f32, y as f32);
                let (dx, dy) = (xf - cx, yf - cy);
                let mut sum = [0.0f32; 3];
                for i in 0..s {
                    let t = (i as f32 / s as f32 - 0.5) * 2.0;
                    // Sampled bilinearly, as the shader samples: a nearest
                    // pixel flips on the last bit of a position that lands
                    // on a half (the shader's fused multiply-add, its
                    // sin and cos), and the two routes then read different
                    // pixels at every edge (#1440).
                    let (sx, sy) = if mode == 0 {
                        let scale = amount / 100.0;
                        (xf + dx * t * scale, yf + dy * t * scale)
                    } else {
                        let ang = t * (amount * std::f32::consts::PI / 180.0 * 0.5);
                        let (sin_a, cos_a) = ang.sin_cos();
                        (cx + dx * cos_a - dy * sin_a, cy + dx * sin_a + dy * cos_a)
                    };
                    let q = bilinear(f, sx, sy);
                    (0..3).for_each(|c| sum[c] += q[c]);
                }
                let inv = 1.0 / s as f32;
                [sum[0] * inv, sum[1] * inv, sum[2] * inv, f.at(x, y)[3]]
            })
        }
        Levels { in_black, in_white, gamma, out_black, out_white } => {
            if in_black <= 0.0 && (in_white - 1.0).abs() < f32::EPSILON && (gamma - 1.0).abs() < f32::EPSILON
                && out_black <= 0.0 && (out_white - 1.0).abs() < f32::EPSILON
            {
                return p;
            }
            let inv_gamma = if gamma.abs() > f32::EPSILON { 1.0 / gamma } else { 1.0 };
            let in_range = (in_white - in_black).max(0.001);
            // No ceiling: a highlight above the input white keeps its
            // distance above the output white rather than clipping to it.
            each_encoded(p, |s| {
                s.map(|v| out_black + ((v - in_black) / in_range).max(0.0).powf(inv_gamma) * (out_white - out_black))
            })
        }
        Equalize { strength } => {
            let cdf = equalize_cdf(&p, yw);
            // The histogram covers what the display shows; light above
            // white passes through, so equalising does not crush the
            // highlights.
            each_encoded(p, |s| {
                let d = s.map(|v| v.min(1.0));
                let l = yw[0] * d[0] + yw[1] * d[1] + yw[2] * d[2];
                let eq = cdf[(l * 255.0).clamp(0.0, 255.0) as usize];
                let k = if l < 0.001 { 1.0 } else { eq / l };
                [0, 1, 2].map(|i| d[i] + (d[i] * k - d[i]) * strength + (s[i] - 1.0).max(0.0))
            })
        }
        Letterbox { aspect_ratio, bar_color, opacity } => {
            let frame_aspect = wf / hf;
            let (bar_h, bar_w) =
                if frame_aspect > aspect_ratio { (0.0, (wf - hf * aspect_ratio) / 2.0) } else { ((hf - wf / aspect_ratio) / 2.0, 0.0) };
            let bv = if bar_color != 0 { 1.0 } else { 0.0 };
            each_pixel(&p, w, h, |x, y, f| {
                let v = f.at(x, y);
                let (xf, yf) = (x as f32, y as f32);
                if yf < bar_h || yf >= hf - bar_h || xf < bar_w || xf >= wf - bar_w {
                    [
                        v[0] * (1.0 - opacity) + bv * opacity,
                        v[1] * (1.0 - opacity) + bv * opacity,
                        v[2] * (1.0 - opacity) + bv * opacity,
                        v[3] * (1.0 - opacity) + opacity,
                    ]
                } else {
                    v
                }
            })
        }
        FilmGrain { amount, size, color, shadows, seed } => {
            if amount < f32::EPSILON {
                return p;
            }
            let per_px = grain_cells_per_px(size, h);
            each_pixel(&p, w, h, |x, y, f| {
                let o = f.at(x, y);
                let gx = (x as f32 * per_px) as u32;
                let gy = (y as f32 * per_px) as u32;
                let base = gx.wrapping_mul(7919).wrapping_add(gy.wrapping_mul(104729)).wrapping_add(seed.wrapping_mul(15485863));
                let l = luma(o, yw).clamp(0.0, 1.0);
                let depth = amount * 0.35 * (1.0 - shadows * l);
                let n0 = (noise_hash(base) - 0.5) * 2.0 * depth;
                if color != 0 {
                    let n1 = (noise_hash(base.wrapping_add(1)) - 0.5) * 2.0 * depth;
                    let n2 = (noise_hash(base.wrapping_add(2)) - 0.5) * 2.0 * depth;
                    [o[0] + n0, o[1] + n1, o[2] + n2, o[3]]
                } else {
                    [o[0] + n0, o[1] + n0, o[2] + n0, o[3]]
                }
            })
        }
        TiltShift { center, width, softness, radius } => {
            if radius < 0.5 {
                return p;
            }
            let blurred = gaussian(&p, w, h, radius, true);
            let soft = softness.max(0.001);
            each_pixel(&p, w, h, |x, y, f| {
                let ny = (y as f32 + 0.5) / hf;
                let d = (ny - center).abs() - width * 0.5;
                let m = (d / soft).clamp(0.0, 1.0);
                let m = m * m * (3.0 - 2.0 * m);
                let o = f.at(x, y);
                let i = (y * w as usize + x) * 4;
                [
                    o[0] + (blurred[i] - o[0]) * m,
                    o[1] + (blurred[i + 1] - o[1]) * m,
                    o[2] + (blurred[i + 2] - o[2]) * m,
                    o[3],
                ]
            })
        }
        FocusBlur { radius, vertical, band } => {
            if radius < 0.5 || band >= 0.999 {
                return p;
            }
            // Every channel, alpha too: a title's words live in alpha,
            // and a blur that kept it sharp would not soften them. Blurred
            // and mixed premultiplied, so nothing bleeds from the colour
            // under no alpha; the GPU's `SHADER_FOCUS_BLUR_BLEND` twin.
            let premultiplied: Vec<f32> = p.chunks_exact(4).flat_map(|q| [q[0] * q[3], q[1] * q[3], q[2] * q[3], q[3]]).collect();
            let blurred = gaussian(&premultiplied, w, h, radius, true);
            each_pixel(&p, w, h, |x, y, f| {
                let m = focus_blur_mix(if vertical { (y as f32 + 0.5) / hf } else { (x as f32 + 0.5) / wf }, band);
                let o = f.at(x, y);
                let i = (y * w as usize + x) * 4;
                let op = [o[0] * o[3], o[1] * o[3], o[2] * o[3], o[3]];
                let mixed: [f32; 4] = std::array::from_fn(|c| op[c] + (blurred[i + c] - op[c]) * m);
                if mixed[3] > 1e-6 {
                    [mixed[0] / mixed[3], mixed[1] / mixed[3], mixed[2] / mixed[3], mixed[3]]
                } else {
                    [0.0, 0.0, 0.0, mixed[3]]
                }
            })
        }
        // The twin of `old_film::shader_wgsl` (#2744).
        OldFilm { .. } => match effect.old_film_params(w, h, yw) {
            Some(params) => crate::old_film::cpu(&p, &params),
            None => p,
        },
        Vhs { tracking, bleed, noise, scanlines, seed } => {
            let band_y = noise_hash(seed.wrapping_mul(2654435761)) * hf;
            let band_h = hf * 0.06;
            each_pixel(&p, w, h, |x, y, f| {
                let row = y as u32;
                let rh = noise_hash(row.wrapping_mul(7919).wrapping_add(seed.wrapping_mul(15485863)));
                let in_band = (y as f32 - band_y).abs() < band_h;
                let mut dx = (rh - 0.5) * tracking * 3.0;
                if in_band {
                    dx += (rh - 0.5) * tracking * 40.0;
                }
                let o = f.clamped(round(x as f32 + dx), y as i32);
                let sh = f.clamped(round(x as f32 + dx - bleed * 8.0), y as i32);
                let lo = luma(o, yw);
                let ls = luma(sh, yw);
                let mut r = lo + (sh[0] - ls);
                let mut g = lo + (sh[1] - ls);
                let mut b = lo + (sh[2] - ls);
                let mut n = (noise_hash((row * w).wrapping_add(x as u32).wrapping_mul(7919).wrapping_add(seed)) - 0.5) * noise * 0.3;
                if in_band {
                    n *= 3.0;
                }
                r += n;
                g += n;
                b += n;
                if row % 2 == 1 {
                    let k = 1.0 - scanlines * 0.25;
                    r *= k;
                    g *= k;
                    b *= k;
                }
                [r, g, b, o[3]]
            })
        }
        ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
            if red_shift.abs() < f32::EPSILON && blue_shift.abs() < f32::EPSILON {
                return p;
            }
            let (cx, cy) = (center_x * wf, center_y * hf);
            let max_dist = (cx * cx + cy * cy).sqrt().max(1.0);
            each_pixel(&p, w, h, |x, y, f| {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let norm = (dx * dx + dy * dy).sqrt() / max_dist;
                // Sampled bilinearly, as the shader samples: a nearest
                // pixel flips on the last bit of a position landing on a
                // half (#1442).
                let shifted = |shift: f32, c: usize| {
                    let k = 1.0 + shift * norm / max_dist;
                    bilinear(f, cx + dx * k, cy + dy * k)[c]
                };
                let g = f.at(x, y);
                [shifted(red_shift, 0), g[1], shifted(blue_shift, 2), g[3]]
            })
        }
        // TemporalDenoise ran at the head of the pipeline, before the
        // grade; the compositor filters it out before the chain. This
        // arm is what a caller assembling an effect list by hand hits,
        // and passing through is right — the work is already done.
        RollingShutterCorrection { .. } | VideoStabilization { .. } | SpeedRamp { .. } | Frei0r { .. }
        | TemporalDenoise { .. } => p,
        // Their prepared tables are run by the chain (`apply_chain`); a
        // caller here has nothing to run them with.
        Lut3D { .. } | ColourSpaceTransform { .. } => p,
        PictureTransform { .. } => match effect.picture_transform_as_transform() {
            Some(t) => apply(&t, p, w, h, yw),
            None => p,
        },
        // A merge is drawn by the plan's layer mixer, with its foreground;
        // the generator by the chain (`apply_chain`), a media source's
        // picture and what is below by the executor (`draws_a_side_picture`).
        Merge { .. } | Generator { .. } | MediaSource { .. } | TitleSource { .. } | ClipBelow {} | GradeCompound {} => p,
    }
}

/// Film Grain's grain cells per pixel at `size` (a cell is `size` pixels
/// of a 1080-line frame, at least one pixel), worked out once here for
/// both routes. Each pixel's cell is its position times this: a product
/// rounds the same on the CPU and in WGSL, where a division (`x / cell`)
/// is only good to 2.5 ULP, and a position landing on a cell's edge then
/// fell in the next cell on one route alone, with an unrelated grain
/// value, whenever the size was fractional, as between two keys (#1441).
pub(crate) fn grain_cells_per_px(size: f32, height: u32) -> f32 {
    1.0 / (size * height as f32 / 1080.0).max(1.0)
}

/// How many band edges [`PosterizeLadder`] holds on each side of white.
pub(crate) const POSTERIZE_EDGES: usize = 256;

/// Posterize's ladder for `levels` bands: bands even on the display scale,
/// white staying white and the ladder carrying on above it at the same
/// spacing, rather than every highlight landing on white.
///
/// A value's band was `floor(encoded · levels)`. The encoding goes through
/// `pow`, which the shader works out only approximately, so a value whose
/// `encoded · levels` sat within a few ULPs of a whole number fell in the
/// band below on one route and the band above on the other (#1443). So
/// the edges are worked out here, once, as the linear values they decode
/// to, and both routes find a band by comparing the value itself with
/// them: the same comparisons on the same numbers. Only the band's own
/// value is then decoded per route, which is continuous.
pub(crate) struct PosterizeLadder {
    /// The levels, clamped to `[2, 256]`. Fewer than two has no band to
    /// step to (at one, every pixel was 0/0; a keyframe curve's overshoot
    /// or a hand-edited file can ask for it, #1020), and the control's
    /// range ends at 256, which the table is sized for.
    pub(crate) levels: f32,
    /// The linear value each band below white starts at, ascending, then
    /// `f32::MAX` (never reached; WGSL may assume no infinities).
    pub(crate) inside: [f32; POSTERIZE_EDGES],
    /// The same above white, the ladder's steps past it.
    pub(crate) above: [f32; POSTERIZE_EDGES],
    /// How many of `above` are edges. Past the last, the step is worked
    /// out from the encoding (about twice white at 256 levels).
    pub(crate) above_count: u32,
}

impl PosterizeLadder {
    /// `levels` as the ladder takes it (see [`Self::levels`]).
    pub(crate) fn clamped(levels: f32) -> f32 {
        levels.clamp(2.0, POSTERIZE_EDGES as f32)
    }

    /// How many above-white edges a ladder holds: all of them. The highest,
    /// at two levels, is encoded 1 + 256 / 2 = 129, about 1.2e5 in light,
    /// so every edge decodes finite (`new` checks it all the same).
    pub(crate) fn above_count(_levels: f32) -> u32 {
        POSTERIZE_EDGES as u32
    }

    pub(crate) fn new(levels: f32) -> Self {
        let levels = Self::clamped(levels);
        let div = levels - 1.0;
        let mut inside = [f32::MAX; POSTERIZE_EDGES];
        // The band rises at encoded m / levels for m up to ⌈levels − 1⌉,
        // where `min(m, div) / div` reaches white.
        let steps = (div.ceil() as usize).min(POSTERIZE_EDGES);
        for (m, edge) in inside.iter_mut().enumerate().take(steps) {
            *edge = decode((m + 1) as f32 / levels);
        }
        let mut above = [f32::MAX; POSTERIZE_EDGES];
        let mut above_count = 0;
        for (j, edge) in above.iter_mut().enumerate() {
            let t = decode(1.0 + (j + 1) as f32 / levels);
            if !(t.is_finite() && t < f32::MAX) {
                break;
            }
            *edge = t;
            above_count = j as u32 + 1;
        }
        Self { levels, inside, above, above_count }
    }

    /// How many of `edges` (ascending) are at or below `v`, as the shader
    /// counts them (`count_at_or_below`).
    fn count(edges: &[f32; POSTERIZE_EDGES], v: f32) -> u32 {
        let (mut lo, mut hi) = (0usize, POSTERIZE_EDGES);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if edges[mid] <= v {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo as u32
    }

    /// The linear value `v` is posterized to.
    pub(crate) fn band(&self, v: f32) -> f32 {
        let div = self.levels - 1.0;
        let m = Self::count(&self.inside, v) as f32;
        let mut j = Self::count(&self.above, v);
        if j == self.above_count && self.above_count == POSTERIZE_EDGES as u32 {
            // Past the table: the step from the encoding, never below it.
            j = j.max(((encode(v) - 1.0) * self.levels).floor().max(0.0) as u32);
        }
        decode(m.min(div) / div + j as f32 / div)
    }
}

/// The motion shaders' bilinear sample: clamp-to-edge corners around
/// `(fx, fy)`, mixed as WGSL's `mix` mixes.
fn bilinear(f: &Frame, fx: f32, fy: f32) -> Px {
    let (x0f, y0f) = (fx.floor(), fy.floor());
    let (x, y) = (x0f as i32, y0f as i32);
    let (dx, dy) = (fx - x0f, fy - y0f);
    let lerp = |a: Px, b: Px, t: f32| [0, 1, 2, 3].map(|c| a[c] * (1.0 - t) + b[c] * t);
    let top = lerp(f.clamped(x, y), f.clamped(x + 1, y), dx);
    let bottom = lerp(f.clamped(x, y + 1), f.clamped(x + 1, y + 1), dx);
    lerp(top, bottom, dy)
}

/// Video stabilisation's warp, the CPU twin of `SHADER_STABILIZATION`:
/// each output pixel, scaled about the centre by `crop_zoom`, is taken
/// from where the frame's correction `h` (a row-major 3x3 homography,
/// padded to 12) carries it (#698).
pub(crate) fn stabilize(p: &[f32], w: u32, h: u32, m: &[f32; 12], crop_zoom: f32) -> Vec<f32> {
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    each_pixel(p, w, h, |px, py, f| {
        let x = (px as f32 - cx) / crop_zoom + cx;
        let y = (py as f32 - cy) / crop_zoom + cy;
        let w_div = m[6] * x + m[7] * y + m[8];
        let sx = (m[0] * x + m[1] * y + m[2]) / w_div;
        let sy = (m[3] * x + m[4] * y + m[5]) / w_div;
        bilinear(f, sx, sy)
    })
}

/// Rolling-shutter correction's warp, the CPU twin of
/// `SHADER_ROLLING_SHUTTER`: each row is taken through its own affine
/// transform, the strength already in them (#698). A row without one,
/// past the end of the list, keeps its pixels.
pub(crate) fn rolling_shutter(p: &[f32], w: u32, h: u32, rows: &[crate::rolling_shutter::RowTransform]) -> Vec<f32> {
    each_pixel(p, w, h, |x, y, f| match rows.get(y) {
        Some(t) => {
            let (fx, fy) = (x as f32, y as f32);
            bilinear(f, t.a * fx + t.b * fy + t.tx, t.c * fx + t.d * fy + t.ty)
        }
        None => f.at(x, y),
    })
}

/// HSV hue shift (degrees), saturation and value multipliers, as the
/// shader computes them.
fn hsv_adjust(s: [f32; 3], hue_shift: f32, saturation: f32, value: f32) -> [f32; 3] {
    let [r, g, b] = s;
    let cmax = r.max(g).max(b);
    let cmin = r.min(g).min(b);
    let delta = cmax - cmin;
    let mut h = 0.0f32;
    if delta > 0.001 {
        h = if (cmax - r).abs() < 0.001 {
            ((g - b) / delta) % 6.0
        } else if (cmax - g).abs() < 0.001 {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        h *= 60.0;
        if h < 0.0 {
            h += 360.0;
        }
    }
    let sat = if cmax > 0.001 { delta / cmax } else { 0.0 };
    let h = (h + hue_shift + 360.0) % 360.0;
    let ns = (sat * saturation).clamp(0.0, 1.0);
    let nv = cmax * value;
    let c = nv * ns;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = nv - c;
    let (rr, gg, bb) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    [rr + m, gg + m, bb + m]
}

/// The noise shader's hash.
fn noise_hash(p: u32) -> f32 {
    let mut x = p;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = (x >> 16) ^ x;
    x as f32 / 4294967295.0
}

/// Bins in the equalise CDF, and so the length of the GPU buffer that
/// carries it.
///
/// Named rather than written as 256 at each site: the hue/sat LUT's
/// size was once spelled out in a comment at a second site, and the
/// comment went stale the moment a seventh curve arrived. A constant
/// the function's own signature uses cannot drift from it.
pub const EQUALIZE_CDF_LEN: usize = 256;

/// Histogram equalisation's lookup: the cumulative distribution of the
/// display-encoded luma in [`EQUALIZE_CDF_LEN`] bins, normalised past
/// the first occupied bin. The luma is by the working space's weights
/// `yw`, as everywhere else (#1020). The GPU builds the same in its
/// histogram and CDF passes (#1215), held to this; its break path uses
/// this function.
pub fn equalize_cdf(linear: &[f32], yw: [f32; 3]) -> [f32; EQUALIZE_CDF_LEN] {
    equalize_cdf_of(&equalize_histogram(linear, yw), linear.len() / 4)
}

/// Equalize's histogram: each pixel's display luma, `yw · encode(rgb)`
/// clamped to 0–1, counted in bin `floor(l × 255)`.
pub fn equalize_histogram(linear: &[f32], yw: [f32; 3]) -> [u32; EQUALIZE_CDF_LEN] {
    let mut hist = [0u32; EQUALIZE_CDF_LEN];
    for c in linear.chunks_exact(4) {
        let l = (yw[0] * encode(c[0]) + yw[1] * encode(c[1]) + yw[2] * encode(c[2])).clamp(0.0, 1.0);
        hist[(l * 255.0) as usize] += 1;
    }
    hist
}

/// The CDF of `hist`, a histogram of `pixels` pixels, normalised past
/// its first occupied bin; the identity ramp for a frame in one bin.
pub fn equalize_cdf_of(hist: &[u32; EQUALIZE_CDF_LEN], pixels: usize) -> [f32; EQUALIZE_CDF_LEN] {
    let total = pixels as f32;
    let first = hist.iter().position(|&h| h > 0).unwrap_or(0);
    let min_cum: u32 = hist[..=first].iter().sum();
    let mut cdf = [0.0f32; 256];
    let mut cum = 0u32;
    for i in 0..256 {
        cum += hist[i];
        cdf[i] = if total > min_cum as f32 {
            ((cum.saturating_sub(min_cum)) as f32 / (total - min_cum as f32)).clamp(0.0, 1.0)
        } else {
            i as f32 / 255.0
        };
    }
    cdf
}

/// The one CPU LUT lookup, for the Lut3D node (through `klipp_color`)
/// and the global LUT alike,
/// as the GPU LUT pass does it: `pre` into the LUT's primaries, the
/// sRGB encoding clamped to 0–1 as the index, trilinear or tetrahedral,
/// the result decoded, `post` back to the working space, and the mix
/// with the original by `opacity` in linear light. `data` is RGB
/// triples, blue-major, `size`³ of them; a table too short for its
/// size is left alone rather than read past.
pub fn apply_3d_lut_bracketed(
    rgba_linear: &mut [f32],
    data: &[f32],
    size: usize,
    opacity: f32,
    pre: Option<[[f32; 3]; 3]>,
    post: Option<[[f32; 3]; 3]>,
    tetrahedral: bool,
) {
    let n = size;
    if opacity <= 0.0 || n < 2 || data.len() < n * n * n * 3 {
        return;
    }
    let n1 = (n - 1) as f32;

    rgba_linear.par_chunks_exact_mut(4).for_each(|pixel| {
        let original = [pixel[0], pixel[1], pixel[2]];

        let lut_in_linear = if let Some(m) = pre {
            mul3(m, original)
        } else {
            original
        };
        // Looked up on the sRGB encoding, as the GPU LUT shader does and
        // as a `.cube` look is built: it expects display-encoded input.
        // Indexing linear values put everything below mid-grey into the
        // first cells of a 13³ LUT, so the look read its curve in the
        // wrong place and banded (#414).
        let lut_in = lut_in_linear.map(|c| encode(c.clamp(0.0, 1.0)));

        let r_idx = (lut_in[0] * n1).clamp(0.0, n1);
        let g_idx = (lut_in[1] * n1).clamp(0.0, n1);
        let b_idx = (lut_in[2] * n1).clamp(0.0, n1);
        let r0 = (r_idx.floor() as usize).min(n - 2);
        let g0 = (g_idx.floor() as usize).min(n - 2);
        let b0 = (b_idx.floor() as usize).min(n - 2);
        let r1 = r0 + 1;
        let g1 = g0 + 1;
        let b1 = b0 + 1;
        let rf = r_idx - r0 as f32;
        let gf = g_idx - g0 as f32;
        let bf = b_idx - b0 as f32;
        let idx = |ri: usize, gi: usize, bi: usize| (bi * n * n + gi * n + ri) * 3;

        let mut lut_out = [0.0_f32; 3];
        for (ch, slot) in lut_out.iter_mut().enumerate() {
            let c000 = data[idx(r0, g0, b0) + ch];
            let c100 = data[idx(r1, g0, b0) + ch];
            let c010 = data[idx(r0, g1, b0) + ch];
            let c110 = data[idx(r1, g1, b0) + ch];
            let c001 = data[idx(r0, g0, b1) + ch];
            let c101 = data[idx(r1, g0, b1) + ch];
            let c011 = data[idx(r0, g1, b1) + ch];
            let c111 = data[idx(r1, g1, b1) + ch];
            *slot = if tetrahedral {
                // The unit cube split into six tetrahedra by the order of
                // the fractional coordinates: four surrounding corners
                // instead of all eight, which holds a LUT's straight
                // lines straight where trilinear bows them.
                if rf > gf {
                    if gf > bf {
                        c000 + (c100 - c000) * rf + (c110 - c100) * gf + (c111 - c110) * bf
                    } else if rf > bf {
                        c000 + (c100 - c000) * rf + (c101 - c100) * bf + (c111 - c101) * gf
                    } else {
                        c000 + (c001 - c000) * bf + (c101 - c001) * rf + (c111 - c101) * gf
                    }
                } else if rf > bf {
                    c000 + (c010 - c000) * gf + (c110 - c010) * rf + (c111 - c110) * bf
                } else if gf > bf {
                    c000 + (c010 - c000) * gf + (c011 - c010) * bf + (c111 - c011) * rf
                } else {
                    c000 + (c001 - c000) * bf + (c011 - c001) * gf + (c111 - c011) * rf
                }
            } else {
                let c00 = c000 + (c100 - c000) * rf;
                let c01 = c001 + (c101 - c001) * rf;
                let c10 = c010 + (c110 - c010) * rf;
                let c11 = c011 + (c111 - c011) * rf;
                let c0 = c00 + (c10 - c00) * gf;
                let c1 = c01 + (c11 - c01) * gf;
                c0 + (c1 - c0) * bf
            };
        }

        let lut_out = lut_out.map(|c| decode(c.clamp(0.0, 1.0)));
        let post_rgb = if let Some(m) = post {
            mul3(m, lut_out)
        } else {
            lut_out
        };

        pixel[0] = original[0] + (post_rgb[0] - original[0]) * opacity;
        pixel[1] = original[1] + (post_rgb[1] - original[1]) * opacity;
        pixel[2] = original[2] + (post_rgb[2] - original[2]) * opacity;
    });
}

fn mul3(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

/// A 3D LUT on the CPU, as `GpuEffectsPipeline::apply_3d_lut_gpu` applies
/// it: indexed by the display-encoded colour (clamped), the result
/// decoded and mixed with the original by `opacity` in linear light.
/// `data` is RGB triples, blue-major, `size`³ of them. The lookup is
/// [`apply_3d_lut_bracketed`], shared with the Lut3D node: two copies
/// of it drifted apart once (#414, #422).
pub fn apply_3d_lut(mut linear: Vec<f32>, data: &[f32], size: u32, opacity: f32, tetrahedral: bool) -> Vec<f32> {
    apply_3d_lut_bracketed(&mut linear, data, size as usize, opacity, None, None, tetrahedral);
    linear
}

/// How much of the blurred layer a focus blur takes at `u` along its
/// axis: none inside `band` around the middle, all of it at the frame's
/// edges, smoothly between. Mirrored by `SHADER_FOCUS_BLUR_BLEND`.
pub(crate) fn focus_blur_mix(u: f32, band: f32) -> f32 {
    let d = (u - 0.5).abs() * 2.0;
    let t = ((d - band) / (1.0 - band).max(0.001)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
