//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn spill_lay(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let r = rect();
    let p = region_pixel(r, gid.x);
    if p.x < 0 { return; }
    let idx = u32(p.y) * params.width + u32(p.x);
    let top = vec4<f32>(params.color.rgb, input[idx]);
    if params.mode == 0u {
        output[idx] = pack(top);
        return;
    }
    let bottom = unpack(second[idx]);
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
