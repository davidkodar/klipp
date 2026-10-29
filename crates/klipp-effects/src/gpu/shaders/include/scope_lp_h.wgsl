
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let y = gid.y;
    if (ox >= p.out_w || y >= p.height) { return; }
    let cx = i32(ox * p.r + (p.r - 1u) / 2u);
    let last = i32(p.width) - 1;
    let reach = i32(p.radius);
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var d = -reach; d <= reach; d = d + 1) {
        let w = tap_weight(d);
        sum = sum + tap(u32(clamp(cx + d, 0, last)), y) * w;
        wsum = wsum + w;
    }
    dst[y * p.out_w + ox] = vec4<f32>(sum / wsum, 0.0);
}
