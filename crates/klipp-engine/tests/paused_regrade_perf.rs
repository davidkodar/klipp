//! How long a paused 4K HDR frame takes to regrade — what a grade slider
//! costs while parked. A probe, not a pass/fail test: run with
//! `cargo test -p klipp-engine --test paused_regrade_perf -- --ignored --nocapture`.
//!
//! The real `PlaybackEngine` runs headless on one graded 4K PQ clip, is
//! parked on a frame, and has a slider dragged the way the UI drags one:
//! `BeginNodeParamEdit`, a `ParamChange` per step on the engine's queue,
//! `EndNodeParamEdit`. Each step is timed from the change to the new
//! frame, with the node editor closed and then open (asking for a
//! thumbnail of every node); the engine's own perf samples give the
//! breakdown.

mod common;

use std::time::Duration;

use klipp_core::AppCommand;

#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_pq_regrade() {
    // The viewer at full 4K ("Max playback resolution" 2160), or at the
    // height given in KLIPP_PROBE_HEIGHT.
    let height = std::env::var("KLIPP_PROBE_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(2160);
    klipp_engine::video_decoder::set_max_playback_height(height);
    let Some(src) = common::source("pq", 3840, 2160, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 3840, 2160);

    let drag = |label: &str| {
        parked.begin_drag();
        let steps: Vec<f64> = (0..16).filter_map(|i| parked.step(0.5 + i as f32 * 0.05)).collect();
        let release = parked.end_drag();
        std::thread::sleep(Duration::from_millis(300));
        for s in parked.perf().iter().rev().take(3).rev() {
            eprintln!(
                "  engine: {}x{} total {:6.1} ms | grade {:5.1} fx {:4.1} lut {:4.1} composite {:5.1} | encode {:5.1} scope {:4.1}",
                s.width, s.height, s.total_ms, s.grade_ms, s.builtin_ms, s.lut_ms, s.composite_ms, s.reencode_ms, s.scope_ms,
            );
        }
        let mut sorted = steps[1..].to_vec(); // the first step pays for the drag's setup
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "{label}: slider step to new frame median {:.1} ms (best {:.1}, worst {:.1}); release to full-res frame {release:.1?} ms",
            sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1],
        );
    };
    drag("node editor closed");
    // The node editor open: it asks for a thumbnail of every node but
    // Output, on every paused frame.
    parked.send(AppCommand::SetVisibleNodeThumbs {
        clip_id: parked.clip_id,
        node_ids: vec![parked.source_node, parked.exposure_node],
    });
    std::thread::sleep(Duration::from_millis(800));
    drag("node editor open");
}
