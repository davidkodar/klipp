
struct Params {
    width: u32, height: u32, threshold: f32, intensity: f32,
    hal_r: f32, hal_g: f32, hal_b: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> halation: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let o = unpack(original[idx]);
    let bl = unpack(halation[idx]).xyz;
    let l = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), bl.xyz);
    let h = vec3<f32>(params.hal_r, params.hal_g, params.hal_b) * l * params.intensity;
    output[idx] = pack(vec4<f32>(screen3_at_ceiling(o.xyz, h), o.w));
}
