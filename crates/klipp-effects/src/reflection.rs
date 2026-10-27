//! Reflection (#2707): a layer mirrored about a waterline, as on water, a
//! glossy floor, a wet street or glass. Below the line each pixel is the
//! one as far above it, moved sideways by a ripple of horizontal bands
//! that breaks into strips and gaps further from the line, smeared down
//! more the further it is, fading out, and tinted toward the surface's
//! colour. `submerge` hides the layer's own pixels below the line, so
//! letters dipping into the water show only their reflection there.
//!
//! The ripple is [`crate::displace`]'s noise, so the two move alike. The
//! GPU runs [`shader_wgsl`], the CPU [`cpu`], the same computation.
//!
//! The effect never reads the picture below the layer: how the reflected
//! rows mix with it (`blend`) is the compositor's, which blends the rows
//! below the line with that mode ([`BuiltinEffect::split_blend`]).
//!
//! [`BuiltinEffect::split_blend`]: crate::BuiltinEffect::split_blend

use crate::displace::fbm;
use crate::wgsl::*;

/// How the reflected rows mix with the picture below.
pub const NORMAL: u32 = 0;
pub const SCREEN: u32 = 1;
pub const SOFT_LIGHT: u32 = 2;
pub const OVERLAY: u32 = 3;

/// The water surfaces' blend: chosen against footage (#2707).
const CALM_BLEND: u32 = NORMAL;

/// Where the ripple comes from: its noise. (The picture below is to come.)
pub const FROM_NOISE: u32 = 0;

/// The surfaces a reflection starts from, by name, label and
/// description: water still and rippled, a glossy floor, a wet street,
/// glass. One table for the app's presets and the assistant's kinds.
pub const SURFACES: &[(&str, &str, &str)] = &[
    ("calm_water", "Calm Water", "A slow sideways wobble that breaks into bands further down"),
    ("rippled_water", "Rippled Water", "Choppy water: a stronger ripple in more broken strips"),
    ("glossy_floor", "Glossy Floor", "A still, soft mirror on a polished floor"),
    ("wet_street", "Wet Street", "Long smeared streaks on wet tarmac"),
    ("glass", "Glass", "A faint, crisp mirror"),
];

/// The reflection on the surface `name` (see [`SURFACES`]), its waterline
/// at `waterline`.
pub fn surface(name: &str, waterline: f32) -> Option<crate::BuiltinEffect> {
    // Water takes its tint and darkness from the water measured under it
    // (#2795); the other surfaces keep theirs.
    let e = |ripple_amount, ripple_size, ripple_speed, breakup, smear, fade, opacity, darken, contact, blend, tint, tint_amount, submerge| crate::BuiltinEffect::Reflection {
        waterline, gap: 0.0, ripple_amount, ripple_size, ripple_speed, breakup, smear, fade, opacity, darken, contact, blend, tint, tint_amount,
        tint_auto: submerge, darken_auto: submerge, water: [0, 0, 0], water_key: 0,
        submerge, ripple_from: FROM_NOISE, seed: 0, time: 0.0,
    };
    Some(match name {
        "calm_water" => e(10.0, 60.0, 0.6, 0.6, 0.5, 0.45, 0.85, 0.45, 0.6, CALM_BLEND, [25, 85, 100], 0.2, true),
        "rippled_water" => e(16.0, 40.0, 1.2, 0.7, 0.55, 0.5, 0.8, 0.5, 0.6, CALM_BLEND, [25, 85, 100], 0.25, true),
        "glossy_floor" => e(0.0, 60.0, 0.0, 0.0, 0.15, 0.7, 0.45, 0.2, 0.3, NORMAL, [0, 0, 0], 0.0, false),
        "wet_street" => e(3.0, 120.0, 0.2, 0.35, 0.8, 0.6, 0.6, 0.4, 0.4, OVERLAY, [30, 30, 35], 0.15, false),
        "glass" => e(0.0, 60.0, 0.0, 0.0, 0.0, 0.35, 0.3, 0.0, 0.0, SCREEN, [0, 0, 0], 0.0, false),
        _ => return None,
    })
}

/// How much darker a reflection on water of colour `water` (sRGB) is
/// (#2795): more on dark water, which gives back little, less on bright;
/// never past 0.15 to 0.7, so glints or a black frame can't take it to
/// either end.
pub fn auto_darken(water: [u8; 3]) -> f32 {
    let l = (0.2126 * water[0] as f32 + 0.7152 * water[1] as f32 + 0.0722 * water[2] as f32) / 255.0;
    (0.2 + 0.5 * (1.0 - l)).clamp(0.15, 0.7)
}

/// The effect's numbers as both computations take them, in pixels of the
/// layer it runs on.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    /// The waterline's row.
    pub waterline: f32,
    pub ripple_amount: f32,
    pub ripple_size: f32,
    pub ripple_speed: f32,
    pub breakup: f32,
    pub smear: f32,
    pub fade: f32,
    pub opacity: f32,
    /// The tint, linear light.
    pub tint: [f32; 3],
    pub tint_amount: f32,
    pub submerge: u32,
    pub seed: u32,
    pub time: f32,
    /// How much darker than the layer it is, 0 to 1: water reflects
    /// little.
    pub darken: f32,
    /// How dark the thin band where the layer meets the line is, 0 to 1.
    pub contact: f32,
    /// How far below the layer's base the water is, pixels.
    pub gap: f32,
}
// 20 fields x 4 bytes = 80 bytes

/// An sRGB byte as linear light, as the tint is given.
pub fn linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Where the pixel `d` rows below the line comes from, across and down,
/// and how much of it is kept by the break-up: the shared part of both
/// computations.
fn field(p: &Params, x: f32, d: f32) -> (f32, f32, f32) {
    let size = p.ripple_size.max(1.0);
    let t = p.time * p.ripple_speed;
    // Bands: features `size` across and a fifth as tall, so the water
    // wobbles mostly sideways, more the further from the line.
    let q = V3 { x: x / size, y: d / (size * 0.2), z: t };
    let grow = 0.35 + 0.65 * clamp(d / (size * 2.0), 0.0, 1.0);
    // A finer jitter over the bands, so no two strips move as one copy.
    let fine = V3 { x: x / (size * 0.4), y: d / (size * 0.07), z: t * 1.3 };
    let dx = p.ripple_amount * grow * (fbm(q, p.seed) + 0.35 * fbm(fine, p.seed.wrapping_add(3)));
    let dy = p.ripple_amount * 0.12 * fbm(q + V3 { x: 41.3, y: 7.9, z: 0.0 }, p.seed.wrapping_add(1));
    // Strips and gaps, longer across than down, of uneven heights (their
    // rows bent by a coarser field), more of them further from the line:
    // all there are a few band widths down.
    let bend = 1.2 * fbm(V3 { x: x / (size * 1.7), y: d / (size * 0.5), z: t * 0.4 }, p.seed.wrapping_add(4));
    let b = fbm(V3 { x: x / (size * 3.0), y: d / (size * 0.12) + bend, z: t * 0.6 }, p.seed.wrapping_add(2));
    let thr = -0.7 + 1.5 * p.breakup * clamp(d / (size * 1.5), 0.0, 1.0);
    let keep = smoothstep(thr - 0.16, thr + 0.16, b);
    // The water's own small ripples through it, darker and lighter, the
    // more the more it breaks up.
    let glint = fbm(V3 { x: x / (size * 0.3), y: d / (size * 0.05), z: t * 1.7 }, p.seed.wrapping_add(5));
    let keep = keep * clamp(1.0 + glint * 0.8 * p.breakup, 0.0, 1.0);
    (dx, dy, keep)
}

/// The reflected colour: darker than the layer, as water reflects
/// little, and its hue turned toward the tint at the same brightness.
fn colour(p: &Params, c: [f32; 3]) -> [f32; 3] {
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    // Darker, and richer by as much: what water gives back is the
    // colour without the white of the highlights.
    let darken = clamp(p.darken, 0.0, 1.0);
    let l = luma(c);
    let dark = c.map(|v| max(l + (v - l) * (1.0 + 0.6 * darken), 0.0) * (1.0 - darken));
    let k = luma(dark) / luma(p.tint).max(1e-4);
    std::array::from_fn(|i| mix(dark[i], p.tint[i] * k, p.tint_amount))
}

/// The thin darker band where the layer meets the line (#2707), the
/// layer's shadow on the water: its edge wobbles with the ripple, so it
/// is not ruled straight. 1 where there is none.
fn contact_shade(p: &Params, x: f32, d: f32) -> f32 {
    let size = p.ripple_size.max(1.0);
    let wobble = (p.ripple_amount * 0.25 + 1.0) * fbm(V3 { x: x / (size * 0.6), y: 0.0, z: p.time * p.ripple_speed * 0.8 }, p.seed.wrapping_add(6));
    let band = max(size * 0.12, 2.0);
    // Off as the layer floats clear of the water.
    let resting = 1.0 - smoothstep(0.0, band, p.gap);
    1.0 - 0.6 * clamp(p.contact, 0.0, 1.0) * resting * (1.0 - smoothstep(0.0, band, d + wobble))
}

/// How much of the reflection is left `d` rows below the line.
fn faded(p: &Params, d: f32) -> f32 {
    let reach = mix(1.0, 0.05, clamp(p.fade, 0.0, 1.0));
    let f = 1.0 - clamp(d / p.height.max(1) as f32 / reach, 0.0, 1.0);
    f * f
}

/// The picture at (`sx`, `sy`), bilinear and premultiplied, clear past
/// its edges.
fn sample(at: &dyn Fn(i32, i32) -> [f32; 4], sx: f32, sy: f32) -> [f32; 4] {
    let (x0, y0) = (sx.floor() as i32, sy.floor() as i32);
    let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
    let mut sum = [0.0f32; 4];
    for (dx, dy, w) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
        let c = at(x0 + dx, y0 + dy);
        for k in 0..3 {
            sum[k] += c[k] * c[3] * w;
        }
        sum[3] += c[3] * w;
    }
    sum
}

/// The reflection at (`x`, `y`) of the picture `at`, straight RGBA.
pub fn pixel(p: &Params, at: &dyn Fn(i32, i32) -> [f32; 4], x: f32, y: f32) -> [f32; 4] {
    let own = at(x as i32, y as i32);
    // Below the water's surface, `gap` under the line the layer is
    // mirrored about.
    let d = y + 0.5 - p.waterline - p.gap.max(0.0);
    if d <= 0.0 {
        return own;
    }
    let (dx, dy, keep) = field(p, x, d);
    // Stretched down the further it is from the line, longer than the
    // layer is tall, and smeared over seven taps that spread with it.
    let stretch = 1.0 + p.smear * 2.5 * clamp(d / (p.ripple_size.max(1.0) * 2.0), 0.0, 1.0);
    let r = p.smear * d * 0.5;
    let (sx, sy) = (x + dx, p.waterline - d / stretch + dy - 0.5);
    let mut sum = [0.0f32; 4];
    for k in 0..7 {
        let s = sample(at, sx, sy + r * (k as f32 - 3.0) / 3.0);
        for c in 0..4 {
            sum[c] += s[c] / 7.0;
        }
    }
    let a = sum[3] * faded(p, d) * keep * p.opacity;
    let mut out = [0.0f32; 4];
    if sum[3] > 1e-6 {
        out[..3].copy_from_slice(&colour(p, [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3]]));
        let shade = contact_shade(p, x, d);
        for c in &mut out[..3] {
            *c *= shade;
        }
    }
    out[3] = a;
    if p.submerge != 0 || own[3] <= 0.0 {
        return out;
    }
    // The layer's own pixel over its reflection.
    let alpha = own[3] + out[3] * (1.0 - own[3]);
    if alpha <= 1e-6 {
        return [0.0; 4];
    }
    let mut over = [0.0f32; 4];
    for c in 0..3 {
        over[c] = (own[c] * own[3] + out[c] * out[3] * (1.0 - own[3])) / alpha;
    }
    over[3] = alpha;
    over
}

/// The CPU's Reflection of `pixels`, straight RGBA `p.width` × `p.height`.
pub fn cpu(pixels: &[f32], p: &Params) -> Vec<f32> {
    let (w, h) = (p.width as i32, p.height as i32);
    let at = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return [0.0; 4];
        }
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let mut out = vec![0.0; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            out[((y * w + x) * 4) as usize..][..4].copy_from_slice(&pixel(p, &at, x as f32, y as f32));
        }
    }
    out
}

/// The GPU's Reflection: one pass, each pixel below the line from its
/// mirror above. After the pixel storage and the displace noise.
macro_rules! shader_wgsl {
    () => {
        "
struct Params {
    width: u32,
    height: u32,
    waterline: f32,
    ripple_amount: f32,
    ripple_size: f32,
    ripple_speed: f32,
    breakup: f32,
    smear: f32,
    fade: f32,
    opacity: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    tint_amount: f32,
    submerge: u32,
    seed: u32,
    time: f32,
    darken: f32,
    contact: f32,
    gap: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn at(x: i32, y: i32) -> vec4<f32> {
    let w = i32(params.width);
    let h = i32(params.height);
    if x < 0 || y < 0 || x >= w || y >= h { return vec4<f32>(0.0); }
    return unpack(input[u32(y * w + x)]);
}

fn sample(sx: f32, sy: f32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    var sum = vec4<f32>(0.0);
    let c00 = at(x0, y0);
    let c10 = at(x0 + 1, y0);
    let c01 = at(x0, y0 + 1);
    let c11 = at(x0 + 1, y0 + 1);
    sum += vec4<f32>(c00.rgb * c00.a, c00.a) * ((1.0 - fx) * (1.0 - fy));
    sum += vec4<f32>(c10.rgb * c10.a, c10.a) * (fx * (1.0 - fy));
    sum += vec4<f32>(c01.rgb * c01.a, c01.a) * ((1.0 - fx) * fy);
    sum += vec4<f32>(c11.rgb * c11.a, c11.a) * (fx * fy);
    return sum;
}

fn reflected(x: f32, y: f32) -> vec4<f32> {
    let own = at(i32(x), i32(y));
    let d = y + 0.5 - params.waterline - max(params.gap, 0.0);
    if d <= 0.0 { return own; }
    let h = f32(max(params.height, 1u));
    let size = max(params.ripple_size, 1.0);
    let t = params.time * params.ripple_speed;
    let q = vec3<f32>(x / size, d / (size * 0.2), t);
    let grow = 0.35 + 0.65 * clamp(d / (size * 2.0), 0.0, 1.0);
    let fine = vec3<f32>(x / (size * 0.4), d / (size * 0.07), t * 1.3);
    let dx = params.ripple_amount * grow * (dn_fbm(q, params.seed) + 0.35 * dn_fbm(fine, params.seed + 3u));
    let dy = params.ripple_amount * 0.12 * dn_fbm(q + vec3<f32>(41.3, 7.9, 0.0), params.seed + 1u);
    let bend = 1.2 * dn_fbm(vec3<f32>(x / (size * 1.7), d / (size * 0.5), t * 0.4), params.seed + 4u);
    let b = dn_fbm(vec3<f32>(x / (size * 3.0), d / (size * 0.12) + bend, t * 0.6), params.seed + 2u);
    let thr = -0.7 + 1.5 * params.breakup * clamp(d / (size * 1.5), 0.0, 1.0);
    let glint = dn_fbm(vec3<f32>(x / (size * 0.3), d / (size * 0.05), t * 1.7), params.seed + 5u);
    let keep = smoothstep(thr - 0.16, thr + 0.16, b) * clamp(1.0 + glint * 0.8 * params.breakup, 0.0, 1.0);
    let stretch = 1.0 + params.smear * 2.5 * clamp(d / (size * 2.0), 0.0, 1.0);
    let r = params.smear * d * 0.5;
    let sx = x + dx;
    let sy = params.waterline - d / stretch + dy - 0.5;
    var sum = vec4<f32>(0.0);
    for (var k = 0; k < 7; k++) {
        sum += sample(sx, sy + r * (f32(k) - 3.0) / 3.0) / 7.0;
    }
    let reach = mix(1.0, 0.05, clamp(params.fade, 0.0, 1.0));
    let f = 1.0 - clamp(d / h / reach, 0.0, 1.0);
    var out = vec4<f32>(0.0, 0.0, 0.0, sum.a * f * f * keep * params.opacity);
    if sum.a > 1e-6 {
        let lw = vec3<f32>(0.2126, 0.7152, 0.0722);
        let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
        let darken = clamp(params.darken, 0.0, 1.0);
        let c = sum.rgb / sum.a;
        let l = dot(c, lw);
        let dark = max(vec3<f32>(l) + (c - vec3<f32>(l)) * (1.0 + 0.6 * darken), vec3<f32>(0.0)) * (1.0 - darken);
        let k = dot(dark, lw) / max(dot(tint, lw), 1e-4);
        let wobble = (params.ripple_amount * 0.25 + 1.0) * dn_fbm(vec3<f32>(x / (size * 0.6), 0.0, params.time * params.ripple_speed * 0.8), params.seed + 6u);
        let band = max(size * 0.12, 2.0);
        let resting = 1.0 - smoothstep(0.0, band, params.gap);
        let shade = 1.0 - 0.6 * clamp(params.contact, 0.0, 1.0) * resting * (1.0 - smoothstep(0.0, band, d + wobble));
        out = vec4<f32>(mix(dark, tint * k, params.tint_amount) * shade, out.a);
    }
    if params.submerge != 0u || own.a <= 0.0 { return out; }
    let alpha = own.a + out.a * (1.0 - own.a);
    if alpha <= 1e-6 { return vec4<f32>(0.0); }
    return vec4<f32>((own.rgb * own.a + out.rgb * out.a * (1.0 - own.a)) / alpha, alpha);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    if idx >= w * params.height { return; }
    output[idx] = pack(reflected(f32(idx % w), f32(idx / w)));
}
"
    };
}
pub(crate) use shader_wgsl;

#[cfg(test)]
mod tests {
    use super::*;

    fn params(w: u32, h: u32) -> Params {
        Params {
            width: w,
            height: h,
            waterline: h as f32 * 0.5,
            ripple_amount: 0.0,
            ripple_size: 40.0,
            ripple_speed: 1.0,
            breakup: 0.0,
            smear: 0.0,
            fade: 0.0,
            opacity: 1.0,
            tint: [0.0; 3],
            tint_amount: 0.0,
            submerge: 1,
            seed: 0,
            time: 0.0,
            darken: 0.0,
            contact: 0.0,
            gap: 0.0,
        }
    }

    /// A picture whose every row is its own grey: row `y` is `y / h`.
    fn rows(w: u32, h: u32) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).flat_map(move |_| { let v = y as f32 / h as f32; [v, v, v, 1.0] })).collect()
    }

    /// Still and unrippled, the rows below the line are those as far above
    /// it, and the ones above are untouched.
    #[test]
    fn calm_it_mirrors_the_rows_about_the_line() {
        let (w, h) = (8, 20);
        let p = params(w, h);
        let px = rows(w, h);
        let out = cpu(&px, &p);
        let row = |buf: &[f32], y: u32| buf[(y * w * 4) as usize];
        for y in 0..10 {
            assert_eq!(row(&out, y), row(&px, y), "above the line, row {y}");
        }
        for k in 0..10 {
            assert!((row(&out, 10 + k) - row(&px, 9 - k)).abs() < 1e-5, "row {} mirrors row {}", 10 + k, 9 - k);
        }
    }

    /// The fade takes the reflection away with distance; without
    /// `submerge` the layer's own pixels stay over it.
    #[test]
    fn it_fades_with_distance_and_submerge_hides_the_layer_below_the_line() {
        let (w, h) = (4, 40);
        let px = rows(w, h);
        let alpha = |p: &Params, y: u32| cpu(&px, p)[((y * w) * 4 + 3) as usize];
        let p = Params { fade: 0.8, ..params(w, h) };
        assert!(alpha(&p, 21) > alpha(&p, 26) && alpha(&p, 26) > alpha(&p, 30), "falls off");
        assert_eq!(alpha(&p, 39), 0.0, "gone far below");
        // Not submerged, the opaque layer covers its reflection.
        let p = Params { submerge: 0, fade: 0.8, ..params(w, h) };
        assert_eq!(alpha(&p, 39), 1.0);
        assert_eq!(cpu(&px, &p)[(30 * w * 4) as usize], px[(30 * w * 4) as usize]);
    }

    /// The ripple moves the reflection sideways more than down, it moves
    /// in time, and the break-up cuts gaps that grow with distance.
    #[test]
    fn the_ripple_is_sideways_bands_that_break_up_further_away() {
        let p = Params { ripple_amount: 10.0, ripple_size: 30.0, breakup: 1.0, ..params(200, 400) };
        let (mut across, mut down) = (0.0f32, 0.0f32);
        let (mut near_kept, mut far_kept) = (0.0f32, 0.0f32);
        for x in 0..200 {
            let near = field(&p, x as f32, 8.0);
            let far = field(&p, x as f32, 180.0);
            across += far.0.abs();
            down += far.1.abs();
            near_kept += near.2;
            far_kept += far.2;
        }
        assert!(across > down * 4.0, "mostly sideways: {across} vs {down}");
        assert!(near_kept > far_kept * 1.5, "more gaps further down: {near_kept} vs {far_kept}");
        let later = Params { time: 0.5, ..p };
        assert_ne!(field(&p, 50.0, 60.0).0, field(&later, 50.0, 60.0).0, "moves in time");
    }

    /// With a gap the layer floats: its reflection starts as much lower,
    /// the rows between left as they are.
    #[test]
    fn a_gap_floats_the_layer_and_starts_the_reflection_lower() {
        let (w, h) = (4, 40);
        let px = rows(w, h);
        let row = |buf: &[f32], y: u32| buf[(y * w * 4) as usize];
        let p = Params { gap: 4.0, waterline: 16.0, ..params(w, h) };
        let out = cpu(&px, &p);
        for y in 0..20 {
            assert_eq!(row(&out, y), row(&px, y), "above the water, row {y}");
        }
        for k in 0..10 {
            assert!((row(&out, 20 + k) - row(&px, 15 - k)).abs() < 1e-5, "row {} mirrors row {}", 20 + k, 15 - k);
        }
    }

    /// The contact band darkens the reflection right at the line, by as
    /// much as asked, and not once the layer floats clear.
    #[test]
    fn the_contact_band_darkens_at_the_line_unless_the_layer_floats() {
        let (w, h) = (8, 60);
        let px = [0.8f32, 0.6, 0.2, 1.0].repeat((w * h) as usize);
        let first = |p: &Params| cpu(&px, p)[((30 * w + 2) * 4) as usize];
        let far = |p: &Params| cpu(&px, p)[((50 * w + 2) * 4) as usize];
        let flat = Params { waterline: 30.0, ripple_size: 40.0, ..params(w, h) };
        let shaded = Params { contact: 1.0, ..flat };
        assert!(first(&shaded) < first(&flat) * 0.7, "darker at the line: {} vs {}", first(&shaded), first(&flat));
        assert!((far(&shaded) - far(&flat)).abs() < 1e-5, "only a thin band");
        let floating = Params { gap: 20.0, waterline: 10.0, ..shaded };
        assert!((first(&floating) - first(&Params { contact: 0.0, ..floating })).abs() < 1e-5, "no band under a floating layer");
    }

    /// A clear pixel lends no colour to the smear, and a reflection
    /// beyond the picture's top is clear.
    #[test]
    fn clear_pixels_lend_no_colour_and_past_the_top_is_clear() {
        let (w, h) = (4, 20);
        let mut px = vec![0.0f32; (w * h * 4) as usize];
        // A white opaque row 8, and red where it is clear around it.
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(if y == 8 { &[1.0, 1.0, 1.0, 1.0] } else { &[1.0, 0.0, 0.0, 0.0] });
            }
        }
        let p = Params { smear: 1.0, waterline: 10.0, ..params(w, h) };
        let out = cpu(&px, &p);
        let i = ((11 * w) * 4) as usize;
        assert!(out[i + 3] > 0.0 && (out[i + 1] - 1.0).abs() < 1e-4, "white, not pink: {:?}", &out[i..i + 4]);
        let p = Params { waterline: 4.0, ..params(w, h) };
        assert_eq!(cpu(&px, &p)[((15 * w) * 4 + 3) as usize], 0.0, "nothing above the top to reflect");
    }
}
