//! D1 (the acceptance spec's C9 through a published Grade-group port):
//! each matte producer inside a clip's grade, its matte published by the
//! grade group and read by an effect outside it, on a picture-truth clip
//! (node-graph M2 step 9c-2). The masked, mixed blur draws what the parts
//! give, `graded + (blurred - graded) × mix × matte` with the matte as the
//! grade makes it, on the CPU and on the GPU. (It lived in klipp-nodes'
//! masked_effect_matrix as an ignored stub; the lowering from a published
//! port to the run's nodes is the renderer's, so it is tested here.)

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::PortBinding;
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 96;
const H: u32 = 54;
const MIX: f32 = 0.75;

/// Stripes across a hue ramp, so a blur shows and each key finds an edge.
fn chart(w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let stripe = if (x / 4) % 2 == 0 { 0.9 } else { 0.15 };
            v.extend_from_slice(&[stripe * fx + 0.05, 0.15 + 0.6 * fy, stripe * (1.0 - fx), 1.0]);
        }
    }
    v
}

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(chart(w, h))), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

#[derive(Clone, Copy, Debug)]
enum Producer {
    Qualifier,
    LumaKey,
    DifferenceKey,
    Window,
}

/// A flat plate for the difference key: the chart's mid grey, so what
/// differs from it keys.
fn plate() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_d1_plate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("plate.png");
    if !path.exists() {
        let px = vec![110u8; (W * H * 3) as usize];
        image::save_buffer(&path, &px, W, H, image::ExtendedColorType::Rgb8).unwrap();
    }
    path
}

fn producer(p: Producer) -> Node {
    match p {
        Producer::Qualifier => node(NodeKind::Qualifier, NodeParams {
            qualifier: Some(klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 90.0, sat_low: 0.05, ..Default::default() }),
            ..Default::default()
        }),
        Producer::LumaKey => node(NodeKind::LumaKey, NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.3, high: 0.4, soft: 0.05, ..Default::default() }), ..Default::default() }),
        Producer::DifferenceKey => node(NodeKind::DifferenceKey, NodeParams {
            difference_key: Some(klipp_core::DifferenceKeySettings { enabled: true, plate_path: Some(plate()), plate_frame: None, tolerance: 0.1, soft: 0.3, invert: false, match_space: klipp_core::DifferenceKeyMatchSpace::Rgb }),
            ..Default::default()
        }),
        Producer::Window => {
            use klipp_core::{Mask, MaskShape, MaskStack};
            let masks = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.2, ..Mask::default() }] };
            node(NodeKind::Mask, NodeParams { masks: Some(masks), ..Default::default() })
        }
    }
}

/// A media clip graded by an exposure, `p` beside it reading the source,
/// with `effects`; and `p`'s node.
fn clip(p: Producer, effects: Vec<E>) -> (Clip, NodeId) {
    let (s, e, o) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, NodeParams { exposure: Some(0.2), ..Default::default() }), node(NodeKind::Output, NodeParams::default()));
    let k = producer(p);
    let edges = vec![
        Edge { from_node: s.id, from_port: 0, to_node: e.id, to_port: 0 },
        Edge { from_node: e.id, from_port: 0, to_node: o.id, to_port: 0 },
        Edge { from_node: s.id, from_port: 0, to_node: k.id, to_port: 0 },
    ];
    let id = k.id;
    let graph = NodeGraph { output_node_id: o.id, nodes: vec![s, e, o, k], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    c.set_grade(graph);
    let c = TestClip::of(c).effects(effects).build();
    (c, id)
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, gpu: bool, inspect: Inspect) -> (Vec<f32>, klipp_compose::RenderStats) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), inspect);
    (out, found.stats)
}

fn blur() -> E {
    E::GaussianBlur { radius: 4.0 }
}

/// The clip with its blur masked by `p` through the grade's published port,
/// mixed at `MIX` on luminance.
fn masked(p: Producer) -> Sequence {
    let (mut c, k) = clip(p, vec![blur()]);
    let pic = c.picture_v2().unwrap().picture.clone();
    let group = pic.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let fx = pic.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    c.apply_picture_ops(&[
        GraphOp::PublishOutput { group: at(group), endpoint: PortBinding { node: k, port: "out".into() }, key: "matte".into(), label: "Matte".into() },
        GraphOp::Connect { from: at(group), from_port: "matte".into(), to: at(fx), to_port: "mask".into() },
        GraphOp::SetMaskChannel { path: at(fx), channel: Some("luminance".into()) },
        GraphOp::SetMix { path: at(fx), mix: Some(MIX) },
    ])
    .unwrap_or_else(|e| panic!("{p:?}: {e}"));
    sequence(c)
}

/// The oracle from the clip's parts on `gpu`'s route: its graded picture,
/// its blur unmasked, and `p`'s matte as the grade shows it.
fn oracle(p: Producer, gpu: bool) -> Vec<f32> {
    let (graded, k) = clip(p, vec![]);
    let (blurred, _) = clip(p, vec![blur()]);
    let id = graded.id;
    let g = render(&sequence(graded.clone()), gpu, Inspect::default()).0;
    let b = render(&sequence(blurred), gpu, Inspect::default()).0;
    let m = render(&sequence(graded), gpu, Inspect { clip: Some(id), matte_view: Some(klipp_compose::inspect::grade_node(id, k)), ..Default::default() }).0;
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let mix = klipp_effects::mask_mix::MaskMix::new(MIX, klipp_effects::mask_mix::MaskChannel::Luminance, luma).unwrap();
    g.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .zip(m.chunks_exact(4))
        .flat_map(|((g, b), m)| mix.pixel([g[0], g[1], g[2], g[3]], [b[0], b[1], b[2], b[3]], Some([m[0], m[1], m[2], m[3]])))
        .collect()
}

fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn every_producer_through_a_published_group_port_matches_the_oracle() {
    let gpu_there = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu_there || !required(), "required, but there is no GPU");
    let mut failures = Vec::new();
    for p in [Producer::Qualifier, Producer::LumaKey, Producer::DifferenceKey, Producer::Window] {
        let seq = masked(p);
        for gpu in [false, true] {
            if gpu && !gpu_there {
                eprintln!("UNVERIFIED: {p:?} on the GPU");
                continue;
            }
            let want = oracle(p, gpu);
            let (got, stats) = render(&seq, gpu, Inspect::default());
            let worst = worst_levels(&got, &want);
            eprintln!("{p:?} (gpu {gpu}): {worst} levels from the oracle; {stats:?}");
            if worst > 1 {
                failures.push(format!("{p:?} (gpu {gpu}): {worst} levels from the oracle"));
            }
            if (stats.effects_mask_unavailable, stats.effects_blended_on_cpu, stats.effects_on_cpu) != (0, 0, 0) {
                failures.push(format!("{p:?} (gpu {gpu}): the mask was wired in the grade's run, not around it: {stats:?}"));
            }
            // The matte does something: not the plain blur, not the graded picture.
            let (graded, _) = clip(p, vec![]);
            let (blurred, _) = clip(p, vec![blur()]);
            let (g, b) = (render(&sequence(graded), gpu, Inspect::default()).0, render(&sequence(blurred), gpu, Inspect::default()).0);
            if worst_levels(&got, &g) <= 2 || worst_levels(&got, &b) <= 2 {
                failures.push(format!("{p:?} (gpu {gpu}): the masked blur is the graded picture or the plain blur: the matte isn't doing anything here"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
