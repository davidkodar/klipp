//#include pixel_storage

struct Params { width: u32, height: u32, factor: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> unused: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> layer: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(layer[idx]);
    if idx == 0u && arrayLength(&unused) == 0u { return; }
    layer[idx] = pack(vec4<f32>(p.xyz, p.w * params.factor));
}
