
struct Params { width: u32, height: u32, mix: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> other: array<vec4<f32>>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// Blend: `a + (b - a) * mix`, where `a` is the chain's picture (port 0)
// and `b` is the composite buffer (port 1).
//
// All four channels, alpha included — unlike SHADER_GRADE_MIX, which
// keeps the current alpha because it is mixing a grade back against
// the same picture. Two different pictures have two different mattes,
// and the mix has to carry them both.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let a = unpack(input[idx]);
    let b = unpack(other[idx]);
    output[idx] = pack(a + (b - a) * params.mix);
}
