//#include scope_lut_common

@group(0) @binding(0) var<storage, read> src: array<u32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.out_w || gid.y >= p.out_h) { return; }
    let sx = min(gid.x * p.step, p.width - 1u);
    let sy = min(gid.y * p.step, p.height - 1u);
    let v = src[sy * p.width + sx];
    let e = vec3<f32>(f32(v & 0xffu), f32((v >> 8u) & 0xffu), f32((v >> 16u) & 0xffu)) / 255.0;
    dst[gid.y * p.out_w + gid.x] = pack(lookup(e));
}
