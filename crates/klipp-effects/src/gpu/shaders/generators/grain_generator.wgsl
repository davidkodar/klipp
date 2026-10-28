//#include pixel_storage

struct Params {
    width: u32, height: u32, frame: u32, _pad: u32,
    color: vec4<f32>,
    cell: f32, depth: f32, _p0: f32, _p1: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn rounded(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(pack2x16float(c.xy)), unpack2x16float(pack2x16float(c.zw)));
}
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn grain_at(x: u32, y: u32, frame: u32) -> f32 {
    var h = (x * 0x8da6b343u) ^ (y * 0xd8163841u) ^ (frame * 0xcb1ab31fu);
    h ^= h >> 15u;
    h = h * 0x2c1b3c6du;
    h ^= h >> 12u;
    h = h * 0x297a2d39u;
    h ^= h >> 15u;
    // `u32::MAX as f32`, as the CPU divides by.
    return (f32(h) / 4294967296.0) * 2.0 - 1.0;
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let gx = u32(f32(i % params.width) / params.cell);
    let gy = u32(f32(i / params.width) / params.cell);
    let n = grain_at(gx, gy, params.frame) * params.depth;
    let byte = floor(clamp(params.color.rgb + vec3<f32>(n), vec3<f32>(0.0), vec3<f32>(255.0)));
    let c = rounded(vec4<f32>(byte, params.color.a) / 255.0);
    let rgb = vec3<f32>(linear(c.r), linear(c.g), linear(c.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), c.a));
}
