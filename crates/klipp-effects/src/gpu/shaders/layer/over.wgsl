//#include pixel_storage

struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn over(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let top = unpack(input[idx]);
    let bottom = unpack(second[idx]);
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
