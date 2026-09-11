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

use rayon::prelude::*;

/// The unified grade's controls, as `GpuEffectsPipeline::apply_color_grade`
/// takes them.
#[derive(Clone, Copy, Debug)]
pub struct ColorGrade {
    pub exposure: f32,
    pub contrast: f32,
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
        let exp_factor = 2f32.powf(g.exposure);
        r *= exp_factor;
        gr *= exp_factor;
        b *= exp_factor;

        // 2. Tone controls.
        let luma1 = l[0] * r + l[1] * gr + l[2] * b;
        let hi_w = (luma1 - 0.214).max(0.0) * (1.0 / (1.0 - 0.214));
        let lo_w = (0.214 - luma1).max(0.0) * (1.0 / 0.214);
        let white_w = (luma1 - 0.604).max(0.0) * (1.0 / (1.0 - 0.604));
        let black_w = (0.033 - luma1).max(0.0) * (1.0 / 0.033);
        let tone_adj = g.highlights * hi_w + g.shadows * lo_w + g.whites * white_w + g.blacks * black_w;
        r += tone_adj;
        gr += tone_adj;
        b += tone_adj;

        // 3. Colour wheels.
        let luma2 = l[0] * r + l[1] * gr + l[2] * b;
        match g.log_wheels {
            Some([low, mid, high]) => {
                let log_luma = ((luma2.max(0.001) + 0.001).log2() + 10.0) / 10.0;
                let low_w = (-((log_luma - 0.15) * (log_luma - 0.15)) / (2.0 * 0.15 * 0.15)).exp();
                let mid_w = (-((log_luma - 0.50) * (log_luma - 0.50)) / (2.0 * 0.20 * 0.20)).exp();
                let high_w = (-((log_luma - 0.85) * (log_luma - 0.85)) / (2.0 * 0.15 * 0.15)).exp();
                r = r + low.0 * low_w + mid.0 * mid_w + high.0 * high_w;
                gr = gr + low.1 * low_w + mid.1 * mid_w + high.1 * high_w;
                b = b + low.2 * low_w + mid.2 * mid_w + high.2 * high_w;
            }
            None => {
                let lift_w = (1.0 - luma2).max(0.0);
                let gain_w = luma2.max(0.0);
                let gamma_w = (luma2.clamp(0.0, 1.0) * 3.141_592_65).sin().max(0.0);
                r = r + g.lift.0 * lift_w + g.gamma.0 * gamma_w + g.gain.0 * gain_w + g.offset.0;
                gr = gr + g.lift.1 * lift_w + g.gamma.1 * gamma_w + g.gain.1 * gain_w + g.offset.1;
                b = b + g.lift.2 * lift_w + g.gamma.2 * gamma_w + g.gain.2 * gain_w + g.offset.2;
            }
        }

        // 4. Contrast, pivoting on 0.18.
        let cs = 1.0 + g.contrast;
        r = (r - 0.18) * cs + 0.18;
        gr = (gr - 0.18) * cs + 0.18;
        b = (b - 0.18) * cs + 0.18;

        // 5. Saturation.
        let luma3 = l[0] * r + l[1] * gr + l[2] * b;
        r = luma3 + (r - luma3) * g.saturation;
        gr = luma3 + (gr - luma3) * g.saturation;
        b = luma3 + (b - luma3) * g.saturation;

        // 6. White balance.
        let (wr, wg, wbl) = (
            wb[0][0] * r + wb[0][1] * gr + wb[0][2] * b,
            wb[1][0] * r + wb[1][1] * gr + wb[1][2] * b,
            wb[2][0] * r + wb[2][1] * gr + wb[2][2] * b,
        );
        (r, gr, b) = (wr, wg, wbl);

        // 7. Hue.
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
        let gamma_w = (y.clamp(0.0, 1.0) * 3.141_592_65).sin().max(0.0);
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
    pub s: f32,
    pub l: f32,
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
        s = (delta / (1.0 - (2.0 * l - 1.0).abs())).clamp(0.0, 1.0);
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
    PictureHsl { h, s, l, magnitude }
}

/// `picture_rgb`: HSL back to linear light.
pub fn picture_rgb(h: f32, s: f32, l: f32, magnitude: f32) -> [f32; 3] {
    hsl_to_rgb(h, s, l).map(|v| dec_signed(v * magnitude))
}

/// `hsl_rgb_norm`.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s < 0.0001 {
        return [l, l, l];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c * 0.5;
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
pub fn huesat(px: &mut [f32], lut: &[f32; 1536], mix: f32, active: u32) {
    let on = |c: u32| active & (1 << c) != 0;
    px.par_chunks_exact_mut(4).for_each(|p| {
        let orig = [p[0], p[1], p[2]];
        let hsl = picture_hsl(orig);
        let lum_axis = hsl.lum_axis();
        let (mut h, mut s, mut l) = (hsl.h, hsl.s, hsl.l);
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
            l = (l + (hvl - 0.5)).clamp(0.0, 1.0);
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
        let rgb = picture_rgb(h, s, l, hsl.magnitude);
        for c in 0..3 {
            p[c] = orig[c] + (rgb[c] - orig[c]) * mix;
        }
    });
}

/// `SHADER_COLOR_WARPER`: twelve 30° sectors, cosine-blended.
pub fn color_warper(px: &mut [f32], hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) {
    px.par_chunks_exact_mut(4).for_each(|p| {
        let hsl = picture_hsl([p[0], p[1], p[2]]);
        let h_deg = hsl.h * 360.0;
        let sector_f = h_deg / 30.0;
        let sector_low = (sector_f.floor() as u32 % 12) as usize;
        let sector_high = (sector_low + 1) % 12;
        let t = sector_f - sector_f.floor();
        let blend = (1.0 - (t * 3.141_592_65).cos()) * 0.5;
        let hue_shift = hue_shifts[sector_low] * (1.0 - blend) + hue_shifts[sector_high] * blend;
        let sat_shift = sat_shifts[sector_low] * (1.0 - blend) + sat_shifts[sector_high] * blend;
        let sat_weight = hsl.s.clamp(0.0, 1.0);
        let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
        let new_s = (hsl.s + sat_shift * sat_weight).clamp(0.0, 1.0);
        let rgb = picture_rgb(new_h, new_s, hsl.l, hsl.magnitude);
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

/// `SHADER_GRADE_MIX`: `kept + (current - kept) * mix`.
pub fn grade_mix(px: &mut [f32], kept: &[f32], mix: f32) {
    px.par_chunks_exact_mut(4).zip(kept.par_chunks_exact(4)).for_each(|(p, o)| {
        for c in 0..3 {
            p[c] = o[c] + (p[c] - o[c]) * mix;
        }
    });
}
