//#include pixel_storage

struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn rows(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let y = gid.x;
    if y >= params.height { return; }
    var left = params.width;
    var right = 0u;
    for (var x = 0u; x < params.width; x++) {
        if unpack(input[y * params.width + x]).a > 0.001 {
            left = min(left, x);
            right = x + 1u;
        }
    }
    output[y] = vec4<f32>(f32(left), f32(y), f32(right), f32(y + 1u));
}
