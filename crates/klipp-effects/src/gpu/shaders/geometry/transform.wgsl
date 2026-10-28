//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    cos_rx: f32,
    sin_rx: f32,
    cos_ry: f32,
    sin_ry: f32,
    focal: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let dx = f32(x) - params.anchor_x - params.position_x;
    let dy = f32(y) - params.anchor_y - params.position_y;

    // Inverse Z-rotation (2D)
    let rx = dx * params.cos_r + dy * params.sin_r;
    let ry = -dx * params.sin_r + dy * params.cos_r;

    // Inverse scale
    var sx = rx * params.inv_sx;
    var sy = ry * params.inv_sy;

    // 3D tilt: the point on the tilted picture this pixel sees — the
    // ray through the pinhole meeting the picture's plane. Mirrors
    // `transform::unproject` on the CPU; see there for the maths.
    if params.cos_rx != 1.0 || params.cos_ry != 1.0 {
        let a = params.sin_rx;
        let b = params.sin_ry;
        let f = params.focal;
        var u = 0.0;
        var v = 0.0;
        var seen = true;
        if f > 0.0 {
            let a11 = sx * b - f * params.cos_ry;
            let a12 = -sx * a * params.cos_ry - f * a * b;
            let a21 = sy * b;
            let a22 = -sy * a * params.cos_ry - f * params.cos_rx;
            let det = a11 * a22 - a12 * a21;
            if abs(det) < 1e-9 {
                seen = false;
            } else {
                let b1 = -sx * f;
                let b2 = -sy * f;
                u = (b1 * a22 - a12 * b2) / det;
                v = (a11 * b2 - b1 * a21) / det;
                seen = f + (u * b - v * a * params.cos_ry) > 0.0;
            }
        } else if abs(params.cos_rx) < 1e-6 || abs(params.cos_ry) < 1e-6 {
            seen = false;
        } else {
            v = sy / params.cos_rx;
            u = (sx - v * a * b) / params.cos_ry;
        }
        if seen {
            sx = u;
            sy = v;
        } else {
            sx = -1.0e9;
            sy = -1.0e9;
        }
    }

    sx = sx + params.anchor_x;
    sy = sy + params.anchor_y;

    // Bilinear sample — same reasoning as the CPU path in
    // klipp-effects/src/transform.rs: nearest-neighbour quantises
    // sub-pixel motion to integer steps and produces a low-rate
    // stutter on slow scrolls (credits, slides, ticker crawls).
    // Storage buffers are linear-f32 here, so the lerp is in
    // linear light — cleaner than the CPU path's sRGB approximation.
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);

    let p00 = sample_pixel(x0, y0);
    let p10 = sample_pixel(x1, y0);
    let p01 = sample_pixel(x0, y1);
    let p11 = sample_pixel(x1, y1);

    let top = mix(p00, p10, fx);
    let bot = mix(p01, p11, fx);
    output[idx] = pack(mix(top, bot, fy));
}

fn sample_pixel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    let src_idx = u32(y) * params.width + u32(x);
    return unpack(input[src_idx]);
}
