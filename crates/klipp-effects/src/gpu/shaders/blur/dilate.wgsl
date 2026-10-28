//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(params.radius);
    // From the centre, not from black: a channel darker than 0 (a colour
    // outside the working gamut) is a legal value to spread.
    var max_px = unpack(input[u32(y) * w + u32(x)]);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            max_px = max(max_px, unpack(input[sy * w + sx]));
        }
    }
    output[idx] = pack(max_px);
}
