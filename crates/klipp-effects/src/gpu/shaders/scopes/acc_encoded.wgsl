//#include scope_common_acc

@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = gid.x * p.step; let sy = gid.y * p.step;
    if (sx >= p.width || sy >= p.height) { return; }
    bin_sample(sx, src[sy * p.width + sx].rgb);
}
