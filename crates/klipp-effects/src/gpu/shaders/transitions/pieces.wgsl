//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    count: u32,
    samples: u32,
    background_to: u32,
    subject: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> from_buf: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> to_buf: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var<storage, read> pieces: array<f32>;
@group(0) @binding(5) var<storage, read> matte: array<f32>;

const STRIDE: u32 = 60u;
const O_BOX: u32 = 9u;
const O_JAC: u32 = 13u;
const O_SIDE: u32 = 14u;
const O_SHADE: u32 = 15u;
const O_OPACITY: u32 = 16u;
const O_SHADOW: u32 = 17u;
const O_AA: u32 = 21u;
const O_MATTE: u32 = 22u;
const O_PLANES: u32 = 24u;

// The matte at a picture point, as `piece_sample` reads the pictures;
// nothing without one (`pieces::matte_sample`).
fn matte_sample(sx: f32, sy: f32) -> f32 {
    if params.subject != 2u {
        return 0.0;
    }
    let fx = min(max(sx, 0.0), f32(params.width) - 1.0);
    let fy = min(max(sy, 0.0), f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let m00 = matte[y0 * params.width + x0];
    let m10 = matte[y0 * params.width + x1];
    let m01 = matte[y1 * params.width + x0];
    let m11 = matte[y1 * params.width + x1];
    let top = m00 + (m10 - m00) * tx;
    let bot = m01 + (m11 - m01) * tx;
    return clamp(top + (bot - top) * ty, 0.0, 1.0);
}

// The share of the piece at `base` the matte leaves at a picture point
// (`pieces::matte_share`).
fn matte_share(base: u32, sx: f32, sy: f32) -> f32 {
    let part = pieces[base + O_MATTE];
    if params.subject == 0u || part == 0.0 {
        return 1.0;
    }
    let v = matte_sample(sx - 0.5, sy - 0.5);
    if part > 0.0 {
        return v;
    }
    return 1.0 - v;
}

fn piece_sample(to_side: bool, sx: f32, sy: f32) -> vec4<f32> {
    let fx = min(max(sx, 0.0), f32(params.width) - 1.0);
    let fy = min(max(sy, 0.0), f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    var c00: vec4<f32>;
    var c10: vec4<f32>;
    var c01: vec4<f32>;
    var c11: vec4<f32>;
    if to_side {
        c00 = unpack(to_buf[y0 * params.width + x0]);
        c10 = unpack(to_buf[y0 * params.width + x1]);
        c01 = unpack(to_buf[y1 * params.width + x0]);
        c11 = unpack(to_buf[y1 * params.width + x1]);
    } else {
        c00 = unpack(from_buf[y0 * params.width + x0]);
        c10 = unpack(from_buf[y0 * params.width + x1]);
        c01 = unpack(from_buf[y1 * params.width + x0]);
        c11 = unpack(from_buf[y1 * params.width + x1]);
    }
    let top = c00 + (c10 - c00) * tx;
    let bot = c01 + (c11 - c01) * tx;
    return top + (bot - top) * ty;
}

// The distance inside the piece at `base` of the picture point the
// screen point (x, y) maps to (x: in picture pixels), and that point
// (y, z); w < 0 outside its box or behind the camera.
fn piece_at(base: u32, x: f32, y: f32) -> vec4<f32> {
    if x < pieces[base + O_BOX] || y < pieces[base + O_BOX + 1u] || x > pieces[base + O_BOX + 2u] || y > pieces[base + O_BOX + 3u] {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    let wz = pieces[base + 6u] * x + pieces[base + 7u] * y + pieces[base + 8u];
    if wz <= 1e-9 {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    let sx = (pieces[base] * x + pieces[base + 1u] * y + pieces[base + 2u]) / wz;
    let sy = (pieces[base + 3u] * x + pieces[base + 4u] * y + pieces[base + 5u]) / wz;
    var sd: f32 = 1.0e6;
    for (var k: u32 = 0u; k < 12u; k = k + 1u) {
        let o = base + O_PLANES + k * 3u;
        sd = min(sd, pieces[o] * sx + pieces[o + 1u] * sy + pieces[o + 2u]);
    }
    return vec4<f32>(sd, sx, sy, 1.0);
}

fn coverage(base: u32, sd: f32) -> f32 {
    let aa = pieces[base + O_AA] * pieces[base + O_JAC];
    if aa <= 1e-4 {
        if sd >= -0.01 {
            return 1.0;
        }
        return 0.0;
    }
    return clamp(sd / aa + 0.5, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width) + 0.5;
    let y = f32(idx / params.width) + 0.5;
    let hx = x + 0.001953125;
    let hy = y + 0.0029296875;
    var bg: vec4<f32>;
    if params.background_to == 1u {
        bg = unpack(to_buf[idx]);
    } else {
        bg = unpack(from_buf[idx]);
    }
    var total = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var s: u32 = 0u; s < 8u; s = s + 1u) {
        if s >= params.samples {
            break;
        }
        var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        var alpha: f32 = 0.0;
        var shadow: f32 = 0.0;
        var subject_alpha: f32 = 0.0;
        for (var i: u32 = 0u; i < 129u; i = i + 1u) {
            if i >= params.count {
                break;
            }
            let base = (s * params.count + i) * STRIDE;
            if alpha < 0.999 {
                let hit = piece_at(base, hx, hy);
                if hit.w > 0.0 {
                    let cov0 = coverage(base, hit.x) * pieces[base + O_OPACITY];
                    if cov0 > 0.0 {
                        var at = piece_at(base, x, y);
                        if at.w <= 0.0 {
                            at = vec4<f32>(hit.x, 0.0, 0.0, 1.0);
                        }
                        let cov = cov0 * matte_share(base, at.y, at.z);
                        let c = piece_sample(pieces[base + O_SIDE] > 0.5, at.y - 0.5, at.z - 0.5);
                        var k = (1.0 - alpha) * cov;
                        if pieces[base + O_MATTE] < 0.0 && params.subject != 0u {
                            k = min((1.0 - alpha) * cov / (1.0 - subject_alpha), 1.0 - alpha);
                        }
                        if pieces[base + O_MATTE] > 0.0 {
                            subject_alpha = subject_alpha + k;
                        }
                        let shade = pieces[base + O_SHADE];
                        acc = acc + vec4<f32>(k * c.x * shade, k * c.y * shade, k * c.z * shade, k * c.w);
                        alpha = alpha + k;
                    }
                }
            }
            if pieces[base + O_SHADOW + 2u] > 0.0 {
                let sh = piece_at(base, hx - pieces[base + O_SHADOW], hy - pieces[base + O_SHADOW + 1u]);
                if sh.w > 0.0 {
                    let a = pieces[base + O_SHADOW + 2u] * clamp(sh.x / pieces[base + O_JAC] / pieces[base + O_SHADOW + 3u] + 0.5, 0.0, 1.0) * matte_share(base, sh.y, sh.z);
                    shadow = 1.0 - (1.0 - shadow) * (1.0 - a);
                }
            }
        }
        let dark = 1.0 - shadow;
        let rest = 1.0 - alpha;
        total = total + vec4<f32>(acc.x + rest * bg.x * dark, acc.y + rest * bg.y * dark, acc.z + rest * bg.z * dark, acc.w + rest * bg.w);
    }
    output[idx] = pack(total / f32(params.samples));
}
