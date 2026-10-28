
struct Params { width: u32, height: u32, mix: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> other: array<vec4<f32>>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// Layer: the over operator. Foreground is the chain's picture
// (port 0), background is the composite buffer (port 1).
//
// Alpha composites as `fa + ba * (1 - fa)` rather than saturating at
// 1, so stacking two half-transparent layers gives 0.75 and stays
// available to whatever reads the matte downstream.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let f = unpack(input[idx]);
    let b = unpack(other[idx]);
    let fa = clamp(f.w, 0.0, 1.0);
    let ba = clamp(b.w, 0.0, 1.0);
    let inv = 1.0 - fa;
    output[idx] = pack(vec4<f32>(
        f.x * fa + b.x * inv,
        f.y * fa + b.y * inv,
        f.z * fa + b.z * inv,
        fa + ba * inv,
    ));
}
