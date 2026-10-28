
// Must match `klipp_color::gamut::gpu_layout`.
const G_MATRICES: u32 = 2u;
const G_WHITE: u32 = 29u;
const G_TARGET: u32 = 32u;

const KNEE: f32 = 0.8;
const CHROMA_FLOOR: f32 = 0.5;
const MAX_LIGHTNESS_TRADED: f32 = 5.0;

fn g_l_steps() -> u32 { return u32(tables[0]); }
fn g_h_steps() -> u32 { return u32(tables[1]); }
fn g_reach_len() -> u32 { return g_l_steps() * g_h_steps() + g_l_steps(); }
fn g_source() -> u32 { return G_TARGET + g_reach_len(); }
fn g_cusps() -> u32 { return g_source() + g_reach_len(); }

fn g_mat(m: u32, v: vec3<f32>) -> vec3<f32> {
    let o = G_MATRICES + m * 9u;
    return vec3<f32>(
        tables[o + 0u] * v.x + tables[o + 1u] * v.y + tables[o + 2u] * v.z,
        tables[o + 3u] * v.x + tables[o + 4u] * v.y + tables[o + 5u] * v.z,
        tables[o + 6u] * v.x + tables[o + 7u] * v.y + tables[o + 8u] * v.z,
    );
}

fn g_white() -> vec3<f32> {
    return vec3<f32>(tables[G_WHITE], tables[G_WHITE + 1u], tables[G_WHITE + 2u]);
}

fn lab_f(t: f32) -> f32 {
    if t > 0.008856 { return pow(t, 1.0 / 3.0); }
    return 7.787 * t + 16.0 / 116.0;
}

fn lab_g(t: f32) -> f32 {
    let cube = t * t * t;
    if cube > 0.008856 { return cube; }
    return (t - 16.0 / 116.0) / 7.787;
}

fn xyz_to_lab(xyz: vec3<f32>) -> vec3<f32> {
    let w = g_white();
    let fx = lab_f(xyz.x / w.x);
    let fy = lab_f(xyz.y / w.y);
    let fz = lab_f(xyz.z / w.z);
    return vec3<f32>(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
}

fn lab_to_xyz(lab: vec3<f32>) -> vec3<f32> {
    let fy = (lab.x + 16.0) / 116.0;
    let fx = fy + lab.y / 500.0;
    let fz = fy - lab.z / 200.0;
    let w = g_white();
    return vec3<f32>(lab_g(fx) * w.x, lab_g(fy) * w.y, lab_g(fz) * w.z);
}

fn hue_cell(h: f32) -> vec3<f32> {
    let n = f32(g_h_steps());
    let hf = (h - floor(h / 360.0) * 360.0) / 360.0 * n;
    let h0 = floor(hf);
    return vec3<f32>(h0 - floor(h0 / n) * n, (h0 + 1.0) - floor((h0 + 1.0) / n) * n, hf - h0);
}

fn lightness_cell(l: f32) -> vec2<f32> {
    let lf = (clamp(l, 0.0, 100.0) / 100.0) * f32(g_l_steps() - 1u);
    let l0 = min(floor(lf), f32(g_l_steps() - 2u));
    return vec2<f32>(l0, lf - l0);
}

fn reach_at(base: u32, l: f32, h: f32) -> f32 {
    let lc = lightness_cell(l);
    let hc = hue_cell(h);
    let hs = g_h_steps();
    let r0 = base + u32(lc.x) * hs;
    let r1 = base + (u32(lc.x) + 1u) * hs;
    let a = mix(tables[r0 + u32(hc.x)], tables[r0 + u32(hc.y)], hc.z);
    let b = mix(tables[r1 + u32(hc.x)], tables[r1 + u32(hc.y)], hc.z);
    return mix(a, b, lc.y);
}

fn reach_least_at(base: u32, l: f32) -> f32 {
    let mins = base + g_l_steps() * g_h_steps();
    let lc = lightness_cell(l);
    return min(tables[mins + u32(lc.x)], tables[mins + u32(lc.x) + 1u]);
}

fn cusp_at(h: f32) -> vec2<f32> {
    let hc = hue_cell(h);
    let base = g_cusps();
    let a = vec2<f32>(tables[base + u32(hc.x) * 2u], tables[base + u32(hc.x) * 2u + 1u]);
    let b = vec2<f32>(tables[base + u32(hc.y) * 2u], tables[base + u32(hc.y) * 2u + 1u]);
    return mix(a, b, hc.z);
}

fn compress_to_bt709(rgb_in: vec3<f32>) -> vec3<f32> {
    let scale = max(max(max(rgb_in.x, rgb_in.y), rgb_in.z), 1.0);
    var n = rgb_in;
    if scale > 1.0 { n = rgb_in / scale; }
    let lab = xyz_to_lab(g_mat(0u, n));
    let l = lab.x;
    let a = lab.y;
    let b = lab.z;
    let c = sqrt(a * a + b * b);
    if c <= CHROMA_FLOOR { return rgb_in; }
    if c <= reach_least_at(G_TARGET, l) * KNEE { return rgb_in; }
    let h = degrees(atan2(b, a));
    let held = reach_at(G_TARGET, l, h);
    if c <= held * KNEE { return rgb_in; }
    let cusp = cusp_at(h);
    var lightness = l;
    if c > held && l > cusp.x {
        let want = min(c, cusp.y);
        let fl = max(cusp.x, l - MAX_LIGHTNESS_TRADED);
        var lo = fl;
        var hi = l;
        for (var i = 0; i < 8; i = i + 1) {
            let mid = 0.5 * (lo + hi);
            if reach_at(G_TARGET, mid, h) >= want { lo = mid; } else { hi = mid; }
        }
        lightness = lo;
    }
    let t = reach_at(G_TARGET, lightness, h);
    let knee = t * KNEE;
    let sm = reach_at(g_source(), lightness, h);
    if sm <= t * (1.0 + 1e-4) && lightness == l { return rgb_in; }
    var mapped_c: f32;
    if sm > knee {
        mapped_c = min(knee + (c - knee) * ((t - knee) / (sm - knee)), t);
    } else {
        mapped_c = min(c, t);
    }
    let k = max(mapped_c, 0.0) / c;
    let mapped = g_mat(1u, lab_to_xyz(vec3<f32>(lightness, a * k, b * k)));
    return mapped * scale;
}

fn bt2020_to_bt709_display(rgb: vec3<f32>) -> vec3<f32> {
    return max(compress_to_bt709(g_mat(2u, rgb)), vec3<f32>(0.0));
}
