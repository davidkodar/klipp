
struct Params { width: u32, height: u32, gain: f32, strength: f32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// A scale on the light, which is what an exposure change is: a
// shutter catching less of a light's cycle multiplies the frame by a
// constant, and undoing it multiplies back. Doing this on the encoded
// values would make the correction depend on how dark the frame was.
//
// Alpha is left alone — the frame got darker, it did not become more
// transparent.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let g = 1.0 + (params.gain - 1.0) * params.strength;
    output[idx] = pack(vec4<f32>(p.x * g, p.y * g, p.z * g, p.w));
}
