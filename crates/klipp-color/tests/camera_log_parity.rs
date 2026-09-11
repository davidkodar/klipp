//! Klipp's camera log curves against independent references, across all
//! 1024 10-bit code values.
//!
//! References: OpenColorIO's built-in manufacturer transforms
//! (tools/gen_ocio_camera_log_reference.py) where OCIO has them,
//! colour-science otherwise (tools/gen_colour_camera_log_reference.py).
//! colour-science was itself checked against OCIO on the seven formats
//! both cover and agreed to within 2e-5 stops.
//!
//! Before this test existed Klipp carried two copies of every curve —
//! one for the decoder, one for the InputTransform node — and between
//! them nine of fourteen formats were wrong by stops: LogC4 by 20, D-Log
//! by 23, Canon Log 3 negative at mid grey. There is now one copy.

use klipp_core::{ColorTransfer, LogFormat};

/// (fixture, format, decoder transfer if the decoder can decode it)
const FORMATS: &[(&str, LogFormat, Option<ColorTransfer>)] = &[
    ("ocio_camlog_SLog3", LogFormat::SLog3, Some(ColorTransfer::SLog3)),
    ("ocio_camlog_LogC3", LogFormat::LogC3, Some(ColorTransfer::LogC3)),
    ("ocio_camlog_LogC4", LogFormat::LogC4, Some(ColorTransfer::LogC4)),
    ("ocio_camlog_VLog", LogFormat::VLog, Some(ColorTransfer::VLog)),
    ("ocio_camlog_CanonLog3", LogFormat::CanonLog3, Some(ColorTransfer::CanonLog3)),
    ("ocio_camlog_RedLog3G10", LogFormat::RedLog3G10, Some(ColorTransfer::RedLog3G10)),
    ("ocio_camlog_AppleLog", LogFormat::AppleLog, Some(ColorTransfer::AppleLog)),
    ("colour_camlog_SLog2", LogFormat::SLog2, Some(ColorTransfer::SLog2)),
    ("colour_camlog_CanonLog", LogFormat::CanonLog, Some(ColorTransfer::CanonLog)),
    ("colour_camlog_DJIDLog", LogFormat::DJIDLog, Some(ColorTransfer::DJIDLog)),
    ("colour_camlog_FLog", LogFormat::FLog, Some(ColorTransfer::FLog)),
    ("colour_camlog_NLog", LogFormat::NLog, Some(ColorTransfer::NLog)),
    ("colour_camlog_GoProFlat", LogFormat::GoProFlat, Some(ColorTransfer::GoProFlat)),
    ("colour_camlog_LeicaLLog", LogFormat::LeicaLLog, None),
];

/// Where there is light to measure: error in stops. Nikon N-Log's two
/// published segments meet with a 0.09% (1.3e-3 stop) step at their
/// breakpoint, and which side code 452 lands on is float rounding — so
/// the bound sits just above that. Every other format measures ~1e-5.
const MAX_STOPS: f32 = 2e-3;
/// Near black, where ratios explode and nobody grades by stops: absolute.
const MAX_ABS_NEAR_BLACK: f32 = 1e-4;

fn fixture(name: &str) -> Vec<(f32, f32)> {
    let path = format!("{}/tests/fixtures/{name}.csv", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("code"))
        .map(|l| {
            let mut it = l.split(',');
            (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
        })
        .collect()
}

#[test]
fn every_camera_log_curve_matches_its_reference_at_every_code() {
    let mut failures = Vec::new();
    for (name, format, _) in FORMATS {
        let (mut stops, mut abs) = (0.0_f32, 0.0_f32);
        for (code, reference) in fixture(name) {
            let klipp = klipp_color::transforms::log_to_linear(code, *format);
            if reference > 0.01 && klipp > 0.0 {
                stops = stops.max((klipp / reference).log2().abs());
            } else {
                abs = abs.max((klipp - reference).abs());
            }
        }
        eprintln!("{name:<26} {stops:.2e} stops, {abs:.2e} abs near black");
        if stops > MAX_STOPS || abs > MAX_ABS_NEAR_BLACK {
            failures.push(format!("{name}: {stops:.2e} stops, {abs:.2e} abs"));
        }
    }
    assert!(failures.is_empty(), "curves off their reference:\n  {}", failures.join("\n  "));
}

/// The decoder and the node share one curve. The decoder's only
/// difference is policy: it clamps sub-black toe values to 0.
#[test]
fn the_decoder_uses_the_same_curve_clamped_at_zero() {
    for (name, format, transfer) in FORMATS {
        let Some(transfer) = transfer else { continue };
        for i in 0..1024 {
            let c = i as f32 / 1023.0;
            let shared = klipp_color::camera_log::log_format_to_linear(c, *format).max(0.0);
            let decoder = klipp_color::camera_log::apply_inverse_eotf(c, *transfer);
            assert_eq!(decoder, shared, "{name} at code {i}: decoder and shared curve differ");
        }
    }
}

/// Every source-primaries -> BT.709 matrix, against colour-science
/// (tools/gen_colour_primaries_reference.py), element by element. The
/// hand-typed table this replaced was off by up to 0.57 per element for
/// camera gamuts; S-Gamut3, S-Gamut3.Cine and ARRI Wide Gamut 3 were also
/// checked against the matrices inside OCIO's camera transforms (~5e-3,
/// the manufacturers' own rounding).
#[test]
fn every_primaries_matrix_matches_colour_science() {
    use klipp_core::{ColorPrimaries as P, WorkingColorspace as W};
    let path = format!("{}/tests/fixtures/colour_primaries_to_bt709.csv", env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0;
    for line in std::fs::read_to_string(&path).unwrap().lines().filter(|l| !l.starts_with('#')) {
        let mut it = line.split(',');
        let name = it.next().unwrap();
        let reference: Vec<f32> = it.map(|v| v.parse().unwrap()).collect();
        let p = match name {
            "Bt2020" => P::Bt2020, "Bt601_525" => P::Bt601_525, "Bt601_625" => P::Bt601_625,
            "Bt470M" => P::Bt470M, "DciP3" => P::DciP3, "DisplayP3" => P::DisplayP3,
            "Ebu3213" => P::Ebu3213, "Smpte428" => P::Smpte428, "SGamut3" => P::SGamut3,
            "SGamut3Cine" => P::SGamut3Cine, "VGamut" => P::VGamut,
            "CanonCinemaGamut" => P::CanonCinemaGamut, "AlexaWideGamut3" => P::AlexaWideGamut3,
            "AlexaWideGamut4" => P::AlexaWideGamut4, "RedWideGamut" => P::RedWideGamut,
            "BmdWideGamut" => P::BmdWideGamut,
            other => panic!("fixture names unknown primaries {other}"),
        };
        let m = klipp_color::camera_log::primaries_to_working_matrix(p, W::Rec709Linear);
        let worst = m.iter().flatten().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(worst < 1e-6, "{name}: matrix off colour-science by {worst:.2e}");
        // And the per-pixel form is the same table.
        let px = klipp_color::camera_log::primaries_to_bt709([0.3, 0.6, 0.1], p);
        let want = [
            m[0][0] * 0.3 + m[0][1] * 0.6 + m[0][2] * 0.1,
            m[1][0] * 0.3 + m[1][1] * 0.6 + m[1][2] * 0.1,
            m[2][0] * 0.3 + m[2][1] * 0.6 + m[2][2] * 0.1,
        ];
        assert_eq!(px, want, "{name}: per-pixel and matrix forms disagree");
        checked += 1;
    }
    assert_eq!(checked, 16, "every non-identity primaries set must be pinned");
}
