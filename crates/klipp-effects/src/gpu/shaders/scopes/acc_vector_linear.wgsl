//#include scope_common_acc
//#include scope_common_vector

@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    let c = src[sy * p.width + sx].rgb;
    let m = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    bin_vector(vec3<f32>(encode(m.r), encode(m.g), encode(m.b)));
}
