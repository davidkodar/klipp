
// Must match `klipp_color::hdr::gpu_layout`.
const H_TO_2020: u32 = 1u;
const H_LUMA: u32 = 10u;
const H_PQ_TABLE: u32 = 13u;

fn h_to_bt2020(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        tables[H_TO_2020 + 0u] * v.x + tables[H_TO_2020 + 1u] * v.y + tables[H_TO_2020 + 2u] * v.z,
        tables[H_TO_2020 + 3u] * v.x + tables[H_TO_2020 + 4u] * v.y + tables[H_TO_2020 + 5u] * v.z,
        tables[H_TO_2020 + 6u] * v.x + tables[H_TO_2020 + 7u] * v.y + tables[H_TO_2020 + 8u] * v.z,
    );
}

/// `display_light_to_nits`. Over the peak the pixel scales down as a
/// whole, so a highlight too bright for the master keeps its hue instead
/// of turning towards white channel by channel.
fn h_display_light_to_nits(rgb2020: vec3<f32>, peak: f32) -> vec3<f32> {
    let n = max(rgb2020 * H_REF_WHITE_NITS, vec3<f32>(0.0));
    let m = max(n.x, max(n.y, n.z));
    if m > peak {
        return n * (peak / m);
    }
    return n;
}

/// `pq_encode_nits`, off the shipped table with the same indexing: the
/// curve sampled at the eighth root of luminance, where it is nearly
/// straight, and interpolated.
fn h_pq_encode_nits(nits: f32) -> f32 {
    // The header holds the entry count; the CPU's `PQ_TABLE_SIZE` is one
    // less, because the table carries both ends.
    let n = u32(tables[0]) - 1u;
    let x = clamp(nits / 10000.0, 0.0, 1.0);
    // Three square roots rather than `pow(x, 0.125)`, to land on the
    // same bits the twin's `.sqrt().sqrt().sqrt()` does.
    let t = sqrt(sqrt(sqrt(x))) * f32(n);
    let i = min(u32(t), n - 1u);
    let f = t - f32(i);
    let a = tables[H_PQ_TABLE + i];
    let b = tables[H_PQ_TABLE + i + 1u];
    return a + (b - a) * f;
}

/// `hlg_oetf`: scene light [0,1] → signal [0,1].
fn h_hlg_oetf(e: f32) -> f32 {
    let a = 0.17883277; let b = 0.28466892; let c = 0.55991073;
    let x = clamp(e, 0.0, 1.0);
    if x <= 1.0 / 12.0 {
        return sqrt(3.0 * x);
    }
    return a * log(12.0 * x - b) + c;
}

/// `hlg_encode_nits`: the inverse HLG OOTF on the nominal display, then
/// the OETF, so SDR white at 203 nits lands on BT.2408's 75% signal.
fn h_hlg_encode_nits(nits2020: vec3<f32>) -> vec3<f32> {
    let f = clamp(nits2020, vec3<f32>(0.0), vec3<f32>(H_HLG_PEAK_NITS)) / H_HLG_PEAK_NITS;
    let yd = 0.2627 * f.x + 0.6780 * f.y + 0.0593 * f.z;
    if yd <= 0.0 {
        return vec3<f32>(0.0);
    }
    // Fd = Ys^(g-1) * E with Yd = Ys^g, so E = Fd * Yd^((1-g)/g).
    let k = pow(yd, (1.0 - H_HLG_GAMMA) / H_HLG_GAMMA);
    return vec3<f32>(h_hlg_oetf(f.x * k), h_hlg_oetf(f.y * k), h_hlg_oetf(f.z * k));
}

/// An HDR delivery's signal, and the pixel's light in cd/m² for MaxCLL.
/// `op` is 1 for PQ, 2 for HLG.
///
/// The light is 0 for HLG because the twin measures none there: MaxCLL
/// and MaxFALL are HDR10 metadata, and an HLG file carries neither.
fn hdr_encode(px: vec3<f32>, peak: f32, op: u32) -> vec4<f32> {
    let rgb2020 = h_to_bt2020(px);
    if op == 2u {
        let nits = h_display_light_to_nits(rgb2020, H_HLG_PEAK_NITS);
        return vec4<f32>(h_hlg_encode_nits(nits), 0.0);
    }
    let nits = h_display_light_to_nits(rgb2020, peak);
    return vec4<f32>(
        h_pq_encode_nits(nits.x),
        h_pq_encode_nits(nits.y),
        h_pq_encode_nits(nits.z),
        max(nits.x, max(nits.y, nits.z)),
    );
}
