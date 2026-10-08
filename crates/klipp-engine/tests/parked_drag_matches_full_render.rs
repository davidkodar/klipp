//! A slider dragged on the parked frame shows what letting go shows.
//!
//! While a slider is held the viewer grades the parked frame on the GPU
//! and reads back display bytes (`regrade_parked_on_gpu`); letting go
//! renders it through the shared renderer, as every other paused frame
//! is. The last frame of a drag and the frame after release have the same
//! grade, so they must be the same picture — for a PQ source, graded
//! scene-linear and tone-mapped, and for an SDR one. The drag's frames
//! must also really have taken the GPU path, or the comparison proves
//! nothing.

mod common;

use klipp_core::AppCommand;

fn drag_then_release(label: &str, tags: &str) {
    let Some(src) = common::source(label, 640, 360, tags) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 640, 360);
    let before = parked.frame();

    parked.begin_drag();
    let mut frames = Vec::new();
    for stops in [0.75, 1.0, 1.25] {
        parked.step(stops).expect("a drag step showed no frame");
        frames.push(parked.frame());
    }
    let dragged = parked.perf();
    parked.end_drag().expect("letting go showed no frame");
    let released = parked.frame();

    // The drag's frames came off the GPU path: no display encode on the
    // CPU, no composite.
    for s in dragged.iter().rev().take(3) {
        assert!(
            s.reencode_ms == 0.0 && s.composite_ms == 0.0,
            "{label}: a drag step went through the full render ({s:?})",
        );
    }
    // Each step changed the picture...
    assert_ne!(before, frames[0], "{label}: the first step changed nothing");
    assert_ne!(frames[0], frames[1], "{label}: a step changed nothing");
    // ...and the last is what the full render gives for the same grade.
    let last = frames.last().unwrap();
    let worst = last
        .iter()
        .zip(&released)
        .enumerate()
        .filter(|(i, _)| i % 4 != 3)
        .map(|(_, (a, b))| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap();
    eprintln!("{label}: drag frame vs frame after release: worst {worst} levels");
    assert!(worst <= 1, "{label}: the drag showed a picture {worst} levels off the full render");
}

#[test]
fn a_pq_drag_shows_the_full_render() {
    drag_then_release("pq", common::PQ_TAGS);
}

#[test]
fn an_sdr_drag_shows_the_full_render() {
    drag_then_release("sdr", common::SDR_TAGS);
}

/// With the node editor open the drag still takes the GPU path —
/// thumbnails wait for the release — and the release brings them.
#[test]
fn thumbnails_wait_for_the_release() {
    let Some(src) = common::source("pq", 640, 360, common::PQ_TAGS) else { return };
    let parked = common::Parked::start(src, 640, 360);
    parked.send(AppCommand::SetVisibleNodeThumbs {
        request: 0,
        clip_id: parked.clip_id,
        at: [parked.source_node, parked.exposure_node].into_iter().map(|n| klipp_compose::inspect::grade_node(parked.clip_id, n)).collect(),
    });
    parked.settle();
    parked.begin_drag();
    // The begin goes through the command channel and a step through the
    // parameter queue, so on a loaded machine the first step's frame can
    // be rendered before the engine has seen the drag begin, as a full
    // render. One step lets the drag start; the next is the one judged.
    parked.step(0.5).expect("a drag step showed no frame");
    parked.step(1.0).expect("a drag step showed no frame");
    let s = parked.perf().last().cloned().unwrap();
    assert!(s.reencode_ms == 0.0 && s.composite_ms == 0.0, "thumbnails sent the drag through the full render ({s:?})");
    parked.end_drag().expect("letting go showed no frame");
}
