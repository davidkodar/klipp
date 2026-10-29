//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
@compute @workgroup_size(256)
fn spill_cast(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let r = rect();
    let p = region_pixel(r, gid.x);
    if p.x < 0 { return; }
    let x = p.x - params.offset_x;
    let y = p.y - params.offset_y;
    var a = 0.0;
    if x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height) {
        a = unpack(input[u32(y) * params.width + u32(x)]).a * params.opacity;
    }
    output[u32(p.y) * params.width + u32(p.x)] = a;
}
