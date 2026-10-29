
struct GlowParams { out_w: u32, out_h: u32, radius: u32, glow: u32, sigma: f32, strength: f32, _p0: f32, _p1: f32 };
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> tmp: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: GlowParams;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let y = gid.y;
    if (x >= p.out_w || y >= p.out_h) { return; }
    if (p.glow == 0u) { tmp[y * p.out_w + x] = vec4<f32>(0.0); return; }
    var acc = vec3<f32>(0.0); var wsum = 0.0;
    let r = i32(p.radius);
    for (var d = -r; d <= r; d = d + 1) {
        let sx = i32(x) + d;
        if (sx < 0 || sx >= i32(p.out_w)) { continue; }
        let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
        acc = acc + src[y * p.out_w + u32(sx)].rgb * w;
        wsum = wsum + w;
    }
    tmp[y * p.out_w + x] = vec4<f32>(acc / wsum, 1.0);
}
