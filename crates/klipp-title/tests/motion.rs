//! The motion model as the renderer sees it: glyphs arrive over the
//! entrance, rest, and leave over the exit, counted back from the
//! clip's end.

use klipp_timeline::{Easing, Motion, MotionKind, StaggerUnit, TextBox, TitleData, TitleMotion};
use klipp_title::TitleRenderer;

fn coverage(r: &mut TitleRenderer, td: &TitleData, t: f32, window: Option<f32>) -> usize {
    let px = r.render_with_window(td, 320, 180, None, t, window);
    px.chunks_exact(4).filter(|p| p[3] > 8).count()
}

fn title(motion: TitleMotion) -> TitleData {
    TitleData::single_text(TextBox {
        text: "Motion test".into(),
        font_size: 60.0,
        motion,
        ..Default::default()
    })
}

#[test]
fn glyphs_arrive_over_the_entrance_and_rest_after_it() {
    let mut r = TitleRenderer::new();
    let td = title(TitleMotion::enter(Motion::new(MotionKind::Fade).duration(1.0).easing(Easing::Linear)));
    let start = coverage(&mut r, &td, 0.0, Some(4.0));
    let mid = coverage(&mut r, &td, 0.5, Some(4.0));
    let rest = coverage(&mut r, &td, 2.0, Some(4.0));
    assert_eq!(start, 0, "nothing visible at t=0");
    assert!(mid > 0 && mid <= rest, "mid {mid} rest {rest}");
    assert!(rest > 200, "the text is there once the entrance is over");
}

#[test]
fn the_exit_counts_back_from_the_end_of_the_clip() {
    let mut r = TitleRenderer::new();
    let td = title(TitleMotion::both(
        Motion::new(MotionKind::Fade).duration(0.2),
        Motion::new(MotionKind::Fade).duration(1.0).easing(Easing::Linear),
    ));
    let rest = coverage(&mut r, &td, 2.0, Some(4.0));
    let leaving = coverage(&mut r, &td, 3.5, Some(4.0));
    let gone = coverage(&mut r, &td, 4.0, Some(4.0));
    assert!(rest > 200);
    assert!(leaving < rest, "leaving {leaving} rest {rest}");
    assert_eq!(gone, 0);
    // Without a window the exit cannot be placed and the text stays.
    assert_eq!(coverage(&mut r, &td, 3.9, None), rest);
}

#[test]
fn stagger_by_character_reveals_the_text_progressively() {
    let mut r = TitleRenderer::new();
    let td = title(TitleMotion::enter(
        Motion::new(MotionKind::Typewriter).duration(0.01).stagger(StaggerUnit::Char, 0.1),
    ));
    let early = coverage(&mut r, &td, 0.15, Some(4.0));
    let later = coverage(&mut r, &td, 0.55, Some(4.0));
    let all = coverage(&mut r, &td, 3.0, Some(4.0));
    assert!(early > 0 && early < later && later < all, "{early} {later} {all}");
}

#[test]
fn a_mask_rise_never_paints_above_the_resting_box() {
    let mut r = TitleRenderer::new();
    let td = title(TitleMotion::enter(Motion::new(MotionKind::MaskRise).duration(1.0).easing(Easing::Linear)));
    let rows = |px: &[u8]| -> Vec<usize> {
        (0..180).filter(|y| (0..320).any(|x| px[(y * 320 + x) * 4 + 3] > 8)).collect()
    };
    let rest = r.render_with_window(&td, 320, 180, None, 3.0, Some(4.0));
    let rest_rows = rows(&rest);
    let rising = r.render_with_window(&td, 320, 180, None, 0.5, Some(4.0));
    let rising_rows = rows(&rising);
    assert!(!rising_rows.is_empty());
    let (rest_min, rest_max) = (*rest_rows.first().unwrap(), *rest_rows.last().unwrap());
    assert!(rising_rows.iter().all(|y| *y >= rest_min && *y <= rest_max), "rising rows leak outside the resting box");
}

/// The frame API hands out the resting sprite and a placement when that
/// is all the frame is, and the placement applied is the rendered frame.
#[test]
fn a_placed_frame_is_the_rendered_frame() {
    use klipp_timeline::{ScrollDirection, TitleLoop};
    let mut r = TitleRenderer::new();
    let td = TitleData::single_text(TextBox {
        text: "Rolling\ncredits\nhere".into(),
        font_size: 40.0,
        loops: vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0 }],
        ..Default::default()
    });
    let (w, h) = (320, 180);
    let mut ids = Vec::new();
    for t in [0.5f32, 1.5, 2.5] {
        let want = r.render_with_window(&td, w, h, None, t, Some(4.0));
        let frame = r.frame_with_window(&td, w, h, None, t, Some(4.0));
        let klipp_title::TitleFrame::Placed { sprite, dx, dy, opacity, rows } = frame else {
            panic!("a scroll is the resting render moved, not a render of its own");
        };
        assert_eq!(sprite.pixels.len(), want.len());
        ids.push(sprite.id);
        // Apply the placement here and compare.
        let mut got = vec![0u8; want.len()];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let (sx, sy) = (x - dx, y - dy);
                if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                    continue;
                }
                let d = ((y * w as i32 + x) * 4) as usize;
                let so = ((sy * w as i32 + sx) * 4) as usize;
                got[d..d + 4].copy_from_slice(&sprite.pixels[so..so + 4]);
                if opacity < 0.9999 {
                    got[d + 3] = ((got[d + 3] as f32) * opacity).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        assert_eq!(got, want, "at t={t} the placement differs from the render");
        // The rows the placement names hold every covered pixel.
        for (i, px) in want.chunks_exact(4).enumerate() {
            let y = (i / w as usize) as u32;
            if px[3] != 0 {
                assert!(y >= rows.0 && y < rows.1, "row {y} carries alpha outside rows {rows:?}");
            }
        }
    }
    assert!(ids.windows(2).all(|p| p[0] == p[1]), "one resting render serves every frame: {ids:?}");
}
