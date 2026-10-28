//#include pixel_storage

struct Params { width: u32, height: u32, y0: u32, y1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
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
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let rg = unpack2x16float(input[idx * 2u]);
    let ba = unpack2x16float(input[idx * 2u + 1u]);
    output[idx] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rg.x), srgb_to_linear_c(rg.y), srgb_to_linear_c(ba.x))), ba.y));
}
