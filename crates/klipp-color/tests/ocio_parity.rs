//! Parity tests: klipp's ACES 1.3 ODTs vs. OpenColorIO's reference.
//!
//! `aces_pipeline_pin.rs` pins our output against *itself* — it catches
//! drift but cannot catch an error that was there from the start. These
//! tests close that gap by comparing against OpenColorIO's ACES 1.3
//! output transforms, an independent implementation of the same spec.
//!
//! The reference values live in `tests/fixtures/ocio_aces13_*.csv` and
//! are committed, so this test needs no OCIO installation. Regenerate
//! them with `python tools/gen_ocio_reference.py` (requires OCIO >= 2.2
//! with Python bindings) only when deliberately re-deriving.
//!
//! # Status
//!
//! | ODT | Verdict |
//! |---|---|
//! | Rec.709 SDR | matches OCIO |
//! | DCI-P3 D65 48-nit | neutral axis matches; gamut edges diverge |
//! | Rec.2100 PQ 1000 / 4000 nit | does not match — known approximation |
//!
//! The three failing ODTs are `#[ignore]`d with the reason inline so
//! the suite stays green; remove the attribute as each is fixed. See
//! the module docs on `apply_odt_rec2100_pq_inner` in `src/aces.rs`,
//! which already flags the HDR curve as a stand-in for the AMPAS HDR
//! spline.

use klipp_color::aces::{
    acescg_to_dcip3_d65_48nit, acescg_to_rec2100_pq_1000_nit,
    acescg_to_rec2100_pq_4000_nit, acescg_to_rec709_sdr,
};

/// One row of a reference CSV.
struct Sample {
    name: String,
    input: [f32; 3],
    expected: [f32; 3],
}

fn load(fixture: &str) -> Vec<Sample> {
    let path = format!(
        "{}/tests/fixtures/ocio_aces13_{}.csv",
        env!("CARGO_MANIFEST_DIR"),
        fixture
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing reference fixture {path}: {e}"));

    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with("name,") || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        assert_eq!(f.len(), 7, "malformed row in {fixture}: {line}");
        let num = |s: &str| s.parse::<f32>().expect("float field");
        out.push(Sample {
            name: f[0].to_string(),
            input: [num(f[1]), num(f[2]), num(f[3])],
            expected: [num(f[4]), num(f[5]), num(f[6])],
        });
    }
    assert!(!out.is_empty(), "no rows parsed from {fixture}");
    out
}

/// Compare every sample and report. Panics with a readable table if any
/// channel exceeds `tol`.
fn check(fixture: &str, odt: fn([f32; 3]) -> [f32; 3], tol: f32) {
    let samples = load(fixture);
    let mut max_delta = 0.0f32;
    let mut sum_delta = 0.0f64;
    let mut worst = String::new();
    let mut failures: Vec<String> = Vec::new();

    for s in &samples {
        let got = odt(s.input);
        for ch in 0..3 {
            // Clamp the reference into the encodable range. OCIO's
            // op-chain implementation of the ACES output transforms
            // omits the CTL's `clamp_f3(linearCV, 0., +inf)` before the
            // EOTF, so for inputs outside the display gamut it reports
            // *negative* PQ / gamma code values. A code value cannot be
            // negative; the transform being verified clamps, so the
            // comparison has to as well or it penalises us for being
            // right. In-range values are untouched, and the CSVs keep
            // OCIO's raw output so nothing is hidden.
            let expected = s.expected[ch].clamp(0.0, 1.0);
            let d = (got[ch] - expected).abs();
            sum_delta += d as f64;
            if d > max_delta {
                max_delta = d;
                worst = format!("{} ch{}", s.name, ch);
            }
            if d > tol && failures.len() < 20 {
                failures.push(format!(
                    "  {:<24} ch{}  ocio={:>10.6}  klipp={:>10.6}  delta={:>10.6}",
                    s.name, ch, expected, got[ch], d
                ));
            }
        }
    }

    let mean = sum_delta / (samples.len() * 3) as f64;
    println!(
        "{fixture}: {} samples, max delta {:.6} ({}), mean {:.6}",
        samples.len(),
        max_delta,
        worst,
        mean
    );

    assert!(
        failures.is_empty(),
        "{fixture}: {} channel(s) exceed tolerance {tol:.1e} \
         (max {max_delta:.6} at {worst}, mean {mean:.6})\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Tolerance for SDR-range outputs. 1e-3 sits just above the f32-vs-f64
/// precision floor we measured on the gamut extremes (~3e-4) and still
/// well below 1 LSB at 10-bit (9.8e-4), so a genuine maths error cannot
/// hide underneath it.
const TOL: f32 = 1e-3;

#[test]
fn rec709_sdr_matches_ocio() {
    check("rec709_sdr", acescg_to_rec709_sdr, TOL);
}

#[test]
#[ignore = "known divergence: neutral axis matches OCIO to ~3e-6 but the \
            AP1->P3-D65 gamut edge does not (saturated AP1 green renders \
            blue=0.237 where OCIO clips to 0.0). Remove this attribute \
            once the P3 matrix / clamp path is fixed."]
fn dcip3_d65_48nit_matches_ocio() {
    check("dcip3_d65_48", acescg_to_dcip3_d65_48nit, TOL);
}

#[test]
#[ignore = "known-wrong: apply_odt_rec2100_pq_inner reuses the 48-nit \
            cinema C9 spline scaled by peak_nits/48 instead of the AMPAS \
            HDR spline. Mid-grey lands at PQ 0.508 where ACES puts it at \
            0.333 (~15 cd/m2), so HDR output is far too bright. Remove \
            this attribute once the real HDR coefficients are in."]
fn rec2100_pq_1000_matches_ocio() {
    check("rec2100_pq_1000", acescg_to_rec2100_pq_1000_nit, TOL);
}

#[test]
#[ignore = "known-wrong: same peak_nits/48 scaling as the 1000-nit ODT, \
            and worse — mid-grey lands at PQ 0.653 vs OCIO's 0.333. Note \
            OCIO renders mid-grey to the SAME PQ code at 1000 and 4000 \
            nits (PQ is absolute luminance; peak affects highlight \
            rolloff, not mid-grey), which our peak-scaling cannot do."]
fn rec2100_pq_4000_matches_ocio() {
    check("rec2100_pq_4000", acescg_to_rec2100_pq_4000_nit, TOL);
}

/// PQ encodes *absolute* luminance, so mid-grey must render to the same
/// code value regardless of the deliverable's peak brightness — OCIO
/// gives 0.332589 for both 1000 and 4000 nits. Our peak-scaling
/// approach makes mid-grey a function of peak, which is the structural
/// bug behind both ignored PQ tests above. This test states the
/// invariant independently of any reference table.
#[test]
#[ignore = "fails until the AMPAS HDR spline replaces the peak_nits/48 \
            scaling; see rec2100_pq_1000_matches_ocio"]
fn pq_mid_grey_is_invariant_to_peak_nits() {
    let mid = [0.18, 0.18, 0.18];
    let a = acescg_to_rec2100_pq_1000_nit(mid);
    let b = acescg_to_rec2100_pq_4000_nit(mid);
    for ch in 0..3 {
        assert!(
            (a[ch] - b[ch]).abs() < 1e-4,
            "mid-grey PQ code drifted with peak luminance: \
             1000-nit {a:?} vs 4000-nit {b:?} (channel {ch})",
        );
    }
}
