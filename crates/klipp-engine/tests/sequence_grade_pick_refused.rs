//! The qualifier eyedropper and the Hue-Sat curve picker on one of the
//! sequence's own grades: the timeline grade, or a grade group's stage.
//! The Colour page edits those through a stand-in clip that exists only
//! in the app's copy of the sequence, inside one colour dispatch (stood
//! in, edited, stood down before the engine sync). The pick names that
//! stand-in, and the engine, whose sequence holds no clip by that id,
//! refuses it, so neither view can key such a grade.
//!
//! This pins the inherited defect: when the engine reads a sequence
//! grade's node input, this test changes with it.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, GradeGroup, GradeStage, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source → Qualifier → Output; the Qualifier's id.
fn graph() -> (NodeGraph, NodeId) {
    let qualifier = node(NodeKind::Qualifier, NodeParams { qualifier: Some(Default::default()), ..Default::default() });
    let q = qualifier.id;
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), qualifier, node(NodeKind::Output, NodeParams::default())];
    let edges = nodes.windows(2).map(|p| Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    let output_node_id = nodes.last().unwrap().id;
    (NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }, q)
}

/// Which of the sequence's own grades holds the Qualifier.
#[derive(Clone, Copy, Debug)]
enum Root {
    Timeline,
    PreStage,
}

/// A parked clip on a sequence whose `root` grade holds a Qualifier, as
/// the engine is given it after a colour dispatch (no stand-in clip on
/// any track); the pick sent as the app sends it, naming the stand-in's
/// id, and the engine's answer.
fn pick(root: Root) -> Option<Result<[f32; 3], NodeSampleRefusal>> {
    let src = common::ramp("seqpick", &["-c:v", "ffv1"], "avi")?;
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
    let mut seq = Sequence::new_default();
    (seq.fps, seq.resolution) = (fps, Resolution { width: W, height: H });
    let (g, q) = graph();
    let stand_in = match root {
        Root::Timeline => {
            seq.timeline_grade_slot().set(Some(g));
            seq.timeline_grade_clip_id()
        }
        Root::PreStage => {
            let mut group = GradeGroup::new("Night");
            group.stage_slot(GradeStage::Pre).set(Some(g));
            clip.grade_group = Some(group.id);
            let stand_in = group.stage_clip_id(GradeStage::Pre);
            seq.grade_groups.push(group);
            stand_in
        }
    };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    seq.tracks = vec![v1];
    assert!(seq.all_clips().all(|c| c.id != stand_in), "the stand-in is the app's alone");

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    cmd.send(AppCommand::SetPlayhead(tc(20))).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    cmd.send(AppCommand::SampleNodeInput { clip_id: stand_in, node_id: q, nx: 0.5, ny: 0.5 }).unwrap();
    let t0 = Instant::now();
    let mut answer = None;
    while answer.is_none() && t0.elapsed() < Duration::from_secs(5) {
        if let Ok(AppEvent::NodeInputSampled { clip_id: c, node_id: n, sample, .. }) = ev_rx.recv_timeout(Duration::from_millis(50)) {
            assert_eq!((c, n), (stand_in, q));
            answer = Some(sample);
        }
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    Some(answer.expect("the engine answers the pick"))
}

/// A pick on the timeline grade's or a pre-clip stage's Qualifier is
/// refused: the engine finds no clip by the stand-in's id.
#[test]
fn a_pick_on_a_sequence_grade_is_refused() {
    for root in [Root::Timeline, Root::PreStage] {
        let Some(answer) = pick(root) else { return };
        assert_eq!(answer, Err(NodeSampleRefusal::Why("the clip is no longer on the timeline".into())), "{root:?}");
    }
}
