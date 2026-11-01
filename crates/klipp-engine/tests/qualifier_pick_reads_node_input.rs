//! The qualifier eyedropper reads the picture its node receives, not the
//! graded frame on screen (#587): parked on a frame, `SampleNodeInput`
//! answers with the output of the node feeding the Qualifier.

mod common;

use klipp_timeline::GradeAccess;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source → `before` → Qualifier → Output; the Qualifier's id.
fn graph(before: Option<Node>) -> (NodeGraph, NodeId) {
    let qualifier = node(NodeKind::Qualifier, NodeParams { qualifier: Some(Default::default()), ..Default::default() });
    let q = qualifier.id;
    let nodes: Vec<Node> = std::iter::once(node(NodeKind::Source, NodeParams::default()))
        .chain(before)
        .chain([qualifier, node(NodeKind::Output, NodeParams::default())])
        .collect();
    let edges = nodes.windows(2).map(|p| Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    let output_node_id = nodes.last().unwrap().id;
    (NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }, q)
}

/// Park on a frame of the clip graded by `graph`, pick at the centre
/// under `node`, and return the engine's answer.
fn pick(graph: NodeGraph, node: NodeId, placed: bool) -> Option<Result<[f32; 3], NodeSampleRefusal>> {
    pick_after(graph, node, placed, Duration::from_millis(1500))
}

/// [`pick`], `wait` after the playhead is set: no wait sends the pick
/// before the parked frame has been decoded.
fn pick_after(graph: NodeGraph, node: NodeId, placed: bool, wait: Duration) -> Option<Result<[f32; 3], NodeSampleRefusal>> {
    let src = common::ramp("p587", &["-c:v", "ffv1"], "avi")?;
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: src.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    });
    let tc = |f| common::frame_tc(f, fps);
    let mut clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(30), ..Clip::gap(tc(0), tc(30)) };
    clip.set_grade(graph);
    if placed {
        clip = klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }).build();
    }
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    (seq.fps, seq.resolution) = (fps, Resolution { width: W, height: H });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    seq.tracks = vec![v1];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    cmd.send(AppCommand::SetPlayhead(tc(20))).unwrap();
    // Parked, and the first composite's pipelines built (or not yet).
    std::thread::sleep(wait);
    cmd.send(AppCommand::SampleNodeInput { clip_id, node_id: node, at: klipp_compose::inspect::grade_node(clip_id, node).path, nx: 0.5, ny: 0.5 }).unwrap();
    let t0 = Instant::now();
    let mut answer = None;
    while answer.is_none() && t0.elapsed() < Duration::from_secs(60) {
        if let Ok(AppEvent::NodeInputSampled { clip_id: c, node_id: n, sample, .. }) = ev_rx.recv_timeout(Duration::from_millis(50)) {
            assert_eq!((c, n), (clip_id, node));
            answer = Some(sample);
        }
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    // A decode thread still in ffmpeg's teardown at exit crashes the
    // process (CUDA's context destroyed under it).
    klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
    Some(answer.expect("the engine answers the pick"))
}

#[test]
fn the_pick_reads_what_the_qualifier_receives() {
    let (first, q) = graph(None);
    let Some(source) = pick(first, q, false) else { return };
    let source = source.expect("a Qualifier after Source reads the source");
    assert!(source[0] > 0.05 && source[0] < 0.45, "a mid grey of the ramp: {source:?}");

    // Sent before the parked frame is decoded (as a loaded machine sends
    // it): answered once the frame is there, the same picture.
    let (early, q) = graph(None);
    let early = pick_after(early, q, false, Duration::ZERO).unwrap().expect("a pick before the frame waits for it");
    // Within a step of 8 bits: the parked frame's later picks read its
    // kept 8-bit pre-grade, the first its decoded frame.
    for c in 0..3 {
        assert!((early[c] - source[c]).abs() < 0.005, "channel {c}: the pick before the frame read {early:?}, after it {source:?}");
    }

    // One stop up before the Qualifier doubles what it receives. The
    // screen shows the Qualifier's matte, which the old pick read.
    let up = node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() });
    let (after, q) = graph(Some(up));
    let doubled = pick(after, q, false).unwrap().expect("reads the exposure's output");
    for c in 0..3 {
        assert!((doubled[c] - 2.0 * source[c]).abs() < 0.01, "channel {c}: {doubled:?} is twice {source:?}");
    }

    // A clip scaled in the frame: the point clicked is not the same point
    // of its picture, and the engine says so rather than read it.
    let (g, q) = graph(None);
    assert_eq!(pick(g, q, true).unwrap(), Err(NodeSampleRefusal::Placed));
}
