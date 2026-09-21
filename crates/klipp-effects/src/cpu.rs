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

fn mix(a: Px, b: Px, t: f32) -> Px {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// A read-only frame.
struct Frame<'a> {
    p: &'a [f32],
    w: usize,
    h: usize,
}

impl Frame<'_> {
    fn at(&self, x: usize, y: usize) -> Px {
        let i = (y * self.w + x) * 4;
        [self.p[i], self.p[i + 1], self.p[i + 2], self.p[i + 3]]
    }

    /// Clamp-to-edge sample.
    fn clamped(&self, x: i32, y: i32) -> Px {
        self.at(x.clamp(0, self.w as i32 - 1) as usize, y.clamp(0, self.h as i32 - 1) as usize)
    }
}

/// A new frame, each pixel computed from its coordinates and the input.
fn each_pixel(input: &[f32], w: u32, h: u32, f: impl Fn(usize, usize, &Frame) -> Px + Sync) -> Vec<f32> {
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
fn gaussian(p: &[f32], w: u32, h: u32, sigma: f32, skip_zero: bool) -> Vec<f32> {
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

/// `screen_at_ceiling` in the shaders' shared helpers: the screen blend
/// at the picture's own ceiling, so light lands on a highlight instead of
/// the blend coming back negative.
pub(crate) fn screen_at_ceiling(o: f32, v: f32) -> f32 {
    let w = o.max(v).max(1.0);
    o + v - o * v / w
}

/// Apply one built-in effect. Frei0r plugins are not here (they take
/// 8-bit frames: see `apply_effects_with_time`); stabilisation, rolling-
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
                    let y1 = sy * cos_rx;
                    let z1 = sy * sin_rx;
                    let x2 = sx * cos_ry + z1 * sin_ry;
                    let z2 = -sx * sin_ry + z1 * cos_ry;
                    if focal > 0.0 {
                        let wf = 1.0 + z2 / focal;
                        if wf > 0.01 {
                            sx = x2 / wf;
                            sy = y1 / wf;
                        }
                    } else {
                        sx = x2;
                        sy = y1;
                    }
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
                if method != 0 && spill_strength > 0.0 && px[3] > 0.0 {
                    let mut srgb = s;
                    let key = srgb[key_idx];
                    let (a, b) = (srgb[(key_idx + 1) % 3], srgb[(key_idx + 2) % 3]);
                    let target = if method == 1 { a.max(b) } else { 0.5 * (a + b) };
                    if key > target {
                        srgb[key_idx] = (key + spill_strength * (target - key)).clamp(0.0, 255.0);
                        for c in 0..3 {
                            px[c] = decode(srgb[c] / 255.0);
                        }
                    }
                }
                px
            })
        }
        Bloom { threshold, radius, intensity } => {
            let blurred = gaussian(&bright(&p, threshold, yw), w, h, radius, false);
            let mut out = p;
            out.par_chunks_exact_mut(4).zip(blurred.par_chunks_exact(4)).for_each(|(o, b)| {
                (0..3).for_each(|c| o[c] += b[c] * intensity);
            });
            out
        }
        Glow { radius, intensity } => screen(&p, &gaussian(&p, w, h, radius, false), intensity, 0.0, yw),
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
            // Bands even on the display scale; white stays white, and
            // above it the ladder carries on at the same spacing rather
            // than every highlight landing on white.
            let div = levels - 1.0;
            each_encoded(p, |s| {
                s.map(|v| {
                    let banded = (v.clamp(0.0, 1.0) * levels).floor().min(div) / div;
                    banded + ((v - 1.0).max(0.0) * levels).floor() / div
                })
            })
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
                let mut s = [0.0f32; 4];
                for i in 0..n {
                    let q = f.clamped(round(x as f32 + dx * i as f32), round(y as f32 + dy * i as f32));
                    (0..4).for_each(|c| s[c] += q[c]);
                }
                s.map(|v| v / n as f32)
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
                    let (sx, sy) = if mode == 0 {
                        let scale = amount / 100.0;
                        (round(xf + dx * t * scale), round(yf + dy * t * scale))
                    } else {
                        let ang = t * (amount * std::f32::consts::PI / 180.0 * 0.5);
                        let (sin_a, cos_a) = ang.sin_cos();
                        (round(cx + dx * cos_a - dy * sin_a), round(cy + dx * sin_a + dy * cos_a))
                    };
                    let q = f.clamped(sx, sy);
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
            let cdf = equalize_cdf(&p);
            // The histogram covers what the display shows; light above
            // white passes through, so equalising does not crush the
            // highlights.
            each_encoded(p, |s| {
                let d = s.map(|v| v.min(1.0));
                let l = 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2];
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
            let cell = (size * hf / 1080.0).max(1.0);
            each_pixel(&p, w, h, |x, y, f| {
                let o = f.at(x, y);
                let gx = (x as f32 / cell) as u32;
                let gy = (y as f32 / cell) as u32;
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
        OldFilm { flicker, scratches, dust, weave, sepia, seed } => {
            let fseed = seed.wrapping_mul(2654435761);
            let flick = 1.0 + (noise_hash(fseed) - 0.5) * flicker * 0.4;
            let wx = (noise_hash(fseed.wrapping_add(11)) - 0.5) * weave * 6.0 * wf / 1920.0;
            let wy = (noise_hash(fseed.wrapping_add(23)) - 0.5) * weave * 6.0 * hf / 1080.0;
            let n_scr = (scratches * 4.0).round() as u32;
            let life = seed / 6;
            each_pixel(&p, w, h, |x, y, f| {
                let o = f.clamped(round(x as f32 + wx), round(y as f32 + wy));
                let l = luma(o, yw);
                let mut r = o[0] + (l * 1.07 - o[0]) * sepia;
                let mut g = o[1] + (l * 0.93 - o[1]) * sepia;
                let mut b = o[2] + (l * 0.72 - o[2]) * sepia;
                r *= flick;
                g *= flick;
                b *= flick;
                let mut k = 0u32;
                while k < n_scr {
                    let sx = noise_hash(life.wrapping_add(k.wrapping_mul(97)).wrapping_mul(7919)) * wf;
                    let jitter = (noise_hash(seed.wrapping_add(k.wrapping_mul(31))) - 0.5) * 2.0;
                    if (x as f32 - (sx + jitter)).abs() < 0.8 {
                        r += (1.0 - r) * 0.35;
                        g += (1.0 - g) * 0.35;
                        b += (1.0 - b) * 0.35;
                    }
                    k += 1;
                }
                let cx = x as u32 / 5;
                let cy = y as u32 / 5;
                let dh = noise_hash(cx.wrapping_mul(7919).wrapping_add(cy.wrapping_mul(104729)).wrapping_add(seed.wrapping_mul(15485863)));
                if dh > 1.0 - dust * 0.004 {
                    r *= 0.15;
                    g *= 0.15;
                    b *= 0.15;
                }
                let nx = (x as f32 - wf * 0.5) / (wf * 0.5);
                let ny = (y as f32 - hf * 0.5) / (hf * 0.5);
                let vig = 1.0 - 0.25 * (nx * nx + ny * ny);
                [r * vig, g * vig, b * vig, o[3]]
            })
        }
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
                let shifted = |shift: f32, c: usize| {
                    let k = 1.0 + shift * norm / max_dist;
                    f.clamped(round(cx + dx * k), round(cy + dy * k))[c]
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
    }
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
/// the first occupied bin. The GPU path builds it with this function
/// too, then uploads it to its own storage buffer.
pub fn equalize_cdf(linear: &[f32]) -> [f32; EQUALIZE_CDF_LEN] {
    let mut hist = [0u32; EQUALIZE_CDF_LEN];
    for c in linear.chunks_exact(4) {
        let l = (0.2126 * encode(c[0]) + 0.7152 * encode(c[1]) + 0.0722 * encode(c[2])).clamp(0.0, 1.0);
        hist[(l * 255.0) as usize] += 1;
    }
    let total = (linear.len() / 4) as f32;
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

/// A 3D LUT on the CPU, as `GpuEffectsPipeline::apply_3d_lut_gpu` applies
/// it: indexed by the display-encoded colour (clamped), trilinear, the
/// result decoded and mixed with the original by `opacity` in linear
/// light. `data` is RGB triples, blue-major, `size`³ of them.
pub fn apply_3d_lut(linear: Vec<f32>, data: &[f32], size: u32, opacity: f32, tetrahedral: bool) -> Vec<f32> {
    if opacity <= 0.0 || size < 2 || data.len() < (size * size * size * 3) as usize {
        return linear;
    }
    let n = size as usize;
    let n1 = (n - 1) as f32;
    let at = |r: usize, g: usize, b: usize| {
        let i = (b * n * n + g * n + r) * 3;
        [data[i], data[i + 1], data[i + 2]]
    };
    let lerp3 = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    each_value(linear, |c| {
        let idx = [0, 1, 2].map(|ch| (encode(c[ch]).clamp(0.0, 1.0) * n1).clamp(0.0, n1));
        let i0 = idx.map(|v| (v as usize).min(n - 2));
        let f = [idx[0] - i0[0] as f32, idx[1] - i0[1] as f32, idx[2] - i0[2] as f32];
        let (r0, g0, b0) = (i0[0], i0[1], i0[2]);
        let c00 = lerp3(at(r0, g0, b0), at(r0 + 1, g0, b0), f[0]);
        let c01 = lerp3(at(r0, g0, b0 + 1), at(r0 + 1, g0, b0 + 1), f[0]);
        let c10 = lerp3(at(r0, g0 + 1, b0), at(r0 + 1, g0 + 1, b0), f[0]);
        let c11 = lerp3(at(r0, g0 + 1, b0 + 1), at(r0 + 1, g0 + 1, b0 + 1), f[0]);
        let interpolated = if tetrahedral {
            // The unit cube split into six tetrahedra by the order of the
            // fractional coordinates: the sample is interpolated over the
            // four corners that surround it rather than all eight, which
            // holds a LUT's straight lines straight.
            let (rf, gf, bf) = (f[0], f[1], f[2]);
            let corner = |r: usize, g: usize, b: usize| at(r0 + r, g0 + g, b0 + b);
            let (c000, c111) = (corner(0, 0, 0), corner(1, 1, 1));
            let mix4 = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], x: f32, y: f32, z: f32| {
                [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * x + (c[i] - b[i]) * y + (d[i] - c[i]) * z)
            };
            if rf > gf {
                if gf > bf {
                    mix4(c000, corner(1, 0, 0), corner(1, 1, 0), c111, rf, gf, bf)
                } else if rf > bf {
                    mix4(c000, corner(1, 0, 0), corner(1, 0, 1), c111, rf, bf, gf)
                } else {
                    mix4(c000, corner(0, 0, 1), corner(1, 0, 1), c111, bf, rf, gf)
                }
            } else if rf > bf {
                mix4(c000, corner(0, 1, 0), corner(1, 1, 0), c111, gf, rf, bf)
            } else if gf > bf {
                mix4(c000, corner(0, 1, 0), corner(0, 1, 1), c111, gf, bf, rf)
            } else {
                mix4(c000, corner(0, 0, 1), corner(0, 1, 1), c111, bf, gf, rf)
            }
        } else {
            lerp3(lerp3(c00, c10, f[1]), lerp3(c01, c11, f[1]), f[2])
        };
        let graded = interpolated.map(|v| decode(v.clamp(0.0, 1.0)));
        [
            c[0] + (graded[0] - c[0]) * opacity,
            c[1] + (graded[1] - c[1]) * opacity,
            c[2] + (graded[2] - c[2]) * opacity,
            c[3],
        ]
    })
}
