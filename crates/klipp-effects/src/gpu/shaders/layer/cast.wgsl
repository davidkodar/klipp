//#include pixel_storage

struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn make_cast(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width) - params.offset_x;
    let y = i32(idx / params.width) - params.offset_y;
    var a = 0.0;
    if x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height) {
        a = unpack(input[u32(y) * params.width + u32(x)]).a * params.opacity;
    }
    output[idx] = pack(vec4<f32>(params.color.rgb, a));
}
