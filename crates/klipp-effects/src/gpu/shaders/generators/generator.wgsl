
struct Params {
    width: u32, height: u32, shape: u32, _pad: u32,
    a: vec4<f32>, b: vec4<f32>,
    dir: vec2<f32>, centre: vec2<f32>,
    radius: f32, _p0: u32, _p1: u32, _p2: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let d = vec2<f32>(f32(idx % params.width) + 0.5, f32(idx / params.width) + 0.5) - params.centre;
    var t = 0.0;
    if params.shape == 1u {
        t = 0.5 + dot(d, params.dir) / (2.0 * params.radius);
    } else if params.shape == 2u {
        t = length(d) / params.radius;
    }
    t = clamp(t, 0.0, 1.0);
    output[idx] = pack(params.a + (params.b - params.a) * t);
}
