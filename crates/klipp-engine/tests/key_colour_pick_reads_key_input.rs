//! The Chroma Key's eyedropper reads the picture the key receives, not the
//! keyed frame on screen: parked on a frame, `SampleNodeInput` on the
//! key's node of the clip's picture answers with the output of the node
//! feeding it, though the key has taken the whole frame out.

mod common;

use klipp_timeline::GradeAccess;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_graph_format::path::{NodePath, Root};
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source → (one stop up, if `up`) → Output.
fn grade(up: bool) -> NodeGraph {
    let exposure = up.then(|| node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() }));
    let nodes: Vec<Node> = std::iter::once(node(NodeKind::Source, NodeParams::default())).chain(exposure).chain([node(NodeKind::Output, NodeParams::default())]).collect();
    let edges = nodes.windows(2).map(|p| Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    let output_node_id = nodes.last().unwrap().id;
    NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// A clip of the grey ramp graded by `grade(up)` and keyed out whole (a
/// grey key, tolerance all the way): pick at the centre on the key's node,
/// and return the engine's answer.
fn pick(up: bool, before: Option<klipp_effects::BuiltinEffect>) -> Option<Result<[f32; 3], NodeSampleRefusal>> {
    let src = common::ramp("keypick", &["-c:v", "ffv1"], "avi")?;
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
    clip.set_grade(grade(up));
    let mut built = klipp_timeline::test_clip::TestClip::of(clip);
    if let Some(e) = before {
        built = built.effect(e);
    }
    clip = built
        .effect(klipp_effects::BuiltinEffect::ChromaKey {
            key_color: [128, 128, 128],
            tolerance: 1.0,
            soft_edge: 0.0,
            spill_method: klipp_effects::SpillMethod::None,
            spill_strength: 0.0,
        })
        .build();
    let clip_id = clip.id;
    let key = clip
        .picture_v2()
        .expect("a picture graph")
        .picture
        .graph
        .nodes
        .iter()
        .find(|n| klipp_graph_lift::effect_params::tag(n) == Some("chroma_key"))
        .expect("the key is a node of the clip's picture")
        .id;
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
    std::thread::sleep(Duration::from_millis(1500));
    cmd.send(AppCommand::SampleNodeInput { clip_id, node_id: key, at: NodePath::new(Root::Clip { clip: clip_id.0 }, [key]), nx: 0.5, ny: 0.5 }).unwrap();
    let t0 = Instant::now();
    let mut answer = None;
    while answer.is_none() && t0.elapsed() < Duration::from_secs(60) {
        if let Ok(AppEvent::NodeInputSampled { clip_id: c, node_id: n, sample, .. }) = ev_rx.recv_timeout(Duration::from_millis(50)) {
            assert_eq!((c, n), (clip_id, key));
            answer = Some(sample);
        }
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
    Some(answer.expect("the engine answers the pick"))
}

#[test]
fn the_key_pick_reads_what_the_key_receives() {
    let Some(plain) = pick(false, None) else { return };
    let plain = plain.expect("a key's input is read");
    assert!(plain[0] > 0.05 && plain[0] < 0.45, "a mid grey of the ramp, not the keyed-out frame: {plain:?}");
    // One stop up in the grade before the key doubles what it receives.
    let doubled = pick(true, None).unwrap().expect("a key's input is read");
    for c in 0..3 {
        assert!((doubled[c] - 2.0 * plain[c]).abs() < 0.01, "channel {c}: {doubled:?} is twice {plain:?}");
    }
    // After another effect: the key reads that effect's picture.
    let inverted = pick(false, Some(klipp_effects::BuiltinEffect::Invert { mix: 1.0 })).unwrap().expect("a key after an effect reads it");
    // An invert of the display-referred picture: one less its sRGB code.
    let enc = |l: f32| if l <= 0.003_130_8 { 12.92 * l } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    let dec = |v: f32| if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    for c in 0..3 {
        let want = dec(1.0 - enc(plain[c]));
        assert!((inverted[c] - want).abs() < 0.01, "channel {c}: the key after an invert reads {inverted:?}, the invert of {plain:?} is {want}");
    }
}
