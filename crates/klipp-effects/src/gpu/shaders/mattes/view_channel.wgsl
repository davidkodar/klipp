//#include pixel_storage

struct Params { width: u32, height: u32, channel: u32, _pad0: u32, luma: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let a = clamp(p.w, 0.0, 1.0);
    let rgb = p.xyz * a;
    var v = 0.0;
    switch params.channel {
        case 1u: { v = rgb.x; }
        case 2u: { v = rgb.y; }
        case 3u: { v = rgb.z; }
        case 4u: { v = a; }
        case 5u: { v = dot(rgb, params.luma.xyz); }
        default: {
            output[idx] = pack(vec4<f32>(rgb, 1.0));
            return;
        }
    }
    output[idx] = pack(vec4<f32>(v, v, v, 1.0));
}
