
struct Params {
    width: u32,
    height: u32,
    in_black: f32,
    in_white: f32,
    inv_gamma: f32,
    out_black: f32,
    out_white: f32,
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
    let e = enc3(p.xyz);

    let in_range = max(params.in_white - params.in_black, 0.001);
    // No ceiling: a highlight above the input white keeps its distance
    // above the output white rather than clipping to it.
    let m = max((e - vec3<f32>(params.in_black)) / in_range, vec3<f32>(0.0));

    let g = vec3<f32>(
        pow(m.x, params.inv_gamma),
        pow(m.y, params.inv_gamma),
        pow(m.z, params.inv_gamma),
    );

    let out_range = params.out_white - params.out_black;
    let out = vec3<f32>(params.out_black) + g * out_range;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
