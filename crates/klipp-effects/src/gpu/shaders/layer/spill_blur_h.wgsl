//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
fn at_x(r: vec4<i32>, x: i32, y: i32) -> u32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    if cx < r.x || cx >= r.z { return 0u; }
    return u32(round(clamp(input[u32(y) * params.width + u32(cx)], 0.0, 1.0) * fixed_scale()));
}
@compute @workgroup_size(64)
fn spill_blur_h(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 64u, 0u, gid_flat.z);
    let r = rect();
    let stretches = (r.z - r.x + 31) / 32;
    if stretches <= 0 { return; }
    let y = r.y + i32(gid.x) / stretches;
    if y >= r.w { return; }
    let x0 = r.x + (i32(gid.x) % stretches) * 32;
    let x1 = min(x0 + 32, r.z);
    let inv = 1.0 / (f32(2 * params.radius + 1) * fixed_scale());
    var sum = 0u;
    for (var dx = -params.radius; dx <= params.radius; dx++) {
        sum += at_x(r, x0 + dx, y);
    }
    for (var x = x0; x < x1; x++) {
        output[u32(y) * params.width + u32(x)] = f32(sum) * inv;
        sum = sum + at_x(r, x + params.radius + 1, y) - at_x(r, x - params.radius, y);
    }
}
