//! The render path, held against a published colour chart.
//!
//! This is the test a colourist would ask for before believing any
//! number the tools produce: send a chart whose values are known, read
//! it back off the end of the real renderer, and see whether it came
//! back where it went in. A failure here invalidates every other colour
//! measurement in the program, which is why it is a test of its own and
//! not a line in a bigger one.

use klipp_color::chart::{CHART_WARN_DE, NEUTRAL_CAST_FAIL};
use klipp_compose::selfcheck;
use klipp_compose::RenderOptions;
use klipp_core::WorkingColorspace;

const W: u32 = 480;
const H: u32 = 320;

#[test]
fn the_chart_survives_the_render_path_unchanged() {
    for (gpu, check) in selfcheck::run_both(W, H, WorkingColorspace::Rec709Linear, RenderOptions::default()) {
        let path = if gpu { "GPU" } else { "CPU" };
        // An ungraded clip through the composite is the identity, so the
        // only thing between sending and reading is 8-bit quantisation
        // and the linear round trip: well under one ΔE.
        assert!(
            check.max_de < CHART_WARN_DE,
            "{path}: the chart came back {:.2} ΔE out at worst ({}); mean {:.2}",
            check.max_de,
            check.worst,
            check.mean_de
        );
        assert!(
            check.neutral_cast < NEUTRAL_CAST_FAIL,
            "{path}: the greys came back with {:.2} chroma — the path is not neutral",
            check.neutral_cast
        );
        assert!(check.ramp_ordered, "{path}: the grey ramp came back out of order");
        assert!(check.clipped.is_empty(), "{path}: clipped {:?}", check.clipped);
        assert!(check.trustworthy(), "{path}: {:?}", check.findings());
    }
}

#[test]
fn the_gpu_and_the_cpu_agree_about_the_chart() {
    // The viewer renders on one and an export may take the other. If
    // they disagree about a flat patch, a grade approved in the viewer
    // is not the grade that ships.
    let [(_, cpu), (_, gpu)] = selfcheck::run_both(W, H, WorkingColorspace::Rec709Linear, RenderOptions::default());
    for (a, b) in cpu.patches.iter().zip(gpu.patches.iter()) {
        let de = klipp_color::measure::delta_e2000(a.back, b.back);
        assert!(de < 1.0, "{}: CPU and GPU differ by {de:.2} ΔE", a.name);
    }
}

#[test]
fn a_wider_working_space_still_returns_the_chart() {
    // Rec.2020 as the working space: the chart is sRGB, so it converts
    // in and back out. A matrix applied once each way is the identity;
    // a matrix applied once is a visibly different picture.
    for (gpu, check) in selfcheck::run_both(W, H, WorkingColorspace::Bt2020Linear, RenderOptions::default()) {
        let path = if gpu { "GPU" } else { "CPU" };
        assert!(check.ramp_ordered, "{path}: ramp folded in a wide working space");
        assert!(
            check.neutral_cast < NEUTRAL_CAST_FAIL,
            "{path}: greys picked up {:.2} chroma converting through Rec.2020",
            check.neutral_cast
        );
        assert!(
            check.max_de < CHART_WARN_DE,
            "{path}: {:.2} ΔE out at worst ({}) through Rec.2020",
            check.max_de,
            check.worst
        );
    }
}
