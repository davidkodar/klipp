
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
    let w_i32 = i32(params.width);

    if params.radius < SLIDES_FROM {
        for (var dx = -params.radius; dx <= params.radius; dx++) {
            let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
            sum += unpack(input[y * params.width + u32(sx)]);
        }
        output[idx] = pack(sum * inv);
        return;
    }
    // From SLIDES_FROM the window is slid along a stretch of the row at
    // least as long as itself, by the invocation at the stretch's start;
    // the rest return (#3176). Summed afresh at every pixel, a light
    // wrap's widest box (a sigma of 3200) ran past the GPU's job time
    // limit. `cpu::slide_box` is the same sum.
    let r = params.radius;
    let len = min(w_i32, max(32, diam));
    let x0 = i32(x);
    if x0 % len != 0 { return; }
    let last = w_i32 - 1;
    sum = f32(max(0, r - x0)) * at(y, 0) + f32(max(0, x0 + r - last)) * at(y, last);
    for (var k = max(x0 - r, 0); k <= min(x0 + r, last); k++) {
        sum += at(y, k);
    }
    for (var sx = x0; sx < min(x0 + len, w_i32); sx++) {
        output[y * params.width + u32(sx)] = pack(sum * inv);
        sum += at(y, min(sx + r + 1, last)) - at(y, max(sx - r, 0));
    }
}

// The row's pixel `x`, which the caller keeps on the row.
fn at(y: u32, x: i32) -> vec4<f32> {
    return unpack(input[y * params.width + u32(x)]);
}
