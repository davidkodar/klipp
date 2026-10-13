//! A parked viewer's renders hold the denoise at the clip's source and
//! take it on the next render (#1863): a grade edit below it draws what a
//! renderer holding nothing draws, without running the denoise again. A
//! frame source that names no file (export's, and every other) holds
//! nothing, and a layer above the clip doesn't take the clip's source.

use std::sync::Arc;

use klipp_compose::{FrameSource, HeldFile, Inspect, LinearFrameProviderFn, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 8;
const H: u32 = 4;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn grey(v: f32) -> Vec<f32> {
    (0..W * H).flat_map(|i| [v + i as f32 / 256.0, v, v, 1.0]).collect()
}

/// Every clip's frame a grey with its pts, its neighbours greys either
/// side; a parked viewer's file when `parked`.
struct Source {
    parked: bool,
}

impl FrameSource for Source {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(grey(0.5))), width: w, height: h, pts: Some(secs) })
    }
    fn temporal_frames_linear(&mut self, _: &Clip, _: WorkingColorspace) -> Option<LinearFrameProviderFn> {
        Some(Box::new(|t| Some((Arc::new(grey(0.45 + (t as f32 * 0.07).fract() * 0.1)), W, H))))
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 10.0))
    }
    fn held_file(&mut self, clip: &Clip) -> Option<HeldFile> {
        self.parked.then(|| HeldFile {
            media: clip.media_id().unwrap_or(MediaId::nil()),
            path: "/take.mov".into(),
            len: 1000,
            modified: std::time::SystemTime::UNIX_EPOCH,
            log_format: None,
            proxy: None,
        })
    }
}

/// Source → Exposure(`stops`) → Output.
fn exposure(stops: f32) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// A denoised clip graded by an Exposure node, alone or under a second
/// clip on a half-opaque track.
fn sequence(layered: bool) -> Sequence {
    let c = TestClip::of(Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(1.0), tc(3.0))).effect(E::TemporalDenoise { strength: 1.0, threshold: 0.5 }).build();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![c];
    seq.tracks = vec![v1];
    if layered {
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        v2.opacity = 0.5;
        seq.tracks.push(v2);
    }
    seq
}

/// The Exposure node set to `stops`, as a slider sets it: the grade's
/// nodes keep their ids.
fn set_stops(seq: &mut Sequence, stops: f32) {
    let clip = &mut seq.tracks[0].clips[0];
    let mut g = clip.grade().clone();
    if !g.nodes.iter().any(|n| n.kind == NodeKind::Exposure) {
        g = exposure(stops);
    }
    for n in g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Exposure) {
        n.params.exposure = Some(stops);
    }
    clip.set_grade(g);
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r
}

fn render(r: &mut Renderer, seq: &Sequence, src: &mut Source) -> Vec<u32> {
    r.render_inspected(seq, tc(0.5), W, H, src, RenderOptions::default(), Inspect::default()).0.iter().map(|f| f.to_bits()).collect()
}

/// Five grade steps on the parked frame, alone and under another layer:
/// the denoise runs once and every step is bit for bit a renderer's that
/// holds nothing, which runs it every time.
#[test]
fn a_parked_drag_runs_the_denoise_once() {
    for layered in [false, true] {
        let mut seq = sequence(layered);
        let (mut held, mut plain) = (renderer(), renderer());
        let (mut parked, mut export) = (Source { parked: true }, Source { parked: false });
        for step in 0..5 {
            set_stops(&mut seq, 0.5 + step as f32 * 0.1);
            assert_eq!(render(&mut held, &seq, &mut parked), render(&mut plain, &seq, &mut export), "layered {layered}: step {step}");
        }
        assert_eq!(held.source_node_runs(), 1, "layered {layered}: the denoise ran once");
        assert_eq!(held.executor().held_counts().hits, 4, "layered {layered}");
        assert_eq!(plain.source_node_runs(), 5, "layered {layered}: without a file, every render");
        assert_eq!(plain.executor().held_counts(), Default::default(), "layered {layered}: a source naming no file holds nothing");
    }
}

/// The token is the clip's run's alone: once the frame is drawn, the
/// executor holds no source a later run (a thumbnail's, another
/// renderer's use of it) could take.
#[test]
fn the_source_is_dropped_after_the_clips_run() {
    let mut seq = sequence(false);
    set_stops(&mut seq, 0.5);
    let mut r = renderer();
    render(&mut r, &seq, &mut Source { parked: true });
    assert!(r.executor().holding_for().is_none(), "the source outlived the clip's run");
    let stored = r.executor().held_counts().stored;
    assert_eq!(stored, 1);
    // The same render through a source naming no file: nothing taken.
    render(&mut r, &seq, &mut Source { parked: false });
    assert_eq!(r.executor().held_counts().hits, 0);
    assert_eq!(r.source_node_runs(), 2);
}
