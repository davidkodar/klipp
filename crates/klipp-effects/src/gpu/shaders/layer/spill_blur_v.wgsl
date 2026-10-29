//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(0) var<storage, read> input: array<f32>; // not-pixels: a cast's alpha
@group(0) @binding(2) var<storage, read_write> output: array<f32>; // not-pixels: a cast's alpha
fn at_y(r: vec4<i32>, x: i32, y: i32) -> u32 {
    let cy = clamp(y, 0, i32(params.height) - 1);
    if cy < r.y || cy >= r.w { return 0u; }
    return u32(round(clamp(input[u32(cy) * params.width + u32(x)], 0.0, 1.0) * fixed_scale()));
}
@compute @workgroup_size(64)
fn spill_blur_v(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 64u, 0u, gid_flat.z);
    let r = rect();
    let cols = r.z - r.x;
    if cols <= 0 { return; }
    // Neighbouring threads take neighbouring columns, so they read
    // neighbouring pixels.
    let x = r.x + i32(gid.x) % cols;
    let y0 = r.y + (i32(gid.x) / cols) * 32;
    if y0 >= r.w { return; }
    let y1 = min(y0 + 32, r.w);
    let inv = 1.0 / (f32(2 * params.radius + 1) * fixed_scale());
    var sum = 0u;
    for (var dy = -params.radius; dy <= params.radius; dy++) {
        sum += at_y(r, x, y0 + dy);
    }
    for (var y = y0; y < y1; y++) {
        output[u32(y) * params.width + u32(x)] = f32(sum) * inv;
        sum = sum + at_y(r, x, y + params.radius + 1) - at_y(r, x, y - params.radius);
    }
}
