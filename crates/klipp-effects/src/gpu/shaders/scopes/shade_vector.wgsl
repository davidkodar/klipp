
struct VecShadeParams {
    out_w: u32, out_h: u32, n: u32, colorize: u32,
    zoom: f32, gain: f32, norm: f32, floor: f32,
    luma: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: VecShadeParams;

// The colour of a chroma direction: the hue at (cb, cr) at full
// vividness, so the trace is tinted the way the picture is, where it is.
fn tint_of(cb: f32, cr: f32) -> vec3<f32> {
    let mag = max(length(vec2<f32>(cb, cr)), 1e-4);
    let ncb = cb / mag * 0.35; let ncr = cr / mag * 0.35;
    let kr = p.luma.x; let kg = p.luma.y; let kb = p.luma.z;
    let r = 0.5 + ncr * 2.0 * (1.0 - kr);
    let b = 0.5 + ncb * 2.0 * (1.0 - kb);
    let g = (0.5 - kr * r - kb * b) / kg;
    var c = clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
    c = c / max(max(c.r, c.g), max(c.b, 1e-3));
    return mix(c, vec3<f32>(1.0), 0.12);
}
fn glow_of(n: f32) -> f32 {
    return clamp(log2(1.0 + n * p.gain) / 8.0, 0.0, 1.0);
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;
    let n = f32(p.n);
    // The patch of the plane this pixel shows, in cells.
    let bx0 = ((f32(ox) / f32(p.out_w) - 0.5) / p.zoom + 0.5) * n;
    let bx1 = ((f32(ox + 1u) / f32(p.out_w) - 0.5) / p.zoom + 0.5) * n;
    let by0 = ((f32(oy) / f32(p.out_h) - 0.5) / p.zoom + 0.5) * n;
    let by1 = ((f32(oy + 1u) / f32(p.out_h) - 0.5) / p.zoom + 0.5) * n;
    if (bx1 <= 0.0 || bx0 >= n || by1 <= 0.0 || by0 >= n) {
        out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        return;
    }
    // The patch, with a tent of eight cells around it on every side. An
    // 8-bit 4:2:0 picture's chroma is a lattice up to nine cells apart,
    // and the tent joins the lattice into the cloud the picture is;
    // eight cells is 0.4 % of range.
    let tent = 8.0;
    let area = (bx1 - bx0 + tent) * (by1 - by0 + tent);
    var sum = 0.0;
    let ix0 = u32(max(floor(bx0 - tent), 0.0)); let ix1 = min(u32(ceil(bx1 + tent)), p.n);
    let iy0 = u32(max(floor(by0 - tent), 0.0)); let iy1 = min(u32(ceil(by1 + tent)), p.n);
    for (var iy = iy0; iy < iy1; iy = iy + 1u) {
        let cy = f32(iy) + 0.5;
        let dy = max(max(by0 - cy, cy - by1), 0.0);
        let wy = max(1.0 - dy / tent, 0.0);
        for (var ix = ix0; ix < ix1; ix = ix + 1u) {
            let cx = f32(ix) + 0.5;
            let dx = max(max(bx0 - cx, cx - bx1), 0.0);
            let wx = max(1.0 - dx / tent, 0.0);
            sum = sum + f32(bins[iy * p.n + ix]) * wx * wy;
        }
    }
    if (sum <= 0.0) {
        out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        return;
    }
    let dens = sum / 256.0 * p.norm / area;
    let i = max(glow_of(dens), p.floor);
    var t = vec3<f32>(0.80, 0.95, 0.85);
    if (p.colorize != 0u) {
        let cb = ((bx0 + bx1) * 0.5 / n) - 0.5;
        let cr = 0.5 - ((by0 + by1) * 0.5 / n);
        t = tint_of(cb, cr);
    }
    let rgb = mix(t, vec3<f32>(1.0), i * i * 0.5) * i;
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
