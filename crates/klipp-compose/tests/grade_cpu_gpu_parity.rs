//! Every grade control gives the same picture on the CPU as on the GPU.
//!
//! The GPU grades whenever it can; the CPU grades when there is no usable
//! GPU. Each case renders one node — a single control, or several together,
//! which is where a different order of operations shows — both ways on the
//! same frame and compares the display bytes. Each case must also change
//! the frame: a control one side ignores would match an ignoring other side.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ColorMatchSettings, ColorWarperParams, CurveSet, HueSatCurves, RationalTime};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

/// Hue sweep across, brightness down the frame (a little above 1 at the
/// top, for the scene-linear case), a grey strip along the bottom.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let hue = x as f32 / w as f32 * 6.0;
                let level = 1.3 * (1.0 - y as f32 / h as f32).powi(2) + 0.002;
                let rgb = if y > h * 7 / 8 {
                    [x as f32 / w as f32; 3]
                } else {
                    let f = |n: f32| {
                        let k = (n + hue) % 6.0;
                        1.0 - (k.min(4.0 - k).clamp(0.0, 1.0)) * 0.8
                    };
                    [f(5.0), f(3.0), f(1.0)].map(|c| c * level)
                };
                v.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn render(kind: NodeKind, params: &NodeParams, gpu: bool) -> Vec<f32> {
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params.clone()), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let graph = NodeGraph {
        output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(),
        working_space_override: None, frames: vec![], sticky_notes: vec![],
    };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.node_graph = graph;
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default())
}

/// (worst, mean) channel difference in display bytes.
fn diff(a: &[f32], b: &[f32]) -> (i32, f64) {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    let d: Vec<i32> = a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).abs()).collect();
    (d.iter().copied().max().unwrap_or(0), d.iter().map(|&v| v as f64).sum::<f64>() / d.len() as f64)
}

fn p(set: impl FnOnce(&mut NodeParams)) -> NodeParams {
    let mut p = NodeParams::default();
    set(&mut p);
    p
}

fn curves() -> CurveSet {
    CurveSet {
        master: vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.82], [1.0, 1.0]],
        red: vec![[0.0, 0.0], [0.5, 0.56], [1.0, 1.0]],
        green: vec![[0.0, 0.0], [1.0, 1.0]],
        blue: vec![[0.0, 0.03], [0.5, 0.45], [1.0, 1.0]],
    }
}

fn huesat() -> HueSatCurves {
    HueSatCurves { hue_vs_sat: vec![[0.0, 0.5], [0.33, 0.8], [0.66, 0.4], [1.0, 0.5]], ..Default::default() }
}

fn warper() -> ColorWarperParams {
    let mut w = ColorWarperParams::default();
    w.hue_shifts[0] = 15.0;
    w.sat_shifts[4] = 0.4;
    w
}

fn cases() -> Vec<(&'static str, NodeKind, NodeParams)> {
    use NodeKind::*;
    vec![
        // The Corrector, one control at a time.
        ("exposure", Corrector, p(|p| p.exposure = Some(0.6))),
        ("contrast", Corrector, p(|p| p.contrast = Some(0.3))),
        ("clarity", Corrector, p(|p| p.clarity = Some(0.5))),
        ("saturation", Corrector, p(|p| p.saturation = Some(1.4))),
        ("vibrance", Corrector, p(|p| p.vibrance = Some(0.5))),
        ("tone", Corrector, p(|p| { p.highlights = Some(-0.3); p.shadows = Some(0.2); p.whites = Some(0.1); p.blacks = Some(-0.1) })),
        ("wheels", Corrector, p(|p| { p.lift_x = Some(0.1); p.gamma_y = Some(-0.1); p.gain_master = Some(0.1); p.offset_x = Some(0.05) })),
        ("log wheels", Corrector, p(|p| { p.wheels_log_mode = Some(true); p.log_low_x = Some(0.2); p.log_mid_y = Some(0.15); p.log_high_master = Some(0.1) })),
        ("wheel sat/bri", Corrector, p(|p| { p.gain_sat = Some(0.3); p.lift_bri = Some(0.1) })),
        ("temperature/tint", Corrector, p(|p| { p.temperature = Some(30.0); p.tint = Some(-20.0) })),
        ("hue", Corrector, p(|p| p.hue = Some(30.0))),
        ("curves", Corrector, p(|p| p.curves = Some(curves()))),
        ("hue/sat curves", Corrector, p(|p| p.huesat = Some(huesat()))),
        ("warper", Corrector, p(|p| p.warper_params = Some(warper()))),
        // Together: where the order shows.
        ("contrast+sat+wheels", Corrector, p(|p| { p.contrast = Some(0.3); p.saturation = Some(1.4); p.gain_x = Some(0.2); p.lift_master = Some(0.05) })),
        ("exposure+temperature+contrast", Corrector, p(|p| { p.exposure = Some(0.4); p.temperature = Some(25.0); p.contrast = Some(0.25) })),
        ("a full grade", Corrector, p(|p| {
            p.exposure = Some(0.3); p.contrast = Some(0.25); p.saturation = Some(1.3); p.temperature = Some(25.0);
            p.highlights = Some(-0.2); p.gain_x = Some(0.15); p.vibrance = Some(0.3); p.hue = Some(10.0);
            p.clarity = Some(0.3); p.curves = Some(curves());
        })),
        ("a full grade at 60%", Corrector, p(|p| {
            p.exposure = Some(0.3); p.contrast = Some(0.25); p.saturation = Some(1.3); p.vibrance = Some(0.3);
            p.curves = Some(curves()); p.global_mix = Some(0.6);
        })),
        // The single-purpose nodes.
        ("Color Balance", ColorBalance, p(|p| { p.temperature = Some(30.0); p.tint = Some(10.0) })),
        ("Color Balance hue", ColorBalance, p(|p| p.hue = Some(30.0))),
        ("Color Balance at 50%", ColorBalance, p(|p| { p.temperature = Some(40.0); p.hue = Some(20.0); p.global_mix = Some(0.5) })),
        ("Color Wheels", ColorWheels, p(|p| { p.lift_x = Some(0.1); p.gain_y = Some(0.1) })),
        ("Color Wheels log", ColorWheels, p(|p| { p.wheels_log_mode = Some(true); p.log_low_x = Some(0.2); p.log_high_y = Some(0.1) })),
        ("Color Match", ColorMatch, p(|p| p.color_match = Some(ColorMatchSettings { exposure: 0.3, temperature: 20.0, tint: 10.0, saturation: 1.2 }))),
        ("Exposure", Exposure, p(|p| p.exposure = Some(-0.5))),
        ("Contrast + clarity", Contrast, p(|p| { p.contrast = Some(0.2); p.clarity = Some(0.4) })),
        ("Saturation + vibrance", Saturation, p(|p| { p.saturation = Some(0.8); p.vibrance = Some(0.4) })),
        ("Tone Controls", ToneControls, p(|p| { p.highlights = Some(-0.2); p.blacks = Some(0.1) })),
        ("Wheel Sat/Bri", WheelSatBri, p(|p| { p.gamma_sat = Some(0.3); p.gain_bri = Some(-0.1) })),
        ("RGB Curves", RGBCurves, p(|p| { p.curves = Some(curves()); p.curves_preserve_luma = Some(true) })),
        ("Hue/Sat Curves", HueSatCurves, p(|p| p.huesat = Some(huesat()))),
        ("Color Warper", ColorWarper, p(|p| p.warper_params = Some(warper()))),
        ("Qualifier", Qualifier, p(|p| p.qualifier = Some(red_window()))),
        ("Qualifier off-centre", Qualifier, p(|p| p.qualifier = Some(off_centre_window()))),
    ]
}

/// A flat frame, for the behaviour tests below.
struct Flat([f32; 3]);

impl FrameSource for Flat {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let px = vec![self.0[0], self.0[1], self.0[2], 1.0].repeat((w * h) as usize);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn render_flat_node(value: [f32; 3], kind: NodeKind, params: &NodeParams, gpu: bool) -> [f32; 3] {
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params.clone()), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let graph = NodeGraph {
        output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(),
        working_space_override: None, frames: vec![], sticky_notes: vec![],
    };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.node_graph = graph;
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    let out = r.render(&seq, tc(1.0), W, H, &mut Flat(value), RenderOptions::default());
    let i = ((H / 2 * W + W / 2) * 4) as usize;
    [out[i], out[i + 1], out[i + 2]]
}

fn render_flat(value: f32, params: &NodeParams, gpu: bool) -> [f32; 3] {
    render_flat_node([value; 3], NodeKind::RGBCurves, params, gpu)
}

fn have_gpu() -> bool {
    klipp_effects::gpu::GpuEffectsPipeline::new().is_some()
}

/// A red window over the middle of the hue wheel.
fn red_window() -> klipp_core::QualifierSettings {
    klipp_core::QualifierSettings {
        hue_center: 0.0, hue_width: 60.0, hue_soft: 10.0,
        sat_low: 0.2, sat_high: 1.0, sat_soft: 0.1,
        lum_low: 0.0, lum_high: 1.0, lum_soft: 0.1,
        ..Default::default()
    }
}

/// A narrow window away from hue 0, which is the case that pins the
/// hue *unit*. The window above centres on red, and a hue centre read in
/// the wrong unit — degrees where the shader wants turns — still lands
/// at 0 and the two paths agree by luck. Off centre and narrow, they
/// agree only if both read the centre the same way. (They do: the GPU
/// entry point divides by 360 before the uniform, the CPU caller divides
/// before the twin. Worth a case rather than a re-reading.)
fn off_centre_window() -> klipp_core::QualifierSettings {
    klipp_core::QualifierSettings {
        hue_center: 27.0, hue_width: 20.0, hue_soft: 10.0,
        sat_low: 0.2, sat_high: 0.5, sat_soft: 0.05,
        lum_low: 0.1, lum_high: 0.85, lum_soft: 0.04,
        ..Default::default()
    }
}

/// The hue/sat tools keep a bright colour's colour. Their HSL maths ran
/// on linear light, where saturation is only defined below 1: a
/// highlight's saturation came out negative, clamped to zero, and every
/// bright colour — a neon sign, a sunlit dress — came back grey.
#[test]
fn the_hue_sat_tools_keep_bright_colours() {
    // Saturation lifted on the reds, so the case bites on a red highlight.
    let red_sat = HueSatCurves { hue_vs_sat: vec![[0.0, 0.70], [0.25, 0.5], [1.0, 0.5]], ..Default::default() };
    let huesat_p = p(|p| p.huesat = Some(red_sat));
    let warper_p = p(|p| p.warper_params = Some(warper()));
    for gpu in [false, true] {
        if gpu && !have_gpu() {
            continue;
        }
        for (label, kind, params) in [
            ("Hue/Sat Curves", NodeKind::HueSatCurves, &huesat_p),
            ("Color Warper", NodeKind::ColorWarper, &warper_p),
        ] {
            for input in [[0.8f32, 0.2, 0.1], [4.0, 1.0, 0.5], [12.0, 3.0, 1.5]] {
                let got = render_flat_node(input, kind, params, gpu);
                eprintln!("{label} (gpu {gpu}) {input:?} -> [{:.3}, {:.3}, {:.3}]", got[0], got[1], got[2]);
                let ratio = got[0] / got[2].max(1e-6);
                let was = input[0] / input[2];
                assert!(ratio > was * 0.8, "{label} flattened {input:?} to {got:?}");
                // And it stays about as bright as it was.
                assert!(got[0] > input[0] * 0.5 && got[0] < input[0] * 2.0, "{label}: {input:?} -> {got:?}");
            }
        }
    }
}

/// The qualifier selects a bright colour too: a saturated red qualified
/// at 0.6 but not at 4.0, so a secondary correction quietly skipped
/// every highlight in log or PQ footage.
#[test]
fn the_qualifier_selects_bright_colours() {
    let params = p(|p| p.qualifier = Some(red_window()));
    for gpu in [false, true] {
        if gpu && !have_gpu() {
            continue;
        }
        for input in [[0.6f32, 0.1, 0.1], [4.0, 0.7, 0.7], [12.0, 2.0, 2.0]] {
            let matte = render_flat_node(input, NodeKind::Qualifier, &params, gpu)[0];
            eprintln!("qualifier (gpu {gpu}) {input:?} -> matte {matte:.3}");
            assert!(matte > 0.9, "a bright red did not qualify: {input:?} -> {matte}");
        }
        // A blue is still outside the window.
        let matte = render_flat_node([0.1, 0.2, 4.0], NodeKind::Qualifier, &params, gpu)[0];
        assert!(matte < 0.1, "a bright blue qualified as red: {matte}");
    }
}

/// The curve's axis is the picture, not the light: a point lifted at 0.46
/// — the middle of the curve, where mid-grey sits on screen — moves
/// mid-grey. On the light axis 0.18 was mid-grey and 0.46 was a
/// three-quarter tone, so the same curve barely touched the mids.
#[test]
fn a_curve_moves_the_tones_it_is_drawn_over() {
    let lifted = CurveSet { master: vec![[0.0, 0.0], [0.46, 0.60], [1.0, 1.0]], ..Default::default() };
    let params = p(|p| p.curves = Some(lifted));
    for gpu in [false, true] {
        if gpu && klipp_effects::gpu::GpuEffectsPipeline::new().is_none() {
            continue;
        }
        let got = render_flat(0.18, &params, gpu);
        // 0.18 linear is 0.46 encoded; the curve sends it to 0.60, which
        // is 0.325 in light.
        let want = klipp_color::transfer::srgb_to_linear(0.60);
        eprintln!("mid-grey through a lifted curve (gpu {gpu}): {got:?}, want {want:.4}");
        assert!((got[0] - want).abs() < 0.02, "{got:?} vs {want}");
    }
}

/// Past the ends the curve continues straight, so a curve drawn in the
/// SDR range leaves an HDR highlight where it was instead of flattening
/// it to white: a 4.0 highlight (four times SDR white) used to come back
/// as 1.0 whatever the curve did up there.
#[test]
fn a_curve_keeps_the_highlights_above_white() {
    let shadows = CurveSet { master: vec![[0.0, 0.0], [0.25, 0.34], [1.0, 1.0]], ..Default::default() };
    let params = p(|p| p.curves = Some(shadows));
    for gpu in [false, true] {
        if gpu && klipp_effects::gpu::GpuEffectsPipeline::new().is_none() {
            continue;
        }
        let got = render_flat(4.0, &params, gpu);
        eprintln!("a 4.0 highlight through a shadow curve (gpu {gpu}): {got:?}");
        assert!(got[0] > 3.0, "the highlight was crushed to {:?}", got);
        assert!(got[0] < 5.5, "the highlight ran away to {:?}", got);
    }
}

#[test]
fn every_grade_control_matches_between_cpu_and_gpu() {
    if klipp_effects::gpu::GpuEffectsPipeline::new().is_none() {
        eprintln!("skipping: no GPU");
        return;
    }
    let ungraded = render(NodeKind::Corrector, &NodeParams::default(), true);
    let mut failures = Vec::new();
    for (name, kind, params) in cases() {
        let cpu = render(kind, &params, false);
        let gpu = render(kind, &params, true);
        let (worst, mean) = diff(&cpu, &gpu);
        let (_, gpu_change) = diff(&ungraded, &gpu);
        let (_, cpu_change) = diff(&ungraded, &cpu);
        eprintln!("{name:30} CPU vs GPU worst {worst:3} mean {mean:6.2} | change GPU {gpu_change:6.2} CPU {cpu_change:6.2}");
        if worst > 1 {
            failures.push(format!("{name}: CPU and GPU differ by {worst} levels"));
        }
        if gpu_change < 0.05 {
            failures.push(format!("{name}: the GPU leaves the frame as it was"));
        }
        if cpu_change < 0.05 {
            failures.push(format!("{name}: the CPU leaves the frame as it was"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
