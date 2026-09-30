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

/// #592: a layer's edge fade and a roll's focus band fade it by where it
/// is on screen, on the path that places the resting render and on the
/// one that rasters every layer (forced here by a constant opacity key):
/// each pixel's alpha is the unfaded frame's times the fade there.
#[test]
fn a_faded_roll_fades_by_where_it_is_on_both_paths() {
    use klipp_core::title_motion::{EdgeFade, FocusBand, RampAxis, RollShape, ScreenFade};
    use klipp_timeline::{ScrollDirection, TitleLoop};
    let edges = EdgeFade { top: 0.25, bottom: 0.25, ..EdgeFade::NONE };
    let focus = FocusBand { band: 0.3, edge_opacity: 0.3, blur: 0.0 };
    let roll = |faded: bool, raster: bool| {
        let shape = RollShape { focus: faded.then_some(focus) };
        let mut td = TitleData::single_text_fx(
            TextBox { text: "Rolling\ncredits\nhere\nand\nmore".into(), font_size: 40.0, ..Default::default() },
            klipp_timeline::LayerFx {
                loops: vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape }],
                edge_fade: if faded { edges } else { EdgeFade::NONE },
                ..Default::default()
            },
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
    let fade = ScreenFade::new(edges, Some((RampAxis::Vertical, focus))).unwrap();
    let (w, h) = (320usize, 180usize);
    let mut r = TitleRenderer::new();
    for raster in [false, true] {
        for t in [0.8f32, 2.0, 3.2] {
            let faded_td = roll(true, raster);
            if !raster {
                let frame = r.frame_with_window(&faded_td, w as u32, h as u32, None, t, Some(4.0));
                assert!(matches!(frame, klipp_title::TitleFrame::Placed { shape: Some(_), .. }), "a faded roll is still the resting render placed");
            }
            let plain = r.render_with_window(&roll(false, raster), w as u32, h as u32, None, t, Some(4.0));
            let faded = r.render_with_window(&faded_td, w as u32, h as u32, None, t, Some(4.0));
            let mut lit = 0;
            for (i, (p, s)) in plain.chunks_exact(4).zip(faded.chunks_exact(4)).enumerate() {
                let (x, y) = (i % w, i / w);
                let want = p[3] as f32 * fade.alpha_at((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                assert!((s[3] as f32 - want).abs() <= 1.0, "raster {raster}, t {t}: ({x}, {y}) alpha {} where {want:.1}", s[3]);
                lit += (p[3] > 0) as usize;
            }
            assert!(lit > 100, "raster {raster}, t {t}: the credits are on screen");
        }
    }
}

/// #592: an edge fade is the layer's, wherever it is and however many
/// layers the title has. A still title of two layers, one fading into
/// the left edge: that layer fades there, the other is untouched.
#[test]
fn a_still_layer_fades_into_its_edge_and_its_neighbour_does_not() {
    use klipp_core::title_motion::{EdgeFade, ScreenFade};
    let text = |s: &str, x: f32, left: f32| {
        let mut td = TitleData::single_text_fx(
            TextBox { text: s.into(), font_size: 40.0, ..Default::default() },
            klipp_timeline::LayerFx { edge_fade: EdgeFade { left, ..EdgeFade::NONE }, ..Default::default() },
        );
        td.objects[0].transform.center.x = x;
        td
    };
    let (w, h) = (320usize, 180usize);
    let mut r = TitleRenderer::new();
    // One layer: faded where it is placed.
    let plain = r.render_with_window(&text("Lower third", 0.35, 0.0), w as u32, h as u32, None, 1.0, Some(4.0));
    let faded = r.render_with_window(&text("Lower third", 0.35, 0.4), w as u32, h as u32, None, 1.0, Some(4.0));
    let fade = ScreenFade::new(EdgeFade { left: 0.4, ..EdgeFade::NONE }, None).unwrap();
    for (i, (p, s)) in plain.chunks_exact(4).zip(faded.chunks_exact(4)).enumerate() {
        let want = p[3] as f32 * fade.alpha_at(((i % w) as f32 + 0.5) / w as f32, 0.5);
        assert!((s[3] as f32 - want).abs() <= 1.0, "one layer at ({}, {}): {} where {want:.1}", i % w, i / w, s[3]);
    }
    // Two layers: the faded one fades, the other keeps every pixel.
    let two = |left: f32| {
        let mut td = text("Lower third", 0.35, left);
        let mut other = text("Name", 0.75, 0.0).objects.remove(0);
        other.id = klipp_timeline::TitleObjectId(td.objects.iter().map(|o| o.id.0).max().unwrap() + 1);
        other.transform.center.y = 0.8;
        td.objects.push(other);
        td
    };
    let plain = r.render_with_window(&two(0.0), w as u32, h as u32, None, 1.0, Some(4.0));
    let faded = r.render_with_window(&two(0.4), w as u32, h as u32, None, 1.0, Some(4.0));
    let mut faded_px = 0;
    for (i, (p, s)) in plain.chunks_exact(4).zip(faded.chunks_exact(4)).enumerate() {
        let (x, y) = (i % w, i / w);
        if y < h * 13 / 20 {
            // The faded layer's rows: its alpha times the fade there.
            let want = p[3] as f32 * fade.alpha_at((x as f32 + 0.5) / w as f32, 0.5);
            assert!((s[3] as f32 - want).abs() <= 1.0, "two layers, the faded one at ({x}, {y}): {} where {want:.1}", s[3]);
            faded_px += (s[3] < p[3]) as usize;
        } else if y > h * 7 / 10 {
            assert_eq!(p, s, "two layers, the other one at ({x}, {y}) is untouched");
        }
    }
    assert!(faded_px > 0, "the faded layer does fade");
}

/// #592: a focus band gone soft blurs the roll away from the band, on the
/// raster path too (forced here by a constant opacity key): towards the
/// frame's top the credits spread into the gaps between their lines, in
/// the band they are as sharp as without it. The placed path hands the
/// blur to the compositor (`last_focus_blur`).
#[test]
fn a_soft_focus_band_blurs_the_roll_away_from_the_band() {
    use klipp_core::title_motion::{FocusBand, RollShape};
    use klipp_timeline::{ScrollDirection, TitleLoop};
    let roll = |blur: f32, raster: bool| {
        let shape = RollShape { focus: Some(FocusBand { band: 0.3, edge_opacity: 1.0, blur }) };
        let mut td = TitleData::single_text_fx(
            TextBox { text: "One\n\nTwo\n\nThree\n\nFour\n\nFive\n\nSix".into(), font_size: 120.0, ..Default::default() },
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
    let (w, h) = (320usize, 180usize);
    let mut r = TitleRenderer::new();
    let _ = r.frame_with_window(&roll(6.0, false), w as u32, h as u32, None, 2.0, Some(4.0));
    let (radius, _, band) = r.last_focus_blur().expect("the placed frame asks for the focus blur");
    assert!((radius - 6.0 / 6.0).abs() < 1e-3 && (band - 0.3).abs() < 1e-6, "radius at this frame's scale (180 of 1080) and the band: {radius} {band}");
    let sharp = r.render_with_window(&roll(0.0, true), w as u32, h as u32, None, 2.0, Some(4.0));
    let soft = r.render_with_window(&roll(12.0, true), w as u32, h as u32, None, 2.0, Some(4.0));
    let alpha_rows = |px: &[u8], y0: usize, y1: usize| (y0..y1).map(|y| (0..w).filter(|x| px[(y * w + x) * 4 + 3] > 8).count()).sum::<usize>();
    // Towards the top the soft frame covers more pixels (spread); in the
    // band it is the sharp one.
    assert!(alpha_rows(&soft, 0, 30) > alpha_rows(&sharp, 0, 30), "soft at the top: {} vs {}", alpha_rows(&soft, 0, 30), alpha_rows(&sharp, 0, 30));
    let band_rows = (h * 45 / 100)..(h * 55 / 100);
    for y in band_rows {
        for x in 0..w {
            let i = (y * w + x) * 4;
            assert_eq!(soft[i + 3], sharp[i + 3], "sharp in the band at ({x}, {y})");
        }
    }
}
