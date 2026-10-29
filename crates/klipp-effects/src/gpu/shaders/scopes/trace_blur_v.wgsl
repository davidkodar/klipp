
struct BlurParams { out_w: u32, out_h: u32, mode: u32, gap: u32, radius: u32, _p0: u32, sigma: f32, _p1: f32 };
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: BlurParams;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let y = gid.y;
    if (x >= p.out_w || y >= p.out_h) { return; }
    let oi = y * p.out_w + x;
    if (p.radius == 0u) { dst[oi] = src[oi]; return; }
    var acc = vec4<f32>(0.0); var wsum = 0.0;
    let r = i32(p.radius);
    for (var d = -r; d <= r; d = d + 1) {
        let sy = i32(y) + d;
        if (sy < 0 || sy >= i32(p.out_h)) { continue; }
        let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
        acc = acc + src[u32(sy) * p.out_w + x] * w;
        wsum = wsum + w;
    }
    dst[oi] = acc / wsum;
}
