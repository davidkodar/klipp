//#include scope_lp_params

@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let oy = gid.y;
    if (x >= p.out_w || oy >= p.out_h) { return; }
    let cy = i32(oy * p.r + (p.r - 1u) / 2u);
    let last = i32(p.height) - 1;
    let reach = i32(p.radius);
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var d = -reach; d <= reach; d = d + 1) {
        let w = tap_weight(d);
        sum = sum + src[u32(clamp(cy + d, 0, last)) * p.out_w + x].rgb * w;
        wsum = wsum + w;
    }
    dst[oy * p.out_w + x] = vec4<f32>(sum / wsum, 0.0);
}
