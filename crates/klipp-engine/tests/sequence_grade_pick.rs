//! The qualifier eyedropper and the Hue-Sat curve picker on one of the
//! sequence's own grades: the timeline grade, a grade group's stage
//! (#1428) or a track's grade (#1655). The pick names its node by path,
//! and the engine reads what that node receives: the composite for the
//! timeline grade, the group's member on the frame for a stage, before
//! its own grade (pre) or after it (post), and the track's picture clip
//! on the frame, after its own grade, for the track's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::{NodePath, Root as PathRoot, TrackPart};
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, GradeAccess, GradeGroup, GradeStage, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn up() -> Node {
    node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() })
}

/// Source → `between` → Output.
fn chain(between: Vec<Node>) -> NodeGraph {
    let nodes: Vec<Node> = std::iter::once(node(NodeKind::Source, NodeParams::default()))
        .chain(between)
        .chain([node(NodeKind::Output, NodeParams::default())])
        .collect();
    let edges = nodes.windows(2).map(|p| Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    let output_node_id = nodes.last().unwrap().id;
    NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Source → (one stop up, if `exposed`) → Qualifier → Output; the
/// Qualifier's id.
fn qualified(exposed: bool) -> (NodeGraph, NodeId) {
    let qualifier = node(NodeKind::Qualifier, NodeParams { qualifier: Some(Default::default()), ..Default::default() });
    let q = qualifier.id;
    (chain(exposed.then(up).into_iter().chain([qualifier]).collect()), q)
}

/// Which of the sequence's own grades holds the Qualifier.
#[derive(Clone, Copy, Debug)]
enum Root {
    Timeline,
    Pre,
    Post,
    /// The clip's track's grade (#1655).
    Track,
}

/// One pick, as the app sends it.
struct Pick {
    root: Root,
    /// The Qualifier one stop up from its grade's input.
    exposed: bool,
    /// The member clip's own grade: one stop up.
    own_up: bool,
    /// The member scaled in the frame.
    placed: bool,
    /// The frame parked on; the clip spans 0..30.
    park: i64,
    /// Playing when the pick arrives.
    playing: bool,
    /// Ask about the track's effect list instead.
    track_effects: bool,
}

impl Default for Pick {
    fn default() -> Self {
        Pick { root: Root::Timeline, exposed: false, own_up: false, placed: false, park: 20, playing: false, track_effects: false }
    }
}

/// The engine's answer to `p`, sent with the stand-in's id as the app
/// sends it, and the path the app derives for it.
fn ask(p: Pick) -> Option<Result<[f32; 3], NodeSampleRefusal>> {
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
    if p.own_up {
        clip.set_grade(chain(vec![up()]));
    }
    if p.placed {
        clip = klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }).build();
    }
    let mut seq = Sequence::new_default();
    (seq.fps, seq.resolution) = (fps, Resolution { width: W, height: H });
    let (g, q) = qualified(p.exposed);
    let mut v1 = Track::new("V1", TrackKind::Video);
    let (stand_in, at) = match p.root {
        Root::Timeline => {
            seq.timeline_grade_slot().set(Some(g));
            (seq.timeline_grade_clip_id(), NodePath::new(PathRoot::TimelineGrade { sequence: seq.id.0 }, [q]))
        }
        Root::Pre | Root::Post => {
            let (stage, path_stage) = if matches!(p.root, Root::Pre) { (GradeStage::Pre, Stage::Pre) } else { (GradeStage::Post, Stage::Post) };
            let mut group = GradeGroup::new("Night");
            group.stage_slot(stage).set(Some(g));
            clip.grade_group = Some(group.id);
            let found = (group.stage_clip_id(stage), NodePath::new(PathRoot::GroupStage { group: group.id.0, stage: path_stage }, [q]));
            seq.grade_groups.push(group);
            found
        }
        Root::Track => {
            v1.effects.grade_slot().set(Some(g));
            let part = if p.track_effects { TrackPart::Effects } else { TrackPart::Grade };
            (Sequence::track_grade_clip_id(v1.id), NodePath::new(PathRoot::Track { track: v1.id.0, part }, [q]))
        }
    };
    v1.clips = vec![clip];
    seq.tracks = vec![v1];
    assert!(seq.all_clips().all(|c| c.id != stand_in), "the stand-in is the app's alone");
    // The engine's copy, as the app's sync makes it: groups baked in.
    let engine_copy = klipp_timeline::flatten::flatten_sequence(&seq, std::slice::from_ref(&seq));

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(engine_copy)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    // The pick reads the parked frame, so it waits for that frame and for
    // the picture to settle, not for a time: a loaded machine took longer
    // than the 1.5 s this slept, and the pick found nothing to read (#1828).
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(p.park))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(60)).is_some(), "the parked frame is shown");
    common::settle(&fb);
    // Playing when the pick arrives: the engine takes the two in order.
    if p.playing {
        cmd.send(AppCommand::Play).unwrap();
    }
    cmd.send(AppCommand::SampleNodeInput { clip_id: stand_in, node_id: q, at, nx: 0.5, ny: 0.5 }).unwrap();
    let t0 = Instant::now();
    let mut answer = None;
    while answer.is_none() && t0.elapsed() < Duration::from_secs(60) {
        if let Ok(AppEvent::NodeInputSampled { clip_id: c, node_id: n, sample, .. }) = ev_rx.recv_timeout(Duration::from_millis(50)) {
            assert_eq!((c, n), (stand_in, q), "the answer names what the app asked about");
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

fn times(got: [f32; 3], source: [f32; 3], k: f32, what: &str) {
    for c in 0..3 {
        assert!((got[c] - k * source[c]).abs() < 0.01, "{what}, channel {c}: {got:?} is {k} times {source:?}");
    }
}

/// Each root reads the picture its Qualifier receives.
#[test]
fn a_pick_on_a_sequence_grade_reads_its_node_input() {
    // Nothing graded before the timeline's Qualifier: the source.
    let Some(source) = ask(Pick::default()) else { return };
    let source = source.expect("the timeline grade's Qualifier reads the composite");
    assert!(source[0] > 0.05 && source[0] < 0.45, "a mid grey of the ramp: {source:?}");

    let timeline = ask(Pick { exposed: true, ..Pick::default() }).unwrap().expect("timeline grade");
    times(timeline, source, 2.0, "one stop up in the timeline grade");

    // The pre stage runs before the member's own grade, which it must not see.
    let pre = ask(Pick { root: Root::Pre, exposed: true, own_up: true, ..Pick::default() }).unwrap().expect("pre stage");
    times(pre, source, 2.0, "one stop up in the pre stage, the own grade after it");

    // The post stage receives the own grade's output.
    let post = ask(Pick { root: Root::Post, exposed: true, own_up: true, ..Pick::default() }).unwrap().expect("post stage");
    times(post, source, 4.0, "the own grade's stop, then the post stage's");

    // A track's grade runs after the clip's own: its Qualifier straight
    // after its Source reads the own grade's stop (#1655), and one after
    // a stop of its own reads both.
    let track = ask(Pick { root: Root::Track, own_up: true, ..Pick::default() }).unwrap().expect("track grade");
    times(track, source, 2.0, "the own grade's stop, read at the track grade's Source");
    let track = ask(Pick { root: Root::Track, exposed: true, own_up: true, ..Pick::default() }).unwrap().expect("track grade");
    times(track, source, 4.0, "the own grade's stop, then the track grade's");
    // Control: without the own grade's stop, the source.
    let track = ask(Pick { root: Root::Track, ..Pick::default() }).unwrap().expect("track grade");
    times(track, source, 1.0, "the track grade's input, nothing before it");
}

/// What the engine can't read point for point, it refuses, and says why.
#[test]
fn a_pick_it_cannot_read_is_refused() {
    let Some(off) = ask(Pick { root: Root::Pre, park: 40, ..Pick::default() }) else { return };
    assert_eq!(off, Err(NodeSampleRefusal::Why("no clip of the group is on this frame — park the playhead on one first".into())));
    assert_eq!(ask(Pick { root: Root::Post, placed: true, ..Pick::default() }).unwrap(), Err(NodeSampleRefusal::Placed));
    // Parked off the track's clip, nothing of the track is on the frame.
    assert_eq!(
        ask(Pick { root: Root::Track, park: 40, ..Pick::default() }).unwrap(),
        Err(NodeSampleRefusal::Why("no picture clip of the track is on this frame — park the playhead on one first".into())),
    );
    assert_eq!(ask(Pick { root: Root::Track, placed: true, ..Pick::default() }).unwrap(), Err(NodeSampleRefusal::Placed));
    assert_eq!(
        ask(Pick { root: Root::Track, track_effects: true, ..Pick::default() }).unwrap(),
        Err(NodeSampleRefusal::Why("a track's effect list has no node input to read: pick on a node of its grade".into())),
    );
    assert_eq!(
        ask(Pick { playing: true, ..Pick::default() }).unwrap(),
        Err(NodeSampleRefusal::Why("pause playback first — it reads the parked frame".into())),
    );
}
