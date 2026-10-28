
struct Params {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    amount: f32,
    samples: u32,
    mode: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// Bilinear, clamped at the edges, as the CPU's `bilinear` mixes: a
// nearest pixel flips on the last bit of a position landing on a half
// (a fused multiply-add here, the approximate sin and cos), and the two
// routes then read different pixels at every edge (#1440).
fn sample_linear_rgb(fx: f32, fy: f32, w: u32, h: u32) -> vec3<f32> {
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]).xyz;
    let tr = unpack(input[y0 * w + x1]).xyz;
    let bl = unpack(input[y1 * w + x0]).xyz;
    let br = unpack(input[y1 * w + x1]).xyz;
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = x - cx;
    let dy = y - cy;
    let s = params.samples;

    var sum = vec3<f32>(0.0, 0.0, 0.0);

    if params.mode == 0u {
        // Zoom blur
        let scale = params.amount / 100.0;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            sum = sum + sample_linear_rgb(x + dx * t * scale, y + dy * t * scale, w, h);
        }
    } else {
        // Spin blur
        let half_arc = params.amount * 3.14159265 / 180.0 * 0.5;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            let ang = t * half_arc;
            let cos_a = cos(ang);
            let sin_a = sin(ang);
            sum = sum + sample_linear_rgb(cx + dx * cos_a - dy * sin_a, cy + dx * sin_a + dy * cos_a, w, h);
        }
    }

    let inv = 1.0 / f32(s);
    // Preserve original alpha (alpha stays linear, no gamma).
    let orig_a = unpack(input[idx]).w;
    output[idx] = pack(vec4<f32>(sum * inv, orig_a));
}
