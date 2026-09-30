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
    TitleData::single_text_fx(
        TextBox { text: "Motion test".into(), font_size: 60.0, ..Default::default() },
        klipp_timeline::LayerFx { motion, ..Default::default() },
    )
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
    let td = TitleData::single_text_fx(
        TextBox { text: "Rolling\ncredits\nhere".into(), font_size: 40.0, ..Default::default() },
        klipp_timeline::LayerFx { loops: vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }], ..Default::default() },
    );
    let (w, h) = (320, 180);
    let mut ids = Vec::new();
    for t in [0.5f32, 1.5, 2.5] {
        let want = r.render_with_window(&td, w, h, None, t, Some(4.0));
        let frame = r.frame_with_window(&td, w, h, None, t, Some(4.0));
        let klipp_title::TitleFrame::Placed { sprite, dx, dy, opacity, rows, .. } = frame else {
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

/// A motion rests once it has run: its delay and duration, plus the
/// stagger of its last unit only when it staggers. The stagger time has
/// a default of its own, and counting it for a plain fade kept a
/// 22-letter name "moving" for a second and a half at each end of a
/// four-second lower third, rastered afresh on every frame of it.
#[test]
fn a_motion_without_stagger_rests_as_soon_as_it_has_run() {
    let placed = |td: &TitleData, t: f32| {
        let mut r = TitleRenderer::new();
        matches!(r.frame_with_window(td, 320, 180, None, t, Some(4.0)), klipp_title::TitleFrame::Placed { .. })
    };
    // The resting render placed at full opacity: at rest.
    let resting = |td: &TitleData, t: f32| {
        let mut r = TitleRenderer::new();
        matches!(r.frame_with_window(td, 320, 180, None, t, Some(4.0)), klipp_title::TitleFrame::Placed { opacity, .. } if opacity >= 0.999)
    };
    let fade = || Motion::new(MotionKind::Fade).duration(0.4);
    let mut td = title(TitleMotion::both(fade(), fade()));
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
        tb.text = "David Heinemeier Hansson".into();
    }
    assert!(placed(&td, 0.2) && !resting(&td, 0.2), "fading in: the resting render placed, faded");
    assert!(resting(&td, 0.6), "at rest just after a 0.4 s fade: the cached render");
    assert!(resting(&td, 3.4), "and until the exit starts");
    assert!(placed(&td, 3.8) && !resting(&td, 3.8), "fading out: placed, faded");

    // Staggered by character, the last letter arrives late, and the
    // title is not at rest until it has.
    let mut staggered = title(TitleMotion::enter(fade().stagger(StaggerUnit::Char, 0.05)));
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut staggered.objects[0].content {
        tb.text = "David Heinemeier Hansson".into();
    }
    assert!(!placed(&staggered, 1.0), "the last of 22 letters starts at 1.05 s");
    assert!(placed(&staggered, 1.7));
}

/// #592: a roll's shape fades it by where it is on screen, on the path
/// that places the resting render and on the one that rasters every
/// layer (forced here by a constant opacity key): each pixel's alpha is
/// the unshaped frame's times the ramp at its row.
#[test]
fn a_shaped_roll_fades_by_where_it_is_on_both_paths() {
    use klipp_core::title_motion::{FocusBand, RampAxis, RollShape};
    use klipp_timeline::{ScrollDirection, TitleLoop};
    let shape = RollShape { edge_fade: 0.25, focus: Some(FocusBand { band: 0.3, edge_opacity: 0.3 }) };
    let roll = |shape: RollShape, raster: bool| {
        let mut td = TitleData::single_text_fx(
            TextBox { text: "Rolling\ncredits\nhere\nand\nmore".into(), font_size: 40.0, ..Default::default() },
            klipp_timeline::LayerFx { loops: vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape }], ..Default::default() },
        );
        if raster {
            td.objects[0].opacity_keyframes = vec![klipp_core::Keyframe {
                time_secs: 0.0,
                value: 1.0,
                interpolation: klipp_core::Interpolation::Linear,
                tangent_out: (0.0, 0.0),
                tangent_in: (0.0, 0.0),
            }];
        }
        td
    };
    let ramp = shape.ramp(RampAxis::Vertical).unwrap();
    let (w, h) = (320usize, 180usize);
    let mut r = TitleRenderer::new();
    for raster in [false, true] {
        for t in [0.8f32, 2.0, 3.2] {
            let shaped_td = roll(shape, raster);
            if !raster {
                let frame = r.frame_with_window(&shaped_td, w as u32, h as u32, None, t, Some(4.0));
                assert!(matches!(frame, klipp_title::TitleFrame::Placed { shape: Some(_), .. }), "a shaped roll is still the resting render placed");
            }
            let plain = r.render_with_window(&roll(RollShape::default(), raster), w as u32, h as u32, None, t, Some(4.0));
            let shaped = r.render_with_window(&shaped_td, w as u32, h as u32, None, t, Some(4.0));
            let mut lit = 0;
            for (i, (p, s)) in plain.chunks_exact(4).zip(shaped.chunks_exact(4)).enumerate() {
                let y = i / w;
                let want = p[3] as f32 * ramp.alpha_at((y as f32 + 0.5) / h as f32);
                assert!((s[3] as f32 - want).abs() <= 1.0, "raster {raster}, t {t}: row {y} alpha {} where {want:.1}", s[3]);
                lit += (p[3] > 0) as usize;
            }
            assert!(lit > 100, "raster {raster}, t {t}: the credits are on screen");
        }
    }
}
