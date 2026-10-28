//#include pixel_storage
//#include generator_grain

struct Params {
    width: u32, height: u32, glow: f32, grain: f32,
    a: vec4<f32>, b: vec4<f32>, dir: vec2<f32>, centre: vec2<f32>,
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
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let wh = vec2<f32>(f32(params.width), f32(params.height));
    let xy = vec2<f32>(f32(i % params.width), f32(i / params.width));
    let span = max(wh - vec2<f32>(1.0), vec2<f32>(0.0));
    let half_extent = dot(abs(params.dir), span * 0.5);
    let t = clamp((dot(xy - span * 0.5, params.dir) + half_extent) / max(2.0 * half_extent, 1.0), 0.0, 1.0);
    var c = rounded((params.a + (params.b - params.a) * t) / 255.0);
    if params.glow > 0.0 {
        let d = xy - params.centre;
        let radius = max(wh.x, wh.y) * 0.45;
        let f = exp(-dot(d, d) / (2.0 * radius * radius)) * params.glow;
        if f >= 0.002 {
            c = rounded(vec4<f32>(min(c.rgb + (vec3<f32>(1.0) - c.rgb) * f * 0.6, vec3<f32>(1.0)), c.a));
        }
    }
    if params.grain > 0.0 {
        let n = grain_noise(i);
        c = rounded(vec4<f32>(grain_channel(c.r, n, params.grain), grain_channel(c.g, n, params.grain), grain_channel(c.b, n, params.grain), c.a));
    }
    let rgb = vec3<f32>(linear(c.r), linear(c.g), linear(c.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), c.a));
}
