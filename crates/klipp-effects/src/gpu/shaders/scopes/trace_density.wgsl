
struct ShadeParams {
    out_w: u32, out_h: u32, cols: u32, rows: u32,
    mode: u32, channels: u32, envelope: u32, gap: u32,
    gain: f32, norm: f32, _f0: f32, _f1: f32,
};
@group(0) @binding(0) var<storage, read> sat: array<u32>;
@group(0) @binding(1) var<storage, read> ext: array<u32>;
@group(0) @binding(2) var<storage, read_write> dens: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> p: ShadeParams;

// The summed-area table at the corner below column `c` and level `l`: the
// bins of columns [0, c) and levels [0, l) of the channel at `base`.
fn corner(base: u32, c: i32, l: i32) -> u32 {
    if (c <= 0 || l <= 0) { return 0u; }
    return sat[base + u32(c - 1) * p.rows + u32(l - 1)];
}
// The bins of columns [ca, cb) and levels [la, lb), clipped to the grid,
// and how many cells that is.
fn box_sum(base: u32, ca: i32, cb: i32, la: i32, lb: i32) -> vec2<f32> {
    let ka = clamp(ca, 0, i32(p.cols)); let kb = clamp(cb, 0, i32(p.cols));
    let la2 = clamp(la, 0, i32(p.rows)); let lb2 = clamp(lb, 0, i32(p.rows));
    if (kb <= ka || lb2 <= la2) { return vec2<f32>(0.0, 0.0); }
    let s = corner(base, kb, lb2) - corner(base, ka, lb2) - corner(base, kb, la2) + corner(base, ka, la2);
    return vec2<f32>(f32(s), f32((kb - ka) * (lb2 - la2)));
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;
    var d = vec4<f32>(0.0);

    var ch_first = 0u; var ch_last = 2u;
    var panel_x0 = 0u; var panel_w = p.out_w;
    if (p.mode == 0u) {
        ch_first = 3u; ch_last = 3u;
    } else if (p.mode == 2u) {
        let pw = (p.out_w - 2u * p.gap) / 3u;
        var panel = 0u;
        if (ox >= pw + p.gap) { panel = 1u; }
        if (ox >= 2u * (pw + p.gap)) { panel = 2u; }
        panel_x0 = panel * (pw + p.gap);
        panel_w = pw;
        if (ox >= panel_x0 + pw) { dens[oi] = d; return; }
        ch_first = panel; ch_last = panel;
    }
    let px = ox - panel_x0;
    // The bins this pixel covers: columns by position, levels by value,
    // the top row being level 1.0.
    let c0 = (px * p.cols) / panel_w;
    let c1 = max(((px + 1u) * p.cols) / panel_w, c0 + 1u);
    let v_lo = 1.0 - f32(oy + 1u) / f32(p.out_h);
    let v_hi = 1.0 - f32(oy) / f32(p.out_h);
    let r0 = u32(floor(v_lo * f32(p.rows)));
    let r1 = min(max(u32(ceil(v_hi * f32(p.rows))), r0 + 1u), p.rows);
    let h2 = i32(((r1 - r0 + 3u) / 4u) * 2u);
    let rc = i32((r0 + r1) / 2u);
    for (var ch = ch_first; ch <= ch_last; ch = ch + 1u) {
        if (ch < 3u && (p.channels & (1u << ch)) == 0u) { continue; }
        let b = box_sum(ch * p.cols * p.rows, i32(c0), i32(c1), rc - h2, rc + h2);
        if (b.y > 0.0) { d[ch] = b.x / 256.0 * p.norm / b.y; }
    }
    dens[oi] = d;
}
