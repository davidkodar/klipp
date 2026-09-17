//! Letter-spacing moves glyphs to the right; the layer's working area
//! has to reach as far as the last one, and no further than it needs.

use klipp_timeline::{TextAlignment, TextBox, TitleData, TitleShadow};
use klipp_title::TitleRenderer;

fn title(text: &str, tracking: f32, shadow: bool) -> TitleData {
    let mut tb = TextBox { text: text.into(), font_size: 40.0, tracking, alignment: TextAlignment::Left, ..Default::default() };
    if shadow {
        tb.shadows.push(TitleShadow {
            color: [0, 0, 0, 160],
            offset_x: 0.0,
            offset_y: 3.0,
            blur: 12.0,
            enabled: true,
            offset_x_keyframes: Vec::new(),
            offset_y_keyframes: Vec::new(),
            blur_keyframes: Vec::new(),
        });
    }
    let mut td = TitleData::single_text(tb);
    td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.05, y: 0.5 };
    td.objects[0].transform.anchor_norm = klipp_timeline::TitlePosition { x: 0.0, y: 0.5 };
    td
}

/// Opaque text pixels, and the rightmost column that has any.
fn ink(px: &[u8], w: usize) -> (usize, usize) {
    let mut n = 0;
    let mut right = 0;
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[3] > 200 && p[0] > 200 {
            n += 1;
            right = right.max(i % w);
        }
    }
    (n, right)
}

/// A long tracked line is all there: the same ink as untracked, its
/// last glyph further right by the tracking times the glyphs before it.
/// The padding that makes room for it was a constant 200 glyphs' worth
/// on every side, sixty times the pixels for a short role line.
#[test]
fn tracked_text_is_not_clipped_and_not_padded_by_the_mile() {
    let text = "the quick brown fox jumps over the lazy dog again";
    let (w, h) = (1920usize, 1080usize);
    let mut r = TitleRenderer::new();
    let plain = r.render_with_time(&title(text, 0.0, false), w as u32, h as u32, None, 0.0);
    let tracked = r.render_with_time(&title(text, 6.0, false), w as u32, h as u32, None, 0.0);
    let (n0, right0) = ink(&plain, w);
    let (n1, right1) = ink(&tracked, w);
    assert!(n0 > 1000, "the text is drawn: {n0}");
    assert!((n1 as f64 - n0 as f64).abs() / (n0 as f64) < 0.02, "the same ink, moved: {n0} then {n1}");
    let moved = right1 as f32 - right0 as f32;
    let expected = 6.0 * (text.chars().count() - 1) as f32;
    assert!((moved - expected).abs() < 8.0, "the last glyph moved {moved} px, expected about {expected}");

    // With a blurred shadow the whole layer is worked over, and the work
    // is the text's size, not a fixed mile: a tracked line costs about
    // what the same line untracked does. It cost nine times as much.
    let time = |r: &mut TitleRenderer, tracking: f32, text: &str| {
        let t0 = std::time::Instant::now();
        let _ = r.render_with_time(&title(text, tracking, true), w as u32, h as u32, None, 0.0);
        t0.elapsed().as_secs_f64() * 1000.0
    };
    let _ = time(&mut r, 0.0, "warm the font up");
    let untracked = time(&mut r, 0.0, "Creator of Omarchy");
    let tracked = time(&mut r, 3.0, "Creator of Omarchy.");
    assert!(tracked < untracked * 3.0 + 10.0, "tracked {tracked:.0} ms against {untracked:.0} ms untracked");
}
