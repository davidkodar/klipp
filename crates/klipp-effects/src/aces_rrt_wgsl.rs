//! WGSL port of the ACES RRT and the SDR Rec.709 ODT.
//!
//! Mirrors `klipp_color::aces` line-for-line — the Rust side is the
//! authoritative CPU oracle, this is its GPU twin. Bug-fixes in
//! either form should land in both at the same time. A future parity
//! test will run the same input through both paths and assert ≤ 1
//! LSB at 8-bit display encode.
//!
//! Source: AMPAS aces-dev `transforms/ctl/rrt/RRT.ctl` and
//! `transforms/ctl/odt/sdr/ODT.Academy.Rec709_100nits_dim.ctl`
//! (BSD 3-Clause).
//!
//! # How to use
//!
//! Concatenate [`WGSL_RRT`] then [`WGSL_ODT_REC709_SDR`] into a
//! shader source string after the decoder's `WGSL_PRELUDE` +
//! `WGSL_HELPERS` and before the body that invokes the entry points.
//!
//! Function signatures:
//!   * `apply_rrt(aces_in: vec3<f32>) -> vec3<f32>` — AP0 → OCES
//!   * `apply_odt_rec709_sdr(oces_in: vec3<f32>) -> vec3<f32>` —
//!      OCES (AP0) → display-encoded Rec.709 RGB in `[0, 1]`
//!
//! Display path: `ACEScg → AP1→AP0 → apply_rrt → apply_odt_rec709_sdr
//! → display-encoded RGB`.

/// WGSL fragment containing all RRT helpers and the top-level
/// `apply_rrt(aces: vec3<f32>) -> vec3<f32>`. Constants and matrices
/// are inlined as `const` declarations so the fragment is
/// self-contained and doesn't pull from `WGSL_PRELUDE` (the prelude
/// describes the *decoder's* per-source IDs, not the RRT's
/// constants).
pub const WGSL_RRT: &str = r#"
// ─── RRT constants (verbatim from RRT.ctl) ───────────────────────
const RRT_GLOW_GAIN: f32 = 0.05;
const RRT_GLOW_MID: f32 = 0.08;
const RRT_RED_SCALE: f32 = 0.82;
const RRT_RED_PIVOT: f32 = 0.03;
const RRT_RED_HUE: f32 = 0.0;
const RRT_RED_WIDTH: f32 = 135.0;
const RRT_SAT_FACTOR: f32 = 0.96;
const RRT_TINY: f32 = 1.0e-10;
// f16 minimum — the AMPAS `HALF_MIN` constant. Used as a clamp
// before `log10` to keep the spline finite at zero.
const RRT_HALF_MIN: f32 = 6.103515625e-5;

// AP1 → XYZ row 1 (luminance coefficients), used by the global
// desaturation matrix.
const RRT_AP1_RGB2Y: vec3<f32> = vec3<f32>(0.27222872, 0.67408177, 0.05368952);

// ─── Helpers ─────────────────────────────────────────────────────

fn rrt_max3(rgb: vec3<f32>) -> f32 {
    return max(max(rgb.x, rgb.y), rgb.z);
}

fn rrt_min3(rgb: vec3<f32>) -> f32 {
    return min(min(rgb.x, rgb.y), rgb.z);
}

fn rrt_rgb_2_saturation(rgb: vec3<f32>) -> f32 {
    let mx = max(rrt_max3(rgb), RRT_TINY);
    let mn = max(rrt_min3(rgb), RRT_TINY);
    let denom = max(rrt_max3(rgb), 1.0e-2);
    return (mx - mn) / denom;
}

fn rrt_rgb_2_yc(rgb: vec3<f32>) -> f32 {
    let yc_radius_weight: f32 = 1.75;
    let r = rgb.x; let g = rgb.y; let b = rgb.z;
    let chroma = sqrt(max(b*(b - g) + g*(g - r) + r*(r - b), 0.0));
    return (b + g + r + yc_radius_weight * chroma) / 3.0;
}

fn rrt_rgb_2_hue(rgb: vec3<f32>) -> f32 {
    if rgb.x == rgb.y && rgb.y == rgb.z {
        return 0.0;
    }
    let y = sqrt(3.0) * (rgb.y - rgb.z);
    let x = 2.0 * rgb.x - rgb.y - rgb.z;
    var hue = degrees(atan2(y, x));
    if hue < 0.0 { hue = hue + 360.0; }
    return hue;
}

fn rrt_center_hue(hue: f32, center_h: f32) -> f32 {
    var h = hue - center_h;
    if h < -180.0 { h = h + 360.0; }
    else if h > 180.0 { h = h - 360.0; }
    return h;
}

fn rrt_sigmoid_shaper(x: f32) -> f32 {
    let t = max(1.0 - abs(x / 2.0), 0.0);
    let y = 1.0 + sign(x) * (1.0 - t * t);
    return y / 2.0;
}

fn rrt_glow_fwd(yc_in: f32, glow_gain_in: f32, glow_mid: f32) -> f32 {
    if yc_in <= (2.0 / 3.0) * glow_mid {
        return glow_gain_in;
    }
    if yc_in >= 2.0 * glow_mid {
        return 0.0;
    }
    return glow_gain_in * (glow_mid / yc_in - 0.5);
}

fn rrt_cubic_basis_shaper(x: f32, w: f32) -> f32 {
    let knot_lo = -w / 2.0;
    let knot_hi =  w / 2.0;
    if !(x > knot_lo && x < knot_hi) {
        return 0.0;
    }
    let knot_coord = (x - knot_lo) / (knot_hi - knot_lo) * 4.0;
    let j = i32(floor(knot_coord));
    let t = knot_coord - f32(j);
    // Cubic-monomial basis matrix (column-major access).
    let m = mat4x4<f32>(
        vec4<f32>(-1.0/6.0,  3.0/6.0, -3.0/6.0,  1.0/6.0),
        vec4<f32>( 3.0/6.0, -6.0/6.0,  3.0/6.0,  0.0),
        vec4<f32>(-3.0/6.0,  0.0,      3.0/6.0,  0.0),
        vec4<f32>( 1.0/6.0,  4.0/6.0,  1.0/6.0,  0.0),
    );
    let monomials = vec4<f32>(t * t * t, t * t, t, 1.0);
    var col = 0;
    if j == 0 { col = 3; }
    else if j == 1 { col = 2; }
    else if j == 2 { col = 1; }
    else if j == 3 { col = 0; }
    else { return 0.0; }
    let row = vec4<f32>(m[0][col], m[1][col], m[2][col], m[3][col]);
    let y = dot(row, monomials);
    return y * 1.5;
}

// ─── Segmented spline (RRT tone curve) ───────────────────────────

const RRT_SPLINE_MIN_X: f32 = 5.4931641e-6;   // 0.18 * 2^-15
const RRT_SPLINE_MIN_Y: f32 = 0.0001;
const RRT_SPLINE_MID_X: f32 = 0.18;
const RRT_SPLINE_MID_Y: f32 = 4.8;
const RRT_SPLINE_MAX_X: f32 = 47185.92;       // 0.18 * 2^18
const RRT_SPLINE_MAX_Y: f32 = 10000.0;

fn rrt_eval_spline_segment(cf: vec3<f32>, monomials: vec3<f32>) -> f32 {
    // M_QUAD_BASIS rows = (0.5, -1.0, 0.5), (-1.0, 1.0, 0.5), (0.5, 0, 0).
    let row0 = 0.5 * cf.x - 1.0 * cf.y + 0.5 * cf.z;
    let row1 = -1.0 * cf.x + 1.0 * cf.y + 0.5 * cf.z;
    let row2 = 0.5 * cf.x;
    return row0 * monomials.x + row1 * monomials.y + row2 * monomials.z;
}

fn rrt_segmented_spline_c5_fwd(x: f32) -> f32 {
    let logx = log(max(x, RRT_HALF_MIN)) / log(10.0);
    let log_min = log(RRT_SPLINE_MIN_X) / log(10.0);
    let log_mid = log(RRT_SPLINE_MID_X) / log(10.0);
    let log_max = log(RRT_SPLINE_MAX_X) / log(10.0);
    // Coefs (low and high) — constant arrays.
    let coefs_low_0: f32 = -4.0;
    let coefs_low_1: f32 = -4.0;
    let coefs_low_2: f32 = -3.1573765773;
    let coefs_low_3: f32 = -0.4852499958;
    let coefs_low_4: f32 =  1.8477324706;
    let coefs_high_0: f32 = -0.7185482425;
    let coefs_high_1: f32 =  2.0810307172;
    let coefs_high_2: f32 =  3.6681241237;
    let coefs_high_3: f32 =  4.0;
    let coefs_high_4: f32 =  4.0;

    var logy: f32;
    if logx <= log_min {
        logy = log(RRT_SPLINE_MIN_Y) / log(10.0);
    } else if logx > log_min && logx < log_mid {
        let knot_coord = 3.0 * (logx - log_min) / (log_mid - log_min);
        let j = min(i32(floor(knot_coord)), 2);
        let t = knot_coord - f32(j);
        var cf: vec3<f32>;
        if j == 0 { cf = vec3<f32>(coefs_low_0, coefs_low_1, coefs_low_2); }
        else if j == 1 { cf = vec3<f32>(coefs_low_1, coefs_low_2, coefs_low_3); }
        else { cf = vec3<f32>(coefs_low_2, coefs_low_3, coefs_low_4); }
        logy = rrt_eval_spline_segment(cf, vec3<f32>(t * t, t, 1.0));
    } else if logx >= log_mid && logx < log_max {
        let knot_coord = 3.0 * (logx - log_mid) / (log_max - log_mid);
        let j = min(i32(floor(knot_coord)), 2);
        let t = knot_coord - f32(j);
        var cf: vec3<f32>;
        if j == 0 { cf = vec3<f32>(coefs_high_0, coefs_high_1, coefs_high_2); }
        else if j == 1 { cf = vec3<f32>(coefs_high_1, coefs_high_2, coefs_high_3); }
        else { cf = vec3<f32>(coefs_high_2, coefs_high_3, coefs_high_4); }
        logy = rrt_eval_spline_segment(cf, vec3<f32>(t * t, t, 1.0));
    } else {
        logy = log(RRT_SPLINE_MAX_Y) / log(10.0);
    }
    return pow(10.0, logy);
}

// ─── AP0 ↔ AP1 matrices ──────────────────────────────────────────

fn rrt_ap0_to_ap1_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         1.4514393161 * rgb.x - 0.2365107469 * rgb.y - 0.2149285693 * rgb.z,
        -0.0765537734 * rgb.x + 1.1762296998 * rgb.y - 0.0996759264 * rgb.z,
         0.0083161484 * rgb.x - 0.0060324498 * rgb.y + 0.9977163014 * rgb.z,
    );
}

fn rrt_ap1_to_ap0_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         0.6954522414 * rgb.x + 0.1406786965 * rgb.y + 0.1638690622 * rgb.z,
         0.0447945634 * rgb.x + 0.8596711185 * rgb.y + 0.0955343182 * rgb.z,
        -0.0055258826 * rgb.x + 0.0040252103 * rgb.y + 1.0015006723 * rgb.z,
    );
}

// ─── apply_rrt — top-level RRT pipeline ──────────────────────────
//
// Input: scene-linear AP0 (ACES2065-1).
// Output: post-tonemap AP0 (OCES).

fn apply_rrt(aces_in: vec3<f32>) -> vec3<f32> {
    var aces = aces_in;

    // Glow modulation
    let saturation = rrt_rgb_2_saturation(aces);
    let yc_in = rrt_rgb_2_yc(aces);
    let s = rrt_sigmoid_shaper((saturation - 0.4) / 0.2);
    let added_glow = 1.0 + rrt_glow_fwd(yc_in, RRT_GLOW_GAIN * s, RRT_GLOW_MID);
    aces = aces * added_glow;

    // Red modifier
    let hue = rrt_rgb_2_hue(aces);
    let centered_hue = rrt_center_hue(hue, RRT_RED_HUE);
    let hue_weight = rrt_cubic_basis_shaper(centered_hue, RRT_RED_WIDTH);
    aces.x = aces.x + hue_weight * saturation * (RRT_RED_PIVOT - aces.x) * (1.0 - RRT_RED_SCALE);

    // ACES → AP1
    var ap1 = rrt_ap0_to_ap1_mul(aces);
    ap1 = max(ap1, vec3<f32>(0.0, 0.0, 0.0));

    // Global desaturation: ap1 = sat·in + (1-sat)·Y·[1,1,1]
    let one_minus_s_sat = 1.0 - RRT_SAT_FACTOR;
    let lum = dot(RRT_AP1_RGB2Y, ap1);
    ap1 = RRT_SAT_FACTOR * ap1 + one_minus_s_sat * vec3<f32>(lum, lum, lum);

    // Per-channel tone curve
    let rgb_post = vec3<f32>(
        rrt_segmented_spline_c5_fwd(ap1.x),
        rrt_segmented_spline_c5_fwd(ap1.y),
        rrt_segmented_spline_c5_fwd(ap1.z),
    );

    // AP1 → AP0 (OCES)
    return rrt_ap1_to_ap0_mul(rgb_post);
}
"#;

/// WGSL fragment for the SDR Rec.709 100-nit-dim ODT. Append after
/// [`WGSL_RRT`] so the shared helpers (`rrt_eval_spline_segment`,
/// `RRT_HALF_MIN`, `RRT_AP1_RGB2Y`) and AP0/AP1 matrices come into
/// scope before this fragment uses them.
///
/// Function: `apply_odt_rec709_sdr(oces_in: vec3<f32>) -> vec3<f32>`.
/// Input is OCES (AP0); output is display-encoded Rec.709 RGB in
/// `[0, 1]`, ready to quantise to 8/10-bit.
pub const WGSL_ODT_REC709_SDR: &str = r#"
// ─── ODT constants ───────────────────────────────────────────────
const ODT_SAT_FACTOR: f32 = 0.93;
const ODT_DIM_SURROUND_GAMMA: f32 = 0.9811;
const ODT_CINEMA_WHITE: f32 = 48.0;
const ODT_CINEMA_BLACK: f32 = 0.02;
const ODT_SDR_DISP_GAMMA: f32 = 2.4;

// ─── C9 spline coefficients (ODT_48nits) ─────────────────────────
// Same shape as the RRT's C5 spline but with 10 coefs per side and
// a `(N_KNOTS - 1) = 7` multiplier. j∈[0,7] → cf[j..j+3] indexes
// up to slot 9, the last valid entry.

fn odt_c9_low_coef(idx: i32) -> f32 {
    if idx == 0 { return -1.6989700043; }
    if idx == 1 { return -1.6989700043; }
    if idx == 2 { return -1.4779; }
    if idx == 3 { return -1.2291; }
    if idx == 4 { return -0.8648; }
    if idx == 5 { return -0.448; }
    if idx == 6 { return 0.00518; }
    if idx == 7 { return 0.4511080334; }
    if idx == 8 { return 0.9113744414; }
    return 0.9113744414;
}

fn odt_c9_high_coef(idx: i32) -> f32 {
    if idx == 0 { return 0.5154386965; }
    if idx == 1 { return 0.8470437783; }
    if idx == 2 { return 1.1358; }
    if idx == 3 { return 1.3802; }
    if idx == 4 { return 1.5197; }
    if idx == 5 { return 1.5985; }
    if idx == 6 { return 1.6467; }
    if idx == 7 { return 1.6746091357; }
    if idx == 8 { return 1.687873339; }
    return 1.687873339;
}

fn odt_c9_min_x() -> f32 {
    return rrt_segmented_spline_c5_fwd(0.18 * pow(2.0, -6.5));
}
fn odt_c9_mid_x() -> f32 { return rrt_segmented_spline_c5_fwd(0.18); }
fn odt_c9_max_x() -> f32 {
    return rrt_segmented_spline_c5_fwd(0.18 * pow(2.0, 6.5));
}

fn odt_segmented_spline_c9_fwd(x: f32) -> f32 {
    let logx = log(max(x, RRT_HALF_MIN)) / log(10.0);
    let log_min = log(odt_c9_min_x()) / log(10.0);
    let log_mid = log(odt_c9_mid_x()) / log(10.0);
    let log_max = log(odt_c9_max_x()) / log(10.0);
    var logy: f32;
    if logx <= log_min {
        logy = log(0.02) / log(10.0);
    } else if logx > log_min && logx < log_mid {
        let knot_coord = 7.0 * (logx - log_min) / (log_mid - log_min);
        let j = min(i32(floor(knot_coord)), 7);
        let t = knot_coord - f32(j);
        let cf = vec3<f32>(odt_c9_low_coef(j), odt_c9_low_coef(j + 1), odt_c9_low_coef(j + 2));
        logy = rrt_eval_spline_segment(cf, vec3<f32>(t * t, t, 1.0));
    } else if logx >= log_mid && logx < log_max {
        let knot_coord = 7.0 * (logx - log_mid) / (log_max - log_mid);
        let j = min(i32(floor(knot_coord)), 7);
        let t = knot_coord - f32(j);
        let cf = vec3<f32>(odt_c9_high_coef(j), odt_c9_high_coef(j + 1), odt_c9_high_coef(j + 2));
        logy = rrt_eval_spline_segment(cf, vec3<f32>(t * t, t, 1.0));
    } else {
        logy = log(48.0) / log(10.0);
    }
    return pow(10.0, logy);
}

// ─── Display primaries / OETF helpers ────────────────────────────

// AP1 (D60) → Rec.709 (D65), Bradford-adapted. Same matrix as
// `klipp_color::primaries::acescg_to_bt709` (numerical-inverse pair).
fn odt_acescg_to_rec709_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         1.7050523622 * rgb.x - 0.6217957415 * rgb.y - 0.0832566207 * rgb.z,
        -0.1302600472 * rgb.x + 1.1408029888 * rgb.y - 0.0105430932 * rgb.z,
        -0.0239638781 * rgb.x - 0.1289693959 * rgb.y + 1.1529332720 * rgb.z,
    );
}

// Rec.709 D65 → CIE XYZ (D65). Standard.
fn odt_rec709_to_xyz_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        0.4123907993 * rgb.x + 0.3575843394 * rgb.y + 0.1804807884 * rgb.z,
        0.2126390059 * rgb.x + 0.7151686788 * rgb.y + 0.0721923154 * rgb.z,
        0.0193308187 * rgb.x + 0.1191947798 * rgb.y + 0.9505321522 * rgb.z,
    );
}

// CIE XYZ (D65) → Rec.709 D65. Standard.
fn odt_xyz_to_rec709_mul(xyz: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         3.2409699419 * xyz.x - 1.5373831776 * xyz.y - 0.4986107603 * xyz.z,
        -0.9692436363 * xyz.x + 1.8759675015 * xyz.y + 0.0415550574 * xyz.z,
         0.0556300797 * xyz.x - 0.2039769589 * xyz.y + 1.0569715142 * xyz.z,
    );
}

fn odt_dark_surround_to_dim_surround(rec709_lin: vec3<f32>) -> vec3<f32> {
    let xyz = odt_rec709_to_xyz_mul(rec709_lin);
    let sum = xyz.x + xyz.y + xyz.z;
    if sum < 1e-12 { return rec709_lin; }
    let x_chrom = xyz.x / sum;
    let y_chrom = xyz.y / sum;
    let big_y = pow(max(xyz.y, 0.0), ODT_DIM_SURROUND_GAMMA);
    var xyz_back: vec3<f32>;
    if y_chrom < 1e-12 {
        xyz_back = vec3<f32>(0.0, big_y, 0.0);
    } else {
        let inv_y = 1.0 / y_chrom;
        xyz_back = vec3<f32>(x_chrom * big_y * inv_y, big_y, (1.0 - x_chrom - y_chrom) * big_y * inv_y);
    }
    return odt_xyz_to_rec709_mul(xyz_back);
}

fn odt_y_to_lin_cv(y: f32, y_max: f32, y_min: f32) -> f32 {
    return (y - y_min) / (y_max - y_min);
}

// BT.1886 inverse with L_W=1, L_B=0 (the Rec.709 SDR ODT shape).
// Reduces to pow(L, 1/γ).
fn odt_bt1886_inverse(l: f32) -> f32 {
    return pow(max(l, 0.0), 1.0 / ODT_SDR_DISP_GAMMA);
}

// AP0 → AP1 with the existing `rrt_ap0_to_ap1_mul` from WGSL_RRT.

// ─── apply_odt_rec709_sdr ────────────────────────────────────────

fn apply_odt_rec709_sdr(oces_in: vec3<f32>) -> vec3<f32> {
    // OCES → AP1 rendering space.
    var rgb_pre = rrt_ap0_to_ap1_mul(oces_in);

    // Per-channel C9 tone scale.
    let rgb_post = vec3<f32>(
        odt_segmented_spline_c9_fwd(rgb_pre.x),
        odt_segmented_spline_c9_fwd(rgb_pre.y),
        odt_segmented_spline_c9_fwd(rgb_pre.z),
    );

    // Display Y → linear CV.
    var lin_cv = vec3<f32>(
        odt_y_to_lin_cv(rgb_post.x, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
        odt_y_to_lin_cv(rgb_post.y, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
        odt_y_to_lin_cv(rgb_post.z, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
    );

    // ODT saturation: lin_cv = sat·in + (1-sat)·Y·[1,1,1] using AP1 luma.
    let one_minus_s = 1.0 - ODT_SAT_FACTOR;
    let lum = dot(RRT_AP1_RGB2Y, lin_cv);
    lin_cv = ODT_SAT_FACTOR * lin_cv + one_minus_s * vec3<f32>(lum, lum, lum);

    // AP1 → Rec.709 D65 (with D60→D65 chromatic adaptation baked in).
    var rec709_lin = odt_acescg_to_rec709_mul(lin_cv);

    // Dim-surround compensation.
    rec709_lin = odt_dark_surround_to_dim_surround(rec709_lin);

    // Clamp to display gamut.
    rec709_lin = clamp(rec709_lin, vec3<f32>(0.0), vec3<f32>(1.0));

    // BT.1886 inverse — display-linear → encoded.
    return vec3<f32>(
        odt_bt1886_inverse(rec709_lin.x),
        odt_bt1886_inverse(rec709_lin.y),
        odt_bt1886_inverse(rec709_lin.z),
    );
}
"#;

/// WGSL fragment for the Rec.2100 PQ HDR ODT. Append after
/// [`WGSL_RRT`] and [`WGSL_ODT_REC709_SDR`] so the shared helpers
/// (`rrt_segmented_spline_c5_fwd`, `odt_segmented_spline_c9_fwd`,
/// `RRT_AP1_RGB2Y`, `rrt_eval_spline_segment`, `rrt_ap0_to_ap1_mul`)
/// come into scope.
///
/// Two entry points (one per HDR mastering display peak):
///   * `apply_odt_rec2100_pq_1000_nit(oces) -> vec3<f32>` (HDR10
///      delivery, 1000-nit reference monitor)
///   * `apply_odt_rec2100_pq_4000_nit(oces) -> vec3<f32>` (4000-nit
///      mastering)
///
/// Output is PQ-encoded BT.2020 RGB in `[0, 1]` ready for an HDR10
/// container or a PQ-aware display. Same parity caveat as the Rust
/// oracle: the SDR cinema C9 spline is reused with a post-spline
/// linear scale to lift into HDR luminance — exact AMPAS HDR coefs
/// land in the soak/Macbeth-chart milestone.
pub const WGSL_ODT_REC2100_PQ: &str = r#"
const ODT_PQ_PEAK_LUMINANCE: f32 = 10000.0;

// AP1 (D60) → BT.2020 (D65), Bradford-adapted. Matches
// `klipp_color::primaries::acescg_to_bt2020`.
fn odt_acescg_to_bt2020_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         1.040036 * rgb.x - 0.001154 * rgb.y - 0.038881 * rgb.z,
        -0.011311 * rgb.x + 1.005945 * rgb.y + 0.005367 * rgb.z,
         0.000291 * rgb.x - 0.001263 * rgb.y + 1.000972 * rgb.z,
    );
}

// PQ (SMPTE ST 2084) forward OETF — linear (normalised against the
// 10000 cd/m² PQ peak) → encoded codepoint. Mirrors
// `klipp_color::camera_log::linear_to_pq`.
fn odt_linear_to_pq(l_norm: f32) -> f32 {
    let m1: f32 = 0.15930176;
    let m2: f32 = 78.84375;
    let c1: f32 = 0.8359375;
    let c2: f32 = 18.8515625;
    let c3: f32 = 18.6875;
    let yp = pow(max(l_norm, 0.0), m1);
    return pow((c1 + c2 * yp) / (1.0 + c3 * yp), m2);
}

// Generic Rec.2100 PQ ODT body. The SDR cinema C9 spline produces
// cinema-range luminance (peak ~48 cd/m²); the `peak_nits / 48`
// scale lifts it into the HDR target's range.
fn apply_odt_rec2100_pq_inner(oces_in: vec3<f32>, peak_nits: f32) -> vec3<f32> {
    let rgb_pre = rrt_ap0_to_ap1_mul(oces_in);
    let rgb_cinema = vec3<f32>(
        odt_segmented_spline_c9_fwd(rgb_pre.x),
        odt_segmented_spline_c9_fwd(rgb_pre.y),
        odt_segmented_spline_c9_fwd(rgb_pre.z),
    );
    let scale = peak_nits / 48.0;
    var rgb_hdr = rgb_cinema * scale;

    // ODT saturation: rgb = sat·in + (1-sat)·Y·[1,1,1] using AP1 luma.
    let one_minus_s = 1.0 - ODT_SAT_FACTOR;
    let lum = dot(RRT_AP1_RGB2Y, rgb_hdr);
    rgb_hdr = ODT_SAT_FACTOR * rgb_hdr + one_minus_s * vec3<f32>(lum, lum, lum);

    // AP1 → BT.2020.
    var bt2020 = odt_acescg_to_bt2020_mul(rgb_hdr);

    // Clamp to display peak luminance and PQ-encode.
    bt2020 = clamp(bt2020, vec3<f32>(0.0), vec3<f32>(peak_nits));
    let inv_pq_peak = 1.0 / ODT_PQ_PEAK_LUMINANCE;
    return vec3<f32>(
        odt_linear_to_pq(bt2020.x * inv_pq_peak),
        odt_linear_to_pq(bt2020.y * inv_pq_peak),
        odt_linear_to_pq(bt2020.z * inv_pq_peak),
    );
}

fn apply_odt_rec2100_pq_1000_nit(oces_in: vec3<f32>) -> vec3<f32> {
    return apply_odt_rec2100_pq_inner(oces_in, 1000.0);
}

fn apply_odt_rec2100_pq_4000_nit(oces_in: vec3<f32>) -> vec3<f32> {
    return apply_odt_rec2100_pq_inner(oces_in, 4000.0);
}
"#;

/// WGSL fragment for the DCI-P3 D65 cinema (48-nit) ODT. Append
/// after [`WGSL_RRT`] and [`WGSL_ODT_REC709_SDR`] (the SDR fragment
/// is what ships `odt_segmented_spline_c9_fwd`, `odt_y_to_lin_cv`
/// and the cinema constants we reuse here).
///
/// Entry point: `apply_odt_dcip3_d65_48_nits(oces_in: vec3<f32>) ->
/// vec3<f32>`. Output is DCI-γ-2.6-encoded P3-D65 RGB in `[0, 1]`.
pub const WGSL_ODT_DCIP3_D65: &str = r#"
const ODT_DCI_DISP_GAMMA: f32 = 2.6;

// AP1 (D60) → DCI-P3 D65, Bradford-adapted. Matches
// `klipp_color::primaries::acescg_to_p3d65`.
fn odt_acescg_to_p3d65_mul(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
         1.379214128253 * rgb.x - 0.308864144674 * rgb.y - 0.070349983580 * rgb.z,
        -0.069334858381 * rgb.x + 1.082296746002 * rgb.y - 0.012961887621 * rgb.z,
        -0.002159009514 * rgb.x - 0.045459324837 * rgb.y + 1.047618334351 * rgb.z,
    );
}

// DCI gamma 2.6 inverse OETF. Same shape as `odt_bt1886_inverse`
// from the SDR fragment but with γ = 2.6, L_W = 1, L_B = 0;
// reduces to `pow(L, 1/2.6)`.
fn odt_dci_gamma_inverse(l: f32) -> f32 {
    return pow(max(l, 0.0), 1.0 / ODT_DCI_DISP_GAMMA);
}

fn apply_odt_dcip3_d65_48_nits(oces_in: vec3<f32>) -> vec3<f32> {
    var rgb_pre = rrt_ap0_to_ap1_mul(oces_in);
    let rgb_post = vec3<f32>(
        odt_segmented_spline_c9_fwd(rgb_pre.x),
        odt_segmented_spline_c9_fwd(rgb_pre.y),
        odt_segmented_spline_c9_fwd(rgb_pre.z),
    );
    var lin_cv = vec3<f32>(
        odt_y_to_lin_cv(rgb_post.x, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
        odt_y_to_lin_cv(rgb_post.y, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
        odt_y_to_lin_cv(rgb_post.z, ODT_CINEMA_WHITE, ODT_CINEMA_BLACK),
    );
    // ODT saturation: lin_cv = sat·in + (1-sat)·Y·[1,1,1] using AP1 luma.
    let one_minus_s = 1.0 - ODT_SAT_FACTOR;
    let lum = dot(RRT_AP1_RGB2Y, lin_cv);
    lin_cv = ODT_SAT_FACTOR * lin_cv + one_minus_s * vec3<f32>(lum, lum, lum);
    var p3d65_lin = odt_acescg_to_p3d65_mul(lin_cv);
    // No dim-surround compensation — cinema viewing is dark, not dim.
    p3d65_lin = clamp(p3d65_lin, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(
        odt_dci_gamma_inverse(p3d65_lin.x),
        odt_dci_gamma_inverse(p3d65_lin.y),
        odt_dci_gamma_inverse(p3d65_lin.z),
    );
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Naga-parse the RRT fragment to confirm it's well-formed WGSL.
    /// `naga` is a transitive dep of `wgpu` (same 22.x version pin),
    /// so this adds no unique compile cost — it just exposes the
    /// validator at unit-test time without needing a GPU adapter. A
    /// typo or shape error here would otherwise hide until an
    /// integration test runs the shader.
    #[test]
    fn wgsl_rrt_parses_via_naga() {
        let result = naga::front::wgsl::parse_str(WGSL_RRT);
        match result {
            Ok(_) => (),
            Err(e) => panic!(
                "RRT WGSL failed to parse:\n{}",
                e.emit_to_string(WGSL_RRT),
            ),
        }
    }

    /// Pin the function name the rest of the engine will invoke.
    /// Renaming it requires a coordinated update at the call site;
    /// surface that intent through a test failure rather than a
    /// confused shader compile.
    #[test]
    fn wgsl_rrt_exposes_apply_rrt_entry_point() {
        assert!(WGSL_RRT.contains("fn apply_rrt(aces_in: vec3<f32>) -> vec3<f32>"));
    }

    /// Pin the constant values that mirror the Rust side so a future
    /// Rust-side tweak (e.g. a refined `RRT_SAT_FACTOR`) doesn't
    /// drift the WGSL silently. Mismatch here means the per-target
    /// parity work in later sessions will fail downstream — fail at
    /// unit-test scope first.
    #[test]
    fn wgsl_rrt_constants_match_rust_oracle() {
        for needle in [
            "RRT_GLOW_GAIN: f32 = 0.05",
            "RRT_GLOW_MID: f32 = 0.08",
            "RRT_RED_SCALE: f32 = 0.82",
            "RRT_SAT_FACTOR: f32 = 0.96",
            "RRT_AP1_RGB2Y: vec3<f32> = vec3<f32>(0.27222872, 0.67408177, 0.05368952)",
            "RRT_SPLINE_MID_X: f32 = 0.18",
            "RRT_SPLINE_MID_Y: f32 = 4.8",
        ] {
            assert!(WGSL_RRT.contains(needle), "missing or drifted constant: {needle}");
        }
    }

    /// Naga-parse the RRT + ODT fragments concatenated as the engine
    /// will use them. The ODT references helpers and constants from
    /// the RRT block (`RRT_HALF_MIN`, `RRT_AP1_RGB2Y`,
    /// `rrt_eval_spline_segment`, `rrt_segmented_spline_c5_fwd`,
    /// `rrt_ap0_to_ap1_mul`), so parsing them in isolation would
    /// fail — this test is the one that actually validates the
    /// shipping shader.
    #[test]
    fn wgsl_rrt_plus_odt_parses_via_naga() {
        let combined = format!("{}\n{}", WGSL_RRT, WGSL_ODT_REC709_SDR);
        let result = naga::front::wgsl::parse_str(&combined);
        if let Err(e) = result {
            panic!("RRT+ODT WGSL failed to parse:\n{}", e.emit_to_string(&combined));
        }
    }

    #[test]
    fn wgsl_odt_exposes_apply_odt_rec709_sdr_entry_point() {
        assert!(WGSL_ODT_REC709_SDR.contains(
            "fn apply_odt_rec709_sdr(oces_in: vec3<f32>) -> vec3<f32>"
        ));
    }

    #[test]
    fn wgsl_odt_constants_match_rust_oracle() {
        for needle in [
            "ODT_SAT_FACTOR: f32 = 0.93",
            "ODT_DIM_SURROUND_GAMMA: f32 = 0.9811",
            "ODT_CINEMA_WHITE: f32 = 48.0",
            "ODT_CINEMA_BLACK: f32 = 0.02",
            "ODT_SDR_DISP_GAMMA: f32 = 2.4",
        ] {
            assert!(WGSL_ODT_REC709_SDR.contains(needle),
                "ODT WGSL missing or drifted constant: {needle}");
        }
    }

    /// Naga-parse RRT + SDR ODT + PQ ODT concatenated. The PQ ODT
    /// references helpers from both earlier fragments
    /// (`rrt_segmented_spline_c5_fwd`, `odt_segmented_spline_c9_fwd`,
    /// `RRT_AP1_RGB2Y`, `ODT_SAT_FACTOR`); parsing it on its own
    /// would fail.
    #[test]
    fn wgsl_rrt_plus_odt_sdr_plus_odt_pq_parses_via_naga() {
        let combined = format!("{}\n{}\n{}",
            WGSL_RRT, WGSL_ODT_REC709_SDR, WGSL_ODT_REC2100_PQ);
        if let Err(e) = naga::front::wgsl::parse_str(&combined) {
            panic!("RRT+SDR+PQ WGSL failed to parse:\n{}", e.emit_to_string(&combined));
        }
    }

    #[test]
    fn wgsl_odt_pq_exposes_both_peak_variants() {
        assert!(WGSL_ODT_REC2100_PQ.contains(
            "fn apply_odt_rec2100_pq_1000_nit(oces_in: vec3<f32>) -> vec3<f32>"
        ));
        assert!(WGSL_ODT_REC2100_PQ.contains(
            "fn apply_odt_rec2100_pq_4000_nit(oces_in: vec3<f32>) -> vec3<f32>"
        ));
    }

    #[test]
    fn wgsl_odt_pq_constants_match_rust_oracle() {
        for needle in [
            "ODT_PQ_PEAK_LUMINANCE: f32 = 10000.0",
            // PQ ST 2084 constants — same values as
            // `klipp_color::camera_log::linear_to_pq`.
            "let m1: f32 = 0.15930176;",
            "let m2: f32 = 78.84375;",
            "let c1: f32 = 0.8359375;",
            "let c2: f32 = 18.8515625;",
            "let c3: f32 = 18.6875;",
        ] {
            assert!(WGSL_ODT_REC2100_PQ.contains(needle),
                "PQ ODT WGSL missing or drifted constant: {needle}");
        }
    }

    /// Parse RRT + SDR ODT + DCI-P3 ODT concatenated. The DCI-P3
    /// fragment depends on the SDR fragment for the C9 spline,
    /// `Y_to_lin_cv`, and the cinema constants — naga-validating
    /// it without the prerequisites would fail.
    #[test]
    fn wgsl_rrt_plus_odt_sdr_plus_odt_dcip3_parses_via_naga() {
        let combined = format!("{}\n{}\n{}",
            WGSL_RRT, WGSL_ODT_REC709_SDR, WGSL_ODT_DCIP3_D65);
        if let Err(e) = naga::front::wgsl::parse_str(&combined) {
            panic!("RRT+SDR+DCI-P3 WGSL failed to parse:\n{}", e.emit_to_string(&combined));
        }
    }

    /// Belt-and-braces: validate the FULL display-target inventory
    /// concatenated. RRT + SDR Rec.709 + HDR PQ + DCI-P3. If a
    /// future ODT introduces a name collision or symbol clash, this
    /// test fires before any integration test runs.
    #[test]
    fn wgsl_full_aces_pipeline_parses_via_naga() {
        let combined = format!("{}\n{}\n{}\n{}",
            WGSL_RRT, WGSL_ODT_REC709_SDR, WGSL_ODT_REC2100_PQ, WGSL_ODT_DCIP3_D65);
        if let Err(e) = naga::front::wgsl::parse_str(&combined) {
            panic!("Full ACES pipeline WGSL failed to parse:\n{}", e.emit_to_string(&combined));
        }
    }

    #[test]
    fn wgsl_odt_dcip3_exposes_entry_point() {
        assert!(WGSL_ODT_DCIP3_D65.contains(
            "fn apply_odt_dcip3_d65_48_nits(oces_in: vec3<f32>) -> vec3<f32>"
        ));
    }

    #[test]
    fn wgsl_odt_dcip3_constants_match_rust_oracle() {
        assert!(WGSL_ODT_DCIP3_D65.contains("ODT_DCI_DISP_GAMMA: f32 = 2.6"),
            "DCI-P3 WGSL must pin the DCI cinema gamma");
        // First row of `acescg_to_p3d65` — pinned to catch a future
        // matrix tweak in klipp-color drifting against the WGSL.
        assert!(WGSL_ODT_DCIP3_D65.contains("1.379214128253"),
            "DCI-P3 WGSL acescg_to_p3d65 first-row coefficient drifted");
    }
}
