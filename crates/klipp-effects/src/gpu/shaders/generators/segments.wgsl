//#include pixel_storage

struct Params {
    width: u32, height: u32, count: u32, _pad: u32,
    background: vec4<f32>, color: vec4<f32>,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> rects: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn rounded(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(pack2x16float(c.xy)), unpack2x16float(pack2x16float(c.zw)));
}
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let x = i % params.width;
    let y = i / params.width;
    var c = params.background;
    for (var k = 0u; k < params.count; k++) {
        let r = rects[k];
        if x >= r.x && x < r.z && y >= r.y && y < r.w { c = params.color; break; }
    }
    let s = rounded(c / 255.0);
    let rgb = vec3<f32>(linear(s.r), linear(s.g), linear(s.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), s.a));
}
