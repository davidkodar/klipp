//! The bug, as a test.
//!
//! A PQ clip carries the graph Klipp seeds on import — Source →
//! InputTransform(PQ) → Corrector → Output. With the decoder already
//! decoding PQ, the InputTransform decoded it again the moment the
//! Corrector left its defaults: measured live, temperature 0.1 took luma
//! from 80.9 to 15.6 and near-black pixels from 10.8% to 60.8%.
//!
//! This runs that exact graph the way the engine now does — scene-linear
//! input from the decoder's HDR codes, `GraphExecutor`, then the display
//! tone map — and requires a small grade to make a small change.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::hdr::{apply_hdr_display_tonemap, hdr10_codes_to_scene_linear_rgba, HdrTransfer};
use klipp_core::{color_ids, ColorPrimaries, LogFormat, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::p010_gpu::GpuP010Converter;
use klipp_nodes::{
    executor::GraphExecutor,
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTracks, NodeParams},
};

const W: u32 = 64;
const H: u32 = 32;

fn node(kind: NodeKind, label: &str, params: NodeParams) -> Node {
    Node {
        id: NodeId::new_v4(),
        kind,
        label: label.into(),
        enabled: true,
        params,
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: vec![],
    }
}

/// The graph a PQ clip gets on import, with the Corrector's params given.
fn seeded_pq_graph(corrector: NodeParams) -> NodeGraph {
    let mut it = NodeParams::default();
    it.log_format = Some(LogFormat::PQ);
    let nodes = vec![
        node(NodeKind::Source, "Source", NodeParams::default()),
        node(NodeKind::InputTransform, "Input Transform", it),
        node(NodeKind::Corrector, "Corrector", corrector),
        node(NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = (0..3)
        .map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 })
        .collect();
    let positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 110.0, y: 0.0 }))
        .collect();
    NodeGraph {
        output_node_id: nodes[3].id,
        nodes,
        edges,
        node_positions: positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// HDR P010 frame → the decoder's packed codes → scene-linear, exactly as
/// the engine derives the grade's input.
fn scene_linear_input() -> Option<Vec<f32>> {
    let mut conv = GpuP010Converter::new()?;
    let mut y = Vec::new();
    for row in 0..H {
        for col in 0..W {
            let v: u32 = 64 + ((col * 37 + row * 11) % 877);
            y.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    let mut uv = Vec::new();
    for row in 0..(H / 2) {
        for col in 0..(W / 2) {
            let u = 512 + (((col as i32 - row as i32) * 23).rem_euclid(300)) as u32 - 150;
            let v = 512 + (((col as i32 + row as i32) * 29).rem_euclid(300)) as u32 - 150;
            uv.extend_from_slice(&(((u & 0x3FF) << 6) as u16).to_le_bytes());
            uv.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Rec709Linear);
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT2020_NCL,
        color_ids::TRANSFER_PQ,
        color_ids::PRIMARIES_BT2020,
    ];
    let codes = conv.convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m))?;
    Some(hdr10_codes_to_scene_linear_rgba(&codes, HdrTransfer::Pq, m))
}

fn grade_and_display(exec: &mut GraphExecutor, input: &[f32], graph: &NodeGraph) -> Vec<f32> {
    let (mut out, _) = exec
        .execute_linear(input, graph, W, H, 0.0, 0, None, None)
        .expect("grade must succeed");
    apply_hdr_display_tonemap(&mut out);
    out
}

fn mean_rgb(px: &[f32]) -> [f32; 3] {
    let n = (px.len() / 4) as f32;
    let mut m = [0.0_f32; 3];
    for p in px.chunks_exact(4) {
        for c in 0..3 {
            m[c] += p[c];
        }
    }
    m.map(|v| v / n)
}

fn luma(m: [f32; 3]) -> f32 {
    0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2]
}

/// With the Corrector at its defaults the executor still runs the
/// InputTransform(PQ) node. It must leave the pixels alone — the decoder
/// already decoded PQ — so the output is exactly the input.
#[test]
fn the_seeded_pq_input_transform_leaves_scene_linear_untouched() {
    let Some(input) = scene_linear_input() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let mut exec = GraphExecutor::new();
    let (out, _) = exec
        .execute_linear(&input, &seeded_pq_graph(NodeParams::default()), W, H, 0.0, 0, None, None)
        .expect("grade must succeed");
    let worst = input.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    assert!(worst < 1e-4, "InputTransform(PQ) changed scene-linear pixels by up to {worst}");
}

/// The measured failure, inverted: a temperature grade on the seeded
/// graph must warm the image and leave its brightness roughly alone.
///
/// Uses 20 on the Corrector's -50..50 slider. The live measurement used
/// 0.1, which adds 0.0001 (`t = temp / 100 * 0.1`) — so the 5x crush it
/// caused came entirely from the PQ node switching on, not from the grade.
/// A value that small cannot show whether a grade *works*, only whether
/// it destroys the frame, so this uses one that visibly should.
#[test]
fn a_temperature_grade_on_hdr_warms_it_instead_of_crushing_it() {
    let Some(input) = scene_linear_input() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let mut exec = GraphExecutor::new();
    let identity = grade_and_display(&mut exec, &input, &seeded_pq_graph(NodeParams::default()));

    let mut warm = NodeParams::default();
    warm.temperature = Some(20.0);
    let graded = grade_and_display(&mut exec, &input, &seeded_pq_graph(warm));

    let (mi, mg) = (mean_rgb(&identity), mean_rgb(&graded));
    let ratio = luma(mg) / luma(mi);
    eprintln!(
        "identity mean {mi:?} luma {:.4} | temp 20 mean {mg:?} luma {:.4} | ratio {ratio:.3}",
        luma(mi), luma(mg),
    );
    assert!(
        (0.85..=1.15).contains(&ratio),
        "temperature 20 changed brightness by {:.0}% — the double-decode signature was ~80% darker",
        (ratio - 1.0) * 100.0,
    );
    // A shift worth the name, not float noise pointing the right way. The
    // noise floor is ~1e-5 relative (temperature 0.1 moves R/B by that
    // much); this frame measures +0.8% at temperature 20, so 0.3% sits
    // well clear of both.
    //
    // Why only 0.8%: the Corrector's temperature is an *additive* offset
    // (R += t, B -= t, t = temp / 100 * 0.1). Scene-linear HDR values are
    // large — this frame reaches ~160 — so a fixed offset is a small
    // relative change, where on an SDR clip averaging ~0.2 the same slider
    // position is a big one. That is a property of the operator, made
    // visible by grading in scene-linear; a multiplicative white balance
    // would behave the same on both.
    let (rb_before, rb_after) = (mi[0] / mi[2], mg[0] / mg[2]);
    assert!(
        rb_after > rb_before * 1.003,
        "a positive temperature must shift the balance toward red: R/B {rb_before:.4} -> {rb_after:.4}",
    );
}
