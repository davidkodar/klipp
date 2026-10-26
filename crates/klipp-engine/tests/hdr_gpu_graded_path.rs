//! The GPU-resident graded path, checked against the CPU path it replaces.
//!
//! Graded 4K HDR playback measured 5.8 fps with the grade itself taking
//! 5.7 ms — the rest was 132 MB float frames crossing the bus. This path
//! keeps the frame on the GPU from decode to display: the chain is seeded
//! from the decoder's HDR code values, graded, then tone-mapped and
//! encoded to display u8 on the GPU, and only those 4 bytes a pixel come
//! back. Each GPU step must produce what its CPU counterpart does.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::display::{display_transform_ws, ToneMapOp};
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

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id: NodeId::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: vec![],
    }
}

/// The graph a PQ clip is seeded with, Corrector warmed.
fn graded_pq_graph() -> NodeGraph {
    let it = NodeParams {
        log_format: Some(LogFormat::PQ),
        ..Default::default()
    };
    let corrector = NodeParams {
        temperature: Some(20.0),
        ..Default::default()
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::InputTransform, it),
        node(NodeKind::Corrector, corrector),
        node(NodeKind::Output, NodeParams::default()),
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

struct Frame {
    gpu_codes: std::sync::Arc<wgpu::Buffer>,
    cpu_scene_linear: Vec<f32>,
    matrix4: [[f32; 4]; 3],
}

/// One HDR P010 frame, as both the GPU codes buffer the decoder hands the
/// engine and the CPU scene-linear the CPU path grades.
fn hdr_frame() -> Option<Frame> {
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
    let matrix4 = pack_primaries_matrix(m);
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT2020_NCL,
        color_ids::TRANSFER_PQ,
        color_ids::PRIMARIES_BT2020,
    ];
    let codes = conv.convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, matrix4)?;
    let out = conv.convert_p010_on_gpu(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, matrix4);
    Some(Frame {
        gpu_codes: out.hdr_codes?,
        cpu_scene_linear: hdr10_codes_to_scene_linear_rgba(&codes, HdrTransfer::Pq, m),
        matrix4,
    })
}

/// The CPU path's graded scene-linear result, for reference.
fn cpu_graded(frame: &Frame) -> Vec<f32> {
    let mut exec = GraphExecutor::new();
    exec.execute_linear(&frame.cpu_scene_linear, &graded_pq_graph(), W, H, 0.0, 0, None, None)
        .expect("CPU-path grade")
        .0
}

#[test]
fn a_chain_seeded_from_hdr_codes_on_the_gpu_grades_like_the_cpu_path() {
    let Some(frame) = hdr_frame() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let reference = cpu_graded(&frame);

    let mut exec = GraphExecutor::new();
    assert!(exec.prime_chain_from_gpu_hdr10_buffer(
        &frame.gpu_codes, W, H, color_ids::TRANSFER_PQ, 100.0, frame.matrix4,
    ));
    let (gpu, _) = exec
        .execute_linear(&[], &graded_pq_graph(), W, H, 0.0, 0, None, None)
        .expect("a chain seeded from HDR codes must run with no CPU source");

    assert_eq!(gpu.len(), reference.len());
    // Compare against the input's own precision. PQ's 10-bit code step is
    // 0.9-1.0% relative across its range (it is built to be perceptually
    // even), so a quarter of a step, 2.25e-3, is the line: below it, GPU
    // `pow` and the CPU's table disagree by less than the source can
    // express.
    //
    // Relative to the *pixel's* magnitude, not each channel's own. The
    // working-space matrix mixes channels, so a tiny relative difference in
    // a bright channel lands as an absolute one in a dim neighbour: the
    // worst case here is a saturated green near PQ peak, [-0.58, 108.8,
    // -0.39], whose negative red differs by 0.003 — 3e-5 of the pixel. A
    // per-channel denominator scored that as 3e-3.
    let worst = reference
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .flat_map(|(a, b)| {
            let magnitude = a[..3].iter().fold(1.0_f32, |m, v| m.max(v.abs()));
            (0..3).map(move |c| (a[c] - b[c]).abs() / magnitude)
        })
        .fold(0.0_f32, f32::max);
    eprintln!("GPU seed vs CPU input transform, graded: worst pixel-relative error {worst:.2e}");
    assert!(worst < 2.25e-3, "GPU HDR seed disagrees with the CPU path by {worst:.2e}");
}

#[test]
fn the_gpu_display_encode_matches_the_cpu_tonemap_and_display_transform() {
    let Some(frame) = hdr_frame() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    // CPU path, end to end: grade, tone-map, display encode.
    let mut expected = cpu_graded(&frame);
    apply_hdr_display_tonemap(&mut expected, HdrTransfer::Pq);
    let expected = display_transform_ws(&expected, ToneMapOp::None, WorkingColorspace::Rec709Linear);

    // GPU path: seed, grade, keep on GPU, encode, read back u8.
    let mut exec = GraphExecutor::new();
    assert!(exec.prime_chain_from_gpu_hdr10_buffer(
        &frame.gpu_codes, W, H, color_ids::TRANSFER_PQ, 100.0, frame.matrix4,
    ));
    exec.request_output_on_gpu();
    let (cpu_side, _) = exec
        .execute_linear(&[], &graded_pq_graph(), W, H, 0.0, 0, None, None)
        .expect("grade");
    assert!(exec.output_on_gpu().is_some(), "the result should have stayed on the GPU");
    assert!(cpu_side.is_empty(), "nothing is downloaded when the result stays on the GPU");
    let got = exec.encode_output_to_display_u8(W, H, Some(HdrTransfer::Pq.display_exposure())).expect("GPU display encode");

    assert_eq!(got.len(), expected.len());
    let worst = got
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(i, _)| i % 4 != 3)
        .map(|(_, (a, b))| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap();
    eprintln!("GPU display encode vs CPU tonemap + display transform: worst {worst} LSB");
    assert!(worst <= 1, "GPU and CPU display paths disagree by {worst} LSB");
}

/// Output-on-GPU is a request, not a mode: honoured for one run, and
/// refused when something needs the final pixels on the CPU.
#[test]
fn output_on_gpu_is_single_shot_and_refused_when_the_result_is_tapped() {
    let Some(frame) = hdr_frame() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let graph = graded_pq_graph();
    let final_node = graph.input_of(graph.output_node_id, 0).unwrap();
    let mut exec = GraphExecutor::new();

    // Not requested: normal download.
    assert!(exec.prime_chain_from_gpu_hdr10_buffer(&frame.gpu_codes, W, H, color_ids::TRANSFER_PQ, 100.0, frame.matrix4));
    let (out, _) = exec.execute_linear(&[], &graph, W, H, 0.0, 0, None, None).unwrap();
    assert!(exec.output_on_gpu().is_none());
    assert_eq!(out.len(), (W * H * 4) as usize);

    // Requested, but the final node is the scope tap: its pixels are
    // wanted on the CPU, so the request is declined and the download
    // happens. A tapped node cannot chain, so with only a GPU seed the run
    // reports CpuSourceRequired — the engine's cue to supply real pixels,
    // which is what this does.
    assert!(exec.prime_chain_from_gpu_hdr10_buffer(&frame.gpu_codes, W, H, color_ids::TRANSFER_PQ, 100.0, frame.matrix4));
    exec.request_output_on_gpu();
    assert!(matches!(
        exec.execute_linear(&[], &graph, W, H, 0.0, 0, Some(final_node), None),
        Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired),
    ));
    exec.request_output_on_gpu();
    let (out, _) = exec
        .execute_linear(&frame.cpu_scene_linear, &graph, W, H, 0.0, 0, Some(final_node), None)
        .unwrap();
    assert!(exec.output_on_gpu().is_none(), "a tapped result must come back to the CPU");
    assert_eq!(out.len(), (W * H * 4) as usize);

    // The declined request does not leak into the next run.
    assert!(exec.prime_chain_from_gpu_hdr10_buffer(&frame.gpu_codes, W, H, color_ids::TRANSFER_PQ, 100.0, frame.matrix4));
    let (out, _) = exec.execute_linear(&[], &graph, W, H, 0.0, 0, None, None).unwrap();
    assert!(exec.output_on_gpu().is_none(), "the request is single-shot");
    assert_eq!(out.len(), (W * H * 4) as usize);
}

/// J4 (phase 2b M2, step 11c): a PQ clip's grade, its display step and a
/// glow masked by a window, as the renderer splices them into the clip's
/// plan, draw on the GPU what they draw on the CPU, and the run reports
/// the path a masked effect takes: the matte made off the chain and
/// uploaded, the display step and the masked glow on it, one flush, no
/// effect breaking the chain and no mix refused.
#[test]
fn a_masked_effect_on_an_hdr_clip_takes_the_graded_path() {
    use klipp_color::hdr::SceneToDisplay;
    use klipp_core::{Mask, MaskShape, MaskStack};
    use klipp_effects::mask_mix::MaskChannel;
    use klipp_effects::BuiltinEffect;
    use klipp_nodes::params::EffectBlend;

    let Some(frame) = hdr_frame() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    assert!(
        klipp_effects::registry::effect_types().any(|t| t.tag == "glow" && t.desc.mixable),
        "glow takes a mask"
    );
    let mut graph = graded_pq_graph();
    let step = SceneToDisplay {
        transfer: HdrTransfer::Pq,
        peak_nits: None,
        luma: klipp_color::primaries::luminance_weights(WorkingColorspace::Rec709Linear),
    };
    graph.splice_before_output(node(NodeKind::SceneToDisplay, NodeParams { scene_to_display: Some(step), ..Default::default() }));
    let source = graph.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let window = node(NodeKind::Mask, NodeParams {
        masks: Some(MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.2, ..Mask::default() }] }),
        ..Default::default()
    });
    let glow = node(NodeKind::Effect, NodeParams {
        effects: vec![BuiltinEffect::Glow { radius: 4.0, intensity: 0.8, threshold: 0.0 }],
        effect_blend: Some(EffectBlend { mix: 1.0, channel: MaskChannel::Luminance }),
        ..Default::default()
    });
    graph.edges.push(Edge { from_node: source, from_port: 0, to_node: window.id, to_port: 0 });
    graph.edges.push(Edge { from_node: window.id, from_port: 0, to_node: glow.id, to_port: 1 });
    let (glow_id, window_id) = (glow.id, window.id);
    graph.nodes.push(window);
    graph.splice_before_output(glow);
    assert_eq!(graph.input_of(glow_id, 1), Some(window_id), "the glow reads the window");

    // A scene-referred clip's run, as the renderer says it is (#1867).
    let scene = klipp_nodes::domains::PictureDomain::Scene;
    let mut cpu = GraphExecutor::new_cpu_only();
    cpu.set_source_domain(scene);
    let want = cpu.execute_linear(&frame.cpu_scene_linear, &graph, W, H, 0.0, 0, None, None).expect("CPU run").0;
    let mut gpu = GraphExecutor::new();
    gpu.set_source_domain(scene);
    if gpu.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let got = gpu.execute_linear(&frame.cpu_scene_linear, &graph, W, H, 0.0, 0, None, None).expect("GPU run").0;
    let stats = gpu.last_run_chain_stats();
    assert_eq!((stats.flushes, stats.effect_breaks, stats.effect_mixes_refused), (1, 0, 0), "{stats:?}");
    // The display step maps to [0, 1]; the glow runs on its own shader on
    // either side. A level of an 8-bit picture is the bar, as C9 holds a
    // masked GPU effect to.
    let worst = got.iter().zip(&want).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("J4: worst {worst}");
    assert!(worst <= 1.0 / 255.0, "the GPU differs from the CPU by {worst}");
    // The mask bites: outside the window the glow is gone.
    let mut unmasked = graph.clone();
    unmasked.nodes.iter_mut().find(|n| n.id == glow_id).unwrap().params.effect_blend = None;
    let whole = cpu.execute_linear(&frame.cpu_scene_linear, &unmasked, W, H, 0.0, 0, None, None).unwrap().0;
    assert!(whole != want, "the window changes the picture");
}

/// #1226: a 12-bit PQ gradient's 16-bit codes, seeded on the GPU through
/// the 16-bit bridge and graded there, match the same codes converted and
/// graded on the CPU, within a quarter of the source's own 12-bit step
/// (the 10-bit bridge's bar is a quarter of a 10-bit step).
#[test]
fn a_deep_source_seeded_on_the_gpu_grades_like_the_cpu_path() {
    use klipp_color::hdr::{yuv_to_hdr_codes, SceneCodes, YuvSamples};
    // Luma across the legal range, chroma swept with it, 4:4:4 12-bit.
    let n = (W * H) as usize;
    let y: Vec<u16> = (0..n).map(|i| (256 + i * (3760 - 256) / (n - 1)) as u16).collect();
    let u: Vec<u16> = (0..n).map(|i| (1800 + (i % W as usize) * 8) as u16).collect();
    let v: Vec<u16> = (0..n).map(|i| (2300 - (i / W as usize) * 12) as u16).collect();
    let s = YuvSamples { y: &y, u: &u, v: &v, width: W, height: H, chroma_shift_w: 0, chroma_shift_h: 0, bit_depth: 12 };
    let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT2020_NCL, color_ids::TRANSFER_PQ, color_ids::PRIMARIES_BT2020];
    let codes = std::sync::Arc::new(yuv_to_hdr_codes(&s, W, H, 0, ids));
    assert!(matches!(*codes, SceneCodes::Deep16(_)));
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Rec709Linear);

    let mut cpu = GraphExecutor::new();
    let (reference, _) = cpu
        .execute_linear(&codes.to_scene_linear_rgba(HdrTransfer::Pq, m), &graded_pq_graph(), W, H, 0.0, 0, None, None)
        .expect("CPU-path grade");

    let mut exec = GraphExecutor::new();
    if !exec.prime_chain_from_hdr10_codes(&codes, W, H, color_ids::TRANSFER_PQ, HdrTransfer::Pq.scene_scale(), pack_primaries_matrix(m)) {
        eprintln!("skipping: no GPU to seed");
        return;
    }
    let (gpu, _) = exec.execute_linear(&[], &graded_pq_graph(), W, H, 0.0, 0, None, None).expect("a chain seeded from 16-bit codes runs with no CPU source");
    assert_eq!(gpu.len(), reference.len());
    // Pixel-relative, as the 10-bit test measures it.
    let worst = reference
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .flat_map(|(a, b)| {
            let magnitude = a[..3].iter().fold(1.0_f32, |m, v| m.max(v.abs()));
            (0..3).map(move |c| (a[c] - b[c]).abs() / magnitude)
        })
        .fold(0.0_f32, f32::max);
    eprintln!("16-bit GPU seed vs CPU, graded: worst pixel-relative error {worst:.2e}");
    assert!(worst < 2.25e-3 / 4.0, "the 16-bit GPU seed disagrees with the CPU path by {worst:.2e}");
}
