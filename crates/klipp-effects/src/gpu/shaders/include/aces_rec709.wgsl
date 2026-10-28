
// Must match `klipp_color::aces::gpu_layout`.
const HEADER_LEN: u32 = 12u;
const A_VARIANT: u32 = 6u;
const A_DISP_GAMMA: u32 = 7u;
const A_DIM_SURROUND: u32 = 8u;
const A_Y_WHITE: u32 = 9u;
const A_Y_BLACK: u32 = 10u;
const MATRICES: u32 = 12u;
// Derived, not written out: the offsets below shift whenever a matrix
// slot is added, and two of them written by hand is two chances to
// forget one. (That mistake reads every curve as a matrix row and turns
// the picture solid.)
const MATRIX_COUNT: u32 = 12u;
const SSTS: u32 = MATRICES + MATRIX_COUNT * 9u;
const SSTS_LEN: u32 = 21u;
const CURVES: u32 = SSTS + SSTS_LEN;
const HALF_MIN: f32 = 6.103515625e-5;
const HALF_MAX: f32 = 65504.0;
const RRT_GLOW_GAIN: f32 = 0.05;
const RRT_GLOW_MID: f32 = 0.08;
const RRT_RED_SCALE: f32 = 0.82;
const RRT_RED_PIVOT: f32 = 0.03;
const RRT_RED_HUE: f32 = 0.0;
const RRT_RED_WIDTH: f32 = 135.0;
const CINEMA_WHITE: f32 = 48.0;
const CINEMA_BLACK: f32 = 0.02;
const SDR_DISP_GAMMA: f32 = 2.4;
const DIM_SURROUND_GAMMA: f32 = 0.9811;
const YC_RADIUS_WEIGHT: f32 = 1.75;
const TINY: f32 = 1e-10;

fn mat_mul(m: u32, v: vec3<f32>) -> vec3<f32> {
    let o = MATRICES + m * 9u;
    return vec3<f32>(
        tables[o + 0u] * v.x + tables[o + 1u] * v.y + tables[o + 2u] * v.z,
        tables[o + 3u] * v.x + tables[o + 4u] * v.y + tables[o + 5u] * v.z,
        tables[o + 6u] * v.x + tables[o + 7u] * v.y + tables[o + 8u] * v.z,
    );
}

/// A tone curve from its table: uniform in log10(x), storing log10(y),
/// clamped at the ends — which is exact, because both curves really are
/// constant outside their knots.
fn tone_curve(which: u32, x: f32) -> f32 {
    let base = select(CURVES, CURVES + u32(tables[2]), which == 1u);
    let lo = tables[select(0u, 3u, which == 1u)];
    let hi = tables[select(1u, 4u, which == 1u)];
    let n = u32(tables[select(2u, 5u, which == 1u)]);

    let logx = log(max(x, HALF_MIN)) / log(10.0);
    let t = clamp((logx - lo) / (hi - lo), 0.0, 1.0) * f32(n - 1u);
    let i = min(u32(floor(t)), n - 2u);
    let f = t - floor(t);
    let logy = mix(tables[base + i], tables[base + i + 1u], f);
    return pow(10.0, logy);
}

fn rgb_2_saturation(rgb: vec3<f32>) -> f32 {
    let mx = max(max(rgb.x, rgb.y), rgb.z);
    let mn = min(min(rgb.x, rgb.y), rgb.z);
    return (max(mx, TINY) - max(mn, TINY)) / max(mx, 1.0e-2);
}

fn rgb_2_yc(rgb: vec3<f32>) -> f32 {
    let r = rgb.x; let g = rgb.y; let b = rgb.z;
    let chroma = sqrt(max(b * (b - g) + g * (g - r) + r * (r - b), 0.0));
    return (b + g + r + YC_RADIUS_WEIGHT * chroma) / 3.0;
}

fn rgb_2_hue(rgb: vec3<f32>) -> f32 {
    if rgb.x == rgb.y && rgb.y == rgb.z { return 0.0; }
    let y = sqrt(3.0) * (rgb.y - rgb.z);
    let x = 2.0 * rgb.x - rgb.y - rgb.z;
    var hue = degrees(atan2(y, x));
    if hue < 0.0 { hue = hue + 360.0; }
    return hue;
}

fn center_hue(hue: f32, center_h: f32) -> f32 {
    var h = hue - center_h;
    if h < -180.0 { h = h + 360.0; } else if h > 180.0 { h = h - 360.0; }
    return h;
}

fn sigmoid_shaper(x: f32) -> f32 {
    let t = max(1.0 - abs(x / 2.0), 0.0);
    return (1.0 + sign(x) * (1.0 - t * t)) / 2.0;
}

fn glow_fwd(yc_in: f32, glow_gain_in: f32, glow_mid: f32) -> f32 {
    if yc_in <= (2.0 / 3.0) * glow_mid { return glow_gain_in; }
    if yc_in >= 2.0 * glow_mid { return 0.0; }
    return glow_gain_in * (glow_mid / yc_in - 0.5);
}

/// AMPAS `cubic_basis_shaper`, width `w`, height ~1.5 at zero.
fn cubic_basis_shaper(x: f32, w: f32) -> f32 {
    let k0 = -w / 2.0;
    let k4 = w / 2.0;
    if !(x > k0 && x < k4) { return 0.0; }
    let knot_coord = (x - k0) / (k4 - k0) * 4.0;
    let j = i32(floor(knot_coord));
    let t = knot_coord - floor(knot_coord);
    let m = vec4<f32>(t * t * t, t * t, t, 1.0);
    // The basis columns, reversed as the CTL indexes them.
    var c: vec4<f32>;
    if j == 0 { c = vec4<f32>(1.0 / 6.0, 0.0, 0.0, 0.0); }
    else if j == 1 { c = vec4<f32>(-3.0 / 6.0, 3.0 / 6.0, 3.0 / 6.0, 1.0 / 6.0); }
    else if j == 2 { c = vec4<f32>(3.0 / 6.0, -6.0 / 6.0, 0.0, 4.0 / 6.0); }
    else if j == 3 { c = vec4<f32>(-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0); }
    else { return 0.0; }
    return dot(c, m) * 1.5;
}

fn rrt_sweeteners(aces_in: vec3<f32>) -> vec3<f32> {
    let saturation = rgb_2_saturation(aces_in);
    let yc_in = rgb_2_yc(aces_in);
    let s = sigmoid_shaper((saturation - 0.4) / 0.2);
    let added_glow = 1.0 + glow_fwd(yc_in, RRT_GLOW_GAIN * s, RRT_GLOW_MID);
    var a = aces_in * added_glow;

    let centered_hue = center_hue(rgb_2_hue(a), RRT_RED_HUE);
    let hue_weight = cubic_basis_shaper(centered_hue, RRT_RED_WIDTH);
    a.x = a.x + hue_weight * saturation * (RRT_RED_PIVOT - a.x) * (1.0 - RRT_RED_SCALE);

    // AP0 negatives clipped before the matrix, or a negative channel is
    // redistributed into the other two and pure reds go green.
    a = max(a, vec3<f32>(0.0));
    var ap1 = mat_mul(1u, a);
    ap1 = clamp(ap1, vec3<f32>(0.0), vec3<f32>(HALF_MAX));
    return mat_mul(2u, ap1);
}

fn dark_surround_to_dim_surround(rec709_lin: vec3<f32>) -> vec3<f32> {
    let xyz = mat_mul(5u, rec709_lin);
    let sum = xyz.x + xyz.y + xyz.z;
    if sum < 1e-12 { return rec709_lin; }
    let x_chrom = xyz.x / sum;
    let y_chrom = xyz.y / sum;
    let big_y = pow(max(xyz.y, 0.0), DIM_SURROUND_GAMMA);
    var back: vec3<f32>;
    if y_chrom < 1e-12 {
        back = vec3<f32>(0.0, big_y, 0.0);
    } else {
        let inv_y = 1.0 / y_chrom;
        back = vec3<f32>(x_chrom * big_y * inv_y, big_y, (1.0 - x_chrom - y_chrom) * big_y * inv_y);
    }
    return mat_mul(6u, back);
}

/// AcesCG (AP1) → display-encoded Rec.709, the CPU chain's
/// `acescg_to_rec709_sdr`.
/// `camera_log::linear_to_pq` — ST-2084, evaluated exactly.
///
/// Deliberately *not* the table `klipp_color::hdr::pq_encode_nits`
/// interpolates: that one serves the non-ACES PQ delivery, whose CPU
/// twin reads the same table. The ACES ODT's `Y_2_ST2084` calls the real
/// curve, so this does too, and the two PQ paths agree with their own
/// twins rather than with each other.
fn aces_linear_to_pq(l_norm: f32) -> f32 {
    let m1 = 0.15930176;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let yp = pow(max(l_norm, 0.0), m1);
    return pow((c1 + c2 * yp) / (1.0 + c3 * yp), m2);
}

/// `eval_spline_segment`: the quadratic basis transform from AMPAS
/// `Tonecurves.h`, written out. M_QUAD_BASIS is
/// `[[0.5,-1,0.5], [-1,1,0.5], [0.5,0,0]]` and this is its product with
/// three consecutive coefficients, evaluated at `t`.
fn quad_spline_segment(cf: vec3<f32>, t: f32) -> f32 {
    let a = 0.5 * cf.x - cf.y + 0.5 * cf.z;
    let b = -cf.x + cf.y;
    let c = 0.5 * cf.x + 0.5 * cf.y;
    return a * t * t + b * t + c;
}

/// `ssts`, the single-stage HDR tone scale: an AP1 channel value to
/// absolute display luminance in cd/m². Three quadratic segments per
/// side in log-log space, log-linear extrapolation outside.
///
/// Evaluated rather than read from a table, unlike the two SDR curves.
/// Its output feeds `Y_2_linCV`, which subtracts `Y_MIN` from values
/// that *are* `Y_MIN` near black, so a table's own log round-trip is all
/// that survives the cancellation — and PQ's slope there turns it into
/// about two 10-bit code values of lifted black. The numbers that could
/// drift are `init_TsParams`', and those are what the blob ships.
fn ssts(x: f32) -> f32 {
    let min_x = tables[SSTS + 0u];
    let min_y = tables[SSTS + 1u];
    let min_slope = tables[SSTS + 2u];
    let mid_x = tables[SSTS + 3u];
    let max_x = tables[SSTS + 6u];
    let max_y = tables[SSTS + 7u];
    let max_slope = tables[SSTS + 8u];

    // Floored at TINY rather than HALF_MIN, deliberately, as the twin
    // is: these curves put their min anchor below HALF_MIN, so that
    // floor would make the low extrapolation unreachable and lift black.
    let logx = log(max(x, TINY)) / log(10.0);
    let log_min_x = log(min_x) / log(10.0);
    let log_mid_x = log(mid_x) / log(10.0);
    let log_max_x = log(max_x) / log(10.0);

    var logy = 0.0;
    if logx <= log_min_x {
        logy = logx * min_slope + (log(min_y) / log(10.0) - min_slope * log_min_x);
    } else if logx < log_mid_x {
        let knot = 3.0 * (logx - log_min_x) / (log_mid_x - log_min_x);
        // Capped at the last segment: float rounding can push `knot` to
        // exactly 3 at the boundary, where segment 2 at t = 1 is
        // precisely the mid anchor.
        let j = min(u32(floor(knot)), 2u);
        let t = knot - f32(j);
        let cf = vec3<f32>(
            tables[SSTS + 9u + j],
            tables[SSTS + 9u + j + 1u],
            tables[SSTS + 9u + j + 2u],
        );
        logy = quad_spline_segment(cf, t);
    } else if logx < log_max_x {
        let knot = 3.0 * (logx - log_mid_x) / (log_max_x - log_mid_x);
        let j = min(u32(floor(knot)), 2u);
        let t = knot - f32(j);
        let cf = vec3<f32>(
            tables[SSTS + 15u + j],
            tables[SSTS + 15u + j + 1u],
            tables[SSTS + 15u + j + 2u],
        );
        logy = quad_spline_segment(cf, t);
    } else {
        logy = logx * max_slope + (log(max_y) / log(10.0) - max_slope * log_max_x);
    }
    return pow(10.0, logy);
}

/// The ACES 1.1 single-stage output transform for the Rec.2100 PQ
/// deliverables: sweeteners, the SSTS, then absolute luminance to a PQ
/// signal via the limiting primaries.
/// Called with AP1 values: `acescg_to_display` has already walked the
/// project's basis there.
fn acescg_to_rec2100_pq(acescg: vec3<f32>) -> vec3<f32> {
    let y_max = tables[A_Y_WHITE];
    let y_min = tables[A_Y_BLACK];

    // Sweeteners, shared with the SDR path so the two cannot drift.
    let pre = rrt_sweeteners(mat_mul(0u, acescg));
    let post = vec3<f32>(ssts(pre.x), ssts(pre.y), ssts(pre.z));
    let lin_cv = (post - vec3<f32>(y_min)) / (y_max - y_min);

    // AP1 -> XYZ, still D60-adapted, then gamut-limit to Rec.2020 by
    // round-tripping through its primaries with a clip on the way.
    //
    // The clip happens in a D65-primaries space applied to XYZ that is
    // still D60-adapted, because the CAT only runs afterwards. That
    // white-point mismatch is what the reference does; matching it is
    // the point.
    var xyz = mat_mul(7u, lin_cv);
    let limited = clamp(mat_mul(8u, xyz), vec3<f32>(0.0), vec3<f32>(1.0));
    xyz = mat_mul(9u, limited);
    xyz = mat_mul(10u, xyz);
    let rgb = max(mat_mul(8u, xyz), vec3<f32>(0.0));

    // STRETCH_BLACK: the trip back to absolute luminance uses Ymin = 0,
    // so the darkest values quantise to PQ 0 instead of floating above.
    let nits = max(rgb * y_max, vec3<f32>(0.0));
    return vec3<f32>(
        aces_linear_to_pq(nits.x / 10000.0),
        aces_linear_to_pq(nits.y / 10000.0),
        aces_linear_to_pq(nits.z / 10000.0),
    );
}

/// An AcesCG frame through whichever output transform the blob
/// describes: the two-stage SDR chain (RRT then the C9 ODT) or the
/// single-stage HDR one.
///
/// The SDR deliverables — Rec.709 video and DCI-P3 D65 cinema — are one
/// chain with three numbers changed: the output matrix, the display
/// gamma, and whether the dim-surround compensation runs. A cinema is a
/// dark surround, not a dim one, so the cinema ODT applies neither that
/// nor its saturation partner, and the blob carries an identity in the
/// saturation slot for it.
fn acescg_to_display(working: vec3<f32>) -> vec3<f32> {
    // The project's basis into AP1, which the ODTs are defined on. Its
    // own slot rather than folded into ap1->ap0, which the chain applies
    // a second time below to the post-curve values.
    let acescg = mat_mul(11u, working);
    if u32(tables[A_VARIANT]) == 1u {
        return acescg_to_rec2100_pq(acescg);
    }
    // RRT: to AP0, sweeteners, tone curve, back to AP0 as OCES.
    let ap1_desat = rrt_sweeteners(mat_mul(0u, acescg));
    let rgb_post = vec3<f32>(
        tone_curve(0u, ap1_desat.x),
        tone_curve(0u, ap1_desat.y),
        tone_curve(0u, ap1_desat.z),
    );
    let oces = mat_mul(0u, rgb_post);

    // ODT: to AP1, tone curve to display cd/m2, normalise, desaturate,
    // to the display basis, dim surround, clamp, inverse OETF.
    let pre = mat_mul(1u, oces);
    let post = vec3<f32>(
        tone_curve(1u, pre.x),
        tone_curve(1u, pre.y),
        tone_curve(1u, pre.z),
    );
    let y_white = tables[A_Y_WHITE];
    let y_black = tables[A_Y_BLACK];
    let lin_cv = (post - vec3<f32>(y_black)) / (y_white - y_black);
    let desat = mat_mul(3u, lin_cv);
    var display_lin = mat_mul(4u, desat);
    if tables[A_DIM_SURROUND] != 0.0 {
        display_lin = dark_surround_to_dim_surround(display_lin);
    }
    let clamped = clamp(display_lin, vec3<f32>(0.0), vec3<f32>(1.0));
    // BT.1886 inverse at L_W = 1, L_B = 0 collapses to pow(L, 1/gamma),
    // which is also the DCI gamma-2.6 encode.
    return pow(clamped, vec3<f32>(1.0 / tables[A_DISP_GAMMA]));
}

/// The Rec.709 SDR transform by name, for callers that only ever want
/// that one and whose blob always describes it.
fn acescg_to_rec709_sdr(acescg: vec3<f32>) -> vec3<f32> {
    return acescg_to_display(acescg);
}

