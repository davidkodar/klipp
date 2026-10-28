
struct Params {
    width: u32,
    height: u32,
    mix_val: f32,
    _pad: u32,
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
    let p = unpack(input[idx]);
    let m = params.mix_val;
    let e = enc3(p.xyz);
    // The inverse of a highlight is black, not a negative: invert what
    // the display can show. `e` stays the mix's base, so mix 0 is identity.
    let inv = vec3<f32>(1.0) - clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
    let out = e + (inv - e) * m;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
