//! Picture warps, each written once (#2758): a wave, a twirl, a bulge or
//! pinch, ripples and torn rows. Each function gives the point a pixel
//! is taken from. The Warp effect draws them as the picture's own, timed
//! by the clip; the transitions that bend two shots (Wave Distortion,
//! Swirl, Ripple, Glitch RGB Split) call the same functions with their
//! own strength through the cut, so a fix to one is a fix to both.
//!
//! `include/warp_functions.wgsl` is the GPU's text of them and the functions here
//! are the CPU's, written in Rust through `crate::wgsl` line for line,
//! so the two agree. Each keeps the order of its arithmetic as the
//! transitions had it, so they draw what they drew.

// The shaders' own literals for pi and tau, so the twins read alike.
#![allow(clippy::approx_constant)]

use crate::wgsl::*;

/// The Warp effect's modes.
pub const WAVE: u32 = 0;
pub const TWIRL: u32 = 1;
pub const BULGE: u32 = 2;
pub const RIPPLE: u32 = 3;
pub const BLOCKS: u32 = 4;

/// An integer hash: the same bits on every GPU and on the CPU.
pub(crate) fn hash_u(v: u32) -> u32 {
    let mut x = v;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    (x >> 16) ^ x
}

/// A number from 0 to 1 hashed from three.
pub(crate) fn rand3(a: u32, b: u32, c: u32) -> f32 {
    hash_u(a ^ hash_u(b ^ hash_u(c))) as f32 / 4294967295.0
}

/// The wave: (`x`, `y`) in a `w` × `h` frame bent by a sine `frequency`
/// waves across it, `envelope` pixels at most, `shift` waves along; `dir`
/// 0 across, 1 up and down, 2 both. The point it is taken from, kept in
/// the frame, and the crest there (-1 to 1), what may light it.
// Written as the shader is: each branch sets what it moves.
#[allow(unused_assignments)]
pub fn wave(x: f32, y: f32, w: f32, h: f32, envelope: f32, frequency: f32, dir: u32, shift: f32) -> V3 {
    let ay = (y / h * frequency + shift) * 6.28318530;
    let ax = (x / w * frequency + shift) * 6.28318530;
    let mut off_x: f32 = 0.0;
    let mut off_y: f32 = 0.0;
    let mut crest: f32 = 0.0;
    if dir == 0u32 {
        off_x = sin(ay) * envelope;
        crest = cos(ay);
    } else if dir == 1u32 {
        off_y = sin(ax) * envelope;
        crest = cos(ax);
    } else {
        off_x = sin(ay) * envelope * 0.7;
        off_y = sin(ax) * envelope * 0.7;
        crest = (cos(ay) + cos(ax)) * 0.5;
    }
    let sx = clamp(x + off_x, 0.0, w - 1.0);
    let sy = clamp(y + off_y, 0.0, h - 1.0);
    vec3!(sx, sy, crest)
}

/// The twirl: (`x`, `y`) turned about (`cx`, `cy`) by `angle` radians,
/// less and less away from it, by `exp(-falloff)` at `max_r`. The point
/// it is taken from, kept in the `w` × `h` frame.
pub fn twirl(x: f32, y: f32, w: f32, h: f32, cx: f32, cy: f32, max_r: f32, angle: f32, falloff: f32) -> V2 {
    let dx = x - cx;
    let dy = y - cy;
    let dist = length(vec2!(dx, dy));
    let norm_dist = dist / max_r;
    let swirl_angle = angle * exp(-norm_dist * falloff);
    let cos_a = cos(swirl_angle);
    let sin_a = sin(swirl_angle);
    let sx = clamp(cos_a * dx - sin_a * dy + cx, 0.0, w - 1.0);
    let sy = clamp(sin_a * dx + cos_a * dy + cy, 0.0, h - 1.0);
    vec2!(sx, sy)
}

/// Ripples: rings round (`cx`, `cy`), `rings` to the frame's height,
/// moved `shift` radians outward, pushing each pixel along its radius by
/// `wave × amp` pixels of a 1080-line frame at most. The point it is
/// taken from, kept in the `w` × `h` frame, and the rings' phase there.
pub fn ripple(x: f32, y: f32, w: f32, h: f32, cx: f32, cy: f32, rings: f32, shift: f32, wave: f32, amp: f32) -> V3 {
    let dx = x - cx;
    let dy = y - cy;
    let dist = length(vec2!(dx, dy));
    let phase = dist / h * rings * 6.28318530 - shift;
    let ripple = sin(phase) * wave * amp * h / 1080.0;
    let mut nx: f32 = 0.0;
    let mut ny: f32 = 0.0;
    if dist > 0.1 {
        nx = dx / dist;
        ny = dy / dist;
    }
    let sx = clamp(x + nx * ripple, 0.0, w - 1.0);
    let sy = clamp(y + ny * ripple, 0.0, h - 1.0);
    vec3!(sx, sy, phase)
}

/// A bulge or a pinch: inside `radius` of (`cx`, `cy`), each pixel taken
/// from nearer the centre (`strength` above 0, the middle swells) or
/// further out (below 0, it shrinks), the edge left in place. The point
/// it is taken from.
pub fn bulge(x: f32, y: f32, cx: f32, cy: f32, radius: f32, strength: f32) -> V2 {
    let dx = x - cx;
    let dy = y - cy;
    let r = length(vec2!(dx, dy)) / max(radius, 1.0);
    if r >= 1.0 || r < 0.000001 {
        return vec2!(x, y);
    }
    let k = pow(r, strength);
    vec2!(cx + dx * k, cy + dy * k)
}

/// A torn row's push: the frame cut into bands `band_h` pixels tall,
/// each, at `step`, pushed by up to `envelope × 0.04` of the frame's
/// width `w` or left where it is.
pub fn band_shift(y: f32, band_h: f32, step: u32, envelope: f32, w: f32) -> f32 {
    let band = u32c(y / band_h);
    let on = select(0.0, 1.0, rand3(band, step, 6u32) > 0.35);
    (0.3 + 0.7 * rand3(band, step, 7u32)) * envelope * 0.04 * w * on
}

// The WGSL is `src/gpu/shaders/include/warp_functions.wgsl` (#2890):
// The functions above in WGSL, after a shader's own declarations: the
// hash, then each warp under its name with `warp_` before it.

/// The Warp effect's settings, as the kernels take them.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub mode: u32,
    /// Wave, ripple: how far at most, pixels of a 1080-line frame.
    pub amplitude: f32,
    /// Wave, ripple: a wave's length, a fraction of the frame.
    pub wavelength: f32,
    /// Waves, rings or tears a second.
    pub speed: f32,
    /// Wave: 0 across, 1 up and down, 2 both.
    pub direction: u32,
    /// Twirl: the turn at the centre, degrees.
    pub angle: f32,
    /// Bulge (above 0) or pinch (below); torn rows: how far.
    pub strength: f32,
    /// Twirl, bulge: its reach, a fraction of the frame's height.
    pub radius: f32,
    pub center_x: f32,
    pub center_y: f32,
    /// Torn rows: a band's height, a fraction of the frame's height.
    pub band: f32,
    /// The clip's time, seconds.
    pub time: f32,
}

/// Where pixel (`x`, `y`) of a `w` × `h` frame is taken from under `s`.
pub fn source(s: &Settings, x: f32, y: f32, w: f32, h: f32) -> V2 {
    let (cx, cy) = (s.center_x * w, s.center_y * h);
    match s.mode {
        WAVE => {
            // The phase in turns, wrapped to one: sin repeats, and a
            // long clip's large argument would round apart on the GPU.
            let p = wave(x, y, w, h, s.amplitude * h / 1080.0, 1.0 / max(s.wavelength, 0.001), s.direction, fract(s.time * s.speed));
            vec2!(p.x, p.y)
        }
        TWIRL => twirl(x, y, w, h, cx, cy, max(s.radius * h, 1.0), s.angle * 3.14159265 / 180.0, 3.0),
        BULGE => bulge(x, y, cx, cy, s.radius * h, s.strength),
        RIPPLE => {
            let p = ripple(x, y, w, h, cx, cy, 1.0 / max(s.wavelength, 0.001), fract(s.time * s.speed) * 6.28318530, 1.0, s.amplitude);
            vec2!(p.x, p.y)
        }
        _ => {
            let step = u32c(max(s.time * s.speed, 0.0));
            let band_h = max(s.band * h, 1.0);
            let flip = select(-1.0, 1.0, rand3(u32c(y / band_h), step, 8u32) > 0.5);
            vec2!(clamp(x + flip * band_shift(y, band_h, step, s.strength, w), 0.0, w - 1.0), y)
        }
    }
}

/// The Warp effect on the CPU: each pixel taken from [`source`],
/// premultiplied while mixed, as the GPU's `stylise/warp.wgsl` does.
pub fn cpu(pixels: &[f32], width: u32, height: u32, s: &Settings) -> Vec<f32> {
    let (w, h) = (width as f32, height as f32);
    let at = |x: i32, y: i32| {
        let i = ((y.clamp(0, height as i32 - 1) * width as i32 + x.clamp(0, width as i32 - 1)) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let mut out = vec![0.0; pixels.len()];
    use rayon::prelude::*;
    out.par_chunks_exact_mut(width as usize * 4).enumerate().for_each(|(y, row)| {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let p = source(s, x as f32, y as f32, w, h);
            px.copy_from_slice(&crate::displace::sample_premultiplied(&at, p.x, p.y));
        }
    });
    out
}

/// Whether `s` moves no pixel.
pub fn is_identity(s: &Settings) -> bool {
    match s.mode {
        WAVE | RIPPLE => s.amplitude.abs() < f32::EPSILON,
        TWIRL => s.angle.abs() < f32::EPSILON || s.radius <= 0.0,
        BULGE => s.strength.abs() < f32::EPSILON || s.radius <= 0.0,
        _ => s.strength.abs() < f32::EPSILON,
    }
}

// The WGSL is `src/gpu/shaders/stylise/warp.wgsl` (#2890):
// The GPU's Warp: one pass, each pixel taken from `warp_source`. After
// the pixel storage, `include/warp_functions.wgsl` and the displace sampler.
