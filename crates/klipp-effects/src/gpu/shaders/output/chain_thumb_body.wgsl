
struct Params {
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    offset: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn enc(c: f32) -> u32 {
    let v = clamp(c, 0.0, 1.0);
    let s = select(1.055 * pow(v, 1.0 / 2.4) - 0.055, v * 12.92, v <= 0.0031308);
    return u32(s * 255.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if i >= params.dst_w * params.dst_h { return; }
    let x = i % params.dst_w;
    let y = i / params.dst_w;
    let sy = min(y * params.src_h / params.dst_h, params.src_h - 1u);
    let sx = min(x * params.src_w / params.dst_w, params.src_w - 1u);
    let p = unpack(input[sy * params.src_w + sx]);
    let a = u32(clamp(p.w, 0.0, 1.0) * 255.0);
    output[params.offset + i] = enc(p.x) | (enc(p.y) << 8u) | (enc(p.z) << 16u) | (a << 24u);
}
