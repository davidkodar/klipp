//! Displace (#2702): each pixel taken from a point moved by an animated
//! noise field. The field is a seeded value noise in three dimensions,
//! the picture's two and time, so it moves and evolves in place rather
//! than sliding: water, heat haze, a wobble. Two octaves, the second
//! finer and weaker, keep it from looking like a grid.
//!
//! The GPU runs [`shader_wgsl`]; the CPU runs [`cpu`], the same
//! computation written in Rust, so the two agree. The noise is also what
//! the water reflection's ripple is made of (#2707): [`noise_wgsl`] and
//! [`fbm`] are its one definition.
//!
//! A layer can be straight alpha with any colour where it is clear (a
//! title), so the picture is sampled premultiplied: a clear pixel's
//! colour never bleeds into the ones the field pulls it beside.

use crate::wgsl::*;

/// How a displacement moves pixels: both ways, across only, or up and
/// down only.
pub const BOTH: u32 = 0;
pub const ACROSS: u32 = 1;
pub const UP_DOWN: u32 = 2;

/// A u32 hash of a lattice point and a seed: the same bits on the CPU and
/// the GPU, as both wrap on overflow.
fn hash(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ (z as u32).wrapping_mul(0xcb1a_b31f) ^ seed.wrapping_mul(0x1656_67b1);
    h = (h ^ (h >> 15)).wrapping_mul(0x2c1b_3c6d);
    h = (h ^ (h >> 12)).wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h as f32 / 4_294_967_295.0
}

/// Value noise at `p`, 0 to 1, smooth between lattice points.
fn value(p: V3, seed: u32) -> f32 {
    let i = V3 { x: p.x.floor(), y: p.y.floor(), z: p.z.floor() };
    let f = p - i;
    let u = f * f * (3.0 - f * 2.0);
    let (ix, iy, iz) = (i.x as i32, i.y as i32, i.z as i32);
    let c = |dx: i32, dy: i32, dz: i32| hash(ix + dx, iy + dy, iz + dz, seed);
    let x00 = mix(c(0, 0, 0), c(1, 0, 0), u.x);
    let x10 = mix(c(0, 1, 0), c(1, 1, 0), u.x);
    let x01 = mix(c(0, 0, 1), c(1, 0, 1), u.x);
    let x11 = mix(c(0, 1, 1), c(1, 1, 1), u.x);
    mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z)
}

/// Two octaves of [`value`] noise at `p`, -1 to 1.
pub fn fbm(p: V3, seed: u32) -> f32 {
    let n = value(p, seed) * 0.65 + value(p * 2.03 + V3 { x: 17.0, y: 31.0, z: 7.0 }, seed) * 0.35;
    n * 2.0 - 1.0
}

/// The move at pixel (`x`, `y`): a field of features `size` pixels
/// across, `amount` pixels at most, `time × speed` sizes along in time.
pub fn offset(x: f32, y: f32, amount: f32, size: f32, speed: f32, direction: u32, seed: u32, time: f32) -> V2 {
    let p = V3 { x: x / size.max(1.0), y: y / size.max(1.0), z: time * speed };
    let dx = if direction == UP_DOWN { 0.0 } else { fbm(p, seed) * amount };
    let dy = if direction == ACROSS { 0.0 } else { fbm(p + V3 { x: 41.3, y: 7.9, z: 0.0 }, seed.wrapping_add(1)) * amount };
    V2 { x: dx, y: dy }
}

/// The picture at (`sx`, `sy`), bilinear, premultiplied while it is mixed
/// so a clear pixel lends no colour, and straight again after.
pub fn sample_premultiplied(at: &dyn Fn(i32, i32) -> [f32; 4], sx: f32, sy: f32) -> [f32; 4] {
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
    if sum[3] > 1e-6 {
        [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3], sum[3]]
    } else {
        [0.0; 4]
    }
}

/// The CPU's Displace of `pixels`, straight RGBA `width` × `height`.
#[allow(clippy::too_many_arguments)]
pub fn cpu(pixels: &[f32], width: u32, height: u32, amount: f32, size: f32, speed: f32, direction: u32, seed: u32, time: f32) -> Vec<f32> {
    let (w, h) = (width as i32, height as i32);
    let at = |x: i32, y: i32| {
        let i = ((y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let mut out = vec![0.0; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let d = offset(x as f32, y as f32, amount, size, speed, direction, seed, time);
            let c = sample_premultiplied(&at, x as f32 + d.x, y as f32 + d.y);
            out[((y * w + x) * 4) as usize..][..4].copy_from_slice(&c);
        }
    }
    out
}

/// The noise in WGSL: [`hash`], [`value`], [`fbm`] and [`offset`] as the
/// GPU runs them, for any shader that ripples (#2707). A literal, so a
/// shader can be put together with `concat!`.
macro_rules! noise_wgsl {
    () => {
        "
fn dn_hash(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    var h = (bitcast<u32>(x) * 0x8da6b343u) ^ (bitcast<u32>(y) * 0xd8163841u) ^ (bitcast<u32>(z) * 0xcb1ab31fu) ^ (seed * 0x165667b1u);
    h = (h ^ (h >> 15u)) * 0x2c1b3c6du;
    h = (h ^ (h >> 12u)) * 0x297a2d39u;
    h = h ^ (h >> 15u);
    return f32(h) / 4294967295.0;
}

fn dn_value(p: vec3<f32>, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (vec3<f32>(3.0) - f * 2.0);
    let ix = i32(i.x);
    let iy = i32(i.y);
    let iz = i32(i.z);
    let x00 = mix(dn_hash(ix, iy, iz, seed), dn_hash(ix + 1, iy, iz, seed), u.x);
    let x10 = mix(dn_hash(ix, iy + 1, iz, seed), dn_hash(ix + 1, iy + 1, iz, seed), u.x);
    let x01 = mix(dn_hash(ix, iy, iz + 1, seed), dn_hash(ix + 1, iy, iz + 1, seed), u.x);
    let x11 = mix(dn_hash(ix, iy + 1, iz + 1, seed), dn_hash(ix + 1, iy + 1, iz + 1, seed), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

fn dn_fbm(p: vec3<f32>, seed: u32) -> f32 {
    let n = dn_value(p, seed) * 0.65 + dn_value(p * 2.03 + vec3<f32>(17.0, 31.0, 7.0), seed) * 0.35;
    return n * 2.0 - 1.0;
}

fn dn_offset(x: f32, y: f32, amount: f32, size: f32, speed: f32, direction: u32, seed: u32, time: f32) -> vec2<f32> {
    let p = vec3<f32>(x / max(size, 1.0), y / max(size, 1.0), time * speed);
    var d = vec2<f32>(0.0);
    if direction != 2u { d.x = dn_fbm(p, seed) * amount; }
    if direction != 1u { d.y = dn_fbm(p + vec3<f32>(41.3, 7.9, 0.0), seed + 1u) * amount; }
    return d;
}
"
    };
}
pub(crate) use noise_wgsl;

/// [`sample_premultiplied`] in WGSL, reading the shader's `input` at its
/// `params.width` × `params.height`: what a shader that moves pixels
/// samples by (the Warp's too). After its declarations.
macro_rules! sample_wgsl {
    () => {
        "
fn at(x: i32, y: i32) -> vec4<f32> {
    let w = i32(params.width);
    let h = i32(params.height);
    return unpack(input[u32(clamp(y, 0, h - 1) * w + clamp(x, 0, w - 1))]);
}

fn sample_premultiplied(sx: f32, sy: f32) -> vec4<f32> {
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
    if sum.a > 1e-6 { return vec4<f32>(sum.rgb / sum.a, sum.a); }
    return vec4<f32>(0.0);
}

"
    };
}
pub(crate) use sample_wgsl;

/// The GPU's Displace: one pass, each pixel from where the field moves
/// it, premultiplied while mixed. After the pixel storage and
/// [`noise_wgsl`]; [`sample_wgsl`] after it.
macro_rules! shader_wgsl {
    () => {
        "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    speed: f32,
    direction: u32,
    seed: u32,
    time: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    if idx >= w * params.height { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);
    let d = dn_offset(x, y, params.amount, params.size, params.speed, params.direction, params.seed, params.time);
    output[idx] = pack(sample_premultiplied(x + d.x, y + d.y));
}
"
    };
}
pub(crate) use shader_wgsl;

#[cfg(test)]
mod tests {
    use super::*;

    /// The field is the same at a point whenever it is asked, moves with
    /// time, spans both signs, and keeps to its axis when told to.
    #[test]
    fn the_field_is_steady_moves_in_time_and_keeps_its_axis() {
        let a = offset(10.0, 20.0, 8.0, 40.0, 1.0, BOTH, 3, 0.5);
        assert_eq!(a, offset(10.0, 20.0, 8.0, 40.0, 1.0, BOTH, 3, 0.5));
        assert_ne!(a, offset(10.0, 20.0, 8.0, 40.0, 1.0, BOTH, 3, 1.5), "it moves");
        assert_eq!(offset(10.0, 20.0, 8.0, 40.0, 1.0, ACROSS, 3, 0.5).y, 0.0);
        assert_eq!(offset(10.0, 20.0, 8.0, 40.0, 1.0, UP_DOWN, 3, 0.5).x, 0.0);
        let samples: Vec<f32> = (0..400).map(|i| offset(i as f32 * 7.0, i as f32 * 3.0, 1.0, 30.0, 1.0, BOTH, 9, 0.0).x).collect();
        assert!(samples.iter().any(|v| *v > 0.2) && samples.iter().any(|v| *v < -0.2), "both ways");
        assert!(samples.iter().all(|v| v.abs() <= 1.0), "within the amount");
    }

    /// A clear pixel's colour is never pulled into a visible one.
    #[test]
    fn a_clear_pixel_lends_no_colour() {
        let at = |x: i32, _y: i32| if x == 0 { [1.0, 0.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0, 1.0] };
        let c = sample_premultiplied(&at, 0.5, 0.0);
        assert_eq!(c, [0.0, 0.0, 1.0, 0.5]);
    }
}
