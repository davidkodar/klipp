
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
    var lo = 0; var hi = i32(p.out_w);
    if (p.mode == 2u) {
        let pw = (p.out_w - 2u * p.gap) / 3u;
        var panel = 0u;
        if (x >= pw + p.gap) { panel = 1u; }
        if (x >= 2u * (pw + p.gap)) { panel = 2u; }
        let x0 = panel * (pw + p.gap);
        if (x >= x0 + pw) { dst[oi] = vec4<f32>(0.0); return; }
        lo = i32(x0); hi = i32(x0 + pw);
    }
    var acc = vec4<f32>(0.0); var wsum = 0.0;
    let r = i32(p.radius);
    for (var d = -r; d <= r; d = d + 1) {
        let sx = i32(x) + d;
        if (sx < lo || sx >= hi) { continue; }
        let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
        acc = acc + src[y * p.out_w + u32(sx)] * w;
        wsum = wsum + w;
    }
    dst[oi] = acc / wsum;
}
