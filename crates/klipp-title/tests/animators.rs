//! A layer's animators drawn (#1510): part of the text posed unit by
//! unit, in any order, the same on every render of a seed.

use klipp_core::{Interpolation, Keyframe};
use klipp_timeline::{Animator, AnimatorProps, LayerFx, RangeSelector, SelectOrder, SelectUnit, TextBox, TitleData};
use klipp_title::{TitleFrame, TitleRenderer};

const W: u32 = 1280;
const H: u32 = 720;
const TEXT: &str = "one MIDDLE two";

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn title(animators: Vec<Animator>) -> TitleData {
    TitleData::single_text_fx(TextBox { text: TEXT.into(), font_size: 80.0, ..Default::default() }, LayerFx { animators, ..Default::default() })
}

/// The middle word's letters turned a quarter, in an order shuffled by
/// `seed`: every letter starts turned and, as the range's start sweeps
/// over the two seconds, each turns back in its place in the order. The
/// range is limited to the middle word, so the other words never move.
fn turn_the_middle(seed: u32) -> Animator {
    Animator {
        selector: RangeSelector { order: SelectOrder::Random { seed }, start_keyframes: vec![key(0.0, 0.0), key(2.0, 1.0)], ..Default::default() },
        within: Some(RangeSelector { start: 3.0 / 12.0, end: 9.0 / 12.0, ..Default::default() }),
        props: AnimatorProps { rotation: 90.0, ..Default::default() },
        ..Default::default()
    }
}

fn glyph_frame(r: &mut TitleRenderer, td: &TitleData, t: f32) -> Vec<klipp_title::GlyphPose> {
    match r.frame_with_window(td, W, H, None, t, Some(4.0)) {
        TitleFrame::Glyphs { poses, .. } => poses,
        _ => panic!("at t={t} turned letters are the resting glyphs posed"),
    }
}

/// The issue's case: the middle word only, each letter turning in a
/// random order, the same on every render of the seed.
#[test]
fn the_middle_word_turns_letter_by_letter_in_a_seeded_order() {
    let td = title(vec![turn_the_middle(7)]);
    let still = title(Vec::new());
    let mut r = TitleRenderer::new();
    let parents: Vec<char> = TEXT.chars().collect();
    // Which glyph is a letter of the middle word, by its character.
    let middle: Vec<bool> = parents.iter().map(|c| c.is_uppercase()).collect();
    let ranks = klipp_core::title_motion::select_ranks(SelectOrder::Random { seed: 7 }, 12);
    let char_unit = klipp_core::title_motion::units_of(&parents, klipp_timeline::StaggerUnit::Char);
    let mut turned_back_at = vec![f32::MAX; parents.len()];
    for step in 0..=40 {
        let t = step as f32 * 0.05;
        let poses = glyph_frame(&mut r, &td, t);
        assert_eq!(poses.len(), parents.len());
        for (g, p) in poses.iter().enumerate() {
            if !middle[g] {
                assert_eq!(p.rotation, 0.0, "at t={t} {:?} never turns", parents[g]);
            } else if p.rotation == 0.0 && turned_back_at[g] == f32::MAX {
                turned_back_at[g] = t;
            }
        }
    }
    // Each middle letter turned back, in the order the seed shuffled.
    let mut letters: Vec<usize> = (0..parents.len()).filter(|&g| middle[g]).collect();
    assert!(letters.iter().all(|&g| turned_back_at[g] < f32::MAX), "{turned_back_at:?}");
    letters.sort_by_key(|&g| ranks[char_unit[g]]);
    assert!(letters.windows(2).all(|p| turned_back_at[p[0]] <= turned_back_at[p[1]]), "by rank: {letters:?} {turned_back_at:?}");
    let in_text_order: Vec<usize> = (0..parents.len()).filter(|&g| middle[g]).collect();
    assert_ne!(letters, in_text_order, "not left to right");
    let mut reversed = in_text_order.clone();
    reversed.reverse();
    assert_ne!(letters, reversed, "nor right to left");

    // Halfway, only the middle word's pixels differ from the still text.
    let half = r.render_with_window(&td, W, H, None, 1.0, Some(4.0));
    let rest = r.render_with_window(&still, W, H, None, 1.0, Some(4.0));
    assert_ne!(half, rest, "letters are turned halfway");
    let boxes = TitleRenderer::new().render_layout(&still, W, H).glyph_boxes;
    let (x0, x1) = (boxes[4][0] - 40, boxes[9][2] + 40);
    for (i, (a, b)) in half.chunks_exact(4).zip(rest.chunks_exact(4)).enumerate() {
        let x = (i % W as usize) as i32;
        if a != b {
            assert!(x >= x0 && x < x1, "pixel at x {x} changed outside the middle word ({x0}..{x1})");
        }
    }
    // The same seed draws the same frame every time; another seed, not.
    let again = TitleRenderer::new().render_with_window(&td, W, H, None, 1.0, Some(4.0));
    assert_eq!(half, again);
    let other = TitleRenderer::new().render_with_window(&title(vec![turn_the_middle(8)]), W, H, None, 1.0, Some(4.0));
    assert_ne!(half, other);
    // Done, the text is at rest: the still text's frame.
    assert_eq!(r.render_with_window(&td, W, H, None, 2.5, Some(4.0)), r.render_with_window(&still, W, H, None, 2.5, Some(4.0)));
}

/// A colour (or a blur) is drawn by rastering the layer, a turn by
/// posing its glyphs: the coloured letters take the colour, the others
/// keep theirs.
#[test]
fn a_colour_animator_rasters_its_layer_and_colours_its_letters() {
    let colour = Animator {
        selector: RangeSelector { unit: SelectUnit::Word, start: 1.0 / 3.0, end: 2.0 / 3.0, ..Default::default() },
        props: AnimatorProps { colour: Some([0, 0, 255, 255]), ..Default::default() },
        ..Default::default()
    };
    let td = title(vec![colour]);
    let mut r = TitleRenderer::new();
    assert!(matches!(r.frame_with_window(&td, W, H, None, 1.0, Some(4.0)), TitleFrame::Rendered { .. }));
    let px = r.render_with_window(&td, W, H, None, 1.0, Some(4.0));
    let boxes = TitleRenderer::new().render_layout(&title(Vec::new()), W, H).glyph_boxes;
    let solid_in = |b: [i32; 4]| {
        (b[1]..b[3]).flat_map(|y| (b[0]..b[2]).map(move |x| (x, y))).map(|(x, y)| &px[((y as u32 * W + x as u32) * 4) as usize..][..4]).find(|p| p[3] == 255).map(|p| [p[0], p[1], p[2]])
    };
    assert_eq!(solid_in(boxes[5]), Some([0, 0, 255]), "a middle letter is blue");
    assert_eq!(solid_in(boxes[0]), Some([255, 255, 255]), "the first word keeps its colour");
    let spin = Animator { props: AnimatorProps { rotation: 30.0, ..Default::default() }, ..Default::default() };
    assert!(matches!(r.frame_with_window(&title(vec![spin]), W, H, None, 1.0, Some(4.0)), TitleFrame::Glyphs { .. }));
}

/// Animators all switched off draw as none at all.
#[test]
fn switched_off_animators_draw_as_none() {
    let off = Animator { enabled: false, ..turn_the_middle(7) };
    let mut r = TitleRenderer::new();
    for t in [0.0f32, 1.0] {
        assert_eq!(r.render_with_window(&title(vec![off.clone()]), W, H, None, t, Some(4.0)), r.render_with_window(&title(Vec::new()), W, H, None, t, Some(4.0)));
    }
    assert!(matches!(r.frame_with_window(&title(vec![off]), W, H, None, 1.0, Some(4.0)), TitleFrame::Placed { .. }));
}

/// A title of two layers is rastered whole, and an animator's turn is
/// drawn there too: a tall "I" turned a quarter lies on its side, the
/// other layer's letter untouched.
#[test]
fn a_rastered_layer_turns_its_letters_too() {
    use klipp_timeline::{default_text_transform, TitleObjectContent, TitlePosition};
    let layers = |turn: bool| {
        let mut td = TitleData::single_text(TextBox::default());
        td.objects.clear();
        for (text, x, animators) in [("I", 0.3, if turn { vec![Animator { props: AnimatorProps { rotation: 90.0, ..Default::default() }, ..Default::default() }] } else { Vec::new() }), ("x", 0.7, Vec::new())] {
            let mut at = default_text_transform();
            at.center = TitlePosition { x, y: 0.5 };
            let id = td.add_object(TitleObjectContent::TextBox(TextBox { text: text.into(), font_size: 160.0, ..Default::default() }), at);
            td.object_mut(id).unwrap().fx = Some(LayerFx { animators, ..Default::default() });
        }
        td
    };
    let mut r = TitleRenderer::new();
    assert!(matches!(r.frame_with_window(&layers(true), W, H, None, 1.0, Some(4.0)), TitleFrame::Rendered { .. }));
    // The inked box of the frame's left or right half: (width, height).
    let ink = |px: &[u8], left: bool| {
        let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0, u32::MAX, 0);
        for (i, p) in px.chunks_exact(4).enumerate() {
            let (x, y) = (i as u32 % W, i as u32 / W);
            if p[3] > 128 && (x < W / 2) == left {
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
        (x1 - x0, y1 - y0)
    };
    let (turned, still) = (r.render_with_window(&layers(true), W, H, None, 1.0, Some(4.0)), r.render_with_window(&layers(false), W, H, None, 1.0, Some(4.0)));
    let (tw, th) = ink(&turned, true);
    let (sw, sh) = ink(&still, true);
    assert!(sh > sw * 2 && tw > th * 2, "the I stood {sw}x{sh} and lies {tw}x{th}");
    assert!(tw.abs_diff(sh) <= 3 && th.abs_diff(sw) <= 3, "turned, not stretched: {sw}x{sh} to {tw}x{th}");
    assert_eq!(ink(&turned, false), ink(&still, false), "the other layer is untouched");
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// A title without animators draws as it did before them, byte for
/// byte, still and through staggered, whole and blurred motions: the
/// hashes were taken with the renderer as it stood without them, and
/// the same frames on that renderer (#1510).
#[test]
fn a_title_without_animators_draws_as_before() {
    use klipp_timeline::{Easing, Motion, MotionKind, StaggerUnit, TitleMotion};
    let mut got = Vec::new();
    for motion in [
        TitleMotion::default(),
        TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0).stagger(StaggerUnit::Char, 0.05)),
        TitleMotion::enter(Motion::new(MotionKind::ScaleUp).duration(1.0).easing(Easing::OutBack).stagger(StaggerUnit::Word, 0.1)),
        TitleMotion::enter(Motion::new(MotionKind::FocusIn).duration(1.0).stagger(StaggerUnit::Char, 0.05)),
    ] {
        let td = TitleData::single_text_fx(TextBox { text: "one MIDDLE two\nline".into(), font_size: 70.0, ..Default::default() }, LayerFx { motion, ..Default::default() });
        let mut r = TitleRenderer::new();
        for t in [0.3f32, 0.6] {
            got.push(fnv(&r.render_with_window(&td, 960, 540, None, t, Some(4.0))));
        }
    }
    assert_eq!(got, PINNED);
}

/// The frames above, still twice and at 0.3 s and 0.6 s of each motion,
/// on the renderer before animators and on this one. The staggered
/// frames moved when posed glyphs stopped being rounded to whole levels
/// before the frame's own rounding (#190): measured against the frames
/// before, at most 1 level premultiplied, on 5 to 17 pixels of each.
/// The focus-in frames grew where the blur reaches past the words'
/// resting box, which their raster had cut it at: 646 and 848 pixels,
/// each one empty before, the rest the same byte for byte.
const PINNED: [u64; 8] = [
    8183618076000127518, 8183618076000127518,
    7048149134191800083, 16027533902423877051,
    3315793853365973377, 9774031115400215890,
    10981212073083227338, 10630925023139708698,
];
