//#include pixel_storage

struct Params { width: u32, height: u32, channel: u32, invert: u32, luma_r: f32, luma_g: f32, luma_b: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> source: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> matte: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(source[idx]);
    var rgb = p.xyz;
    if params.channel != 0u && p.w > 0.0001 {
        rgb = clamp(rgb / p.w, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    var raw: f32;
    switch params.channel {
        case 0u: { raw = p.w; }
        case 1u: { raw = params.luma_r * rgb.x + params.luma_g * rgb.y + params.luma_b * rgb.z; }
        case 2u: { raw = rgb.x; }
        case 3u: { raw = rgb.y; }
        default: { raw = rgb.z; }
    }
    var m = clamp(raw, 0.0, 1.0);
    if params.invert != 0u { m = 1.0 - m; }
    matte[idx] = pack(vec4<f32>(m, m, m, m));
}
