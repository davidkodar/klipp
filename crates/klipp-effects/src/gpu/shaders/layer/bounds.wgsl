//#include pixel_storage

struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn bounds(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    if gid.x != 0u { return; }
    var box = vec4<f32>(f32(params.width), f32(params.height), 0.0, 0.0);
    for (var y = 0u; y < params.height; y++) {
        let row = input[y];
        if row.z > row.x { box = vec4<f32>(min(box.xy, row.xy), max(box.zw, row.zw)); }
    }
    output[0] = box;
}
