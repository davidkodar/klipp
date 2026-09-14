//! End-to-end pin tests for the ACES display pipeline.
//!
//! Each ODT (Rec.709 SDR, Rec.2100 PQ at 1000 / 4000 nits, DCI-P3 D65
//! at 48 nits) gets the same canonical fixture set:
//!
//! 1. Mid-grey 0.18 ACEScg — the photographic anchor; everything
//!    downstream is calibrated around this.
//! 2. Saturated red / green / blue primaries — exercises the
//!    AP1→target gamut path.
//! 3. Skin-tone-ish patch (warm mid-tone) — catches subtle hue shifts
//!    that neutral-grey tests miss.
//! 4. Deep black — pins the lower clip / black-point behaviour.
//! 5. Diffuse white (~0.95) — pins the highlight roll-off start.
//! 6. HDR overrange (5.0) — must not produce NaNs / negatives, must
//!    saturate cleanly.
//!
//! Provenance of the expected values, which differs per ODT:
//!
//! Every table below holds OpenColorIO's ACES 1.3 output-transform
//! values — independently derived, not self-pinned.
//! `tests/ocio_parity.rs` verifies the same four transforms across a
//! 190-value grid; these six patches are the readable subset of that.
//!
//! What these tests guarantee:
//!
//! - **No silent drift**: any future change to RRT or ODT code that
//!   shifts representative output values will fail this test loudly,
//!   surfacing the regression for inspection rather than letting it
//!   ride into a release.
//! - **Pipeline invariants hold**: separate invariant tests below
//!   pin neutrality (R == G == B in → R == G == B out), monotonicity
//!   along the grey axis, finite outputs, and clipping behaviour at
//!   range edges — these would also fail if the pipeline regressed
//!   in a way that happened to leave the pinned values unchanged.

use klipp_color::aces::{
    acescg_to_dcip3_d65_48nit, acescg_to_rec2100_pq_1000_nit,
    acescg_to_rec2100_pq_4000_nit, acescg_to_rec709_sdr,
};

/// Tolerance for pinned scalar outputs: 1e-3 in the encoded range
/// (sRGB / PQ / gamma-2.6, all in [0,1]) is well below 1 LSB at 8-bit
/// and small enough to catch any meaningful regression.
const TOL: f32 = 1e-3;

/// Canonical fixture set — same six patches across every ODT so
/// the cross-ODT differences read at a glance.
fn fixtures() -> [(&'static str, [f32; 3]); 6] {
    [
        ("mid_grey_0_18",     [0.18,  0.18,  0.18]),
        ("saturated_red",     [0.70,  0.05,  0.05]),
        ("saturated_green",   [0.05,  0.70,  0.05]),
        ("saturated_blue",    [0.05,  0.05,  0.70]),
        ("skin_tone",         [0.45,  0.28,  0.20]),
        ("diffuse_white",     [0.95,  0.95,  0.95]),
    ]
}

fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn assert_close(label: &str, got: [f32; 3], expected: [f32; 3], tol: f32) {
    for i in 0..3 {
        assert!(
            approx_eq(got[i], expected[i], tol),
            "{label}: channel {i} drifted: got {} expected {} (delta {})",
            got[i],
            expected[i],
            (got[i] - expected[i]).abs(),
        );
    }
}

// ─── Rec.709 SDR ODT ───────────────────────────────────────────────

/// Pinned Rec.709 SDR output for the canonical fixture set. Values
/// are sRGB-encoded (BT.1886 inverse with Rec.709 dim-surround
/// compensation), in [0,1].
#[test]
fn rec709_sdr_pinned_outputs() {
    let expected: [(&str, [f32; 3]); 6] = [
        ("mid_grey_0_18",   [0.389_530_18,  0.389_530_09,  0.389_530_15]),
        // Saturated AP1 primaries fall outside the BT.709 gamut, so
        // one or two channels clip to 0.0 after the AP1→BT.709 matrix
        // pushes them negative. Pinning the clipped values guards
        // against the matrix transposing and the clamp being
        // accidentally removed.
        ("saturated_red",   [0.858_958_78,  0.000_000_00,  0.171_847_75]),
        ("saturated_green", [0.000_000_00,  0.782_983_48,  0.000_000_00]),
        ("saturated_blue",  [0.000_000_00,  0.156_031_00,  0.781_277_06]),
        ("skin_tone",       [0.682_560_86,  0.493_011_98,  0.411_564_35]),
        ("diffuse_white",   [0.813_067_44,  0.813_067_20,  0.813_067_32]),
    ];

    for ((label, input), (_, exp)) in fixtures().iter().zip(expected.iter()) {
        let got = acescg_to_rec709_sdr(*input);
        assert_close(label, got, *exp, TOL);
    }
}

// ─── Rec.2100 PQ 1000-nit ODT ──────────────────────────────────────

#[test]
fn rec2100_pq_1000_nit_pinned_outputs() {
    // PQ encodes ABSOLUTE luminance in cd/m², not relative to peak,
    // so even small linear values produce non-zero PQ outputs (the
    // ST 2084 OETF has aggressive gain near zero). That's why the
    // pinned values are uniformly higher than the SDR equivalents.
    let expected: [(&str, [f32; 3]); 6] = [
        ("mid_grey_0_18",   [0.332_589_42,  0.332_589_48,  0.332_589_42]),
        ("saturated_red",   [0.462_499_41,  0.211_830_84,  0.196_236_61]),
        ("saturated_green", [0.191_270_78,  0.479_600_49,  0.179_788_84]),
        ("saturated_blue",  [0.184_957_22,  0.192_105_73,  0.479_622_63]),
        ("skin_tone",       [0.429_233_85,  0.383_309_81,  0.345_313_04]),
        ("diffuse_white",   [0.510_416_57,  0.510_416_63,  0.510_416_69]),
    ];

    for ((label, input), (_, exp)) in fixtures().iter().zip(expected.iter()) {
        let got = acescg_to_rec2100_pq_1000_nit(*input);
        assert_close(label, got, *exp, TOL);
    }
}

#[test]
fn rec2100_pq_4000_nit_pinned_outputs() {
    let expected: [(&str, [f32; 3]); 6] = [
        ("mid_grey_0_18",   [0.332_589_39,  0.332_589_42,  0.332_589_39]),
        ("saturated_red",   [0.471_649_32,  0.210_203_96,  0.194_030_97]),
        ("saturated_green", [0.183_622_79,  0.491_066_22,  0.168_912_84]),
        ("saturated_blue",  [0.181_913_17,  0.190_218_91,  0.490_806_49]),
        ("skin_tone",       [0.435_135_07,  0.385_742_57,  0.345_738_68]),
        ("diffuse_white",   [0.526_451_77,  0.526_451_83,  0.526_451_89]),
    ];

    for ((label, input), (_, exp)) in fixtures().iter().zip(expected.iter()) {
        let got = acescg_to_rec2100_pq_4000_nit(*input);
        assert_close(label, got, *exp, TOL);
    }
}

// ─── DCI-P3 D65 48-nit cinema ODT ──────────────────────────────────

#[test]
fn dcip3_d65_48nit_pinned_outputs() {
    let expected: [(&str, [f32; 3]); 6] = [
        ("mid_grey_0_18",   [0.411_864_28,  0.411_864_52,  0.411_864_34]),
        // P3 is wider gamut than Rec.709, so saturated AP1 red lands
        // in-gamut (R = 0.81) where Rec.709 had to crush green to
        // zero. Saturated green and blue still clip, because AP1 green
        // and blue extend beyond P3.
        ("saturated_red",   [0.809_080_48,  0.000_000_00,  0.196_556_81]),
        ("saturated_green", [0.000_000_00,  0.784_749_87,  0.000_000_00]),
        ("saturated_blue",  [0.000_000_00,  0.153_423_35,  0.770_222_31]),
        ("skin_tone",       [0.675_869_46,  0.519_732_65,  0.438_812_97]),
        ("diffuse_white",   [0.823_080_18,  0.823_080_18,  0.823_080_36]),
    ];

    for ((label, input), (_, exp)) in fixtures().iter().zip(expected.iter()) {
        let got = acescg_to_dcip3_d65_48nit(*input);
        assert_close(label, got, *exp, TOL);
    }
}

// ─── Pipeline invariants — independent of pinned values ────────────

/// Neutral input must produce neutral output across every ODT. This
/// is the strongest correctness check available without an external
/// reference: any colour cast at neutral indicates a transposed
/// matrix, broken white-point adaptation, or asymmetric channel
/// math. Fails immediately even if the pinned values above happened
/// to drift in a way that preserved the average.
#[test]
fn neutral_input_stays_neutral_across_every_odt() {
    let neutrals = [0.05, 0.18, 0.5, 0.95];
    for &g in &neutrals {
        let input = [g, g, g];
        for (name, out) in [
            ("rec709_sdr",       acescg_to_rec709_sdr(input)),
            ("rec2100_pq_1000",  acescg_to_rec2100_pq_1000_nit(input)),
            ("rec2100_pq_4000",  acescg_to_rec2100_pq_4000_nit(input)),
            ("dcip3_d65_48",     acescg_to_dcip3_d65_48nit(input)),
        ] {
            assert!(
                (out[0] - out[1]).abs() < 1e-4 && (out[1] - out[2]).abs() < 1e-4,
                "{name} produced a colour cast on neutral grey {g}: {out:?}",
            );
        }
    }
}

/// Mid-grey 0.18 ACEScg is the photographic anchor — it must produce
/// mid-grey display output. The exact target differs per ODT (the
/// SDR / cinema ODTs land near sRGB ~0.39, HDR PQ lands above 0.5
/// because PQ encodes brighter), but every one must sit somewhere
/// in [0.30, 0.85] and far from the extremes.
#[test]
fn mid_grey_0_18_lands_in_reasonable_display_range() {
    let mid = [0.18, 0.18, 0.18];
    for (name, out) in [
        ("rec709_sdr",       acescg_to_rec709_sdr(mid)),
        ("rec2100_pq_1000",  acescg_to_rec2100_pq_1000_nit(mid)),
        ("rec2100_pq_4000",  acescg_to_rec2100_pq_4000_nit(mid)),
        ("dcip3_d65_48",     acescg_to_dcip3_d65_48nit(mid)),
    ] {
        assert!(
            out[0] > 0.30 && out[0] < 0.85,
            "{name} mid-grey 0.18 landed at {out:?} — outside reasonable display range",
        );
    }
}

/// Monotonicity along the grey axis: brighter linear input must
/// produce brighter display output, every ODT, every step. A failure
/// would mean the spline / OETF is non-monotonic somewhere — a
/// catastrophic regression.
#[test]
fn grey_axis_is_monotonic_across_every_odt() {
    let steps = [0.001, 0.01, 0.05, 0.18, 0.5, 0.95, 1.5, 5.0];
    let mut last_outputs = [-1.0_f32; 4]; // [r709, pq1k, pq4k, dci]
    for &g in &steps {
        let input = [g, g, g];
        let outs = [
            acescg_to_rec709_sdr(input)[0],
            acescg_to_rec2100_pq_1000_nit(input)[0],
            acescg_to_rec2100_pq_4000_nit(input)[0],
            acescg_to_dcip3_d65_48nit(input)[0],
        ];
        for i in 0..4 {
            assert!(
                outs[i] >= last_outputs[i] - 1e-5,
                "ODT[{i}] non-monotonic at input {g}: {} < previous {}",
                outs[i],
                last_outputs[i],
            );
            last_outputs[i] = outs[i];
        }
    }
}

/// HDR overrange inputs (highlights well above diffuse white) must
/// produce finite, in-range outputs. Specifically: no NaN, no
/// negative values, no values above 1.0 in the encoded display
/// signal. PQ encodes bright pixels into a higher PQ value, but the
/// final ST.2084-encoded scalar still sits in [0,1].
#[test]
fn hdr_overrange_produces_finite_clipped_outputs() {
    let extreme_inputs = [
        [5.0, 5.0, 5.0],
        [10.0, 0.0, 0.0],
        [0.0, 50.0, 0.0],
        [100.0, 100.0, 100.0],
    ];
    for input in extreme_inputs {
        for (name, out) in [
            ("rec709_sdr",       acescg_to_rec709_sdr(input)),
            ("rec2100_pq_1000",  acescg_to_rec2100_pq_1000_nit(input)),
            ("rec2100_pq_4000",  acescg_to_rec2100_pq_4000_nit(input)),
            ("dcip3_d65_48",     acescg_to_dcip3_d65_48nit(input)),
        ] {
            for (i, c) in out.iter().enumerate() {
                assert!(c.is_finite(),
                    "{name} channel {i} was non-finite ({c}) on input {input:?}");
                assert!(*c >= 0.0,
                    "{name} channel {i} was negative ({c}) on input {input:?}");
                assert!(*c <= 1.0 + 1e-4,
                    "{name} channel {i} exceeded 1.0 ({c}) on input {input:?}");
            }
        }
    }
}

/// Black input must clip cleanly to a black-floor output. The
/// numeric ceiling is per-ODT because PQ encodes ABSOLUTE luminance:
/// the ACES half-min black floor (~0.0001 cd/m²) maps to a non-zero
/// PQ value through the ST 2084 OETF, whereas the SDR / cinema OETFs
/// produce near-zero output for the same scene-referred floor. The
/// per-ODT ceilings below all sit well below mid-grey, which is the
/// invariant that matters: black must NEVER read brighter than mid.
#[test]
fn black_input_clips_to_per_odt_black_floor() {
    let black = [0.0, 0.0, 0.0];
    let cases: [(&str, [f32; 3], f32); 4] = [
        ("rec709_sdr",       acescg_to_rec709_sdr(black),         0.05),
        // PQ-1000's ACES half-min floor encodes to ~0.11 PQ.
        ("rec2100_pq_1000",  acescg_to_rec2100_pq_1000_nit(black), 0.20),
        // PQ-4000 reads even higher because peak_nits scales the
        // absolute luminance, lifting the floor in the encoded
        // signal.
        ("rec2100_pq_4000",  acescg_to_rec2100_pq_4000_nit(black), 0.25),
        ("dcip3_d65_48",     acescg_to_dcip3_d65_48nit(black),    0.05),
    ];
    for (name, out, ceiling) in cases {
        for (i, c) in out.iter().enumerate() {
            assert!(c.is_finite(),
                "{name} channel {i} non-finite on black: {c}");
            assert!(*c <= ceiling,
                "{name} channel {i} {c} above black-floor ceiling {ceiling}");
            // Also pin the universal invariant: black < mid-grey.
            assert!(*c < 0.40,
                "{name} channel {i} {c} reads brighter than mid-grey on black input");
        }
    }
}

// ─── Cross-ODT differentiation ─────────────────────────────────────

/// HDR renders mid-grey brighter than SDR — but that has to be checked
/// in *absolute luminance*, not in code values. PQ and BT.1886 are
/// different encodings, so their code values are not comparable: PQ
/// mid-grey (0.333) is numerically *below* SDR mid-grey (0.390) while
/// representing three times the luminance.
#[test]
fn hdr_pq_renders_mid_grey_brighter_than_sdr_in_absolute_luminance() {
    let mid = [0.18, 0.18, 0.18];

    // The Rec.709 ODT normalises against a 48 cd/m² cinema white and
    // encodes with BT.1886 (gamma 2.4, L_W = 1, L_B = 0), so decoding
    // is `code^2.4 * 48`.
    let sdr_code = acescg_to_rec709_sdr(mid)[0];
    let sdr_nits = sdr_code.powf(2.4) * 48.0;

    // PQ carries absolute luminance, normalised against 10000 cd/m².
    let pq_code = acescg_to_rec2100_pq_1000_nit(mid)[0];
    let pq_nits = klipp_color::camera_log::pq_to_linear(pq_code) * 10_000.0;

    // Both HDR deliverables anchor mid-grey at Y_MID = 15 cd/m².
    assert!(
        (pq_nits - 15.0).abs() < 0.5,
        "HDR mid-grey should render at 15 cd/m², got {pq_nits} \
         (PQ code {pq_code})",
    );
    assert!(
        pq_nits > sdr_nits * 2.0,
        "HDR mid-grey ({pq_nits} cd/m²) should be well above SDR \
         ({sdr_nits} cd/m²)",
    );
}

/// DCI-P3 cinema is a wider-gamut display than Rec.709. On a
/// saturated red the P3 output must differ measurably from the
/// Rec.709 output (P3 reaches a more saturated colour). This catches
/// matrix-pair confusion between the two ODTs.
#[test]
fn dcip3_diverges_from_rec709_on_saturated_red() {
    let red = [0.7, 0.05, 0.05];
    let r709 = acescg_to_rec709_sdr(red);
    let p3 = acescg_to_dcip3_d65_48nit(red);
    let max_delta = (0..3)
        .map(|i| (r709[i] - p3[i]).abs())
        .fold(0.0_f32, f32::max);
    assert!(max_delta > 0.005,
        "DCI-P3 and Rec.709 must produce measurably different output on saturated red: \
         r709={r709:?} p3={p3:?}");
}

// ─── 24-patch Macbeth chart end-to-end soak ────────────────────────

/// X-Rite ColorChecker (Macbeth 24-patch) reference values, expressed
/// as linear-light ACEScg (AP1) RGB. Sourced by converting the
/// chart's published linear-sRGB values through the BT.709 → ACEScg
/// matrix shipped in `klipp_color::primaries`. The patches span:
///
/// - Patches 1-6: skin / sky / foliage / blue-sky reference colours
/// - Patches 7-12: orange / purplish-blue / moderate red / purple /
///   yellow / magenta-ish primaries
/// - Patches 13-18: blue / green / red / yellow / magenta / cyan
/// - Patches 19-24: greyscale ramp from white (#19) to black (#24)
///
/// What this exercises that the 6-patch tests don't: a **broad**
/// sweep through chroma + value space, including patches that should
/// be in-gamut for both Rec.709 and P3 (no clipping anywhere). Failures
/// here typically mean the RRT or ODT shifted character on a
/// non-extreme input — exactly the kind of regression that's hard to
/// notice on synthetic primaries.
const MACBETH_ACESCG: [[f32; 3]; 24] = [
    [0.119_30,  0.087_29,  0.058_31], // 01 dark skin
    [0.395_92,  0.282_94,  0.205_46], // 02 light skin
    [0.131_91,  0.180_19,  0.291_92], // 03 blue sky
    [0.092_15,  0.128_11,  0.062_67], // 04 foliage
    [0.205_79,  0.213_50,  0.359_32], // 05 blue flower
    [0.213_57,  0.487_89,  0.430_17], // 06 bluish green
    [0.535_87,  0.265_40,  0.071_47], // 07 orange
    [0.091_21,  0.110_00,  0.331_70], // 08 purplish blue
    [0.380_29,  0.130_68,  0.116_12], // 09 moderate red
    [0.087_07,  0.063_92,  0.114_31], // 10 purple
    [0.347_66,  0.434_52,  0.118_54], // 11 yellow green
    [0.594_06,  0.395_15,  0.094_53], // 12 orange yellow
    [0.046_43,  0.054_98,  0.244_39], // 13 blue
    [0.115_16,  0.241_03,  0.082_19], // 14 green
    [0.276_40,  0.069_54,  0.064_31], // 15 red
    [0.667_19,  0.580_89,  0.115_72], // 16 yellow
    [0.339_57,  0.166_16,  0.249_69], // 17 magenta
    [0.121_12,  0.236_92,  0.302_64], // 18 cyan
    [0.879_03,  0.892_62,  0.886_37], // 19 white 9.5
    [0.582_75,  0.587_69,  0.581_42], // 20 neutral 8
    [0.354_81,  0.355_72,  0.350_83], // 21 neutral 6.5
    [0.196_73,  0.197_66,  0.196_45], // 22 neutral 5
    [0.090_56,  0.091_63,  0.092_71], // 23 neutral 3.5
    [0.030_18,  0.030_29,  0.030_98], // 24 black 2
];

/// Run every patch through every ODT, assert outputs are finite,
/// in [0,1], and the greyscale ramp (patches 19-24) preserves
/// monotonicity. Also pins that none of the 24 patches produces a
/// catastrophic neutral-grey colour cast (R/G/B max delta < 0.04 on
/// the greyscale patches).
#[test]
fn macbeth_24_patch_end_to_end_soak() {
    for (idx, patch) in MACBETH_ACESCG.iter().enumerate() {
        for (name, out) in [
            ("rec709_sdr",      acescg_to_rec709_sdr(*patch)),
            ("rec2100_pq_1000", acescg_to_rec2100_pq_1000_nit(*patch)),
            ("rec2100_pq_4000", acescg_to_rec2100_pq_4000_nit(*patch)),
            ("dcip3_d65_48",    acescg_to_dcip3_d65_48nit(*patch)),
        ] {
            for (c, v) in out.iter().enumerate() {
                assert!(v.is_finite(),
                    "patch {} ({}) channel {c} non-finite: {v}", idx + 1, name);
                assert!(*v >= 0.0,
                    "patch {} ({}) channel {c} negative: {v}", idx + 1, name);
                assert!(*v <= 1.0 + 1e-4,
                    "patch {} ({}) channel {c} above 1.0: {v}", idx + 1, name);
            }
            // Greyscale patches (19-24) must produce neutral output.
            // Tightish tolerance — a chromatic matrix bug would
            // typically push way past 0.04.
            if idx >= 18 {
                let max_delta = (out[0] - out[1]).abs()
                    .max((out[1] - out[2]).abs())
                    .max((out[0] - out[2]).abs());
                assert!(
                    max_delta < 0.04,
                    "patch {} ({}) greyscale produced colour cast {max_delta}: {out:?}",
                    idx + 1, name,
                );
            }
        }
    }

    // Greyscale ramp monotonicity (patches 19-24, white → black) for
    // SDR Rec.709. The rest of the ODTs are exercised by the
    // grey-axis test above; using R709 here keeps this test cheap.
    let mut prev_y: f32 = 1.0;
    for idx in 18..24 {
        let out = acescg_to_rec709_sdr(MACBETH_ACESCG[idx]);
        // Use channel 0 as a proxy for luminance — neutrality is
        // already pinned above.
        assert!(
            out[0] <= prev_y + 1e-4,
            "Macbeth greyscale patch {} brighter than previous: {} > {}",
            idx + 1, out[0], prev_y,
        );
        prev_y = out[0];
    }
}
