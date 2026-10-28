//#include pixel_storage

struct Params { width: u32, height: u32, bx0: u32, by0: u32, bw: u32, bh: u32, _p0: u32, _p1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if i >= params.bw * params.bh { return; }
    let x = params.bx0 + i % params.bw;
    let y = params.by0 + i / params.bw;
    if x >= params.width || y >= params.height { return; }
    let rg = unpack2x16float(input[i * 2u]);
    let ba = unpack2x16float(input[i * 2u + 1u]);
    output[y * params.width + x] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rg.x), srgb_to_linear_c(rg.y), srgb_to_linear_c(ba.x))), ba.y));
}
