
struct LutParams {
    width: u32, height: u32, step: u32, out_w: u32,
    out_h: u32, size: u32, transfer: u32, scale: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
};
@group(0) @binding(1) var<storage, read> lut: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;
@group(0) @binding(3) var<uniform> p: LutParams;

fn srgb_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}
fn encode(v: f32) -> f32 {
    switch p.transfer {
        case 1u: { return pow(clamp(v, 0.0, 1.0), 1.0 / 2.4); }
        default: { return srgb_encode(v); }
    }
}
fn at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let i = ((b * p.size + g) * p.size + r) * 3u;
    return vec3<f32>(lut[i], lut[i + 1u], lut[i + 2u]);
}
fn lookup(rgb: vec3<f32>) -> vec3<f32> {
    let n1 = f32(p.size - 1u);
    let q = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * n1;
    let i0 = min(vec3<u32>(floor(q)), vec3<u32>(p.size - 2u));
    let f = q - vec3<f32>(i0);
    let i1 = i0 + vec3<u32>(1u);
    let c00 = mix(at(i0.x, i0.y, i0.z), at(i1.x, i0.y, i0.z), f.x);
    let c10 = mix(at(i0.x, i1.y, i0.z), at(i1.x, i1.y, i0.z), f.x);
    let c01 = mix(at(i0.x, i0.y, i1.z), at(i1.x, i0.y, i1.z), f.x);
    let c11 = mix(at(i0.x, i1.y, i1.z), at(i1.x, i1.y, i1.z), f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}
fn pack(c: vec3<f32>) -> u32 {
    let q = vec3<u32>(round(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0));
    return q.x | (q.y << 8u) | (q.z << 16u) | (255u << 24u);
}
