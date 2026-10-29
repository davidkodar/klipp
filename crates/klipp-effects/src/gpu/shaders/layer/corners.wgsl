//#include pixel_storage

struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn corners(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let box = second[0];
    let short = min(box.z - box.x, box.w - box.y);
    let r = params.radius * short;
    var pixel = unpack(input[idx]);
    if box.z > box.x && r > 0.5 {
        let pos = vec2<f32>(f32(idx % params.width), f32(idx / params.width)) + vec2<f32>(0.5);
        let dxy = max(max(box.xy + vec2<f32>(r) - pos, pos - (box.zw - vec2<f32>(r))), vec2<f32>(0.0));
        let d = length(dxy) - r;
        pixel.a *= clamp(0.5 - d / max(params.feather * short, 1.0), 0.0, 1.0);
    }
    output[idx] = pack(pixel);
}
