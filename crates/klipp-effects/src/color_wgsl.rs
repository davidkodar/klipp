//! WGSL prelude + helper functions for shaders that branch on a
//! source's colourspace IDs.
//!
//! The numeric IDs match `klipp_core::color_ids` exactly; we don't
//! depend on klipp-core here (`klipp-core` already depends on this
//! crate via `BuiltinEffect`, so a back-edge would cycle). Whenever
//! `klipp_core::color_ids` constants are renumbered, update both
//! sides — there's a unit test in `klipp_core::color_ids::tests` that
//! pins the values, and a smoke test in this crate that pins the
//! same values from the other side. A divergence will fail one or
//! the other immediately.
//!
//! Concatenate [`WGSL_PRELUDE`] then [`WGSL_HELPERS`] at the top of
//! any shader source string that needs colourspace-aware YUV→RGB
//! decoding.

/// Numeric ID constants exposed to WGSL via `const` declarations.
/// These MUST match `klipp_core::color_ids`'s constants. The
/// `pinned_ids_match_klipp_core` test in this crate guards that.
pub const WGSL_PRELUDE: &str = r#"
const COLOR_RANGE_LIMITED: u32 = 0u;
const COLOR_RANGE_FULL: u32 = 1u;

const MATRIX_BT709: u32 = 0u;
const MATRIX_BT601: u32 = 1u;
const MATRIX_BT2020_NCL: u32 = 2u;
const MATRIX_SMPTE240: u32 = 3u;
const MATRIX_IDENTITY: u32 = 4u;

const TRANSFER_BT709: u32 = 0u;
const TRANSFER_SRGB: u32 = 1u;
const TRANSFER_LINEAR: u32 = 2u;
const TRANSFER_PQ: u32 = 3u;
const TRANSFER_HLG: u32 = 4u;
const TRANSFER_GAMMA22: u32 = 5u;
const TRANSFER_GAMMA28: u32 = 6u;

const PRIMARIES_BT709: u32 = 0u;
const PRIMARIES_BT2020: u32 = 1u;
const PRIMARIES_BT601_525: u32 = 2u;
const PRIMARIES_BT601_625: u32 = 3u;
const PRIMARIES_DCI_P3: u32 = 4u;
const PRIMARIES_DISPLAY_P3: u32 = 5u;

// Camera log curves — IDs >= 100, matching klipp_core::color_ids.
const TRANSFER_SLOG3: u32 = 100u;
const TRANSFER_SLOG2: u32 = 101u;
const TRANSFER_VLOG: u32 = 102u;
const TRANSFER_LOGC3: u32 = 103u;
const TRANSFER_LOGC4: u32 = 104u;
const TRANSFER_CANON_LOG: u32 = 105u;
const TRANSFER_CANON_LOG3: u32 = 106u;
const TRANSFER_RED_LOG3G10: u32 = 107u;
const TRANSFER_BMD_FILM_GEN5: u32 = 108u;
const TRANSFER_DJI_DLOG: u32 = 109u;
const TRANSFER_FLOG: u32 = 110u;
const TRANSFER_APPLE_LOG: u32 = 111u;
const TRANSFER_NLOG: u32 = 112u;
const TRANSFER_GOPRO_FLAT: u32 = 113u;

// Camera-native gamuts — IDs >= 100.
const PRIMARIES_S_GAMUT3_CINE: u32 = 100u;
const PRIMARIES_S_GAMUT3: u32 = 101u;
const PRIMARIES_V_GAMUT: u32 = 102u;
const PRIMARIES_CANON_CINEMA_GAMUT: u32 = 103u;
const PRIMARIES_ALEXA_WIDE_GAMUT_3: u32 = 104u;
const PRIMARIES_ALEXA_WIDE_GAMUT_4: u32 = 105u;
const PRIMARIES_RED_WIDE_GAMUT: u32 = 106u;
const PRIMARIES_BMD_WIDE_GAMUT: u32 = 107u;
"#;

/// Helper functions for the four conversion stages:
///
/// 1. `yuv_normalise(y, u, v, range, bit_depth_max)` — raw codepoint
///    `f32` → normalised `(Y in [0,1], U/V in ~[-0.5,+0.5])`.
/// 2. `yuv_to_rgb_display(yuv, matrix_id)` → display-encoded RGB
///    (gamma-encoded for SDR; PQ/HLG-encoded for HDR transfers).
/// 3. `display_to_linear(rgb, transfer_id)` → linear-light in source
///    primaries.
/// 4. `primaries_to_bt709(rgb, primaries_id)` → linear in the engine's
///    BT.709 working space.
///
/// Plus `linear_to_srgb_vec` for re-encoding to the existing sRGB-u8
/// hand-off shape consumed by `rgba_u8_to_linear` downstream.
pub const WGSL_HELPERS: &str = r#"
fn yuv_normalise(y_val: f32, u_val: f32, v_val: f32, range: u32, bit_depth_max: f32) -> vec3<f32> {
    if range == COLOR_RANGE_FULL {
        return vec3<f32>(
            y_val / bit_depth_max,
            (u_val - bit_depth_max * 0.5 - 0.5) / bit_depth_max,
            (v_val - bit_depth_max * 0.5 - 0.5) / bit_depth_max,
        );
    }
    // Limited range. Spec gives explicit head/foot at each bit
    // depth: 8-bit Y in [16,235] / UV in [16,240]; 10-bit Y in
    // [64,940] / UV in [64,960]; same ratios at 12 bits. Encode by
    // scaling the 8-bit constants up.
    let scale = bit_depth_max / 255.0;
    let y_off = 16.0 * scale;
    let y_div = 219.0 * scale;
    let uv_off = 128.0 * scale;
    let uv_div = 224.0 * scale;
    return vec3<f32>(
        (y_val - y_off) / y_div,
        (u_val - uv_off) / uv_div,
        (v_val - uv_off) / uv_div,
    );
}

fn yuv_to_rgb_display(yuv: vec3<f32>, matrix_id: u32) -> vec3<f32> {
    let y = yuv.x;
    let u = yuv.y;
    let v = yuv.z;
    if matrix_id == MATRIX_BT601 {
        return vec3<f32>(
            y + 1.402 * v,
            y - 0.344136 * u - 0.714136 * v,
            y + 1.772 * u,
        );
    }
    if matrix_id == MATRIX_BT2020_NCL {
        return vec3<f32>(
            y + 1.4746 * v,
            y - 0.16455 * u - 0.57135 * v,
            y + 1.8814 * u,
        );
    }
    if matrix_id == MATRIX_SMPTE240 {
        return vec3<f32>(
            y + 1.576 * v,
            y - 0.227 * u - 0.477 * v,
            y + 1.826 * u,
        );
    }
    if matrix_id == MATRIX_IDENTITY {
        // ffmpeg's `AVCOL_SPC_RGB`: the "Y/U/V" planes are actually
        // G/B/R. Real NV12 sources never hit this path.
        return vec3<f32>(v, y, u);
    }
    // BT.709 — also the default for unrecognised IDs.
    return vec3<f32>(
        y + 1.5748 * v,
        y - 0.1873 * u - 0.4681 * v,
        y + 1.8556 * u,
    );
}

fn srgb_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    if s <= 0.04045 { return s / 12.92; }
    return pow((s + 0.055) / 1.055, 2.4);
}

fn pq_eotf_scalar(c: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let p = pow(max(c, 0.0), 1.0 / m2);
    let num = max(p - c1, 0.0);
    let den = c2 - c3 * p;
    return pow(num / max(den, 1e-6), 1.0 / m1);
}

fn hlg_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let a = 0.17883277;
    let b = 0.28466892;
    let cc = 0.55991073;
    if s <= 0.5 { return (s * s) / 3.0; }
    return (exp((s - cc) / a) + b) / 12.0;
}

// Sony S-Log3 inverse OETF (display-encoded → scene-linear).
// Sony's published formula. Output is scene-referred linear with
// 0.18 = mid-grey reflectance.
fn slog3_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    // Code-value breakpoint at 171.2102946929 / 1023.
    let cv = s * 1023.0;
    if cv >= 171.2102946929 {
        return (pow(10.0, (cv - 420.0) / 261.5)) * (0.18 + 0.01) - 0.01;
    }
    return (cv - 95.0) * 0.01125000 / (171.2102946929 - 95.0);
}

// Sony S-Log2 inverse OETF.
fn slog2_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    // Sony S-Log2 reference: y = (0.432699 * log10(x*0.166666 +
    // 0.037584) + 0.616596 + 0.03), inverted analytically.
    return (pow(10.0, (s - 0.616596 - 0.03) / 0.432699) - 0.037584) / 0.166666;
}

// Panasonic V-Log inverse OETF.
fn vlog_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let cut2: f32 = 0.181;
    if s < cut2 {
        return (s - 0.125) / 5.6;
    }
    return pow(10.0, (s - 0.598206) / 0.241514) - 0.00873;
}

// Canon Log inverse OETF.
fn canon_log_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let normed = (s - 0.0730597) / 0.529136;
    // Re-derive linear from log10 form: y = 0.529136 * log10(10.1596 * x + 1) + 0.0730597
    return (pow(10.0, normed) - 1.0) / 10.1596;
}

// Canon Log 3 inverse OETF.
fn canon_log3_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    if s < 0.04076 {
        return (s - 0.092864) / 2.3613;
    }
    if s <= 0.105357 {
        return (s - 0.073059) / 2.3613;
    }
    return (pow(10.0, (s - 0.069886) / 0.42889) - 1.0) / 14.98325;
}

// ARRI Log C3 (LogC3 EI 800) inverse OETF.
fn logc3_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let cut: f32 = 0.149658;
    if s > cut {
        return (pow(10.0, (s - 0.385537) / 0.247190) - 0.052272) / 5.555556;
    }
    return (s - 0.092809) / 5.367655;
}

// ARRI Log C4 inverse OETF.
fn logc4_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let a: f32 = 2231.82854938;
    let b: f32 = 0.092864125;
    let c1: f32 = 0.069885769;
    if s < c1 {
        return (s - 0.092864125) / 5.555556;
    }
    return (pow(2.0, (s * 14.0 - 6.0)) - b) / a;
}

// RED Log3G10 inverse OETF.
fn red_log3g10_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    return (pow(10.0, s / 0.224282) - 1.0) / 155.975327;
}

// Blackmagic Film Gen 5 inverse OETF (approximate; BMD doesn't
// publish a closed-form curve).
fn bmd_film_gen5_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let cut: f32 = 0.005;
    if s < cut { return s / 8.0; }
    return pow(10.0, (s - 0.385537) / 0.245) - 0.0173;
}

// DJI D-Log inverse OETF.
fn dji_dlog_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    if s <= 0.14 {
        return (s - 0.0929) / 6.025;
    }
    return (pow(10.0, (s - 0.584555) / 0.256663) - 0.0108) / 0.9892;
}

// Fuji F-Log inverse OETF.
fn flog_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let a: f32 = 0.555556;
    if s < 0.100537 {
        return (s - 0.092864) / 1.970095;
    }
    return (pow(10.0, (s - 0.790453) / 0.344676) - 0.064829) / a;
}

// Apple Log inverse OETF (iPhone 15 Pro+) — Apple Log Profile,
// 2023 whitepaper. Three-segment curve with constants:
//   R0=-0.05641088, RT=0.01, C=47.28711236,
//   BETA=0.00964052, GAMMA=0.08550479, DELTA=0.69336945.
// Pt (toe-to-log transition in V space) = C*(RT-R0)^2 ≈ 0.20859.
// Mirrors klipp_color::camera_log::apple_log_to_linear exactly so
// the GPU and CPU paths produce identical scene-linear values.
// Previous body was a placeholder scaffold (s/357.04167 toe,
// 2^((s-0.47132)/0.05)*0.01 log) — wrong by ~13× at mid-gray.
fn apple_log_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let r0: f32 = -0.05641088;
    let rt: f32 = 0.01;
    let c_const: f32 = 47.28711236;
    let beta: f32 = 0.00964052;
    let gamma: f32 = 0.08550479;
    let delta: f32 = 0.69336945;
    let pt: f32 = c_const * (rt - r0) * (rt - r0);
    if s < pt {
        return sqrt(s / c_const) + r0;
    }
    return pow(2.0, (s - delta) / gamma) - beta;
}

// Nikon N-Log inverse OETF (Z 6/7/8/9).
// Two-segment per the Nikon N-Log Reference (2018):
//   V <  Vt:  L = ((V - 0.0928)/0.6007)^3 - 0.0075   (cube-root toe)
//   V >= Vt:  L = exp((V - 0.619)/0.14)               (log)
// Vt ≈ 0.5101 — the V-space value where the toe expression last
// holds. Mirrors klipp_color::camera_log::nlog_to_linear exactly
// so GPU and CPU produce identical scene-linear values. Previous
// body used a placeholder cube-of-(s+0.0075)/1242.0 toe and a log
// expression with a literal `/0.328*0.328` no-op — wrong on every
// Nikon N-Log clip flowing through the GPU input-transform path.
fn nlog_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    let v_break: f32 = 0.5101;
    if s < v_break {
        let t = (s - 0.0928) / 0.6007;
        return t * t * t - 0.0075;
    }
    return exp((s - 0.619) / 0.14);
}

// GoPro ProTune Flat inverse OETF — approximated as gamma 1.6 since
// GoPro doesn't publish the exact curve.
fn gopro_flat_eotf_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    return pow(s, 1.6);
}

// Wrap a per-channel log EOTF scalar so its output is clamped to
// non-negative. Below-foot signals (camera noise floor) otherwise
// produce small negatives that, after the gamut-conversion matrix
// and display_transform's 0-clamp, leave the strongest channel as
// the only survivor — manifests as a strong colour cast on dark
// scenes (the "everything looks red" symptom on stage footage).
fn pos3(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(max(rgb.x, 0.0), max(rgb.y, 0.0), max(rgb.z, 0.0));
}

fn apply_log_eotf(rgb: vec3<f32>, transfer_id: u32) -> vec3<f32> {
    if transfer_id == TRANSFER_SLOG3 {
        return pos3(vec3<f32>(slog3_eotf_scalar(rgb.x), slog3_eotf_scalar(rgb.y), slog3_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_SLOG2 {
        return pos3(vec3<f32>(slog2_eotf_scalar(rgb.x), slog2_eotf_scalar(rgb.y), slog2_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_VLOG {
        return pos3(vec3<f32>(vlog_eotf_scalar(rgb.x), vlog_eotf_scalar(rgb.y), vlog_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_CANON_LOG {
        return pos3(vec3<f32>(canon_log_eotf_scalar(rgb.x), canon_log_eotf_scalar(rgb.y), canon_log_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_CANON_LOG3 {
        return pos3(vec3<f32>(canon_log3_eotf_scalar(rgb.x), canon_log3_eotf_scalar(rgb.y), canon_log3_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_LOGC3 {
        return pos3(vec3<f32>(logc3_eotf_scalar(rgb.x), logc3_eotf_scalar(rgb.y), logc3_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_LOGC4 {
        return pos3(vec3<f32>(logc4_eotf_scalar(rgb.x), logc4_eotf_scalar(rgb.y), logc4_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_RED_LOG3G10 {
        return pos3(vec3<f32>(red_log3g10_eotf_scalar(rgb.x), red_log3g10_eotf_scalar(rgb.y), red_log3g10_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_BMD_FILM_GEN5 {
        return pos3(vec3<f32>(bmd_film_gen5_eotf_scalar(rgb.x), bmd_film_gen5_eotf_scalar(rgb.y), bmd_film_gen5_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_DJI_DLOG {
        return pos3(vec3<f32>(dji_dlog_eotf_scalar(rgb.x), dji_dlog_eotf_scalar(rgb.y), dji_dlog_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_FLOG {
        return pos3(vec3<f32>(flog_eotf_scalar(rgb.x), flog_eotf_scalar(rgb.y), flog_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_APPLE_LOG {
        return pos3(vec3<f32>(apple_log_eotf_scalar(rgb.x), apple_log_eotf_scalar(rgb.y), apple_log_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_NLOG {
        return pos3(vec3<f32>(nlog_eotf_scalar(rgb.x), nlog_eotf_scalar(rgb.y), nlog_eotf_scalar(rgb.z)));
    }
    if transfer_id == TRANSFER_GOPRO_FLAT {
        return pos3(vec3<f32>(gopro_flat_eotf_scalar(rgb.x), gopro_flat_eotf_scalar(rgb.y), gopro_flat_eotf_scalar(rgb.z)));
    }
    return rgb;
}

fn display_to_linear(rgb: vec3<f32>, transfer_id: u32) -> vec3<f32> {
    if transfer_id == TRANSFER_LINEAR { return rgb; }
    if transfer_id == TRANSFER_PQ {
        return vec3<f32>(pq_eotf_scalar(rgb.x), pq_eotf_scalar(rgb.y), pq_eotf_scalar(rgb.z));
    }
    if transfer_id == TRANSFER_HLG {
        return vec3<f32>(hlg_eotf_scalar(rgb.x), hlg_eotf_scalar(rgb.y), hlg_eotf_scalar(rgb.z));
    }
    if transfer_id == TRANSFER_GAMMA22 {
        return vec3<f32>(pow(max(rgb.x, 0.0), 2.2), pow(max(rgb.y, 0.0), 2.2), pow(max(rgb.z, 0.0), 2.2));
    }
    if transfer_id == TRANSFER_GAMMA28 {
        return vec3<f32>(pow(max(rgb.x, 0.0), 2.8), pow(max(rgb.y, 0.0), 2.8), pow(max(rgb.z, 0.0), 2.8));
    }
    if transfer_id >= 100u {
        // Camera log curves live above 100; route to apply_log_eotf
        // which dispatches to the per-camera analytic formula.
        return apply_log_eotf(rgb, transfer_id);
    }
    // BT.709 / BT.601 / BT.2020-{10,12} all use the BT.1886 shape,
    // which matches sRGB closely enough at u8 precision. Default for
    // unrecognised transfer IDs as well.
    return vec3<f32>(srgb_eotf_scalar(rgb.x), srgb_eotf_scalar(rgb.y), srgb_eotf_scalar(rgb.z));
}

fn primaries_to_bt709(rgb: vec3<f32>, primaries_id: u32) -> vec3<f32> {
    if primaries_id == PRIMARIES_BT2020 {
        return vec3<f32>(
             1.6605 * rgb.x - 0.5876 * rgb.y - 0.0728 * rgb.z,
            -0.1246 * rgb.x + 1.1329 * rgb.y - 0.0083 * rgb.z,
            -0.0182 * rgb.x - 0.1006 * rgb.y + 1.1187 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_BT601_525 {
        return vec3<f32>(
             0.93954 * rgb.x + 0.05033 * rgb.y + 0.01013 * rgb.z,
             0.01788 * rgb.x + 0.96567 * rgb.y + 0.01646 * rgb.z,
            -0.00164 * rgb.x - 0.00492 * rgb.y + 1.00656 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_BT601_625 {
        return vec3<f32>(
             1.0440 * rgb.x - 0.0440 * rgb.y + 0.0000 * rgb.z,
             0.0000 * rgb.x + 1.0000 * rgb.y + 0.0000 * rgb.z,
             0.0000 * rgb.x + 0.0118 * rgb.y + 0.9882 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_DCI_P3 || primaries_id == PRIMARIES_DISPLAY_P3 {
        return vec3<f32>(
             1.2249 * rgb.x - 0.2247 * rgb.y + 0.0000 * rgb.z,
            -0.0420 * rgb.x + 1.0419 * rgb.y + 0.0000 * rgb.z,
            -0.0197 * rgb.x - 0.0786 * rgb.y + 1.0979 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_S_GAMUT3_CINE {
        // Sony S-Gamut3.Cine → BT.709 (D65, derived from Sony's
        // published primaries chromaticities).
        return vec3<f32>(
             1.6266404351 * rgb.x - 0.5402460389 * rgb.y - 0.0863943962 * rgb.z,
            -0.1040630717 * rgb.x + 1.1066743058 * rgb.y - 0.0026112341 * rgb.z,
            -0.0035849832 * rgb.x - 0.0793220782 * rgb.y + 1.0828952420 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_S_GAMUT3 {
        // Sony S-Gamut3 → BT.709.
        return vec3<f32>(
             1.5523622260 * rgb.x - 0.4338073145 * rgb.y - 0.1185549155 * rgb.z,
            -0.1727860457 * rgb.x + 1.2107692775 * rgb.y - 0.0379832320 * rgb.z,
            -0.0163914533 * rgb.x - 0.0922769512 * rgb.y + 1.1086684045 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_V_GAMUT {
        // Panasonic V-Gamut → BT.709 (D65).
        return vec3<f32>(
             1.806576 * rgb.x - 0.695697 * rgb.y - 0.110879 * rgb.z,
            -0.170395 * rgb.x + 1.305812 * rgb.y - 0.135417 * rgb.z,
            -0.020835 * rgb.x - 0.149682 * rgb.y + 1.170517 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_CANON_CINEMA_GAMUT {
        // Canon Cinema Gamut → BT.709 (D65).
        return vec3<f32>(
             1.6630010742 * rgb.x - 0.4274550481 * rgb.y - 0.2355460261 * rgb.z,
            -0.1018375801 * rgb.x + 1.4476878574 * rgb.y - 0.3458502773 * rgb.z,
             0.0046119080 * rgb.x - 0.0763517721 * rgb.y + 1.0717398641 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_ALEXA_WIDE_GAMUT_3 {
        // ARRI Alexa Wide Gamut 3 → BT.709.
        return vec3<f32>(
             1.485583 * rgb.x - 0.401961 * rgb.y - 0.083622 * rgb.z,
            -0.077419 * rgb.x + 1.176592 * rgb.y - 0.099173 * rgb.z,
             0.008382 * rgb.x - 0.072473 * rgb.y + 1.064091 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_ALEXA_WIDE_GAMUT_4 {
        // ARRI Alexa Wide Gamut 4 → BT.709 (Alexa 35).
        return vec3<f32>(
             1.789066 * rgb.x - 0.620925 * rgb.y - 0.168141 * rgb.z,
            -0.245664 * rgb.x + 1.422466 * rgb.y - 0.176802 * rgb.z,
            -0.001624 * rgb.x - 0.142113 * rgb.y + 1.143737 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_RED_WIDE_GAMUT {
        // RED Wide Gamut RGB → BT.709 (RWG → REC709, REDcine X).
        return vec3<f32>(
             1.412345 * rgb.x - 0.524711 * rgb.y + 0.112366 * rgb.z,
            -0.177228 * rgb.x + 1.337117 * rgb.y - 0.159889 * rgb.z,
             0.024029 * rgb.x - 0.230596 * rgb.y + 1.206567 * rgb.z,
        );
    }
    if primaries_id == PRIMARIES_BMD_WIDE_GAMUT {
        // Blackmagic Wide Gamut Gen 5 → BT.709.
        return vec3<f32>(
             1.4842933208 * rgb.x - 0.4234837425 * rgb.y - 0.0608095783 * rgb.z,
            -0.0958915308 * rgb.x + 1.1813166193 * rgb.y - 0.0854250885 * rgb.z,
            -0.0010055519 * rgb.x - 0.0529880375 * rgb.y + 1.0539935894 * rgb.z,
        );
    }
    return rgb;
}

fn linear_to_srgb_scalar(c: f32) -> f32 {
    let s = clamp(c, 0.0, 1.0);
    if s <= 0.0031308 { return s * 12.92; }
    return 1.055 * pow(s, 1.0 / 2.4) - 0.055;
}

fn linear_to_srgb_vec(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_scalar(rgb.x), linear_to_srgb_scalar(rgb.y), linear_to_srgb_scalar(rgb.z));
}
"#;

#[cfg(test)]
mod tests {
    /// Pin the numeric IDs so a future klipp-core renumbering breaks
    /// the build instead of producing silently wrong shader output.
    /// If this test fails, update both `klipp_core::color_ids` AND
    /// the `WGSL_PRELUDE` constant in this file to match.
    #[test]
    fn pinned_ids_match_klipp_core() {
        // We can't import klipp-core here (cycle), so just pin the
        // numeric values literally. The companion test on the
        // klipp-core side
        // (`color_ids::tests::ids_for_default_bt709_limited_sdr` and
        // friends) pins them from the other direction.
        let prelude = super::WGSL_PRELUDE;
        assert!(prelude.contains("COLOR_RANGE_LIMITED: u32 = 0u"));
        assert!(prelude.contains("COLOR_RANGE_FULL: u32 = 1u"));
        assert!(prelude.contains("MATRIX_BT709: u32 = 0u"));
        assert!(prelude.contains("MATRIX_BT601: u32 = 1u"));
        assert!(prelude.contains("MATRIX_BT2020_NCL: u32 = 2u"));
        assert!(prelude.contains("MATRIX_SMPTE240: u32 = 3u"));
        assert!(prelude.contains("MATRIX_IDENTITY: u32 = 4u"));
        assert!(prelude.contains("TRANSFER_BT709: u32 = 0u"));
        assert!(prelude.contains("TRANSFER_SRGB: u32 = 1u"));
        assert!(prelude.contains("TRANSFER_LINEAR: u32 = 2u"));
        assert!(prelude.contains("TRANSFER_PQ: u32 = 3u"));
        assert!(prelude.contains("TRANSFER_HLG: u32 = 4u"));
        assert!(prelude.contains("TRANSFER_GAMMA22: u32 = 5u"));
        assert!(prelude.contains("TRANSFER_GAMMA28: u32 = 6u"));
        assert!(prelude.contains("PRIMARIES_BT709: u32 = 0u"));
        assert!(prelude.contains("PRIMARIES_BT2020: u32 = 1u"));
        assert!(prelude.contains("PRIMARIES_BT601_525: u32 = 2u"));
        assert!(prelude.contains("PRIMARIES_BT601_625: u32 = 3u"));
        assert!(prelude.contains("PRIMARIES_DCI_P3: u32 = 4u"));
        assert!(prelude.contains("PRIMARIES_DISPLAY_P3: u32 = 5u"));
    }

    #[test]
    fn helpers_define_required_functions() {
        let h = super::WGSL_HELPERS;
        for name in [
            "fn yuv_normalise(",
            "fn yuv_to_rgb_display(",
            "fn display_to_linear(",
            "fn primaries_to_bt709(",
            "fn linear_to_srgb_vec(",
            "fn srgb_eotf_scalar(",
            "fn pq_eotf_scalar(",
            "fn hlg_eotf_scalar(",
        ] {
            assert!(h.contains(name), "missing WGSL helper: {name}");
        }
    }
}
