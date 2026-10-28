
struct Params {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
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
    let mx = max(max(p.x, p.y), p.z);
    let mn = min(min(p.x, p.y), p.z);
    let sat = mx - mn;
    let luma = params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
    let weight = 1.0 - clamp(sat, 0.0, 1.0);
    let boost = 1.0 + params.amount * weight;
    let out = vec3<f32>(
        luma + (p.x - luma) * boost,
        luma + (p.y - luma) * boost,
        luma + (p.z - luma) * boost,
    );
    output[idx] = pack(vec4<f32>(out, p.w));
}
