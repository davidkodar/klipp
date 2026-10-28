
struct Params {
    width: u32,
    height: u32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    wb0: f32, wb1: f32, wb2: f32,
    wb3: f32, wb4: f32, wb5: f32,
    wb6: f32, wb7: f32, wb8: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    lift_r: f32,
    lift_g: f32,
    lift_b: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
    offset_r: f32,
    offset_g: f32,
    offset_b: f32,
    hue: f32,
    global_mix: f32,
    contrast_pivot_encoded: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    encoding: u32,
    log_mode: u32,
    log_low_r: f32, log_low_g: f32, log_low_b: f32,
    log_mid_r: f32, log_mid_g: f32, log_mid_b: f32,
    log_high_r: f32, log_high_g: f32, log_high_b: f32,
    log_low_range: f32,
    log_high_range: f32,
    luma_mix: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// The grade model, transcribed from klipp_color::grade_model. Change
// one, change the other; the parity tests hold them equal.
const GRADE_GAMMA: f32 = 2.4;
const MID_GREY_ENCODED: f32 = 0.4894371;
fn grade_encode(x: f32) -> f32 {
    let e = pow(abs(x), 1.0 / GRADE_GAMMA);
    return select(e, -e, x < 0.0);
}
fn grade_decode(e: f32) -> f32 {
    let x = pow(abs(e), GRADE_GAMMA);
    return select(x, -x, e < 0.0);
}
// The grade encoding chosen (#467): 0 BT.1886, 1 ACEScct, 2 ACEScc.
// klipp_grade_model::{encode, decode, band_luma}, transcribed.
fn grade_encode_in(x: f32) -> f32 {
    if params.encoding == 1u {
        if x <= 0.0078125 { return 10.5402377 * x + 0.072905534; }
        return (log2(min(x, 65504.0)) + 9.72) / 17.52;
    }
    if params.encoding == 2u {
        if x <= 0.0 { return (-16.0 + 9.72) / 17.52; }
        if x < exp2(-15.0) { return (log2(exp2(-16.0) + x * 0.5) + 9.72) / 17.52; }
        return (log2(min(x, 65504.0)) + 9.72) / 17.52;
    }
    return grade_encode(x);
}
fn grade_decode_in(e: f32) -> f32 {
    let top = (log2(65504.0) + 9.72) / 17.52;
    if params.encoding == 1u {
        if e <= 0.15525114 { return (e - 0.072905534) / 10.5402377; }
        if e < top { return exp2(e * 17.52 - 9.72); }
        return 65504.0;
    }
    if params.encoding == 2u {
        if e < (9.72 - 15.0) / 17.52 { return (exp2(e * 17.52 - 9.72) - exp2(-16.0)) * 2.0; }
        if e < top { return exp2(e * 17.52 - 9.72); }
        return 65504.0;
    }
    return grade_decode(e);
}
fn band_luma(l: f32) -> f32 {
    if params.encoding == 0u { return l; }
    return grade_encode(grade_decode_in(l));
}
fn cdl_channel(e: f32, lift: f32, gamma: f32, gain: f32, offset: f32) -> f32 {
    let slope = (1.0 + gain) * (1.0 - lift);
    let t = e * slope + lift + offset;
    let power = 1.0 / max(1.0 + gamma, 0.05);
    return select(t, pow(t, power), t > 0.0);
}
fn contrast_channel_at(e: f32, amount: f32, pivot: f32) -> f32 {
    return (e - pivot) * (1.0 + amount) + pivot;
}
fn grade_smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
// (highlights, shadows, whites, blacks): klipp_grade_model::tone_weights.
fn tone_weights(l: f32) -> vec4<f32> {
    return vec4<f32>(
        0.22 * grade_smoothstep(MID_GREY_ENCODED, 0.85, l),
        0.13 * grade_smoothstep(0.0, 0.22, l) * (1.0 - grade_smoothstep(0.22, MID_GREY_ENCODED, l)),
        grade_smoothstep(0.65, 0.95, l),
        1.0 - grade_smoothstep(0.10, 0.40, l),
    );
}
fn grade_gauss(l: f32, centre: f32, sigma: f32) -> f32 {
    return exp(-((l - centre) * (l - centre)) / (2.0 * sigma * sigma));
}
// (low, mid, high), the low and high bands where the ranges put them
// (#900): klipp_grade_model::log_wheel_weights_in.
fn log_wheel_weights(l: f32) -> vec3<f32> {
    let low = clamp(params.log_low_range, 0.0, MID_GREY_ENCODED - 0.08);
    let high = clamp(params.log_high_range, MID_GREY_ENCODED + 0.08, 1.2);
    return vec3<f32>(grade_gauss(l, low, 0.14), grade_gauss(l, MID_GREY_ENCODED, 0.16), grade_gauss(l, high, 0.14));
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    // 1. Exposure (2^stops) — physically correct in linear.
    // 1. Exposure and white balance in linear light.
    let exp_factor = pow(2.0, params.exposure);
    r = r * exp_factor;
    g = g * exp_factor;
    b = b * exp_factor;
    let wb_r = params.wb0 * r + params.wb1 * g + params.wb2 * b;
    let wb_g = params.wb3 * r + params.wb4 * g + params.wb5 * b;
    let wb_b = params.wb6 * r + params.wb7 * g + params.wb8 * b;
    r = wb_r;
    g = wb_g;
    b = wb_b;
    // 2. In the grade encoding (klipp_color::grade_model, the CPU twin):
    //    the tone bands, the wheels as the ASC CDL, contrast about mid grey.
    let luma1 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    let l_band = band_luma(grade_encode_in(luma1));
    let tw = tone_weights(l_band);
    let tone_adj = params.highlights * tw.x + params.shadows * tw.y + params.whites * tw.z + params.blacks * tw.w;
    var er = grade_encode_in(r) + tone_adj;
    var eg = grade_encode_in(g) + tone_adj;
    var eb = grade_encode_in(b) + tone_adj;
    let before_wheels = vec3<f32>(er, eg, eb);
    if params.log_mode != 0u {
        let lw = log_wheel_weights(l_band);
        er = er + params.log_low_r * lw.x + params.log_mid_r * lw.y + params.log_high_r * lw.z;
        eg = eg + params.log_low_g * lw.x + params.log_mid_g * lw.y + params.log_high_g * lw.z;
        eb = eb + params.log_low_b * lw.x + params.log_mid_b * lw.y + params.log_high_b * lw.z;
    } else {
        er = cdl_channel(er, params.lift_r, params.gamma_r, params.gain_r, params.offset_r);
        eg = cdl_channel(eg, params.lift_g, params.gamma_g, params.gain_g, params.offset_g);
        eb = cdl_channel(eb, params.lift_b, params.gamma_b, params.gain_b, params.offset_b);
    }
    // The luma mix (#900), klipp_grade_model::luma_mix: the wheels'
    // change to luma taken back by its complement, in linear light.
    if params.luma_mix < 1.0 {
        let luma_c = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
        let pre = vec3<f32>(grade_decode_in(before_wheels.x), grade_decode_in(before_wheels.y), grade_decode_in(before_wheels.z));
        let post = vec3<f32>(grade_decode_in(er), grade_decode_in(eg), grade_decode_in(eb));
        let back = (1.0 - clamp(params.luma_mix, 0.0, 1.0)) * (dot(luma_c, pre) - dot(luma_c, post));
        er = grade_encode_in(post.x + back);
        eg = grade_encode_in(post.y + back);
        eb = grade_encode_in(post.z + back);
    }
    er = contrast_channel_at(er, params.contrast, params.contrast_pivot_encoded);
    eg = contrast_channel_at(eg, params.contrast, params.contrast_pivot_encoded);
    eb = contrast_channel_at(eb, params.contrast, params.contrast_pivot_encoded);
    r = grade_decode_in(er);
    g = grade_decode_in(eg);
    b = grade_decode_in(eb);
    // 3. Back in linear light: saturation about the working space's luma.
    let luma3 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    r = luma3 + (r - luma3) * params.saturation;
    g = luma3 + (g - luma3) * params.saturation;
    b = luma3 + (b - luma3) * params.saturation;
    if abs(params.hue) >= 0.01 {
        let rot = hue_rotate_hsl(vec3<f32>(r, g, b), params.hue);
        r = rot.x;
        g = rot.y;
        b = rot.z;
    }

    // 8. Global mix (blend with original).
    r = orig.x + (r - orig.x) * params.global_mix;
    g = orig.y + (g - orig.y) * params.global_mix;
    b = orig.z + (b - orig.z) * params.global_mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
