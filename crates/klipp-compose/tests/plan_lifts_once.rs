//! Playing with no edit lifts each clip's picture once, not once a frame
//! (#1089, phase 2b M2, step 10c). The plan route reads a clip's effects
//! from its lifted picture every frame; the lift is cached on the clip
//! and shared by the render copy's clones, so frames after the first read
//! it warm. Its own test binary, its cases one at a time: `COLD_LIFTS`
//! is process-wide.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

/// `COLD_LIFTS` is process-wide: each case counts with the other idle.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v: Vec<f32> = (0..w * h).flat_map(|i| [(i % w) as f32 / w as f32, 0.4, 0.3, 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn graded_clip(from: f64, to: f64) -> Clip {
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
    let graph = NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) };
    c.set_grade(graph);
    c.set_effect_list(vec![E::Gamma { gamma: 1.2 }, E::Vibrance { amount: 0.3 }], vec![true; 2]);
    c
}

fn sequence(clips: Vec<Clip>, track_fx: Vec<E>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut t = Track::new("V", TrackKind::Video);
    let n = track_fx.len();
    t.effects.edit_video_effects(|fx, on| {
        *fx = track_fx;
        *on = vec![true; n];
    });
    t.clips = clips;
    seq.tracks = vec![t];
    seq
}

#[test]
fn playing_lifts_each_clip_once() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // A graded clip on a track with effects of its own (so the render copy
    // is baked), and a compound clip of a scene holding another: the
    // flattened copy the viewer plays.
    let scene = sequence(vec![graded_clip(0.0, 2.0)], vec![]);
    // The document's clips are lifted already (a save, a load, an open
    // picture): a copy that takes another id inside a compound instance
    // must not read its original's lift, nor lift again every frame.
    scene.tracks[0].clips[0].picture_v2().unwrap();
    let mut compound = Clip::sequence(scene.id, tc(2.0), tc(4.0));
    compound.source_out = tc(2.0);
    let outer = sequence(vec![graded_clip(0.0, 2.0), compound], vec![E::Invert { mix: 0.1 }]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene]);

    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let before = klipp_timeline::COLD_LIFTS.load(Ordering::Relaxed);
    let mut plan = 0;
    for f in 0..96 {
        // The viewer's copy is cloned per frame; clones share the lift.
        let frame_copy = flat.clone();
        let (_, found) = r.render_inspected(&frame_copy, tc(f as f64 / 24.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
        plan += found.stats.effects_followed_grade;
    }
    let lifts = klipp_timeline::COLD_LIFTS.load(Ordering::Relaxed) - before;
    if plan == 0 {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but no clip took the plan route");
        eprintln!("UNVERIFIED: no GPU plan route");
        return;
    }
    let clips = flat.tracks.iter().map(|t| t.clips.len() as u64).sum::<u64>();
    assert!(lifts <= clips, "{lifts} cold lifts over 96 frames for {clips} clips ({plan} plan renders)");
}

#[test]
fn a_precompositions_instance_lifts_once_too() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // A precomposition's scene is an instance: its clips are copies under
    // ids derived from the instance. The original is lifted already.
    let scene = sequence(vec![graded_clip(0.0, 2.0)], vec![]);
    scene.tracks[0].clips[0].picture_v2().unwrap();
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(2.0));
    pre.source_out = tc(2.0);
    let outer = sequence(vec![pre], vec![]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene]);

    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let before = klipp_timeline::COLD_LIFTS.load(Ordering::Relaxed);
    let mut plan = 0;
    for f in 0..48 {
        let frame_copy = flat.clone();
        let (_, found) = r.render_inspected(&frame_copy, tc(f as f64 / 24.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
        plan += found.stats.effects_followed_grade;
    }
    let lifts = klipp_timeline::COLD_LIFTS.load(Ordering::Relaxed) - before;
    if plan == 0 {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but no clip took the plan route");
        eprintln!("UNVERIFIED: no GPU plan route");
        return;
    }
    assert!(lifts <= 2, "{lifts} cold lifts over 48 frames ({plan} plan renders)");
}
