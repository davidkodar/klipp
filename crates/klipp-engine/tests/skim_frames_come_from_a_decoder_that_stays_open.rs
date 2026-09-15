//! Skim frames come from a decoder that stays open per file: the second
//! position on the same file reuses it, a step forward is answered from
//! its read-ahead, and the frame is the frame at that time. The ramp
//! fixture's luma rises with the frame number, so "which frame is this"
//! can be read off the picture.
//!
//! Every position used to open a fresh software decoder with a thread
//! per core and convert on the CPU, which is what spun the fans up
//! while skimming.

mod common;

use klipp_engine::skim::SkimDecoders;
use std::time::{Duration, Instant};

fn mean_r(rgba: &[u8]) -> f64 {
    rgba.chunks(4).map(|p| p[0] as f64).sum::<f64>() / (rgba.len() / 4) as f64
}

#[test]
fn a_skimmed_file_keeps_its_decoder_and_steps_forward_from_the_read_ahead() {
    let Some(path) = common::hevc_ramp() else { return };
    let mut skim = SkimDecoders::new(3);
    let at = |skim: &mut SkimDecoders, secs: f64| {
        let t0 = Instant::now();
        let f = skim.frame_at(&path, 0, secs, 1080, Duration::from_secs(2)).expect("a skim frame");
        assert_eq!((f.width, f.height), (common::RAMP_W, common::RAMP_H));
        (mean_r(&f.rgba), f.reused_decoder, t0.elapsed())
    };

    let (a, reused_a, _) = at(&mut skim, 0.40);
    let (b, reused_b, t_b) = at(&mut skim, 0.44);
    let (c, reused_c, t_c) = at(&mut skim, 0.48);
    assert!(!reused_a && reused_b && reused_c, "the first position opens the decoder; the next ones reuse it");
    assert_eq!(skim.open_count(), 1);
    assert!(a < b && b < c, "each frame on is brighter on the ramp: {a:.1} {b:.1} {c:.1}");
    assert!(t_b < Duration::from_millis(60) && t_c < Duration::from_millis(60), "steps forward are answered from the read-ahead, not a fresh decode: {t_b:?} {t_c:?}");

    let (far, _, _) = at(&mut skim, 1.20);
    assert!(far > c, "a position far ahead is decoded too: {far:.1} vs {c:.1}");
    let (back, _, _) = at(&mut skim, 0.40);
    assert!((back - a).abs() < 2.0, "back to the first position gives the same frame: {back:.1} vs {a:.1}");

    skim.sweep(Duration::ZERO);
    assert_eq!(skim.open_count(), 0, "an idle sweep closes the decoder");
}

#[test]
fn the_most_recently_used_decoders_are_kept() {
    let Some(path) = common::hevc_ramp() else { return };
    let mut skim = SkimDecoders::new(1);
    let copy = common::damaged(&path, "skim-copy", 0, 0, 0);
    skim.frame_at(&path, 0, 0.4, 1080, Duration::from_secs(2)).unwrap();
    skim.frame_at(&copy, 0, 0.4, 1080, Duration::from_secs(2)).unwrap();
    assert_eq!(skim.open_count(), 1, "one decoder at most");
    let f = skim.frame_at(&path, 0, 0.4, 1080, Duration::from_secs(2)).unwrap();
    assert!(!f.reused_decoder, "the first file's decoder was evicted by the second");
}

#[test]
fn a_resting_position_is_answered_with_the_frame_it_shows_without_a_seek() {
    let Some(path) = common::hevc_ramp() else { return };
    let mut skim = SkimDecoders::new(1);
    let mut at = |secs: f64| {
        let f = skim.frame_at(&path, 0, secs, 1080, Duration::from_secs(2)).expect("a skim frame");
        (mean_r(&f.rgba), f.repeated)
    };
    // 25 fps: frame 10 is shown from 0.40 s until 0.44 s.
    let (frame10, _) = at(0.41);
    let (again, repeated) = at(0.41);
    assert!(repeated && again == frame10, "the same position again is the frame already delivered, without a seek");
    let (later, repeated_later) = at(0.43);
    assert!(repeated_later && later == frame10, "a later time inside the same frame is that frame too");
    let (frame11, repeated11) = at(0.44);
    assert!(!repeated11 && frame11 > frame10, "the next frame's time is the next frame: {frame11:.1} vs {frame10:.1}");

    // Between two frames from a decoder that has not delivered either: the
    // frame shown at that time, not the one after it.
    let mut fresh = SkimDecoders::new(1);
    let f = fresh.frame_at(&path, 0, 0.43, 1080, Duration::from_secs(2)).expect("a skim frame");
    assert!((mean_r(&f.rgba) - frame10).abs() < 2.0, "0.43 s is frame 10: {:.1} vs {frame10:.1}", mean_r(&f.rgba));
}
