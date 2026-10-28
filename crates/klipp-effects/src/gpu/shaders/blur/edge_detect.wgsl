
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    mix_val: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

fn get_lum(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    let p = unpack(input[u32(cy) * params.width + u32(cx)]);
    return dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), p.xyz);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);

    // Sobel Gx
    let gx = -1.0 * get_lum(x - 1, y - 1) + 1.0 * get_lum(x + 1, y - 1)
           + -2.0 * get_lum(x - 1, y)     + 2.0 * get_lum(x + 1, y)
           + -1.0 * get_lum(x - 1, y + 1) + 1.0 * get_lum(x + 1, y + 1);
    // Sobel Gy
    let gy = -1.0 * get_lum(x - 1, y - 1) + -2.0 * get_lum(x, y - 1) + -1.0 * get_lum(x + 1, y - 1)
           +  1.0 * get_lum(x - 1, y + 1) +  2.0 * get_lum(x, y + 1) +  1.0 * get_lum(x + 1, y + 1);

    let edge = clamp(sqrt(gx * gx + gy * gy) * params.strength, 0.0, 1.0);

    let orig = unpack(input[idx]);
    let m = params.mix_val;
    let mixed = orig.xyz * (1.0 - m) + vec3<f32>(edge) * m;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
