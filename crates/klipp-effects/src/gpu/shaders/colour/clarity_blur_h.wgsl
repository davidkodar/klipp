
struct Params {
    width: u32, height: u32, radius: u32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    let r = i32(params.radius);
    var sum = 0.0;
    var count = 0u;
    for (var dx = -r; dx <= r; dx = dx + 1) {
        let sx = clamp(x + dx, 0, i32(params.width) - 1);
        let p = unpack(input[u32(y) * params.width + u32(sx)]);
        sum += params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
        count += 1u;
    }
    let luma = sum / f32(count);
    // Store linear luma in the .x channel; downstream passes read it as
    // a linear [0,1] scalar.
    output[idx] = pack(vec4<f32>(luma, 0.0, 0.0, 0.0));
}
