
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    samples: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
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
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    var sum = vec4<f32>(0.0);
    var pre = vec3<f32>(0.0);
    let w = i32(params.width);
    let h = i32(params.height);
    for (var i = 0u; i < params.samples; i = i + 1u) {
        let sx = clamp(i32(round(x + params.dx * f32(i))), 0, w - 1);
        let sy = clamp(i32(round(y + params.dy * f32(i))), 0, h - 1);
        let si = u32(sy) * params.width + u32(sx);
        let q = unpack(input[si]);
        sum += q;
        pre += q.xyz * q.w;
    }
    let inv = 1.0 / f32(params.samples);
    if sum.w == f32(params.samples) {
        output[idx] = pack(sum * inv);
        return;
    }
    var colour = vec3<f32>(0.0);
    if sum.w > 