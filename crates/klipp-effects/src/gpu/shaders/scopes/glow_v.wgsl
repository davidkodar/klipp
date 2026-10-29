
struct GlowParams { out_w: u32, out_h: u32, radius: u32, glow: u32, sigma: f32, strength: f32, _p0: f32, _p1: f32 };
@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> tmp: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> packed: array<u32>;
@group(0) @binding(3) var<uniform> p: GlowParams;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x; let y = gid.y;
    if (x >= p.out_w || y >= p.out_h) { return; }
    var rgb = src[y * p.out_w + x].rgb;
    if (p.glow != 0u) {
        var acc = vec3<f32>(0.0); var wsum = 0.0;
        let r = i32(p.radius);
        for (var d = -r; d <= r; d = d + 1) {
            let sy = i32(y) + d;
            if (sy < 0 || sy >= i32(p.out_h)) { continue; }
            let w = exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
            acc = acc + tmp[u32(sy) * p.out_w + x].rgb * w;
            wsum = wsum + w;
        }
        rgb = rgb + acc / wsum * p.strength;
    }
    let c = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0 + 0.5;
    packed[y * p.out_w + x] = u32(c.r) | (u32(c.g) << 8u) | (u32(c.b) << 16u) | (255u << 24u);
}
