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
fn exposure_plus_one_stop_on_mid_grey_doubles_linear_value() {
    // +1 stop on 0.18 linear should give 0.36 linear. The old u8
    // code gave ~0.92 sRGB (= 0.83 linear) because it multiplied
    // gamma-encoded bytes. This is the canonical exposure bug and
    // the single most important test in the migration.
    let mut frame = solid_linear(4, 4, MID_GREY_LINEAR);
    klipp_color::linear::apply_exposure(&mut frame, 1.0);
    let expected = solid_linear(4, 4, [0.36, 0.36, 0.36, 1.0]);
    assert_exact(&expected, &frame, 1e-4);
}

#[test]
fn exposure_minus_one_stop_on_mid_grey_halves_linear_value() {
    let mut frame = solid_linear(4, 4, MID_GREY_LINEAR);
    klipp_color::linear::apply_exposure(&mut frame, -1.0);
    let expected = solid_linear(4, 4, [0.09, 0.09, 0.09, 1.0]);
    assert_exact(&expected, &frame, 1e-4);
}

#[test]
fn exposure_zero_stops_is_identity() {
    let mut frame = solid_linear(4, 4, [0.3, 0.6, 0.9, 1.0]);
    let original = frame.clone();
    klipp_color::linear::apply_exposure(&mut frame, 0.0);
    assert_exact(&original, &frame, 0.0);
}

#[test]
fn exposure_preserves_highlights_above_one() {
    // +2 stops on 0.6 linear gives 2.4 — *not* clamped. This is
    // what makes "recover highlights" possible at later stages.
    let mut frame = solid_linear(1, 1, [0.6, 0.6, 0.6, 1.0]);
    klipp_color::linear::apply_exposure(&mut frame, 2.0);
    for i in 0..3 {
        assert!(
            (frame[i] - 2.4).abs() < 1e-4,
            "channel {i}: got {} (expected 2.4, not clipped)",
            frame[i],
        );
    }
}

// ---- contrast --------------------------------------------------------

#[test]
fn contrast_zero_is_identity() {
    let mut frame = solid_linear(4, 4, [0.1, 0.5, 0.9, 1.0]);
    let original = frame.clone();
    klipp_color::linear::apply_contrast(&mut frame, 0.0);
    assert_exact(&original, &frame, 0.0);
}

#[test]
fn contrast_does_not_shift_mid_grey() {
    // Any contrast around 0.18 must leave 0.18 fixed. The sRGB
    // version mis-fixed at 0.5 sRGB (≈ 0.21 linear), visibly
    // bright-shifting every contrast adjustment.
    let mut frame = solid_linear(2, 2, MID_GREY_LINEAR);
    klipp_color::linear::apply_contrast(&mut frame, 0.5);
    let expected = solid_linear(2, 2, MID_GREY_LINEAR);
    // Through the grade encoding and back: a float round trip.
    assert_exact(&expected, &frame, 1e-5);
}

// ---- saturation ------------------------------------------------------

#[test]
fn saturation_one_is_identity() {
    let mut frame = solid_linear(1, 1, [0.8, 0.2, 0.5, 1.0]);
    let original = frame.clone();
    klipp_color::linear::apply_saturation(&mut frame, 1.0);
    assert_exact(&original, &frame, 0.0);
}

#[test]
fn saturation_zero_produces_bt709_luma() {
    // A pure linear red (1, 0, 0, 1) at factor 0 must produce
    // (0.2126, 0.2126, 0.2126) — the BT.709 luma of pure red.
    let mut frame = solid_linear(1, 1, [1.0, 0.0, 0.0, 1.0]);
    klipp_color::linear::apply_saturation(&mut frame, 0.0);
    let expected = solid_linear(1, 1, [0.2126, 0.2126, 0.2126, 1.0]);
    assert_exact(&expected, &frame, 1e-4);
}

#[test]
fn saturation_zero_on_grey_is_invariant() {
    // Grey is already saturation-free; any factor should leave it.
    let mut frame = solid_linear(1, 1, [0.5, 0.5, 0.5, 1.0]);
    let original = frame.clone();
    klipp_color::linear::apply_saturation(&mut frame, 0.0);
    assert_exact(&original, &frame, 0.0);
    klipp_color::linear::apply_saturation(&mut frame, 3.0);
    assert_exact(&original, &frame, 0.0);
}

// ---- compositing -----------------------------------------------------

#[test]
fn normal_blend_half_alpha_red_over_black_is_half_red_linear() {
    // Pure red at 50 % alpha over black, in linear space, is
    // (0.5, 0, 0). The sRGB version gave ≈ 0.73 linear (visibly
    // brighter than physics), which manifested as dark edges and
    // gamma banding on every soft transition.
    use klipp_effects::composite::blend_layer_linear;
    let mut dst = vec![0.0_f32, 0.0, 0.0, 1.0];
    let src = vec![1.0_f32, 0.0, 0.0, 1.0];
    blend_layer_linear(&mut dst, &src, 0.5, klipp_core::BlendMode::Normal);
    let expected = vec![0.5_f32, 0.0, 0.0, 1.0];
    assert_exact(&expected, &dst, 1e-5);
}

#[test]
fn multiply_blend_self_is_square_linear() {
    // Multiply: 0.5 × 0.5 = 0.25 linear. The sRGB version gave
    // ~0.137 linear (much too dark), which is why every "Multiply"
    // composite in sRGB engines crushes shadows.
    use klipp_effects::composite::blend_layer_linear;
    let mut dst = vec![0.5_f32, 0.5, 0.5, 1.0];
    let src = vec![0.5_f32, 0.5, 0.5, 1.0];
    blend_layer_linear(&mut dst, &src, 1.0, klipp_core::BlendMode::Multiply);
    let expected = vec![0.25_f32, 0.25, 0.25, 1.0];
    assert_exact(&expected, &dst, 1e-5);
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

/// Apple Log's mid-grey. The placeholder this replaces asserted 0.384 →
/// 0.18; the published curve puts 18% grey at 0.4875 and reads 0.384 as
/// 0.072, which is what Klipp decodes. Both numbers come from Apple's
/// white paper — `0.08550479 * log2(x + 0.00842546) + 0.69336945` — and
/// `tests/camera_log_parity.rs` checks the whole curve against
/// colour-science, so this pins the one value a colourist knows by heart.
///
/// The decoder applies it: `klipp_color::camera_log` is the one copy of
/// every camera curve, and the graph's InputTransform node only records
/// which curve a clip carries.
#[test]
fn apple_log_middle_grey_decodes_to_0_18_linear() {
    let grey = klipp_color::camera_log::apply_inverse_eotf(0.4875, klipp_core::ColorTransfer::AppleLog);
    assert!((grey - 0.18).abs() < 0.005, "Apple Log 0.4875 decoded to {grey}, expected ~0.18");
    let low = klipp_color::camera_log::apply_inverse_eotf(0.384, klipp_core::ColorTransfer::AppleLog);
    assert!((low - 0.072).abs() < 0.005, "Apple Log 0.384 decoded to {low}, expected ~0.072");
}

/// Every camera curve is non-decreasing over the code range the decoder
/// feeds it. A kink shows up as a posterised highlight in the grade.
#[test]
fn log_to_linear_monotonic() {
    use klipp_core::LogFormat as F;
    let formats = [
        F::SLog3, F::SLog2, F::VLog, F::LogC3, F::LogC4, F::CanonLog, F::CanonLog3,
        F::RedLog3G10, F::BmdFilmGen5, F::DJIDLog, F::FLog, F::AppleLog, F::NLog, F::GoProFlat,
        F::PQ, F::HLG, F::Srgb, F::Linear,
    ];
    for format in formats {
        let mut previous = f32::NEG_INFINITY;
        for i in 0..=1023u32 {
            let v = klipp_color::transforms::log_to_linear(i as f32 / 1023.0, format);
            assert!(v.is_finite(), "{format:?} at code {i} gave {v}");
            assert!(v >= previous - 1e-6, "{format:?} dips at code {i}: {previous} → {v}");
            previous = v;
        }
    }
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
fn scope_bins_match_linear_distribution() {
    // Histogram of a linear ramp, fed into the new scope ingest
    // path, must distribute bins near-uniformly. The sRGB u8 scopes
    // above concentrate counts into the shadow bins because sRGB
    // gamma packs more codes into the dark end; linear scopes don't.
    let frame = linear_ramp(256, 1);
    let hist = klipp_color::scopes::compute_histogram_linear(
        &frame,
        256,
        1,
        klipp_color::scopes::ScopeBasis::default(),
    );
    // step_by(2) samples 128 pixels. Near-uniform ⇒ most bins are
    // hit and no single bin dominates.
    let populated = hist.r.iter().filter(|&&c| c > 0).count();
    assert!(populated >= 120, "only {populated} bins populated — expected near-uniform");
    let max = *hist.r.iter().max().unwrap();
    assert!(max <= 5, "a single bin with {max} samples means non-uniform distribution");
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
fn hdr_maxcll_is_measured_on_the_pq_signal() {
    // MaxCLL / MaxFALL (CTA-861.3) of a PQ master: the brightest pixel's
    // brightest channel, and the brightest frame's average of it, in
    // cd/m². The encoder measures the PQ signal it writes, so a 1000-nit
    // highlight must read back as 1000 through the PQ curve. The
    // container atoms themselves are covered by klipp-render's
    // `hdr_export` tests — this one pins the math.
    use klipp_color::hdr::{pq_decode_nits, pq_encode_nits, LightLevelStats};

    let mut stats = LightLevelStats::new();
    // A 720p frame of SDR white (203 nits) with one 1000-nit pixel.
    let pixels = 1280 * 720;
    let white = pq_decode_nits(pq_encode_nits(203.0));
    let peak = pq_decode_nits(pq_encode_nits(1000.0));
    let average = (white * (pixels - 1) as f32 + peak) / pixels as f32;
    stats.add_frame(peak, average);

    assert_eq!(stats.max_cll_nits(), 1000, "1000-nit highlight master should report MaxCLL=1000");
    assert_eq!(stats.max_fall_nits(), 203, "MaxFALL should be SDR white, which dominates");
}

// Pure sanity: the harness itself must never grow silent.
#[test]
fn linear_to_srgb_keeps_mid_grey_stable() {
    let e = linear_to_srgb(0.18);
    assert!((e - 0.461_5).abs() < 1e-3);
}
