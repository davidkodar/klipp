//! The HDR Zones node (#240): the CPU executor draws exactly what
//! `klipp_effects::cpu_grade::hdr_zones` (the grade model, per pixel)
//! draws, and the GPU chain draws the same within a part in ten thousand,
//! each zone moved alone, its boundary and falloff moved too, in both
//! working spaces. A zone changes only the pixels it reaches, and a node
//! with every zone neutral is bypassed, its picture bit for bit.
//!
//! A missing GPU leaves the GPU half unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_color::grade_model::hdr_zones::{stops, weight, Terms, ZONES};
use klipp_core::{HdrZonesSettings, WorkingColorspace};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 8;
const TOLERANCE: f32 = 1e-4;

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Mid grey from 10 stops under to 8 over across the frame, each row a
/// different colour of it, alpha kept apart.
fn frame() -> Vec<f32> {
    let tints = [[1.0, 1.0, 1.0], [1.3, 0.9, 0.6], [0.5, 1.0, 1.4], [1.6, 0.4, 0.8], [0.9, 1.2, 0.7], [1.0, 0.95, 1.05], [0.2, 0.6, 1.8], [1.8, 1.1, 0.3]];
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let v = 0.18 * (-10.0 + 18.0 * x as f32 / (W - 1) as f32).exp2();
            let t = tints[y as usize];
            [v * t[0], v * t[1], v * t[2], 0.25 + 0.75 * x as f32 / W as f32]
        })
        .collect()
}

fn graph(z: HdrZonesSettings) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let s = node(NodeKind::Source, NodeParams::default());
    let d = node(NodeKind::HdrZones, NodeParams { hdr_zones: Some(z), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let edges = vec![Edge { from_node: s.id, from_port: 0, to_node: d.id, to_port: 0 }, Edge { from_node: d.id, from_port: 0, to_node: o.id, to_port: 0 }];
    NodeGraph { output_node_id: o.id, nodes: vec![s, d, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Zone `i` moved alone: exposure, a colour and saturation.
fn moved(i: usize) -> HdrZonesSettings {
    let mut z = HdrZonesSettings::default();
    z.exposure[i] = 0.75;
    z.colour_x[i] = 0.5;
    z.colour_y[i] = -0.3;
    z.saturation[i] = 1.4;
    z
}

fn cases() -> Vec<(String, HdrZonesSettings)> {
    let mut out: Vec<_> = (0..ZONES).map(|i| (format!("zone {i}"), moved(i))).collect();
    let mut placed = moved(5);
    placed.boundary[5] = 1.5;
    placed.falloff[5] = 2.5;
    out.push(("zone 5 placed at 1.5, 2.5 wide".into(), placed));
    let mut hard = moved(2);
    hard.falloff[2] = 0.0;
    out.push(("zone 2 with a hard edge".into(), hard));
    out
}

fn terms(z: &HdrZonesSettings, ws: WorkingColorspace) -> Terms {
    let colour = std::array::from_fn(|i| klipp_nodes::executor::hdr_zone_colour(z.colour_x[i], z.colour_y[i]));
    Terms::new(z.exposure, colour, z.saturation, z.boundary, z.falloff, klipp_color::primaries::luminance_weights(ws))
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs() / y.abs().max(1.0)).fold(0.0, f32::max)
}

#[test]
fn each_zone_draws_the_grade_model_on_the_cpu_and_the_gpu_chain() {
    let mut gpu = GraphExecutor::new();
    let on_gpu = gpu.gpu_pipeline_or_init().is_some();
    assert!(on_gpu || !required(), "required, but there is no GPU");
    let input = frame();
    for ws in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG] {
        let mut cpu = GraphExecutor::new_cpu_only();
        cpu.set_working_colorspace(ws);
        gpu.set_working_colorspace(ws);
        for (case, z) in cases() {
            let t = terms(&z, ws);
            let mut want = input.clone();
            klipp_effects::cpu_grade::hdr_zones(&mut want, &t);
            assert!(want != input, "{ws:?} {case}: changes the picture");
            let got = cpu.execute_linear(&input, &graph(z.clone()), W, H, 0.0, 0, None, None).unwrap().0;
            assert!(got.iter().map(|v| v.to_bits()).eq(want.iter().map(|v| v.to_bits())), "cpu: {ws:?} {case}");
            if !on_gpu {
                continue;
            }
            let got = gpu.execute_linear(&input, &graph(z.clone()), W, H, 0.0, 0, None, None).unwrap().0;
            let stats = gpu.last_run_chain_stats();
            assert_eq!((stats.cpu_nodes, stats.flushes), (1, 1), "gpu: {ws:?} {case}: on the chain ({stats:?})");
            let w = worst(&got, &want);
            eprintln!("gpu: {ws:?} {case}: worst {w}");
            assert!(w <= TOLERANCE, "gpu: {ws:?} {case}: worst {w}");
            // Control: the zone a quarter stop off draws far from it, so
            // the comparison bites on where a zone sits.
            let mut off = z.clone();
            for b in &mut off.boundary {
                *b += 0.25;
            }
            let mut shifted = input.clone();
            klipp_effects::cpu_grade::hdr_zones(&mut shifted, &terms(&off, ws));
            if !case.starts_with("zone 0") {
                assert!(worst(&got, &shifted) > 10.0 * TOLERANCE, "gpu: {ws:?} {case}: a quarter stop off is told apart");
            }
        }
    }
    if !on_gpu {
        eprintln!("UNVERIFIED: no GPU");
    }
}

/// A zone changes only what it reaches: a lower zone leaves every pixel
/// past its falloff above alone, bit for bit, an upper one every pixel
/// below. Control: Global changes them all.
#[test]
fn a_zone_changes_only_the_pixels_it_reaches() {
    let input = frame();
    let luma = klipp_color::primaries::luminance_weights(WorkingColorspace::Rec709Linear);
    let mut cpu = GraphExecutor::new_cpu_only();
    for i in 0..ZONES {
        let z = moved(i);
        let got = cpu.execute_linear(&input, &graph(z.clone()), W, H, 0.0, 0, None, None).unwrap().0;
        let (mut kept, mut changed) = (0, 0);
        for (a, b) in got.chunks_exact(4).zip(input.chunks_exact(4)) {
            let s = stops(luma[0] * b[0] + luma[1] * b[1] + luma[2] * b[2]);
            let reached = weight(i, s, z.boundary[i], z.falloff[i]) > 0.0;
            if reached {
                changed += usize::from(a != b);
            } else {
                assert_eq!(a, b, "zone {i}: a pixel at {s} stops it doesn't reach");
                kept += 1;
            }
        }
        assert!(changed > 0, "zone {i} changes what it reaches");
        if i == 0 {
            assert_eq!(kept, 0, "Global reaches everything");
        } else {
            assert!(kept > 0, "zone {i} leaves something alone");
        }
    }
}

/// Every zone neutral (the boundaries moved, which place nothing): the
/// node is bypassed, on the CPU and the GPU, the picture bit for bit.
#[test]
fn a_neutral_node_is_bypassed() {
    let input = frame();
    let mut z = HdrZonesSettings::default();
    z.boundary[3] = -2.0;
    z.falloff[5] = 3.0;
    let mut gpu = GraphExecutor::new();
    let on_gpu = gpu.gpu_pipeline_or_init().is_some();
    let mut cpu = GraphExecutor::new_cpu_only();
    let exs: Vec<&mut GraphExecutor> = if on_gpu { vec![&mut cpu, &mut gpu] } else { vec![&mut cpu] };
    for ex in exs {
        let got = ex.execute_linear(&input, &graph(z.clone()), W, H, 0.0, 0, None, None).unwrap().0;
        assert!(got.iter().map(|v| v.to_bits()).eq(input.iter().map(|v| v.to_bits())));
        assert_eq!(ex.last_run_chain_stats().extends, 0, "nothing dispatched");
    }
}

/// The zones' presets (#240) are starting points that do what their names
/// say: each named once, each changes the picture, the roll-off darkens
/// only above the light zone, Lift shadows brightens only below mid grey,
/// and the split tone warms the highlights and cools the shadows.
/// Control: a neutral node changes nothing (`a_neutral_node_is_bypassed`).
#[test]
fn the_presets_do_what_their_names_say() {
    // Scene-referred (HDR, log): the boundaries the stops below are set by.
    let presets = HdrZonesSettings::presets(true);
    let mut names: Vec<&str> = presets.iter().map(|(n, _, _)| *n).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), presets.len(), "each named once");
    let input = frame();
    let luma = klipp_color::primaries::luminance_weights(WorkingColorspace::Rec709Linear);
    let mut cpu = GraphExecutor::new_cpu_only();
    let run = |cpu: &mut GraphExecutor, z: &HdrZonesSettings| cpu.execute_linear(&input, &graph(z.clone()), W, H, 0.0, 0, None, None).unwrap().0;
    // The grey row's pixels by stop.
    let grey = |out: &[f32]| -> Vec<(f32, [f32; 3])> {
        (0..W as usize).map(|x| {
            let (a, b) = (&out[x * 4..x * 4 + 3], &input[x * 4..x * 4 + 3]);
            (stops(luma[0] * b[0] + luma[1] * b[1] + luma[2] * b[2]), [a[0], a[1], a[2]])
        }).collect()
    };
    for (name, _, z) in &presets {
        assert!(!z.is_neutral(), "{name}");
        assert_ne!(run(&mut cpu, z), input, "{name} changes the picture");
        assert_eq!(HdrZonesSettings::preset(&name.to_uppercase(), true).as_ref(), Some(z), "{name} found by name, any case");
    }
    let rolled = grey(&run(&mut cpu, &HdrZonesSettings::preset("HDR highlight roll-off", true).unwrap()));
    for (s, px) in &rolled {
        let was = 0.18 * s.exp2();
        if *s < 1.4 { assert!((px[1] - was).abs() <= 1e-6 * was.max(1.0), "roll-off at {s} stops"); }
        if *s > 3.5 { assert!(px[1] < was * 0.75, "roll-off darkens {s} stops"); }
    }
    let lifted = grey(&run(&mut cpu, &HdrZonesSettings::preset("Lift shadows", true).unwrap()));
    for (s, px) in &lifted {
        let was = 0.18 * s.exp2();
        if *s > -0.4 { assert!((px[1] - was).abs() <= 1e-6 * was.max(1.0), "Lift shadows leaves {s} stops"); }
        if (-3.0..-1.6).contains(s) { assert!(px[1] > was * 1.2, "Lift shadows opens {s} stops"); }
    }
    let split = grey(&run(&mut cpu, &HdrZonesSettings::preset("Warm highlights, cool shadows", true).unwrap()));
    for (s, px) in &split {
        if *s > 3.2 { assert!(px[0] > px[2], "warm at {s} stops: {px:?}"); }
        if (-4.0..-1.6).contains(s) { assert!(px[2] > px[0], "cool at {s} stops: {px:?}"); }
    }
}
