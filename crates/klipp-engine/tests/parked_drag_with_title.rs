//! A slider dragged on the parked frame with a title over the clip
//! (#2351). The parked regrade grades the clip alone, so it laid the
//! title over its bytes from a CPU raster; the frame now takes the GPU
//! stack, which composites the title on the GPU as an export does. Each
//! step's frame shows that step's value, not the one before: it is the
//! frame the full render gives for the same grade. A drag with no title
//! keeps the regrade.

mod common;

use klipp_timeline::{LayerFx, TextBox, TextGlow, TitleData};

fn title() -> TitleData {
    TitleData::single_text_fx(
        TextBox { text: "Over the grade".into(), font_size: 90.0, color: [240, 220, 120, 255], ..Default::default() },
        LayerFx { glows: vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 10.0, radius_keyframes: vec![], intensity: 0.8, intensity_keyframes: vec![] }], ..Default::default() },
    )
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

fn drag_with_title(label: &str, tags: &str) {
    let Some(src) = common::source(label, 640, 360, tags) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_under_title(src, 640, 360, title());
    let overlays = klipp_engine::engine::cpu_title_overlays();
    let before = parked.frame();
    // Each value dragged to and let go of there: the drag's frame is the
    // released one, so the step showed its own value.
    let mut last = before.clone();
    for stops in [0.9, 1.4] {
        parked.begin_drag_seen(stops - 0.3);
        parked.step(stops).expect("a drag step showed no frame");
        let dragged = parked.frame();
        assert_ne!(dragged, last, "{label}: the step to {stops} changed nothing");
        parked.end_drag().expect("letting go showed no frame");
        parked.settle();
        let released = parked.frame();
        let d = worst(&dragged, &released);
        eprintln!("{label}: drag frame at {stops} vs released: worst {d} levels");
        assert!(d <= 1, "{label}: the drag at {stops} showed a picture {d} levels off the full render");
        last = released;
    }
    assert_eq!(klipp_engine::engine::cpu_title_overlays(), overlays, "{label}: no title laid over from a CPU raster");
}

#[test]
fn an_sdr_drag_under_a_title_shows_each_step() {
    drag_with_title("sdr", common::SDR_TAGS);
}

#[test]
fn a_pq_drag_under_a_title_shows_each_step() {
    drag_with_title("pq", common::PQ_TAGS);
}

/// With no title, a drag keeps the parked regrade: no composite, no
/// display encode on the CPU.
#[test]
fn a_drag_without_a_title_keeps_the_regrade() {
    let Some(src) = common::source("sdr", 640, 360, common::SDR_TAGS) else { return };
    let parked = common::Parked::start(src, 640, 360);
    parked.begin_drag_seen(0.25);
    for stops in [0.75, 1.0] {
        parked.step(stops).expect("a drag step showed no frame");
    }
    for s in parked.perf().iter().rev().take(2) {
        assert!(s.in_param_edit && s.reencode_ms == 0.0 && s.composite_ms == 0.0, "a drag step left the regrade ({s:?})");
    }
    parked.end_drag();
}
