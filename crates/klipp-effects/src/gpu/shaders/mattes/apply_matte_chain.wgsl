
struct Params { width: u32, height: u32, mix: f32, channel: u32, luma: vec3<f32>, invert: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> kept: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> matte: array<vec4<f32>>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let graded = unpack(input[idx]);
    let plain = unpack(kept[idx]);
    let m = unpack(matte[idx]);
    var c = m.x;
    if params.channel == 1u { c = dot(m.xyz, params.luma); } else if params.channel == 2u { c = m.w; }
    if params.invert == 1u { c = 1.0 - clamp(c, 0.0, 1.0); }
    let k = clamp(c * params.mix, 0.0, 1.0);
    output[idx] = pack(vec4<f32>(
        plain.x + (graded.x - plain.x) * k,
        plain.y + (graded.y - plain.y) * k,
        plain.z + (graded.z - plain.z) * k,
        1.0,
    ));
}
