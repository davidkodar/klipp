
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
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
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let orig = unpack(input[idx]);

    // 3x3 average of neighbors (excluding center)
    var avg = vec4<f32>(0.0);
    let w_i32 = i32(params.width);
    let h_i32 = i32(params.height);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
            let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
            avg += unpack(input[u32(sy) * params.width + u32(sx)]);
        }
    }
    avg = avg / 9.0;

    // Unsharp mask: original + amount * (original - blurred)
    let sharpened = vec4<f32>(
        orig.x + params.amount * (orig.x - avg.x),
        orig.y + params.amount * (orig.y - avg.y),
        orig.z + params.amount * (orig.z - avg.z),
        orig.w,  // preserve alpha
    );
    output[idx] = pack(sharpened);
}
