
struct Params {
    width: u32, height: u32, light: u32, kind: u32,
    scene_scale: f32, exposure: f32, peak: f32, _pad: u32,
    luma_r: f32, luma_g: f32, luma_b: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn h_working_luma() -> vec3<f32> {
    return vec3<f32>(params.luma_r, params.luma_g, params.luma_b);
}
//#include pixel_storage
//#include hdr_display_light

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    var rgb = vec3<f32>(0.0);
    if params.light == 1u {
        rgb = hdr_display_light(p.xyz, params.kind, params.scene_scale, params.peak);
    } else {
        rgb = tone_map_hdr_vec(p.xyz, params.exposure);
    }
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
