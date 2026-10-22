//! Spacing animation reuses coverage while preserving raster layout.
use klipp_core::{Interpolation, Keyframe};
use klipp_timeline::{
    Motion, MotionKind, StaggerUnit, TextAlignment, TextBox, TitleData, TitleMotion,
};
use klipp_title::{TitleFrame, TitleRenderer};
fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        interpolation: Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
        handles: Default::default(),
    }
}
fn title(text: &str, alignment: TextAlignment) -> TitleData {
    TitleData::single_text(TextBox {
        text: text.into(),
        font_size: 70.0,
        alignment,
        tracking: 2.0,
        tracking_keyframes: vec![key(0.0, 2.0), key(2.0, 16.0)],
        ..Default::default()
    })
}
#[test]
fn cached_spacing_matches_static_layout_and_scrubs_without_new_sprites() {
    for alignment in [
        TextAlignment::Left,
        TextAlignment::Center,
        TextAlignment::Right,
    ] {
        for text in ["Spaced text", "Two\nlines"] {
            let td = title(text, alignment);
            let mut r = TitleRenderer::new();
            let mut id = None;
            for t in [0.0, 0.7, 2.0, 1.0, 0.0] {
                let f = r.frame_with_window(&td, 960, 540, None, t, Some(4.0));
                let TitleFrame::Glyphs { sprite, .. } = f else {
                    panic!("expected glyphs for {text} {alignment:?} at {t}")
                };
                assert_eq!(
                    *id.get_or_insert(sprite.id),
                    sprite.id,
                    "one sprite across scrubbing"
                );
                let got = r.render_with_window(&td, 960, 540, None, t, Some(4.0));
                let mut still = td.clone();
                let tb = still.primary_text_mut().unwrap();
                *tb = tb.at(t as f64);
                let want =
                    TitleRenderer::new().render_with_window(&still, 960, 540, None, t, Some(4.0));
                let worst = got
                    .iter()
                    .zip(&want)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(worst <= 1, "{text} {alignment:?} t={t}: worst {worst}");
            }
        }
    }
}
#[test]
fn motion_and_tracking_share_coverage() {
    let mut td = title("Give it a little space.", TextAlignment::Center);
    td.objects[0].fx.as_mut().unwrap().motion = TitleMotion::both(
        Motion::new(MotionKind::MaskRise)
            .duration(0.9)
            .stagger(StaggerUnit::Word, 0.1),
        Motion::new(MotionKind::Fade).duration(0.6),
    );
    let mut r = TitleRenderer::new();
    for t in [0.0, 0.4, 1.5, 3.7, 4.0] {
        assert!(matches!(
            r.frame_with_window(&td, 960, 540, None, t, Some(4.0)),
            TitleFrame::Glyphs { .. }
        ));
        let got = r.render_with_window(&td, 960, 540, None, t, Some(4.0));
        let mut still = td.clone();
        let tb = still.primary_text_mut().unwrap();
        *tb = tb.at(t as f64);
        tb.font_size_keyframes = vec![key(0.0, tb.font_size)];
        let want = TitleRenderer::new().render_with_window(&still, 960, 540, None, t, Some(4.0));
        let worst = got
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 1, "motion t={t}: worst {worst}");
    }
}
#[test]
fn unsupported_layouts_keep_the_raster_route() {
    let base = title("Tracking", TextAlignment::Center);
    for case in 0..7 {
        let mut td = base.clone();
        let tb = td.primary_text_mut().unwrap();
        match case {
            0 => tb.underline = true,
            1 => tb.tracking = -40.0,
            2 => tb.alignment = TextAlignment::Justify,
            3 => td.objects[0].transform.center.x = -0.1,
            4 => {
                tb.style_runs = vec![klipp_timeline::StyleRun {
                    start: 0,
                    end: 3,
                    tracking: Some(8.0),
                    ..klipp_timeline::StyleRun::default_for_range(0, 3)
                }]
            }
            5 => tb.font_size_keyframes = vec![key(0.0, 70.0)],
            _ => tb.tracking_keyframes = vec![key(0.0, -40.0)],
        }
        assert!(
            matches!(
                TitleRenderer::new().frame_with_window(&td, 960, 540, None, 1.0, Some(4.0)),
                TitleFrame::Rendered { .. }
            ),
            "case {case}"
        );
    }
}

#[test]
fn edited_titles_invalidate_cached_coverage() {
    let mut td = title("Cache", TextAlignment::Center);
    let mut r = TitleRenderer::new();
    let id = |r: &mut TitleRenderer, td: &TitleData| match r.frame_with_window(
        td,
        960,
        540,
        None,
        1.0,
        Some(4.0),
    ) {
        TitleFrame::Glyphs { sprite, .. } => sprite.id,
        _ => panic!("expected glyphs"),
    };
    let first = id(&mut r, &td);
    td.primary_text_mut().unwrap().color = [255, 30, 40, 255];
    let colour = id(&mut r, &td);
    assert_ne!(first, colour);
    td.primary_text_mut().unwrap().text = "Edited".into();
    assert_ne!(colour, id(&mut r, &td));
    let encoded = serde_json::to_string(&td).unwrap();
    let loaded: TitleData = serde_json::from_str(&encoded).unwrap();
    assert_eq!(id(&mut r, &td), id(&mut r, &loaded));
    r.invalidate_cache();
    assert_ne!(id(&mut r, &td), colour);
}
