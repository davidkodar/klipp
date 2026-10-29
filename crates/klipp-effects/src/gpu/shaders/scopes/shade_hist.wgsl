
struct HistShadeParams {
    out_w: u32, out_h: u32, rows: u32, panels: u32,
    log: u32, _p0: u32, _p1: u32, _p2: u32,
    fill: f32, headroom: f32, _f0: f32, _f1: f32,
    rect0: vec4<u32>, rect1: vec4<u32>, rect2: vec4<u32>,
    chans: vec4<u32>,
};
@group(0) @binding(0) var<storage, read> hist: array<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: HistShadeParams;

// The channel colours. Unlike the waveform's, each primary sums to the
// same 1.28, so where red, green and blue fill the same area they add to
// a neutral grey rather than a tint.
fn tint(ch: u32) -> vec3<f32> {
    switch ch {
        case 0u: { return vec3<f32>(1.0, 0.12, 0.10); }
        case 1u: { return vec3<f32>(0.12, 0.90, 0.18); }
        case 2u: { return vec3<f32>(0.16, 0.26, 1.0); }
        default: { return vec3<f32>(0.86, 0.93, 0.90); }
    }
}
fn rect(i: u32) -> vec4<u32> {
    switch i {
        case 0u: { return p.rect0; }
        case 1u: { return p.rect1; }
        default: { return p.rect2; }
    }
}
// The count every curve of this picture is scaled to: the tallest level
// of the channels drawn, away from the clipped ends.
fn peak() -> f32 {
    let n = p.rows;
    let mask = p.chans[0] | p.chans[1] | p.chans[2];
    var inner = 0.0; var whole = 0.0;
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        if ((mask & (1u << ch)) == 0u) { continue; }
        inner = max(inner, hist[4u * n + ch]);
        whole = max(whole, hist[4u * n + 4u + ch]);
    }
    if (inner > 0.0) { return inner; }
    return max(whole, 1.0);
}
// The curve's height over pixel column `px` of a panel `pw` wide, as a
// fraction of the panel: the tallest level the column covers, so a
// narrow spike keeps its height at any panel width.
fn height(ch: u32, px: i32, pw: u32, pk: f32) -> f32 {
    let x = u32(clamp(px, 0, i32(pw) - 1));
    let l0 = (x * p.rows) / pw;
    let l1 = min(max(((x + 1u) * p.rows) / pw, l0 + 1u), p.rows);
    var m = 0.0;
    for (var l = l0; l < l1; l = l + 1u) {
        m = max(m, hist[ch * p.rows + l]);
    }
    let v = m / pk;
    if (p.log != 0u) {
        // Three decades from the peak down to nothing.
        return min(log2(1.0 + v * 1024.0) / log2(1025.0) * p.headroom, 1.0);
    }
    return min(v * p.headroom, 1.0);
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;
    let pk = peak();
    var rgb = vec3<f32>(0.0);
    for (var i = 0u; i < p.panels; i = i + 1u) {
        let r = rect(i);
        if (ox < r.x || ox >= r.x + r.z || oy < r.y || oy >= r.y + r.w) { continue; }
        let px = i32(ox - r.x);
        let ph = f32(r.w);
        // This pixel's centre, in pixels above the panel's floor.
        let yc = ph - (f32(oy - r.y) + 0.5);
        for (var ch = 0u; ch < 4u; ch = ch + 1u) {
            if ((p.chans[i] & (1u << ch)) == 0u) { continue; }
            let h = height(ch, px, r.z, pk) * ph;
            let hl = height(ch, px - 1, r.z, pk) * ph;
            let hr = height(ch, px + 1, r.z, pk) * ph;
            // The area under the curve, anti-aliased at its top.
            let cover = clamp(h - (yc - 0.5), 0.0, 1.0);
            // The curve: the span it passes through in this column, joined
            // half way to each neighbour, so a steep flank is a continuous
            // line rather than a dot per column. Nothing along an empty floor.
            let lo = min(h, min((h + hl) * 0.5, (h + hr) * 0.5));
            let hi = max(h, max((h + hl) * 0.5, (h + hr) * 0.5));
            var line = 0.0;
            if (hi >= 0.5) {
                let d = max(max(lo - yc, yc - hi), 0.0);
                line = clamp(1.25 - d, 0.0, 1.0);
            }
            // The fill brightens towards the curve, as light collects at a
            // phosphor edge; at its fullest it stays dim enough that the
            // curves drawn over it read.
            let grad = 0.55 + 0.45 * clamp(yc / max(h, 1.0), 0.0, 1.0);
            let t = tint(ch);
            rgb = rgb + t * (cover * p.fill * 0.6 * grad) + mix(t, vec3<f32>(1.0), 0.35) * line;
        }
    }
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
