//#include scope_lut_common

@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.out_w || gid.y >= p.out_h) { return; }
    let sx = min(gid.x * p.step, p.width - 1u);
    let sy = min(gid.y * p.step, p.height - 1u);
    let c = src[sy * p.width + sx].rgb;
    let m = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    let e = vec3<f32>(encode(m.r), encode(m.g), encode(m.b));
    dst[gid.y * p.out_w + gid.x] = pack(lookup(e));
}
