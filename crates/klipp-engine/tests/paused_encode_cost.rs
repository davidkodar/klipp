//! Where a paused 4K frame's time goes, broken down by stage.
//!
//! A probe, not a pass/fail test:
//! `cargo test --release -p klipp-engine --test paused_encode_cost -- --ignored --nocapture`
//!
//! [`paused_seek_perf`] measures what the wait feels like; this measures
//! which stage to shorten. The interesting figure is `encode`, which is the
//! display transform over a 4K frame — 33 MB out of 132 MB in — and how it
//! compares with the render that produced it.

mod common;

use std::time::Duration;

use klipp_core::{AppCommand, RationalTime};

#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_stage_costs() {
    klipp_engine::video_decoder::set_max_playback_height(2160);
    let Some(src) = common::hevc_source("pq-hevc", 3840, 2160, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 3840, 2160);
    let tc = |f: i64| RationalTime::from_frames(f, common::FPS);

    let report = |label: &str, cmd: AppCommand| {
        let before = parked.perf().len();
        let g = parked.generation();
        parked.send(cmd);
        // And any follow-up frame (a paused refresh) with it.
        let _ = parked.settle_after(g, Duration::from_secs(60));
        let samples = parked.perf();
        for s in samples.iter().skip(before) {
            eprintln!(
                "{label:22} {w}x{h} total {total:6.1} | grade {grade:5.1} composite {comp:5.1} \
                 lut {lut:5.1} encode {enc:5.1} scope {scope:5.1}",
                w = s.width,
                h = s.height,
                total = s.total_ms,
                grade = s.grade_ms,
                comp = s.composite_ms,
                lut = s.lut_ms,
                enc = s.reencode_ms,
                scope = s.scope_ms,
            );
        }
    };

    report("click far 10->40", AppCommand::SetPlayhead(tc(40)));
    report("click near 40->43", AppCommand::SetPlayhead(tc(43)));
    report("click back 43->12", AppCommand::SetPlayhead(tc(12)));
    report("step forward", AppCommand::SeekRelative(1));
    report("step forward", AppCommand::SeekRelative(1));
    report("step back", AppCommand::SeekRelative(-1));
    // Re-visiting a frame already rendered: the render-cache path, which
    // expands the cached bytes back to float and encodes them again.
    report("revisit 40 (cached)", AppCommand::SetPlayhead(tc(40)));
    report("revisit 43 (cached)", AppCommand::SetPlayhead(tc(43)));
}
