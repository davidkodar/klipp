//#include pixel_storage

struct Params {
    width: u32, height: u32, transfer_id: u32, scene_scale: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = input[idx];
    let code = vec3<f32>(f32(p & 1023u), f32((p >> 10u) & 1023u), f32((p >> 20u) & 1023u)) / 1023.0;
    // PQ/HLG return 1.0 = 10 000 nits (scale 100 to 1.0 = 100 nits);
    // camera logs already return reflectance (scale 1).
    let scene = display_to_linear(code, params.transfer_id) * params.scene_scale;
    output[idx] = pack(vec4<f32>( dot(params.m0.xyz, scene), dot(params.m1.xyz, scene), dot(params.m2.xyz, scene), 1.0, ));
}
