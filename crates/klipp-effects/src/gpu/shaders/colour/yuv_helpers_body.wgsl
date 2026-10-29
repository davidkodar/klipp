
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
    // [64,940] / UV in [64,960]; same ratios at 12 bits. BT.709 and
    // BT.2100 scale the 8-bit constants by 2^(n-8) — 4 at 10 bits —
    // not by (2^n - 1) / 255 = 4.012. The latter put 10-bit chroma zero
    // at 513.5 instead of 512, giving every neutral a faint cast, and
    // stretched luma to 64.19..942.6 — while the CPU decode path
    // (camera_log::yuv420p10le_to_linear) used the right 64/876/512/896.
    let scale = (bit_depth_max + 1.0) / 256.0;
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

// Leica L-Log inverse OETF. Equation (2) of the L-Log Reference Manual
// v1.6: a = 8, b = 0.09, c = 0.27, d = 1.3, e = 0.0115, f = 0.6, with the
// segments meeting at 0.1380.
fn leica_llog_eotf_scalar(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if x <= 0.138 {
        return (x - 0.09) / 8.0;
    }
    return (pow(10.0, (x - 0.6) / 0.27) - 0.0115) / 1.3;
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
    if transfer_id == TRANSFER_LEICA_LLOG {
        return pos3(vec3<f32>(leica_llog_eotf_scalar(rgb.x), leica_llog_eotf_scalar(rgb.y), leica_llog_eotf_scalar(rgb.z)));
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

// ─── Scene-referred display tone map (both decoders' previews) ───
// HDR → SDR display tone map. Input is scene-linear, 1.0 = 100 nits;
// output is display-referred linear in [0, 1]. Must match
// `klipp_color::hdr::hdr_display_tonemap` exactly — the preview here
// and a graded frame's tone map are the same curve by construction.
const HDR_DISPLAY_EXPOSURE: f32 = 0.18;
// Camera log: the exposure that keeps 18% grey at display mid grey.
// Must match `klipp_color::hdr::LOG_DISPLAY_EXPOSURE`.
const LOG_DISPLAY_EXPOSURE: f32 = 0.723171;
fn aces_filmic_scalar(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), 0.0, 1.0);
}

fn tone_map_hdr_vec(rgb: vec3<f32>, exposure: f32) -> vec3<f32> {
    let exposed = rgb * exposure;
    return vec3<f32>(
        aces_filmic_scalar(max(exposed.x, 0.0)),
        aces_filmic_scalar(max(exposed.y, 0.0)),
        aces_filmic_scalar(max(exposed.z, 0.0)),
    );
}

