
struct ShadeParams {
    out_w: u32, out_h: u32, cols: u32, rows: u32,
    mode: u32, channels: u32, envelope: u32, gap: u32,
    gain: f32, norm: f32, _f0: f32, _f1: f32,
};
@group(0) @binding(0) var<storage, read> dens: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> ext: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> p: ShadeParams;

// The trace colours. Red, green and blue are kept a little off pure so a
// lone blue trace reads on black and overlaps add towards white.
fn tint(ch: u32) -> vec3<f32> {
    switch ch {
        case 0u: { return vec3<f32>(1.0, 0.16, 0.12); }
        case 1u: { return vec3<f32>(0.18, 1.0, 0.22); }
        case 2u: { return vec3<f32>(0.26, 0.44, 1.0); }
        default: { return vec3<f32>(0.86, 0.93, 0.90); }
    }
}
// Density to brightness, saturating: `1 - e^(-density * gain)`. A lone
// sample stays a dim dot and the gaps between traces stay dark, while a
// dense trace goes bright without clipping into a blob. The picture's
// structure stands out against black. A logarithmic curve lifted every
// stray sample to a visible grey, which in a large scope washed the whole
// trace out.
fn glow_of(n: f32) -> f32 {
    return 1.0 - exp(-n * p.gain);
}
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x; let oy = gid.y;
    if (ox >= p.out_w || oy >= p.out_h) { return; }
    let oi = oy * p.out_w + ox;

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
        if (ox >= panel_x0 + pw) { out[oi] = vec4<f32>(0.0, 0.0, 0.0, 1.0); return; }
        ch_first = panel; ch_last = panel;
    }
    let px = ox - panel_x0;
    let c0 = (px * p.cols) / panel_w;
    let c1 = max(((px + 1u) * p.cols) / panel_w, c0 + 1u);
    let v_lo = 1.0 - f32(oy + 1u) / f32(p.out_h);
    let v_hi = 1.0 - f32(oy) / f32(p.out_h);
    let r0 = u32(floor(v_lo * f32(p.rows)));
    let r1 = min(max(u32(ceil(v_hi * f32(p.rows))), r0 + 1u), p.rows);

    var rgb = vec3<f32>(0.0);
    for (var ch = ch_first; ch <= ch_last; ch = ch + 1u) {
        if (ch < 3u && (p.channels & (1u << ch)) == 0u) { continue; }
        var edge = false;
        if (p.envelope != 0u) {
            for (var c = c0; c < c1; c = c + 1u) {
                let lo = ext[(ch * p.cols + c) * 2u];
                let hi = ext[(ch * p.cols + c) * 2u + 1u];
                if ((lo >= r0 && lo < r1) || (hi >= r0 && hi < r1)) { edge = true; }
            }
        }
        let n = dens[oi][ch];
        if (n <= 0.0 && !edge) { continue; }
        var i = glow_of(n);
        if (edge) { i = max(i, 0.9); }
        // The core of a dense trace goes to white, as a phosphor does.
        let t = tint(ch);
        rgb = rgb + mix(t, vec3<f32>(1.0), i * i * 0.6) * i;
    }
    out[oi] = vec4<f32>(min(rgb, vec3<f32>(1.0)), 1.0);
}
