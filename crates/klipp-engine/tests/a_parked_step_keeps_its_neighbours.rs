//! A slider step on a parked clip with temporal noise reduction reads the
//! frames either side of it from the engine's hold rather than decoding
//! them again (#1863), and draws exactly the picture a fresh render does.

mod common;

use klipp_timeline::{Clip, GradeAccess};

/// Temporal NR on the clip, and the Exposure node at `stops`.
fn denoised(stops: f32) -> impl FnOnce(&mut Clip) {
    move |clip: &mut Clip| {
        *clip = klipp_timeline::test_clip::TestClip::of(clip.clone())
            .effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 })
            .build();
        let mut g = clip.grade().clone();
        for n in &mut g.nodes {
            if n.kind == klipp_nodes::NodeKind::Exposure {
                n.params.exposure = Some(stops);
            }
        }
        clip.set_grade(g);
    }
}

/// Dragged to 1 stop and let go, the parked frame is the frame an engine
/// without the hold draws at 1 stop: the held neighbours are the frames
/// either side, not frames of another time.
#[test]
fn a_step_with_held_neighbours_draws_the_fresh_frame() {
    let Some(src) = common::source("tnr-hold", 320, 180, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dragged = common::Parked::start_with(src.clone(), 320, 180, denoised(0.5));
    dragged.begin_drag();
    for stops in [0.6, 0.8, 1.0] {
        dragged.step(stops).expect("a step shows a frame");
    }
    dragged.end_drag().expect("the release shows a frame");
    dragged.settle();
    let fresh = common::Parked::start_with_unheld(src, 320, 180, denoised(1.0));
    let (a, b) = (dragged.frame(), fresh.frame());
    assert_eq!(a.len(), b.len());
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    assert_eq!(differ, 0, "{differ} bytes differ from a render without the hold");
}

/// A file rewritten on disk while parked (#1863): the next step reads the
/// new file's frames either side, never the ones held from the old file.
/// The parked frame itself is the decoder's and stays, so the same grade
/// draws a different picture only through the neighbours — and exactly
/// the picture of an engine parked alike that holds nothing.
#[test]
fn a_file_rewritten_while_parked_is_read_again() {
    let Some(src) = common::source("tnr-rewrite", 320, 180, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("klipp-tnr-rewrite-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("take.mkv");
    std::fs::copy(&src, &file).unwrap();
    // The same pattern turned negative: other neighbours, the same size,
    // rate and tags.
    let negative = dir.join("negative.mkv");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-vf", "negate", "-c:v", "ffv1"])
        .arg(&negative)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("skipping: ffmpeg could not make the negative");
        return;
    }
    let parked = common::Parked::start_with(file.clone(), 320, 180, denoised(0.5));
    // Parked on the old file too, decoding its neighbours on every render.
    let unheld = common::Parked::start_with_unheld(file.clone(), 320, 180, denoised(0.5));
    // Two slider steps, the second's frame: the first reads the frames
    // either side (and the hold keeps them), the second is drawn from the
    // hold. A step renders at the drag's size and a release at full size,
    // so only steps share neighbours; the frame is taken inside the drag.
    let at = |parked: &common::Parked, stops: f32| {
        parked.begin_drag();
        parked.step(stops + 0.1).expect("a step shows a frame");
        parked.step(stops).expect("a step shows a frame");
        parked.settle();
        let frame = parked.frame();
        parked.end_drag().expect("the release shows a frame");
        parked.settle();
        frame
    };
    let before = at(&parked, 0.6);
    // Both dragged once on the old file, so both hold their parked frame
    // from it, and agree there.
    assert!(at(&unheld, 0.6) == before, "the two engines differ before the rewrite");
    // Replaced, as an editor saving over it would: a new file renamed in.
    let staged = dir.join("take.new.mkv");
    std::fs::copy(&negative, &staged).unwrap();
    std::fs::rename(&staged, &file).unwrap();
    let after = at(&parked, 0.6);
    let reference = at(&unheld, 0.6);
    let _ = std::fs::remove_dir_all(&dir);
    let differ = before.iter().zip(&after).filter(|(x, y)| x != y).count();
    assert!(differ > 0, "the step after the file was rewritten drew the old file's neighbours");
    let off = after.iter().zip(&reference).filter(|(x, y)| x != y).count();
    assert_eq!(off, 0, "{off} bytes differ from an engine that holds nothing");
}
