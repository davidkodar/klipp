//! WGSL port of the ACES Reference Rendering Transform (RRT).
//!
//! Mirrors `klipp_color::aces` line-for-line — the Rust side is the
//! authoritative CPU oracle, this is its GPU twin. Bug-fixes in
//! either form should land in both at the same time. A future parity
//! test will run the same input through both paths and assert ≤ 1
//! LSB at 8-bit display encode.
//!
//! Source: AMPAS aces-dev `transforms/ctl/rrt/RRT.ctl` (BSD 3-Clause).
//!
//! # How to use
//!
//! Concatenate [`WGSL_RRT`] into a shader source string after the
//! decoder's `WGSL_PRELUDE` + `WGSL_HELPERS` and before the body that
//! calls `apply_rrt(...)`. The function takes scene-linear AP0 RGB
//! and returns post-tonemap AP0 (OCES). The display path is
//! ACEScg (AP1) → AP1→AP0 matrix → `apply_rrt` → AP0→AP1 → ODT.
//! The two basis-conversion matrix multiplies fold into the ODT's
//! first stage when wired up; for now this fragment is exercised only
//! by the parity test.

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
    /// drift the WGSL silently. Mismatch here means the parity test
    /// in S4B will fail downstream — fail at unit-test scope first.
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
}
