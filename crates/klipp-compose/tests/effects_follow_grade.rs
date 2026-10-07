//! A clip's effects take its grade's result where it sits on the GPU,
//! with no download and upload between them, and draw exactly what the
//! legacy route draws (#1089, phase 2b M2 step 10).
//!
//! Each case renders one graded clip with effects twice: on the new route
//! and on the legacy one (`set_legacy_effects`), comparing the linear
//! floats bit for bit. The same shaders run on the same f32 frame either
//! way, so anything but equal is a fault. The render counter says which
//! route a frame took, so a case can't pass by both falling back.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[fx * 0.9, fy * 0.7, (1.0 - fx) * 0.6 + 0.05, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A grade that runs on the GPU: one exposure node.
fn graded(effects: Vec<E>) -> Sequence {
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.4), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let graph = NodeGraph {
        output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(),
        working_space_override: None, frames: vec![], sticky_notes: vec![],
    };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(graph);
    let n = effects.len();
    clip.set_effect_list(effects, vec![true; n]);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

/// The frame and how many clips took the new route.
fn render(seq: &Sequence, legacy: bool) -> (Vec<f32>, u32) {
    render_with(seq, legacy, false)
}

/// `render`, with every effect's recording refused when `refuse` (as a
/// lost device refuses it).
fn render_with(seq: &Sequence, legacy: bool, refuse: bool) -> (Vec<f32>, u32) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_legacy_effects(legacy);
    // Both pipelines: the grade's (whose chain the effects follow on) and
    // the renderer's own (the legacy route's).
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(refuse);
    }
    if let Some(g) = r.gpu_pipeline() {
        g.refuse_recording(refuse);
    }
    let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    let chain = r.executor().last_run_chain_stats();
    LAST.with(|l| l.set((found.stats.cpu_layers, chain.effect_breaks, chain.submits)));
    (out, found.stats.effects_followed_grade)
}

thread_local! {
    /// The last render's declines (`cpu_layers`), effect breaks and
    /// submits, for the cases that check them.
    static LAST: std::cell::Cell<(u32, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
}

fn last() -> (u32, u32, u32) {
    LAST.with(|l| l.get())
}

/// Both routes, bit for bit; `Some(followed)` when the GPU was there.
fn both(name: &str, effects: Vec<E>) -> Option<bool> {
    let seq = graded(effects);
    let (new, followed) = render(&seq, false);
    let (declines, breaks, submits) = last();
    let (legacy, legacy_followed) = render(&seq, true);
    assert_eq!(declines, last().0, "{name}: no new declines");
    if followed == 1 {
        let recorded = seq.tracks[0].clips[0].builtin_effects().iter().all(|e| klipp_effects::gpu::slots_needed(e).is_some());
        if recorded {
            assert_eq!((breaks, submits), (0, 1), "{name}: grade and effects in one submit");
        }
    }
    assert_eq!(legacy_followed, 0, "{name}: the legacy route never follows");
    if new.iter().map(|v| v.to_bits()).ne(legacy.iter().map(|v| v.to_bits())) {
        let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        panic!("{name}: the new route differs from the legacy one (worst {worst})");
    }
    Some(followed == 1)
}

fn colour_family() -> Vec<E> {
    vec![
        E::Invert { mix: 0.3 },
        E::Posterize { levels: 9.0 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 12.0, saturation: 1.1, value: 0.95 },
        E::Levels { in_black: 0.02, in_white: 0.95, gamma: 1.1, out_black: 0.0, out_white: 1.0 },
        E::Vibrance { amount: 0.3 },
    ]
}

#[test]
fn the_colour_family_follows_the_grade_and_draws_the_same() {
    let gpu = both("colour family", colour_family()).unwrap();
    if !gpu {
        assert!(!required(), "required, but the effects did not follow the grade on the GPU");
        eprintln!("UNVERIFIED: no GPU route");
    }
}

#[test]
fn an_effect_not_yet_recorded_still_follows_and_draws_the_same() {
    // Blur isn't recorded yet (step 7's later family): it runs in its own
    // pass on the same frame, after the recorded ones.
    let mut fx = colour_family();
    fx.push(E::GaussianBlur { radius: 3.0 });
    fx.push(E::Gamma { gamma: 0.9 });
    let gpu = both("with a blur", fx).unwrap();
    assert!(gpu || !required());
}

#[test]
fn a_clip_with_no_grade_takes_the_legacy_route_and_says_so() {
    let mut seq = graded(colour_family());
    seq.tracks[0].clips[0].set_grade(NodeGraph::default());
    let (_, followed) = render(&seq, false);
    assert_eq!(followed, 0, "nothing on the GPU to follow");
}

#[test]
fn an_effect_that_could_not_be_recorded_sends_the_frame_the_legacy_way() {
    // The grade's picture is gone with the GPU run, so the frame is graded
    // again and takes the legacy route: never drawn without the effect.
    let seq = graded(colour_family());
    let (new, followed) = render_with(&seq, false, true);
    let (legacy, _) = render_with(&seq, true, true);
    assert_eq!(followed, 0, "nothing followed the grade");
    assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "the legacy route's picture");
    let (all, _) = render(&seq, true);
    assert!(new.iter().zip(&all).all(|(a, b)| (a - b).abs() < 1e-4), "with every effect in it");
}
