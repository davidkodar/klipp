//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    red_shift: f32,
    blue_shift: f32,
    center_x: f32,
    center_y: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

// Bilinear, clamped at the edges, as the CPU's `bilinear` mixes: a
// nearest pixel flips on the last bit of a position landing on a half
// (the division, the square root), and the two routes then read
// different pixels at edges (#1442).
fn sample_bilinear(fx: f32, fy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = f32(x) - cx;
    let dy = f32(y) - cy;
    let dist = sqrt(dx * dx + dy * dy);
    let max_dist = max(sqrt(cx * cx + cy * cy), 1.0);
    let norm_dist = dist / max_dist;

    // Red channel: sample shifted
    let r_scale = 1.0 + params.red_shift * norm_dist / max_dist;
    let r_val = sample_bilinear(cx + dx * r_scale, cy + dy * r_scale, w, h).x;

    // Green channel: original position
    let g_pixel = unpack(input[idx]);
    let g_val = g_pixel.y;
    let a_val = g_pixel.w;

    // Blue channel: sample shifted
    let b_scale = 1.0 + params.blue_shift * norm_dist / max_dist;
    let b_val = sample_bilinear(cx + dx * b_scale, cy + dy * b_scale, w, h).z;

    output[idx] = pack(vec4<f32>(r_val, g_val, b_val, a_val));
}
