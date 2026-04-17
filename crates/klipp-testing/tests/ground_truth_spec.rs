//! Ground-truth spec for the CRIT-7 linear-light migration.
//!
//! Each test here expresses *what the correct linear-light result
//! looks like*, independent of any production code path. The tests
//! are marked `#[ignore]` today because the helpers they call do not
//! exist yet — Phases 1-9 wire them up one at a time, and each phase
//! removes `#[ignore]` from the tests it now satisfies. By Phase 10
//! every test here runs on every CI pass.
//!
//! **Reading guide**: the #[ignore]d tests below are the migration
//! contract, in executable form. When `cargo test -p klipp-testing
//! -- --ignored` comes back clean, the migration is done.
//!
//! The non-ignored tests below are "always-on" — they exercise the
//! test harness itself (transfer curves, fixtures, PSNR math) and
//! must pass every commit from now on.

use klipp_testing::diff::{assert_exact, assert_psnr_at_least};
use klipp_testing::fixtures::{
    color_checker_24_linear, encode_linear_to_srgb_u8, linear_ramp, solid_linear,
    MID_GREY_LINEAR, MID_GREY_SRGB_U8,
};
use klipp_testing::transfer::{linear_to_srgb, srgb_to_linear, srgb_u8_to_linear};

// =====================================================================
// Always-on: the test harness itself.
// =====================================================================

#[test]
fn harness_transfer_mid_grey_anchor() {
    // 18 % linear == the sRGB value we call "mid grey."
    // If this ever fails, every linear-light assertion in this file
    // is meaningless.
    assert!((srgb_to_linear(118.0 / 255.0) - 0.18).abs() < 0.005);
}

#[test]
fn harness_fixtures_mid_grey_constant() {
    assert_eq!(MID_GREY_LINEAR, [0.18, 0.18, 0.18, 1.0]);
    let decoded = srgb_u8_to_linear(MID_GREY_SRGB_U8);
    for i in 0..3 {
        assert!((decoded[i] - 0.18).abs() < 0.005);
    }
}

#[test]
fn harness_identical_buffers_exact_match() {
    let buf = solid_linear(16, 16, MID_GREY_LINEAR);
    assert_exact(&buf, &buf, 0.0);
}

#[test]
fn harness_identical_buffers_infinite_psnr() {
    let buf = solid_linear(16, 16, MID_GREY_LINEAR);
    assert_psnr_at_least(&buf, &buf, 100.0);
}

#[test]
fn harness_colorchecker_loaded_ok() {
    let patches = color_checker_24_linear();
    // Last patch (black 2) sits near the bottom of the range.
    assert!(patches[23][0] < 0.1);
    // White patch (index 18) sits near 0.9.
    assert!(patches[18][0] > 0.8);
}

#[test]
fn harness_ramp_monotonic() {
    let buf = linear_ramp(256, 1);
    let reds: Vec<f32> = buf.chunks_exact(4).map(|c| c[0]).collect();
    for pair in reds.windows(2) {
        assert!(pair[1] >= pair[0]);
    }
}

#[test]
fn harness_encode_round_trip() {
    let linear = solid_linear(8, 8, MID_GREY_LINEAR);
    let sr = encode_linear_to_srgb_u8(&linear);
    assert_eq!(sr.len(), linear.len());
    // Mid-grey encodes to ~118 sRGB.
    for chunk in sr.chunks_exact(4) {
        assert!((chunk[0] as i32 - 118).abs() <= 1);
    }
}

// =====================================================================
// The migration contract.
//
// Each `#[ignore]` test below asserts a physically-correct result the
// current u8 sRGB code does not produce. They will start passing, one
// phase at a time, as the linear-light migration progresses. The
// phase that enables each test is noted in the comment so git blame
// makes the progression obvious.
//
// Each `todo!()` marks where the new linear API will plug in.
// Removing the ignore is intentionally part of the same commit that
// replaces the `todo!()` with a real call — the commit that lands the
// implementation is also the commit that promotes the test.
// =====================================================================

// ---- exposure ---------------------------------------------------------

#[test]
#[ignore = "Phase 4 — linear apply_exposure"]
fn exposure_plus_one_stop_on_mid_grey_doubles_linear_value() {
    // +1 stop on 0.18 linear should give 0.36 linear. The current
    // u8 code gives ~0.92 sRGB (= 0.83 linear) because it multiplies
    // gamma-encoded bytes. This test is the canonical exposure bug.
    let _input = solid_linear(4, 4, MID_GREY_LINEAR);
    let _expected = solid_linear(4, 4, [0.36, 0.36, 0.36, 1.0]);
    // let actual = klipp_color::linear::apply_exposure(&_input, 1.0);
    // assert_exact(&_expected, &actual, 1e-4);
    todo!("call linear apply_exposure once Phase 4 lands");
}

#[test]
#[ignore = "Phase 4 — linear apply_exposure"]
fn exposure_minus_one_stop_on_mid_grey_halves_linear_value() {
    let _expected = solid_linear(4, 4, [0.09, 0.09, 0.09, 1.0]);
    todo!("call linear apply_exposure(-1.0) once Phase 4 lands");
}

#[test]
#[ignore = "Phase 4 — linear apply_exposure"]
fn exposure_zero_stops_is_identity() {
    // Every pixel unchanged after +0 EV.
    todo!("call linear apply_exposure(0.0) once Phase 4 lands");
}

#[test]
#[ignore = "Phase 4 — linear apply_exposure"]
fn exposure_preserves_highlights_above_one() {
    // +2 stops on a 0.6 linear pixel gives 2.4 — *not* clamped.
    // This is what makes "recover highlights" possible later.
    todo!("call linear apply_exposure(+2.0) on 0.6 linear");
}

// ---- contrast --------------------------------------------------------

#[test]
#[ignore = "Phase 4 — linear apply_contrast"]
fn contrast_zero_is_identity() {
    todo!("call linear apply_contrast(0.0)");
}

#[test]
#[ignore = "Phase 4 — linear apply_contrast"]
fn contrast_does_not_shift_mid_grey() {
    // Any contrast around 0.18 must leave 0.18 fixed. The sRGB
    // version mis-fixes at 0.5 sRGB (≈ 0.21 linear), visibly
    // bright-shifting every contrast adjustment.
    todo!("apply_contrast(+0.5) on 0.18 linear — should stay ≈ 0.18");
}

// ---- saturation ------------------------------------------------------

#[test]
#[ignore = "Phase 4 — linear apply_saturation"]
fn saturation_one_is_identity() {
    todo!("saturation factor 1.0 must be no-op");
}

#[test]
#[ignore = "Phase 4 — linear apply_saturation"]
fn saturation_zero_produces_bt709_luma() {
    // A pure linear red (1, 0, 0, 1) at factor 0 must produce
    // (0.2126, 0.2126, 0.2126) — the BT.709 luma of pure red.
    let _expected = solid_linear(1, 1, [0.2126, 0.2126, 0.2126, 1.0]);
    todo!("apply_saturation(0.0) on pure red");
}

#[test]
#[ignore = "Phase 4 — linear apply_saturation"]
fn saturation_zero_on_grey_is_invariant() {
    // Grey is already saturation-free; any factor should leave it.
    todo!("apply_saturation(any) on any neutral grey = identity");
}

// ---- compositing -----------------------------------------------------

#[test]
#[ignore = "Phase 5 — linear blend_layer"]
fn normal_blend_half_alpha_red_over_black_is_half_red_linear() {
    // Pure red at 50 % alpha over black, in linear space, is
    // (0.5, 0, 0). The sRGB version gives (≈0.73, 0, 0) in linear
    // terms — darker edges / visible gamma banding.
    todo!("composite red with alpha=0.5 over black");
}

#[test]
#[ignore = "Phase 5 — linear blend_layer"]
fn multiply_blend_self_is_square_linear() {
    // Multiply mode: (0.5 linear) * (0.5 linear) = 0.25. sRGB
    // multiply would give ~0.137 (much darker). This is why
    // "Multiply" in sRGB engines darkens too aggressively.
    todo!("multiply a 0.5 linear grey by itself");
}

// ---- spatial operations (looser PSNR tolerance) ----------------------

#[test]
#[ignore = "Phase 6 — linear gaussian blur"]
fn blur_of_constant_frame_is_constant() {
    // DC invariant: blurring a flat colour must leave that colour.
    // Off by any amount means the blur has an unwanted gamma step
    // somewhere (common regression).
    let _constant = solid_linear(64, 64, [0.5, 0.3, 0.7, 1.0]);
    todo!("GaussianBlur radius=8 on a flat frame; PSNR > 60 dB vs input");
}

#[test]
#[ignore = "Phase 6 — linear gaussian blur"]
fn blur_preserves_linear_mean() {
    // Average luminance of a blurred frame == average of the
    // unblurred frame. In sRGB space this is subtly wrong because
    // averaging gamma-encoded values lowers the mean.
    todo!("compute mean R before/after blur on the ramp; equal within 1e-3");
}

// ---- log footage -----------------------------------------------------

#[test]
#[ignore = "Phase 4 — apply_input_transform linear"]
fn apple_log_middle_grey_decodes_to_0_18_linear() {
    // Per Apple Log whitepaper: the encoded mid-grey (0.384) must
    // decode to 0.18 linear. Current code decodes, *then* re-encodes
    // to sRGB before handing off to effects — this test catches that
    // re-encode going the wrong way.
    todo!("apply_input_transform on Apple-Log 0.384 → 0.18 linear");
}

#[test]
#[ignore = "Phase 4 — apply_input_transform linear"]
fn log_to_linear_monotonic() {
    // For every supported LogFormat, the decoded curve must be
    // strictly non-decreasing. A kink here shows up as posterised
    // highlights in the grade.
    todo!("iterate every LogFormat, verify monotonic curve");
}

// ---- end-to-end round trip -------------------------------------------

#[test]
#[ignore = "Phase 10 — u8 path removed"]
fn decode_to_display_round_trip_is_identity() {
    // 8-bit sRGB in → linear pipeline → sRGB out must round-trip
    // within ±1 code value on the 24 ColorChecker patches. This is
    // the final acceptance test for the migration.
    let _patches = color_checker_24_linear();
    todo!("wire patches through decode → display, compare u8 at boundary");
}

#[test]
#[ignore = "Phase 10 — scopes see the same linear data"]
fn scope_bins_match_linear_distribution() {
    // Histogram of the ramp, in the new scope ingest path, must
    // distribute bins evenly (linear ramp = uniform linear
    // distribution). Current sRGB scopes bunch bins into shadows.
    todo!("waveform on a linear ramp produces uniform bins");
}

// ---- encoder round trip ---------------------------------------------

#[test]
#[ignore = "Phase 8 — encoder reads linear"]
fn export_then_re_decode_round_trip() {
    // Linear → encoder → file → decoder → linear must round-trip
    // within 8-bit quantisation noise. Catches the encoder
    // accidentally re-gamma-encoding or dropping to 8-bit internally.
    todo!("encode linear ColorChecker, decode, compare within 1 u8 step");
}

#[test]
#[ignore = "Phase 11 — HDR metadata populated"]
fn hdr_maxcll_computed_from_linear_master() {
    // MaxCLL (max content light level) from a 1000-nit linear master
    // should report 1000 (± rounding). Current code does not compute
    // this field.
    todo!("export with HdrMode::Pq, inspect output MaxCLL tag == 1000");
}

// Pure sanity: the harness itself must never grow silent.
#[test]
fn linear_to_srgb_keeps_mid_grey_stable() {
    let e = linear_to_srgb(0.18);
    assert!((e - 0.461_5).abs() < 1e-3);
}
