//! A grade's LUT is parsed before the frame that needs it. A 65-point
//! cube is 7 MB of text and about a tenth of a second to parse; read in
//! the first frame that graded with it, that frame was held up 107 ms.
//! The engine parses the sequence's LUTs on a thread of its own as the
//! sequence comes in, and again for one an edit brings.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_nodes::{Node, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

fn cube(name: &str) -> std::path::PathBuf {
    let size = 9;
    let mut text = format!("LUT_3D_SIZE {size}\n");
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                text.push_str(&format!("{} {} {}\n", r as f32 / 8.0, g as f32 / 8.0, b as f32 / 8.0));
            }
        }
    }
    let p = std::env::temp_dir().join(format!("klipp-warm-{name}-{}.cube", uuid::Uuid::new_v4().simple()));
    std::fs::write(&p, text).unwrap();
    p
}

fn graded_clip(lut: &std::path::Path, from: i64, to: i64) -> Clip {
    let fps = Fps::new(common::FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let mut clip = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, source_in: tc(0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) };
    let mut graph = common::exposure_graph(0.0);
    graph.splice_before_output(Node {
        id: klipp_nodes::NodeId::new_v4(), kind: NodeKind::Lut3D, label: "LUT".into(), enabled: true,
        params: NodeParams { lut_path: Some(lut.to_path_buf()), ..Default::default() },
        keyframes: Default::default(), output_link: None, input_links: vec![],
    });
    clip.set_grade(graph);
    clip
}

fn kept_within(path: &std::path::Path, limit: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if klipp_color::shared_lut_is_kept(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn a_sequences_luts_are_parsed_ahead() {
    let (first, later) = (cube("first"), cube("later"));
    let mut seq = Sequence::new_default();
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![graded_clip(&first, 0, 25)];
    seq.tracks = vec![t];
    let shared = Arc::new(klipp_timeline::SharedSequence::new(seq));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::new(Mutex::new(FrameBuffer::new(64, 64))), Arc::new(Mutex::new(FrameBuffer::new(64, 64))),
        Arc::clone(&shared), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let running = engine.spawn();
    // No frame is drawn here (the clip's file is not in the pool): only
    // the warm-up parses it.
    let first_kept = kept_within(&first, Duration::from_secs(5));
    // An edit brings a second LUT.
    {
        let mut s = shared.snapshot().as_ref().clone();
        s.tracks[0].clips.push(graded_clip(&later, 25, 50));
        shared.replace(s);
    }
    let later_kept = kept_within(&later, Duration::from_secs(5));
    let _ = cmd.send(AppCommand::Quit);
    let _ = running.join();
    let _ = (std::fs::remove_file(&first), std::fs::remove_file(&later));
    assert!(first_kept, "the sequence's LUT was not parsed ahead of its frame");
    assert!(later_kept, "the LUT an edit brought was not parsed ahead of its frame");
}
