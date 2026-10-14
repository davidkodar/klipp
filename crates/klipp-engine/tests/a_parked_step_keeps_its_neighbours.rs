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
    // The denoise's output was held between the steps (#1863): taken, not
    // run, on the steps after the first; the engine without the hold
    // never holds one.
    assert!(held_hits(&dragged) >= 2, "the drag took the denoise held: {:#?}", dragged.trail());
    assert_eq!(held_hits(&fresh), 0, "{:?}", fresh.trail().last());
}

/// The renderer's held-output hits the engine last noted, 0 if none.
fn held_hits(parked: &common::Parked) -> u64 {
    held_counts(parked).0
}

/// The renderer's held-output hits and misses the engine last noted.
fn held_counts(parked: &common::Parked) -> (u64, u64) {
    parked
        .trail()
        .iter()
        .rev()
        .find_map(|l| {
            let mut words = l.split("held outputs ").nth(1)?.split(' ');
            let hits = words.next()?.parse().ok()?;
            let misses = words.nth(1)?.parse().ok()?;
            Some((hits, misses))
        })
        .unwrap_or((0, 0))
}

/// Playing empties the held outputs (#1863): back on the same parked
/// frame, presenting it runs the denoise again, a miss, though its key
/// is the one held before.
#[test]
fn playing_empties_the_held_outputs() {
    let Some(src) = common::source("tnr-held-play", 320, 180, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_with(src, 320, 180, denoised(0.5));
    parked.begin_drag();
    for stops in [0.6, 0.7] {
        parked.step(stops).expect("a step shows a frame");
    }
    parked.end_drag().expect("the release shows a frame");
    parked.settle();
    let (hits, _) = held_counts(&parked);
    assert!(hits >= 1, "the drag hit: {:#?}", parked.trail());
    parked.send(klipp_core::AppCommand::Play);
    std::thread::sleep(std::time::Duration::from_millis(300));
    parked.send(klipp_core::AppCommand::Pause);
    parked.settle();
    let (_, misses) = held_counts(&parked);
    // Back on the frame held before playing: presenting it misses, the
    // held outputs gone with the play, though its key is the same.
    let g = parked.generation();
    parked.send(klipp_core::AppCommand::SetPlayhead(common::frame_tc(10, parked.fps)));
    parked.settle_after(g, std::time::Duration::from_secs(20)).expect("the frame parked again");
    let (_, after) = held_counts(&parked);
    assert!(after > misses, "the frame held before playing missed: {misses} -> {after} {:#?}", parked.trail());
}

/// A file rewritten on disk while parked (#1863): the next step reads the
/// new file's frames either side, never the ones held from the old file.
/// The parked frame itself is the decoder's and stays, so the same grade
/// draws a different picture only through the neighbours — and the picture
/// of an engine parked alike that holds nothing, to a level: that one
/// draws its denoise on the GPU chain, the holding one on the CPU, where
/// its output is the pixels held (#1988).
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
    let levels = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0);
    assert!(levels(&at(&unheld, 0.6), &before) <= 1, "the two engines differ before the rewrite");
    // Replaced, as an editor saving over it would: a new file renamed in.
    let staged = dir.join("take.new.mkv");
    std::fs::copy(&negative, &staged).unwrap();
    std::fs::rename(&staged, &file).unwrap();
    let after = at(&parked, 0.6);
    let reference = at(&unheld, 0.6);
    let _ = std::fs::remove_dir_all(&dir);
    let differ = before.iter().zip(&after).filter(|(x, y)| x != y).count();
    assert!(differ > 0, "the step after the file was rewritten drew the old file's neighbours");
    let off = levels(&after, &reference);
    assert!(off <= 1, "{off} levels from an engine that holds nothing");
}
