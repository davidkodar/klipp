//! The heavy titles #1725 measures, shared by the `title_perf` example
//! and the frame-budget test: a turning 3D text, a 200-letter animator
//! with blur, and a blended, masked multi-layer title, over `SECS`.

use klipp_core::keyframe::upsert_keyframe;
use klipp_core::title_motion::{Animator, AnimatorProps, RangeSelector};
use klipp_timeline::{LayerFx, LayerMask, ObjectTransform, TextBox, TitleData, TitleObjectContent, TitlePosition};

pub const SECS: f32 = 4.0;

pub fn turning_3d() -> TitleData {
    let mut t3 = klipp_core::text3d::Text3d::default();
    upsert_keyframe(&mut t3.keys.rotate_y, 0.0, -40.0);
    upsert_keyframe(&mut t3.keys.rotate_y, SECS as f64, 40.0);
    let tb = TextBox { text: "KLIPP 3D".into(), font_size: 180.0, three_d: Some(t3), ..Default::default() };
    TitleData::single_text_fx(tb, LayerFx::default())
}

pub fn letters(blur: f32, keyed: bool) -> TitleData {
    let text: String = "The quick brown fox jumps over the lazy dog. ".repeat(5).chars().take(200).collect();
    let tb = TextBox { text, font_size: 48.0, ..Default::default() };
    let mut fx = LayerFx::default();
    let mut selector = RangeSelector::default();
    if keyed {
        upsert_keyframe(&mut selector.start_keyframes, 0.0, 0.0);
        upsert_keyframe(&mut selector.start_keyframes, SECS as f64, 1.0);
    }
    fx.animators.push(Animator { selector, props: AnimatorProps { dy: 0.6, blur, opacity: 0.0, ..Default::default() }, ..Default::default() });
    TitleData::single_text_fx(tb, fx)
}

pub fn letters_with_blur() -> TitleData {
    let text: String = "The quick brown fox jumps over the lazy dog. ".repeat(5).chars().take(200).collect();
    let tb = TextBox { text, font_size: 48.0, ..Default::default() };
    let mut fx = LayerFx::default();
    let mut selector = RangeSelector::default();
    upsert_keyframe(&mut selector.start_keyframes, 0.0, 0.0);
    upsert_keyframe(&mut selector.start_keyframes, SECS as f64, 1.0);
    fx.animators.push(Animator { selector, props: AnimatorProps { dy: 0.6, blur: 0.3, opacity: 0.0, ..Default::default() }, ..Default::default() });
    TitleData::single_text_fx(tb, fx)
}

pub fn blended_and_masked() -> TitleData {
    let mut td = TitleData::single_text_fx(TextBox { text: "BLEND".into(), font_size: 220.0, ..Default::default() }, LayerFx::default());
    let place = |x: f32, y: f32| ObjectTransform { center: TitlePosition { x, y }, size: TitlePosition { x: 0.6, y: 0.3 }, ..klipp_timeline::default_text_transform() };
    let matte = td.add_object(TitleObjectContent::TextBox(TextBox { text: "MASK".into(), font_size: 260.0, ..Default::default() }), place(0.5, 0.5));
    let screen = td.add_object(TitleObjectContent::TextBox(TextBox { text: "SCREEN".into(), font_size: 160.0, color: [255, 120, 0, 255], ..Default::default() }), place(0.45, 0.55));
    let multiply = td.add_object(TitleObjectContent::TextBox(TextBox { text: "MULTIPLY".into(), font_size: 140.0, color: [0, 160, 255, 255], ..Default::default() }), place(0.55, 0.45));
    for o in td.objects.iter_mut() {
        if o.id == screen {
            o.blend = klipp_core::BlendMode::Screen;
            o.mask = Some(LayerMask { source: matte, kind: Default::default() });
        }
        if o.id == multiply {
            o.blend = klipp_core::BlendMode::Multiply;
            upsert_keyframe(&mut o.transform.center_x_keyframes, 0.0, 0.4);
            upsert_keyframe(&mut o.transform.center_x_keyframes, SECS as f64, 0.6);
        }
    }
    td
}

/// The cases by name, as measured.
pub fn all() -> Vec<(&'static str, TitleData)> {
    vec![
        ("3D text turning", turning_3d()),
        ("200 letters, animator + blur", letters_with_blur()),
        ("200 letters, animator, no blur", letters(0.0, true)),
        ("200 letters, still blur", letters(0.3, false)),
        // The Title tab's widest animator blur, 2 em.
        ("200 letters, 2 em blur", letters(2.0, true)),
        ("blended + masked layers", blended_and_masked()),
    ]
}
