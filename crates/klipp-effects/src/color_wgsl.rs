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

/// Pack a row-major 3x3 matrix into the WGSL-friendly layout used by
/// the decoder shaders' `Params` uniform: three rows, each padded to
/// `vec4<f32>` so consecutive rows land on 16-byte boundaries (WGSL's
/// alignment rule for `vec3<f32>` in uniform memory). The 4th element
/// of each row is unused padding; callers don't read it.
///
/// The shader dots the input RGB against each row to produce the
/// output channel — i.e. `out_r = dot(row0, rgb)` etc. That keeps the
/// WGSL identical regardless of which working space the matrix
/// represents (BT.709 / ACEScg / future BT.2020 etc.).
#[inline]
pub fn pack_primaries_matrix(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[0][1], m[0][2], 0.0],
        [m[1][0], m[1][1], m[1][2], 0.0],
        [m[2][0], m[2][1], m[2][2], 0.0],
    ]
}

/// 3x3 identity in the same packed layout. Useful as a default when a
/// caller doesn't have the source colourspace yet (e.g. the tests
/// that exercise BT.709 SDR sources where source-primaries already
/// match the working-space basis).
pub const IDENTITY_PRIMARIES_MATRIX_PACKED: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

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

// Sony S-Log2: S-Log scaled by 219/155, S-Log applied to the legal-range
// normalised value. Mirrors klipp_color::camera_log::slog2_to_linear.
fn slog2_eotf_scalar(c: f32) -> f32 {
    let y = clamp(c, 0.0, 1.0);
    let x = (y * 1023.0 - 64.0) / 876.0;
    let knee = ((0.432699 * log(0.037584) / log(10.0) + 0.616596 + 0.03) * 876.0 + 64.0) / 1023.0;
    var slog: f32;
    if (y >= knee) {
        slog = pow(10.0, (x - 0.616596 - 0.03) / 0.432699) - 0.037584;
    } else {
        slog = (x - 0.030001223) / 5.0;
    }
    return 219.0 * (slog * 0.9) / 155.0;
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

// Canon Log (v1), legal-range normalised, x 0.9 to reflectance.
// Mirrors klipp_color::camera_log::canon_log_to_linear.
fn canon_log_eotf_scalar(c: f32) -> f32 {
    let x = (clamp(c, 0.0, 1.0) * 1023.0 - 64.0) / 876.0;
    var lin: f32;
    if (x < 0.0730597) {
        lin = -(pow(10.0, (0.0730597 - x) / 0.529136) - 1.0) / 10.1596;
    } else {
        lin = (pow(10.0, (x - 0.0730597) / 0.529136) - 1.0) / 10.1596;
    }
    return lin * 0.9;
}

// Canon Log 3, three segments, x 0.9 so 18% grey lands on 0.18.
// Mirrors klipp_color::camera_log::canon_log3_to_linear.
fn canon_log3_eotf_scalar(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    var lin: f32;
    if (x < 0.097465473) {
        lin = -(pow(10.0, (0.12783901 - x) / 0.36726845) - 1.0) / 14.98325;
    } else if (x <= 0.15277891) {
        lin = (x - 0.12512219) / 1.9754798;
    } else {
        lin = (pow(10.0, (x - 0.12240537) / 0.36726845) - 1.0) / 14.98325;
    }
    return lin * 0.9;
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

// ARRI LogC4, per ARRI's LogC4 specification (2022). Constants:
// a = (2^18 - 16) / 117.45, b = (1023 - 95) / 1023, c = 95 / 1023.
// Mirrors klipp_color::camera_log::logc4_to_linear.
fn logc4_eotf_scalar(c: f32) -> f32 {
    let e = clamp(c, 0.0, 1.0);
    let a = (262144.0 - 16.0) / 117.45;
    let b = (1023.0 - 95.0) / 1023.0;
    let cc = 95.0 / 1023.0;
    return (exp2(14.0 * (e - cc) / b + 6.0) - 64.0) / a;
}

// RED Log3G10 (v2), with its 0.01 linear offset.
// Mirrors klipp_color::camera_log::red_log3g10_to_linear.
fn red_log3g10_eotf_scalar(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    return (pow(10.0, x / 0.224282) - 1.0) / 155.975327 - 0.01;
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

// Fujifilm F-Log. Mirrors klipp_color::camera_log::flog_to_linear.
fn flog_eotf_scalar(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x < 0.100537775) {
        return (x - 0.092864) / 8.735631;
    }
    return pow(10.0, (x - 0.790453) / 0.344676) / 0.555556 - 0.009468 / 0.555556;
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

// Nikon N-Log: 650/150/619 over 1023 in Nikon's integer form.
// Mirrors klipp_color::camera_log::nlog_to_linear.
fn nlog_eotf_scalar(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x < 0.44183773) {
        let t = x / 0.63538612;
        return t * t * t - 0.0075;
    }
    return exp((x - 0.6050831) / 0.14662757);
}

// GoPro Protune (Flat). Mirrors klipp_color::camera_log::gopro_flat_to_linear.
fn gopro_flat_eotf_scalar(c: f32) -> f32 {
    return (pow(113.0, clamp(c, 0.0, 1.0)) - 1.0) / 112.0;
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

// `primaries_to_bt709` used to live here as a switch on `primaries_id`.
// Removed when the gamut-conversion stage moved to a per-frame uniform
// matrix (see `Params.primaries_row{0,1,2}` in `nv12_gpu.rs` /
// `p010_gpu.rs`). Pre-computing the matrix on the CPU (via
// `klipp_color::camera_log::primaries_to_working_matrix`) and dotting
// it against `rgb` in WGSL keeps the shader simple AND lets a single
// matmul handle any working space (BT.709, ACEScg, future BT.2020),
// not just the BT.709 destination the switch was hard-coded for.

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
            "fn linear_to_srgb_vec(",
            "fn srgb_eotf_scalar(",
            "fn pq_eotf_scalar(",
            "fn hlg_eotf_scalar(",
        ] {
            assert!(h.contains(name), "missing WGSL helper: {name}");
        }
    }

    /// Gamut conversion now lives in the decoder shaders' `Params`
    /// uniform, not in `WGSL_HELPERS`. The old switch-on-primaries-id
    /// function is intentionally absent; this test pins that decision
    /// so a future "let's bring it back" tweak fails loudly and gets
    /// a justification in the diff.
    #[test]
    fn helpers_does_not_define_legacy_primaries_to_bt709() {
        assert!(
            !super::WGSL_HELPERS.contains("fn primaries_to_bt709("),
            "primaries_to_bt709 helper was deliberately removed when \
             gamut conversion moved to a uniform matrix; reintroducing \
             it without removing the matrix path would be two sources \
             of truth",
        );
    }

    /// Identity-matrix smoke test for the matrix-packing helper. The
    /// shader treats each row as a `vec3<f32>` (with the `.w` slot
    /// unused padding); the helper must place the identity diagonal
    /// at `[i][i]` and zero everything else.
    #[test]
    fn pack_primaries_matrix_identity() {
        let id = super::pack_primaries_matrix([
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);
        assert_eq!(id, super::IDENTITY_PRIMARIES_MATRIX_PACKED);
    }

    #[test]
    fn pack_primaries_matrix_preserves_first_three_columns() {
        // Arbitrary matrix; each input row shows up in the first three
        // slots of the corresponding output row, with 0.0 padding.
        let m = [
            [0.1, 0.2, 0.3],
            [0.4, 0.5, 0.6],
            [0.7, 0.8, 0.9],
        ];
        let p = super::pack_primaries_matrix(m);
        for i in 0..3 {
            for j in 0..3 {
                assert_eq!(p[i][j], m[i][j], "row {i} col {j}");
            }
            assert_eq!(p[i][3], 0.0, "row {i} pad slot");
        }
    }
}
