//! The grade shaders on the CPU, transcribed: the unified colour grade,
//! clarity, vibrance, wheel sat/bri, RGB curves, hue/sat curves, the
//! colour warper, colour balance and the grade mix. The GPU grades
//! whenever it can and defines what each control does; these run when
//! there is no usable GPU and must give the same picture, which
//! `klipp-compose/tests/grade_cpu_gpu_parity.rs` checks. Change a shader,
//! change its twin here.
//!
//! Each function takes what its GPU entry point takes and works on
//! linear RGBA f32 in place.

use klipp_grade_model::{cdl_channel, contrast_channel_at, grade_decode, grade_encode, log_wheel_weights, tone_weights};
use rayon::prelude::*;
// The WGSL twins spell this `3.14159265`, and that literal rounds to
// exactly this f32 — 0x40490fdb, the same bits — so the twins agree
// despite reading differently. WGSL has no PI constant of its own,
// which is why only this side changed.
use std::f32::consts::PI;

/// The unified grade's controls, as `GpuEffectsPipeline::apply_color_grade`
/// takes them.
#[derive(Clone, Copy, Debug)]
pub struct ColorGrade {
    pub exposure: f32,
    pub contrast: f32,
    /// Where contrast pivots, already through
    /// [`klipp_grade_model::encode_contrast_pivot`].
    ///
    /// Encoded rather than linear so neither this path nor the shader
    /// pays a `powf` per sample, and so both are handed the identical
    /// number rather than each deriving it — the parity tests compare
    /// pixels, and a pivot computed twice is a way for them to drift.
    pub contrast_pivot_encoded: f32,
    pub saturation: f32,
    pub white_balance: [[f32; 3]; 3],
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub lift: (f32, f32, f32),
    pub gamma: (f32, f32, f32),
    pub gain: (f32, f32, f32),
    pub offset: (f32, f32, f32),
    /// Log-mode wheels (low, mid, high), replacing lift/gamma/gain/offset.
    pub log_wheels: Option<[(f32, f32, f32); 3]>,
    pub hue: f32,
    pub global_mix: f32,
    /// The working space's luma coefficients.
    pub luma: [f32; 3],
}

/// `SHADER_COLOR_GRADE`.
pub fn color_grade(px: &mut [f32], g: &ColorGrade) {
    let l = g.luma;
    let wb = g.white_balance;
    px.par_chunks_exact_mut(4).for_each(|p| {
        let orig = [p[0], p[1], p[2]];
        let (mut r, mut gr, mut b) = (orig[0], orig[1], orig[2]);

        // 1. Exposure.
        // 1. Exposure and white balance: linear light, where a stop is a
        //    doubling and a balance is a matrix.
        let exp_factor = 2f32.powf(g.exposure);
        r *= exp_factor;
        gr *= exp_factor;
        b *= exp_factor;
        let (wr, wg, wbl) = (
            wb[0][0] * r + wb[0][1] * gr + wb[0][2] * b,
            wb[1][0] * r + wb[1][1] * gr + wb[1][2] * b,
            wb[2][0] * r + wb[2][1] * gr + wb[2][2] * b,
        );
        (r, gr, b) = (wr, wg, wbl);
        // 2. In the grade encoding (klipp_color::grade_model): the tone
        //    bands, the wheels as the ASC CDL, and contrast about mid grey.
        let luma1 = l[0] * r + l[1] * gr + l[2] * b;
        let l_enc = grade_encode(luma1);
        let [hi_w, lo_w, white_w, black_w] = tone_weights(l_enc);
        let tone_adj = g.highlights * hi_w + g.shadows * lo_w + g.whites * white_w + g.blacks * black_w;
        let mut e = [grade_encode(r) + tone_adj, grade_encode(gr) + tone_adj, grade_encode(b) + tone_adj];
        match g.log_wheels {
            Some([low, mid, high]) => {
                let [low_w, mid_w, high_w] = log_wheel_weights(l_enc);
                let lo = [low.0, low.1, low.2];
                let mi = [mid.0, mid.1, mid.2];
                let hi = [high.0, high.1, high.2];
                for i in 0..3 {
                    e[i] += lo[i] * low_w + mi[i] * mid_w + hi[i] * high_w;
                }
            }
            None => {
                let lift = [g.lift.0, g.lift.1, g.lift.2];
                let gamma = [g.gamma.0, g.gamma.1, g.gamma.2];
                let gain = [g.gain.0, g.gain.1, g.gain.2];
                let offset = [g.offset.0, g.offset.1, g.offset.2];
                for i in 0..3 {
                    e[i] = cdl_channel(e[i], lift[i], gamma[i], gain[i], offset[i]);
                }
            }
        }
        for v in &mut e {
            *v = contrast_channel_at(*v, g.contrast, g.contrast_pivot_encoded);
        }
        (r, gr, b) = (grade_decode(e[0]), grade_decode(e[1]), grade_decode(e[2]));
        // 3. Back in linear light: saturation about the working space's
        //    luma, then the hue rotation.
        let luma3 = l[0] * r + l[1] * gr + l[2] * b;
        r = luma3 + (r - luma3) * g.saturation;
        gr = luma3 + (gr - luma3) * g.saturation;
        b = luma3 + (b - luma3) * g.saturation;
        if g.hue.abs() >= 0.01 {
            [r, gr, b] = hue_rotate_hsl([r, gr, b], g.hue);
        }

        // 8. Mix.
        p[0] = orig[0] + (r - orig[0]) * g.global_mix;
        p[1] = orig[1] + (gr - orig[1]) * g.global_mix;
        p[2] = orig[2] + (b - orig[2]) * g.global_mix;
    });
}

/// `hue_rotate_hsl` in the shaders' shared helpers.
pub fn hue_rotate_hsl(c: [f32; 3], degrees: f32) -> [f32; 3] {
    let magnitude = c[0].max(c[1]).max(c[2]).max(1.0);
    let (r, g, b) = (c[0] / magnitude, c[1] / magnitude, c[2] / magnitude);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    if delta < 0.0001 {
        return c;
    }
    let s = delta / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if mx == r {
        let mut h = ((g - b) / delta) % 6.0;
        if h < 0.0 {
            h += 6.0;
        }
        h / 6.0
    } else if mx == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    h = (h + degrees / 360.0) % 1.0;
    if h < 0.0 {
        h += 1.0;
    }
    let cc = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = cc * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - cc * 0.5;
    let o = match (h * 6.0) as u32 {
        0 => [cc, x, 0.0],
        1 => [x, cc, 0.0],
        2 => [0.0, cc, x],
        3 => [0.0, x, cc],
        4 => [x, 0.0, cc],
        _ => [cc, 0.0, x],
    };
    [(o[0] + m) * magnitude, (o[1] + m) * magnitude, (o[2] + m) * magnitude]
}

fn luma(l: [f32; 3], p: &[f32]) -> f32 {
    l[0] * p[0] + l[1] * p[1] + l[2] * p[2]
}

/// `SHADER_CLARITY_BLUR_H`, `_BLUR_V` and `_APPLY`: a 41-pixel box blur of
/// working-space luma, then midtone detail added back. The GPU's radius
/// is 20.
pub fn clarity(px: &mut [f32], width: u32, height: u32, amount: f32, l: [f32; 3]) {
    let (w, h) = (width as usize, height as usize);
    let r: i32 = 20;
    let lumas: Vec<f32> = px.chunks_exact(4).map(|p| luma(l, p)).collect();
    let mut horiz = vec![0.0f32; w * h];
    horiz.par_chunks_exact_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            let mut count = 0u32;
            for dx in -r..=r {
                let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                sum += lumas[y * w + sx];
                count += 1;
            }
            *out = sum / count as f32;
        }
    });
    let mut blurred = vec![0.0f32; w * h];
    blurred.par_chunks_exact_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            let mut count = 0u32;
            for dy in -r..=r {
                let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                sum += horiz[sy * w + x];
                count += 1;
            }
            *out = sum / count as f32;
        }
    });
    px.par_chunks_exact_mut(4).zip(blurred.par_iter()).for_each(|(p, &bl)| {
        let y = luma(l, p);
        let detail = y - bl;
        let midtone_weight = (1.0 - ((y / 0.18) - 1.0).abs()).max(0.0);
        let adj = detail * amount * midtone_weight;
        p[0] += adj;
        p[1] += adj;
        p[2] += adj;
    });
}

/// `SHADER_VIBRANCE`.
pub fn vibrance(px: &mut [f32], amount: f32, l: [f32; 3]) {
    px.par_chunks_exact_mut(4).for_each(|p| {
        let mx = p[0].max(p[1]).max(p[2]);
        let mn = p[0].min(p[1]).min(p[2]);
        let sat = mx - mn;
        let y = luma(l, p);
        let weight = 1.0 - sat.clamp(0.0, 1.0);
        let boost = 1.0 + amount * weight;
        for c in &mut p[..3] {
            *c = y + (*c - y) * boost;
        }
    });
}

/// `SHADER_WHEEL_SAT_BRI`.
#[allow(clippy::too_many_arguments)]
pub fn wheel_sat_bri(
    px: &mut [f32],
    lift_sat: f32, lift_bri: f32,
    gamma_sat: f32, gamma_bri: f32,
    gain_sat: f32, gain_bri: f32,
    offset_sat: f32, offset_bri: f32,
    l: [f32; 3],
) {
    px.par_chunks_exact_mut(4).for_each(|p| {
        let y = luma(l, p);
        let lift_w = (1.0 - y).max(0.0);
        let gamma_w = (y.clamp(0.0, 1.0) * PI).sin().max(0.0);
        let gain_w = y;
        let bri_adj = lift_bri * lift_w + gamma_bri * gamma_w + gain_bri * gain_w + offset_bri;
        for c in &mut p[..3] {
            *c += bri_adj;
        }
        let sat_adj = lift_sat * lift_w + gamma_sat * gamma_w + gain_sat * gain_w + offset_sat;
        let sat_factor = 1.0 + sat_adj;
        let luma2 = luma(l, p);
        for c in &mut p[..3] {
            *c = luma2 + (*c - luma2) * sat_factor;
        }
    });
}

/// `lut_lookup` in `SHADER_RGB_CURVES`: inside the [0, 1] domain.
fn curve_lookup(lut: &[f32], channel: usize, val: f32) -> f32 {
    let base = channel * 256;
    let idx_f = val.clamp(0.0, 1.0) * 255.0;
    let idx0 = idx_f.clamp(0.0, 254.0) as usize;
    let frac = idx_f - idx0 as f32;
    lut[base + idx0] * (1.0 - frac) + lut[base + idx0 + 1] * frac
}

/// `curve` in `SHADER_RGB_CURVES`: straight past either end, at the slope
/// the curve ends with.
fn curve(lut: &[f32], channel: usize, val: f32) -> f32 {
    let base = channel * 256;
    if val > 1.0 {
        let slope = (lut[base + 255] - lut[base + 254]) * 255.0;
        return lut[base + 255] + (val - 1.0) * slope;
    }
    if val < 0.0 {
        let slope = (lut[base + 1] - lut[base]) * 255.0;
        return lut[base] + val * slope;
    }
    curve_lookup(lut, channel, val)
}

/// `enc_signed` / `dec_signed` in `SHADER_RGB_CURVES`.
fn enc_signed(c: f32) -> f32 {
    let e = crate::linear_to_srgb_scalar(c.abs());
    if c < 0.0 { -e } else { e }
}

fn dec_signed(c: f32) -> f32 {
    let d = crate::srgb_to_linear_scalar(c.abs());
    if c < 0.0 { -d } else { d }
}

/// `SHADER_RGB_CURVES`. `lut` is master, red, green, blue, 256 each; the
/// curves act on the display-encoded picture, preserve-luma and the mix
/// on linear light.
pub fn rgb_curves(px: &mut [f32], lut: &[f32; 1024], preserve_luma: bool, mix: f32, l: [f32; 3]) {
    px.par_chunks_exact_mut(4).for_each(|p| {
        let orig = [p[0], p[1], p[2]];
        let mut r = curve(lut, 1, enc_signed(orig[0]));
        let mut g = curve(lut, 2, enc_signed(orig[1]));
        let mut b = curve(lut, 3, enc_signed(orig[2]));
        r = dec_signed(curve(lut, 0, r));
        g = dec_signed(curve(lut, 0, g));
        b = dec_signed(curve(lut, 0, b));
        if preserve_luma {
            let orig_luma = luma(l, &orig);
            let new_luma = l[0] * r + l[1] * g + l[2] * b;
            if new_luma > 0.0001 {
                let scale = orig_luma / new_luma;
                r *= scale;
                g *= scale;
                b *= scale;
            }
        }
        p[0] = orig[0] + (r - orig[0]) * mix;
        p[1] = orig[1] + (g - orig[1]) * mix;
        p[2] = orig[2] + (b - orig[2]) * mix;
    });
}

/// `PictureHsl` in the shaders' shared helpers: the hue/sat tools' HSL,
/// on the display-encoded picture and scaled into range when the pixel is
/// brighter than white.
#[derive(Clone, Copy)]
pub struct PictureHsl {
    pub h: f32,
    /// Saturation as a fraction of the brightest channel,
    /// `(max - min) / max` — see the shader's `picture_hsl` for why it
    /// is not the HSL definition.
    pub s: f32,
    /// Lightness, for `lum_axis`. Not what a saturation change holds
    /// fixed.
    pub l: f32,
    /// The brightest channel, normalised — what a saturation change
    /// holds fixed.
    pub v: f32,
    pub magnitude: f32,
}

impl PictureHsl {
    /// The luminance axis the lum-driven lookups use: everything above
    /// white sits at the top of it.
    pub fn lum_axis(&self) -> f32 {
        (self.l * self.magnitude).clamp(0.0, 1.0)
    }
}

/// `picture_hsl`.
pub fn picture_hsl(c: [f32; 3]) -> PictureHsl {
    let e = c.map(enc_signed);
    let magnitude = e[0].max(e[1]).max(e[2]).max(1.0);
    let n = e.map(|v| v / magnitude);
    let mx = n[0].max(n[1]).max(n[2]);
    let mn = n[0].min(n[1]).min(n[2]);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    let (mut s, mut h) = (0.0, 0.0);
    if delta > 0.0001 {
        s = (delta / mx.max(0.0001)).clamp(0.0, 1.0);
        h = if mx == n[0] {
            let mut h = ((n[1] - n[2]) / delta) % 6.0;
            if h < 0.0 {
                h += 6.0;
            }
            h / 6.0
        } else if mx == n[1] {
            ((n[2] - n[0]) / delta + 2.0) / 6.0
        } else {
            ((n[0] - n[1]) / delta + 4.0) / 6.0
        };
    }
    PictureHsl { h, s, l, v: mx, magnitude }
}

/// `picture_rgb`: hue, saturation and value back to linear light.
pub fn picture_rgb(h: f32, s: f32, v: f32, magnitude: f32) -> [f32; 3] {
    hsv_to_rgb(h, s, v).map(|c| dec_signed(c * magnitude))
}

/// `hsv_rgb_norm`.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s < 0.0001 {
        return [v, v, v];
    }
    let c = s * v;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let o = match (h * 6.0) as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    [o[0] + m, o[1] + m, o[2] + m]
}

/// `lut_lookup` in `SHADER_HUESAT`: the index clamped, the input not.
fn huesat_lookup(lut: &[f32], curve: usize, t: f32) -> f32 {
    let base = curve * 256;
    let idx_f = t * 255.0;
    let idx0 = idx_f.clamp(0.0, 254.0) as usize;
    let frac = idx_f - idx0 as f32;
    lut[base + idx0] * (1.0 - frac) + lut[base + idx0 + 1] * frac
}

/// `SHADER_HUESAT`. `lut` is the six curves, 256 each, as
/// `build_huesat_lut_data` lays them out; bit `c` of `active` marks
/// curve `c` as edited.
pub fn huesat(px: &mut [f32], lut: &[f32; HUESAT_LUT_LEN], mix: f32, active: u32) {
    let on = |c: u32| active & (1 << c) != 0;
    px.par_chunks_exact_mut(4).for_each(|p| {
        let orig = [p[0], p[1], p[2]];
        let hsl = picture_hsl(orig);
        let lum_axis = hsl.lum_axis();
        // See the shader: the curves move the value, not the lightness,
        // because saturation is a fraction of the brightest channel.
        let (mut h, mut s, mut v) = (hsl.h, hsl.s, hsl.v);
        if on(0) {
            let hvh = huesat_lookup(lut, 0, h);
            h = (h + (hvh - 0.5) * 2.0) % 1.0;
            if h < 0.0 {
                h += 1.0;
            }
        }
        if on(1) {
            let hvs = huesat_lookup(lut, 1, hsl.h);
            s = (s * hvs * 2.0).clamp(0.0, 1.0);
        }
        if on(2) {
            let hvl = huesat_lookup(lut, 2, hsl.h);
            v = (v + (hvl - 0.5)).clamp(0.0, 1.0);
        }
        if on(3) {
            let lvs = huesat_lookup(lut, 3, lum_axis);
            s = (s * lvs * 2.0).clamp(0.0, 1.0);
        }
        if on(4) {
            let svs = huesat_lookup(lut, 4, hsl.s);
            s = svs.clamp(0.0, 1.0);
        }
        if on(5) {
            let ovs = huesat_lookup(lut, 5, hsl.s);
            let orange_dist = (hsl.h - 0.083).abs().min((hsl.h - 0.083 + 1.0).abs().min((hsl.h - 0.083 - 1.0).abs()));
            let weight = (1.0 - orange_dist * 8.0).clamp(0.0, 1.0);
            s += (ovs - s) * weight;
        }
        if on(6) {
            // Brightness as a function of saturation, read off the
            // *incoming* saturation like every other curve indexed by
            // s, so a saturation curve earlier in the list does not
            // silently move this one's input.
            let svl = huesat_lookup(lut, 6, hsl.s);
            v = (v + (svl - 0.5)).clamp(0.0, 1.0);
        }
        let rgb = picture_rgb(h, s, v, hsl.magnitude);
        for c in 0..3 {
            p[c] = orig[c] + (rgb[c] - orig[c]) * mix;
        }
    });
}

/// How many hue/sat curves there are, and how long their LUT is.
///
/// One number for the buffer, the uploads, both twins and the
/// executor's stage.
///
/// The shader binds the LUT as a runtime-sized `array<f32>`, so a
/// buffer left a curve short does not fail to compile. It fails at the
/// upload: `queue.write_buffer` validates the write, and an oversized
/// one reaches the device's uncaptured-error handler — a logged
/// validation error and a LUT that never arrives, rather than a silent
/// out-of-bounds read. Loud, but nowhere near the change that caused
/// it. The buffer was a curve short once while the seventh was being
/// added; sizing everything from here is what stops a repeat.
pub const HUESAT_CURVES: usize = 7;
pub const HUESAT_LUT_LEN: usize = HUESAT_CURVES * 256;

/// `SHADER_COLOR_WARPER`: twelve 30° sectors, cosine-blended.
pub fn color_warper(px: &mut [f32], hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) {
    px.par_chunks_exact_mut(4).for_each(|p| {
        let hsl = picture_hsl([p[0], p[1], p[2]]);
        let h_deg = hsl.h * 360.0;
        let sector_f = h_deg / 30.0;
        let sector_low = (sector_f.floor() as u32 % 12) as usize;
        let sector_high = (sector_low + 1) % 12;
        let t = sector_f - sector_f.floor();
        let blend = (1.0 - (t * PI).cos()) * 0.5;
        let hue_shift = hue_shifts[sector_low] * (1.0 - blend) + hue_shifts[sector_high] * blend;
        let sat_shift = sat_shifts[sector_low] * (1.0 - blend) + sat_shifts[sector_high] * blend;
        let sat_weight = hsl.s.clamp(0.0, 1.0);
        let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
        let new_s = (hsl.s + sat_shift * sat_weight).clamp(0.0, 1.0);
        let rgb = picture_rgb(new_h, new_s, hsl.v, hsl.magnitude);
        p[..3].copy_from_slice(&rgb);
    });
}

/// `SHADER_COLOR_BALANCE`: white balance, hue, mix.
pub fn color_balance(px: &mut [f32], white_balance: [[f32; 3]; 3], hue: f32, global_mix: f32) {
    let wb = white_balance;
    px.par_chunks_exact_mut(4).for_each(|p| {
        let orig = [p[0], p[1], p[2]];
        let mut c = [
            wb[0][0] * orig[0] + wb[0][1] * orig[1] + wb[0][2] * orig[2],
            wb[1][0] * orig[0] + wb[1][1] * orig[1] + wb[1][2] * orig[2],
            wb[2][0] * orig[0] + wb[2][1] * orig[1] + wb[2][2] * orig[2],
        ];
        if hue.abs() >= 0.01 {
            c = hue_rotate_hsl(c, hue);
        }
        for i in 0..3 {
            p[i] = orig[i] + (c[i] - orig[i]) * global_mix;
        }
    });
}

/// The qualifier's HSL window, as `SHADER_QUALIFIER` computes it: a
/// greyscale matte (R=G=B=matte, alpha kept). Hue values are turns, not
/// degrees, as the shader takes them.
#[allow(clippy::too_many_arguments)]
pub fn qualifier_matte(
    px: &mut [f32],
    hue_center: f32, hue_width: f32, hue_soft: f32,
    sat_low: f32, sat_high: f32, sat_soft: f32,
    lum_low: f32, lum_high: f32, lum_soft: f32,
    invert: bool,
) {
    let circular_distance = |a: f32, b: f32| {
        let d = (a - b).abs();
        d.min(1.0 - d)
    };
    let soft_range_centered = |dist: f32, width: f32, soft: f32| {
        let half_w = width * 0.5;
        if dist <= half_w {
            return 1.0;
        }
        if soft <= 0.0001 {
            return 0.0;
        }
        (1.0 - (dist - half_w) / soft).clamp(0.0, 1.0)
    };
    let soft_range = |val: f32, lo: f32, hi: f32, soft: f32| {
        if val >= lo && val <= hi {
            return 1.0;
        }
        if soft <= 0.0001 {
            return 0.0;
        }
        if val < lo {
            (1.0 - (lo - val) / soft).clamp(0.0, 1.0)
        } else {
            (1.0 - (val - hi) / soft).clamp(0.0, 1.0)
        }
    };
    px.par_chunks_exact_mut(4).for_each(|p| {
        let hsl = picture_hsl([p[0], p[1], p[2]]);
        let hue_m = soft_range_centered(circular_distance(hsl.h, hue_center), hue_width, hue_soft);
        let sat_m = soft_range(hsl.s, sat_low, sat_high, sat_soft);
        let lum_m = soft_range(hsl.lum_axis(), lum_low, lum_high, lum_soft);
        let mut matte = hue_m * sat_m * lum_m;
        if invert {
            matte = 1.0 - matte;
        }
        p[0] = matte;
        p[1] = matte;
        p[2] = matte;
    });
}

/// Matte finesse on a qualifier's matte (the same value in R, G and B):
/// clean black and clean white clip and rescale the matte, shrink/grow
/// erodes or dilates it by a square of `|shrink_grow|` pixels, and blur
/// is two box passes of `blur` pixels — a near-Gaussian softening. This
/// runs on the CPU for both paths: the qualifier's matte comes back to
/// the CPU either way, so both are the same code.
///
/// A key without these is unusable in practice: every real key needs at
/// least its noise cleaned and its edge softened.
pub fn matte_finesse(px: &mut [f32], width: u32, height: u32, clean_black: f32, clean_white: f32, shrink_grow: f32, blur: f32) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || px.len() < w * h * 4 {
        return;
    }
    let lo = clean_black.clamp(0.0, 0.99);
    let hi = (1.0 - clean_white).clamp(lo + 0.01, 1.0);
    let mut m: Vec<f32> = px.chunks_exact(4).take(w * h).map(|p| ((p[0] - lo) / (hi - lo)).clamp(0.0, 1.0)).collect();
    let r = shrink_grow.abs().round() as usize;
    if r > 0 {
        let grow = shrink_grow > 0.0;
        let op = move |acc: f32, v: f32| if grow { acc.max(v) } else { acc.min(v) };
        let seed = if grow { 0.0 } else { 1.0 };
        m = separable(&m, w, h, r, seed, op, |acc, _| acc);
    }
    let br = blur.round() as usize;
    if br > 0 {
        for _ in 0..2 {
            m = separable(&m, w, h, br, 0.0, |acc, v| acc + v, |acc, n| acc / n as f32);
        }
    }
    px.par_chunks_exact_mut(4).zip(m.par_iter()).for_each(|(p, v)| {
        p[0] = *v;
        p[1] = *v;
        p[2] = *v;
    });
}

/// A separable window filter: `fold` accumulates the `2r + 1` samples
/// around each pixel (edge-clamped), `finish` turns the accumulator and
/// count into the output; rows first, then columns.
fn separable(m: &[f32], w: usize, h: usize, r: usize, seed: f32, fold: impl Fn(f32, f32) -> f32 + Sync, finish: impl Fn(f32, usize) -> f32 + Sync) -> Vec<f32> {
    let pass = |src: &[f32], len: usize, count: usize| -> Vec<f32> {
        // `src` holds `count` lines of `len` samples; filter along each.
        let mut out = vec![0.0f32; src.len()];
        out.par_chunks_mut(len).zip(src.par_chunks(len)).for_each(|(o, line)| {
            for i in 0..len {
                let (a, b) = (i.saturating_sub(r), (i + r).min(len - 1));
                let mut acc = seed;
                for v in &line[a..=b] {
                    acc = fold(acc, *v);
                }
                o[i] = finish(acc, b - a + 1);
            }
        });
        let _ = count;
        out
    };
    let rows = pass(m, w, h);
    let t = transpose(&rows, w, h);
    let cols = pass(&t, h, w);
    transpose(&cols, h, w)
}

fn transpose(m: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut t = vec![0.0f32; w * h];
    t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = m[y * w + x];
        }
    });
    t
}

/// `SHADER_GRADE_MIX`: `kept + (current - kept) * mix`.
pub fn grade_mix(px: &mut [f32], kept: &[f32], mix: f32) {
    px.par_chunks_exact_mut(4).zip(kept.par_chunks_exact(4)).for_each(|(p, o)| {
        for c in 0..3 {
            p[c] = o[c] + (p[c] - o[c]) * mix;
        }
    });
}

#[cfg(test)]
mod saturation_definition_tests {
    use super::*;

    /// Saturation is a fraction of the brightest channel, so it says
    /// "how much colour is in this" at any brightness.
    ///
    /// The numbers are the decision, so they are written down. The HSL
    /// definition this replaced (user's choice, 2026-09-12) put the
    /// third and fourth rows at 0.497 and 1.000 — it called a
    /// near-white pastel *fully saturated*, and rated a light pastel
    /// pink above a mid saturated red.
    #[test]
    fn saturation_tells_vivid_from_pale_at_every_brightness() {
        let cases = [
            ("mid grey", [0.18, 0.18, 0.18], 0.000),
            ("mid saturated red", [0.40, 0.05, 0.05], 0.627),
            ("light pastel pink", [0.90, 0.72, 0.72], 0.094),
            ("near-white pastel", [1.00, 0.95, 0.95], 0.022),
            ("saturated red highlight", [4.00, 0.50, 0.50], 0.597),
            ("pastel highlight 4x", [4.00, 3.60, 3.60], 0.044),
            ("white highlight 4x", [4.00, 4.00, 4.00], 0.000),
        ];
        for (name, rgb, want) in cases {
            let got = picture_hsl(rgb).s;
            assert!(
                (got - want).abs() < 0.01,
                "{name}: saturation {got:.3}, expected {want:.3}",
            );
        }
    }

    /// The two orderings the old definition got backwards.
    #[test]
    fn a_vivid_colour_reads_more_saturated_than_a_pale_one() {
        let red = picture_hsl([0.40, 0.05, 0.05]).s;
        let pink = picture_hsl([0.90, 0.72, 0.72]).s;
        assert!(red > pink, "a mid red ({red:.3}) must beat a pale pink ({pink:.3})");

        let vivid_hi = picture_hsl([4.0, 0.5, 0.5]).s;
        let pale_hi = picture_hsl([4.0, 3.6, 3.6]).s;
        assert!(
            vivid_hi > pale_hi * 5.0,
            "a vivid highlight ({vivid_hi:.3}) must be clearly above a pale one ({pale_hi:.3})",
        );
        // Neither is pinned at the ceiling, which is what let a
        // saturation boost do nothing up there.
        assert!(vivid_hi < 0.99, "a highlight is reading as fully saturated again");
    }

    /// Scaling exposure leaves saturation where it was, near enough.
    ///
    /// Not exactly: the value is taken from the display-encoded
    /// picture, and that curve is not linear, so a stop of exposure
    /// moves it a little. The old definition moved it to 1.0 and
    /// stayed there.
    #[test]
    fn saturation_barely_moves_with_exposure() {
        let base = [0.30, 0.12, 0.06];
        let at_1 = picture_hsl(base).s;
        for stops in [1.0f32, 2.0, 3.0] {
            let k = 2.0f32.powf(stops);
            let lifted = picture_hsl([base[0] * k, base[1] * k, base[2] * k]).s;
            assert!(
                (lifted - at_1).abs() < 0.12,
                "{stops} stops up: saturation went {at_1:.3} -> {lifted:.3}",
            );
        }
    }

    /// A saturation change holds the brightest channel, so it deepens a
    /// colour rather than brightening it — what a saturation control is
    /// expected to do.
    #[test]
    fn saturating_deepens_rather_than_brightens() {
        let rgb = [0.50, 0.35, 0.30];
        let hsl = picture_hsl(rgb);
        let more = picture_rgb(hsl.h, (hsl.s * 1.6).min(1.0), hsl.v, hsl.magnitude);
        let peak_before = rgb.iter().cloned().fold(f32::MIN, f32::max);
        let peak_after = more.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            (peak_after - peak_before).abs() < 0.02,
            "the brightest channel moved from {peak_before:.3} to {peak_after:.3}",
        );
        let min_before = rgb.iter().cloned().fold(f32::MAX, f32::min);
        let min_after = more.iter().cloned().fold(f32::MAX, f32::min);
        assert!(min_after < min_before, "saturating did not deepen the darkest channel");
    }

    /// Round trip with nothing changed gives the colour back.
    #[test]
    fn the_round_trip_is_lossless() {
        for rgb in [
            [0.18, 0.18, 0.18],
            [0.40, 0.05, 0.05],
            [0.90, 0.72, 0.72],
            [4.00, 0.50, 0.50],
            [4.00, 3.60, 3.60],
            [0.02, 0.30, 0.70],
        ] {
            let h = picture_hsl(rgb);
            let back = picture_rgb(h.h, h.s, h.v, h.magnitude);
            for c in 0..3 {
                assert!(
                    (back[c] - rgb[c]).abs() < 1e-3,
                    "{rgb:?} came back as {back:?}",
                );
            }
        }
    }
}

#[cfg(test)]
mod matte_finesse_tests {
    use super::matte_finesse;

    /// An 11×11 matte with one full pixel in the middle.
    fn dot() -> Vec<f32> {
        let mut m = vec![0.0f32; 11 * 11 * 4];
        let c = (5 * 11 + 5) * 4;
        m[c] = 1.0;
        m[c + 1] = 1.0;
        m[c + 2] = 1.0;
        m
    }
    fn at(m: &[f32], x: usize, y: usize) -> f32 {
        m[(y * 11 + x) * 4]
    }
    fn count_on(m: &[f32]) -> usize {
        m.chunks_exact(4).filter(|p| p[0] > 0.999).count()
    }

    #[test]
    fn grow_dilates_and_shrink_erodes() {
        let mut m = dot();
        matte_finesse(&mut m, 11, 11, 0.0, 0.0, 2.0, 0.0);
        assert_eq!(count_on(&m), 25, "a dot grown by 2 px is a 5×5 square");
        matte_finesse(&mut m, 11, 11, 0.0, 0.0, -2.0, 0.0);
        assert_eq!(count_on(&m), 1, "shrunk back by 2 px it is the dot again");
        assert_eq!(at(&m, 5, 5), 1.0);
    }

    #[test]
    fn blur_spreads_the_matte_and_keeps_its_sum() {
        let mut m = dot();
        matte_finesse(&mut m, 11, 11, 0.0, 0.0, 0.0, 1.0);
        assert!(at(&m, 5, 5) < 1.0 && at(&m, 5, 5) > 0.0);
        assert!(at(&m, 6, 5) > 0.0 && at(&m, 7, 5) > 0.0 && at(&m, 8, 5) == 0.0, "two box passes of 1 px reach 2 px");
        let sum: f32 = m.chunks_exact(4).map(|p| p[0]).sum();
        assert!((sum - 1.0).abs() < 1e-4, "a blur moves matte, it does not make or lose it: {sum}");
    }

    #[test]
    fn clean_black_and_white_clip_and_rescale() {
        let mut m = vec![0.0f32; 4 * 4];
        for (i, v) in [0.05f32, 0.5, 0.95, 1.0].iter().enumerate() {
            m[i * 4] = *v;
            m[i * 4 + 1] = *v;
            m[i * 4 + 2] = *v;
        }
        matte_finesse(&mut m, 4, 1, 0.1, 0.1, 0.0, 0.0);
        assert_eq!(m[0], 0.0, "noise under clean black goes to 0");
        assert!((m[4] - 0.5).abs() < 1e-6, "the middle stays the middle");
        assert_eq!(m[8], 1.0, "above clean white is solid");
        assert_eq!(m[12], 1.0);
    }
}
