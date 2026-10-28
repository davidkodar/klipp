//#include pixel_storage

struct Params { width: u32, height: u32, radius: u32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = i32(idx % params.width);
    let r = i32(params.radius);
    var m = 1.0e30;
    for (var dx = -r; dx <= r; dx++) {
        let sx = x + dx;
        if sx < 0 || sx >= i32(params.width) { continue; }
        m = min(m, unpack(input[y * params.width + u32(sx)]).x);
    }
    output[idx] = pack(vec4<f32>(m, m, m, m));
}
