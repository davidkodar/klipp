
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// Keep with `cpu::BOX_SLIDES_FROM`.
const SLIDES_FROM: i32 = 8;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let h_i32 = i32(params.height);

    if params.radius < SLIDES_FROM {
        for (var dy = -params.radius; dy <= params.radius; dy++) {
            let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
            sum += unpack(input[u32(sy) * params.width + x]);
        }
        output[idx] = pack(sum * inv);
        return;
    }
    // `blur_h.wgsl`'s slide down a stretch of the column; neighbouring
    // invocations take neighbouring columns, so they read neighbouring
    // pixels.
    let r = params.radius;
    let len = min(h_i32, max(32, diam));
    let y0 = i32(y);
    if y0 % len != 0 { return; }
    let last = h_i32 - 1;
    sum = f32(max(0, r - y0)) * at(0, x) + f32(max(0, y0 + r - last)) * at(last, x);
    for (var k = max(y0 - r, 0); k <= min(y0 + r, last); k++) {
        sum += at(k, x);
    }
    for (var sy = y0; sy < min(y0 + len, h_i32); sy++) {
        output[u32(sy) * params.width + x] = pack(sum * inv);
        sum += at(min(sy + r + 1, last), x) - at(max(sy - r, 0), x);
    }
}

// The column's pixel at row `y`, which the caller keeps on the column.
fn at(y: i32, x: u32) -> vec4<f32> {
    return unpack(input[u32(y) * params.width + x]);
}
