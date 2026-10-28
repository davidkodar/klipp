
struct Params {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> blurred: array<vec4<f32>>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let luma = params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
    let blurred_luma = unpack(blurred[idx]).x;
    let detail = luma - blurred_luma;
    // Midtone weight peaked on linear 0.18 (perceptual mid-grey).
    let midtone_weight = max(1.0 - abs((luma / 0.18) - 1.0), 0.0);
    let adj = detail * params.amount * midtone_weight;
    output[idx] = pack(vec4<f32>(
        p.x + adj,
        p.y + adj,
        p.z + adj,
        p.w,
    ));
}
