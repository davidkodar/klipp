//! A slider drag below a heavy node the renderer holds for the parked
//! frame takes the GPU path (#1863, step B3), in the cases that would
//! otherwise go slow without a word: below an OFX node, a source bigger
//! than the timeline, and media at another frame rate than the timeline.
//! Each drag's steps must be graded on the GPU, take the held output (the
//! engine's notes count the hits), and show within a level of what an
//! engine holding nothing shows at the same grade.
//!
//! Its own binary: the OFX case sets `OFX_PLUGIN_PATH`, which the engine
//! reads once, when it first looks for plug-ins.

mod common;

use klipp_core::Fps;
use klipp_timeline::{Clip, GradeAccess};

fn denoise(clip: &mut Clip) {
    *clip = klipp_timeline::test_clip::TestClip::of(clip.clone())
        .effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 })
        .build();
}

/// The C fixture's OFX node between the Source and the Exposure node.
fn ofx_node(clip: &mut Clip) {
    let mut g = clip.grade().clone();
    let id = klipp_nodes::NodeId::new_v4();
    let params = klipp_nodes::NodeParams {
        ofx: Some(klipp_nodes::plugin::OfxNodeParams { plugin: "org.klipp.fixture.channels".into(), values: Default::default(), filter: true }),
        ..Default::default()
    };
    g.nodes.insert(1, klipp_nodes::Node { id, kind: klipp_nodes::NodeKind::Ofx, label: "OFX".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] });
    let (source, exposure) = (g.nodes[0].id, g.nodes[2].id);
    g.edges.retain(|e| !(e.from_node == source && e.to_node == exposure));
    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: id, to_port: 0 });
    g.edges.push(klipp_nodes::Edge { from_node: id, from_port: 0, to_node: exposure, to_port: 0 });
    g.node_positions.insert(id, klipp_nodes::NodePos { x: 50.0, y: 0.0 });
    clip.set_grade(g);
}

/// `heavy` on the clip and its Exposure node at `stops`.
fn graded(heavy: fn(&mut Clip), stops: f32) -> impl FnOnce(&mut Clip) {
    move |clip: &mut Clip| {
        heavy(clip);
        let mut g = clip.grade().clone();
        for n in g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Exposure) {
            n.params.exposure = Some(stops);
        }
        clip.set_grade(g);
    }
}

/// The held-output hits the engine last noted.
fn held_hits(parked: &common::Parked) -> u64 {
    parked.trail().iter().rev().find_map(|l| l.split("held outputs ").nth(1)?.split(' ').next()?.parse().ok()).unwrap_or(0)
}

/// The largest difference between two RGBA frames' colour, in levels.
fn worst(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// A drag of `heavy`'s clip from `src` on a `width × height` timeline at
/// `fps`: its steps on the GPU, taking the held output, and its picture
/// an engine-holding-nothing's.
fn held_drag(label: &str, src: std::path::PathBuf, width: u32, height: u32, fps: Fps, heavy: fn(&mut Clip)) {
    let parked = common::Parked::start_with_at(src.clone(), width, height, fps, graded(heavy, 0.5));
    parked.begin_drag_seen(0.25);
    let hits = held_hits(&parked);
    let mut frames = Vec::new();
    for stops in [0.75, 1.0, 1.25] {
        parked.step(stops).expect("a drag step showed no frame");
        frames.push(parked.frame());
    }
    let dragged = parked.perf();
    for s in dragged.iter().rev().take(3) {
        assert!(s.reencode_ms == 0.0 && s.composite_ms == 0.0, "{label}: a drag step went through the full render ({s:?})");
    }
    let taken = held_hits(&parked) - hits;
    assert!(taken >= 3, "{label}: the steps took the held output {taken} times: {:#?}", parked.trail());
    assert_ne!(frames[0], frames[1], "{label}: a step changed nothing");
    // Letting go (#1939): the parked frame presented again takes the
    // held output on the GPU too, not the full render.
    let before = held_hits(&parked);
    parked.end_drag().expect("letting go showed no frame");
    parked.settle();
    let release = parked.perf().last().copied().expect("the release's sample");
    assert!(release.reencode_ms == 0.0 && release.composite_ms == 0.0, "{label}: the release went through the full render ({release:?})");
    assert!(held_hits(&parked) > before, "{label}: the release took nothing held: {:#?}", parked.trail());
    let released = parked.frame();
    let fresh = common::Parked::start_with_at_unheld(src, width, height, fps, graded(heavy, 1.25));
    let off = worst(frames.last().unwrap(), &fresh.frame());
    let off_released = worst(&released, &fresh.frame());
    eprintln!("{label}: {taken} held, worst {off} levels from an engine holding nothing, the release {off_released}");
    assert!(off <= 1 && off_released <= 1, "{label}: the drag / release showed a picture {off} / {off_released} levels off the full render");
}

#[test]
fn drags_below_held_heavy_nodes_take_the_gpu_path() {
    let at = Fps::new(common::FPS, 1);
    // Read once, by the first engine below, before any other runs.
    let ofx = common::ofx_fixture_bundle();
    if let Some(dir) = &ofx {
        std::env::set_var("OFX_PLUGIN_PATH", dir);
    }
    let Some(src) = common::source("held-gpu", 640, 360, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    held_drag("temporal NR", src.clone(), 640, 360, at, denoise);
    match ofx {
        Some(_) => held_drag("OFX node", src.clone(), 640, 360, at, ofx_node),
        None => eprintln!("UNVERIFIED: no C compiler, the OFX case is skipped"),
    }
    // A source twice the timeline's size, decoded and graded smaller.
    let big = common::source("held-gpu-big", 1280, 720, common::PQ_TAGS).expect("made the source above");
    held_drag("a source bigger than the timeline", big, 640, 360, at, denoise);
    // Media at 25 fps on a 29.97 timeline: its frames' times are not the
    // timeline's.
    held_drag("media at another rate", src, 640, 360, Fps::new(30000, 1001), denoise);
}
