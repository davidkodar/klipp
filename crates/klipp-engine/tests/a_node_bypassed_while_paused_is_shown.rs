//! A grade node bypassed while paused shows the picture without it at
//! once (#2553). The node graph's Bypass writes the grade and says only
//! that the sequence changed; the viewer kept the graded frame.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// Two seconds of one solid colour, lossless.
fn solid(name: &str, colour: &str) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-paused-bypass-{name}-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi"])
        .args(["-i", &format!("color=c={colour}:s={W}x{H}:r={}:d=2", common::FPS)])
        .args(["-vf", &format!("format=yuv420p,setparams=range=tv:{}", common::SDR_TAGS), "-c:v", "ffv1"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

fn item(id: MediaId, path: &std::path::Path, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), common::frame_tc(50, fps), fps)
    }
}

fn clip_of(media_id: MediaId, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    let mut clip = Clip { kind: ClipKind::Media { media_id }, ..Clip::gap(tc(0), tc(50)) };
    clip.source_in = tc(0);
    clip.source_out = tc(50);
    clip
}

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    seq: Arc<klipp_timeline::SharedSequence>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    fn generation(&self) -> u64 {
        self.fb.lock().unwrap().latest_generation()
    }

    /// Wait for a frame newer than `since`, then for the picture to stop
    /// changing.
    fn settle_after(&self, since: u64) -> bool {
        let t0 = std::time::Instant::now();
        while self.generation() == since {
            if t0.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut g = self.generation();
        loop {
            std::thread::sleep(common::QUIET);
            let now = self.generation();
            if now == g {
                return true;
            }
            g = now;
        }
    }

    /// The colour at the centre of the frame on screen.
    fn centre(&self) -> [u8; 3] {
        let fb = self.fb.lock().unwrap();
        let (px, w, h, _) = fb.latest_frame();
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            // A decode thread still in ffmpeg's teardown at exit crashes the
            // process (CUDA's context destroyed under it).
            klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
        }
    }
}

fn grey(c: [u8; 3]) -> u8 {
    ((c[0] as u16 + c[1] as u16 + c[2] as u16) / 3) as u8
}

/// Three stops down, and after it three up that is off: the grey clip
/// parks near black.
fn darken() -> klipp_nodes::NodeGraph {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let down = NodeParams { exposure: Some(-3.0), ..Default::default() };
    let up = NodeParams { exposure: Some(3.0), ..Default::default() };
    let mut was_off = node(NodeKind::Exposure, up);
    was_off.enabled = false;
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, down), was_off, node(NodeKind::Output, NodeParams::default())];
    let edges = (0..3).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[3].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

#[test]
fn a_node_bypassed_while_paused_is_shown_at_once() {
    use klipp_timeline::GradeAccess;
    let Some(file) = solid("grey", "gray") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut t = Track::new("V1", TrackKind::Video);
    let mut clip = clip_of(id, fps);
    clip.set_grade(darken());
    t.clips = vec![clip];
    seq.tracks = vec![t];
    let mut pool = MediaPool::default();
    pool.add(item(id, &file, fps));

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let seq = Arc::new(klipp_timeline::SharedSequence::new(seq));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&seq), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = Some(std::thread::spawn(move || engine.run()));
    let e = Engine { cmd, fb, seq, thread };
    let g = e.generation();
    e.cmd.send(AppCommand::SetPlayhead(common::frame_tc(10, fps))).unwrap();
    assert!(e.settle_after(g), "no first frame");
    let graded = grey(e.centre());
    assert!(graded < 70, "parked on the darkened clip, showing {:?}", e.centre());

    // The grade group's Bypass, as it leaves the clip: the node that was
    // on turned off, the sequence written, and SequenceUpdated.
    let switch = |on: bool| {
        let g = e.generation();
        e.seq.write().unwrap().tracks[0].clips[0].edit_grade(|graph| {
            let node = graph.nodes.iter_mut().find(|n| n.params.exposure == Some(-3.0)).expect("the node Bypass turns off");
            node.enabled = on;
        });
        e.cmd.send(AppCommand::SequenceUpdated).unwrap();
        assert!(e.settle_after(g), "the switch rendered nothing");
    };
    switch(false);
    assert!(grey(e.centre()) > 100, "bypassed, the clip is its own grey; the viewer shows {:?} (graded {graded})", e.centre());
    // Enable: that node on again, the one that was off before still off.
    switch(true);
    assert!(grey(e.centre()) < 70, "enabled, the graded picture is back; the viewer shows {:?}", e.centre());
}
