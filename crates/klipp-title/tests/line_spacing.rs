//! Line spacing moves whole lines: every glyph of a line keeps one
//! baseline whatever its height, and the lines are evenly spaced, a
//! multiple of the font's own line height apart (#1676).

use klipp_timeline::{TextAlignment, TextBox, TitleData};
use klipp_title::TitleRenderer;

/// Lines whose letters reach different heights: a capital, an
/// ascender and an x-height letter, all sitting on the baseline.
const LINES: &str = "Hlo Hlo\nxHl\nHxxl\nlxH";

fn title(text: &str, spacing: f32, columns: u8) -> TitleData {
    let tb = TextBox { text: text.into(), font_size: 40.0, line_spacing: spacing, alignment: TextAlignment::Left, columns, ..Default::default() };
    TitleData::single_text(tb)
}

/// Each line's glyph bottoms, line by line, in frame pixels: the
/// glyphs of `LINES` all sit on the baseline, so a bottom is it.
fn baselines(td: &TitleData) -> Vec<Vec<i32>> {
    let layout = TitleRenderer::new().render_layout(td, 1920, 1080);
    let mut glyphs = layout.glyph_parents.iter().zip(&layout.glyph_boxes).filter(|(c, _)| !c.is_whitespace());
    LINES.lines().map(|line| line.chars().filter(|c| !c.is_whitespace()).map(|_| glyphs.next().expect("a glyph per character").1[3]).collect()).collect()
}

/// The first glyph's baseline of each line.
fn line_baselines(td: &TitleData) -> Vec<i32> {
    baselines(td).iter().map(|l| l[0]).collect()
}

#[test]
fn a_line_keeps_one_baseline_and_lines_are_evenly_spaced() {
    let natural = line_baselines(&title(LINES, 1.0, 1));
    let pitch = natural[1] - natural[0];
    assert!(pitch > 40, "lines a line height apart: {natural:?}");
    for spacing in [0.8, 1.2, 1.5, 2.0] {
        let lines = baselines(&title(LINES, spacing, 1));
        for line in &lines {
            let (lo, hi) = (line.iter().min().unwrap(), line.iter().max().unwrap());
            assert!(hi - lo <= 1, "spacing {spacing}: one baseline per line: {lines:?}");
        }
        let at: Vec<i32> = lines.iter().map(|l| l[0]).collect();
        let want = pitch as f32 * spacing;
        for pair in at.windows(2) {
            let gap = (pair[1] - pair[0]) as f32;
            assert!((gap - want).abs() <= 2.0, "spacing {spacing}: lines {want} apart, got {at:?}");
        }
    }
}

/// Two boxes alike but for their letters set their rows the same
/// distances apart. (The block as a whole is placed by its ink.)
#[test]
fn rows_do_not_depend_on_the_letters() {
    let gaps = |text: &str| -> Vec<i32> { line_baselines(&title(text, 1.3, 1)).windows(2).map(|p| p[1] - p[0]).collect() };
    let (a, b) = (gaps(LINES), gaps(&LINES.replace('l', "o").replace('H', "x")));
    assert!(a.iter().zip(&b).all(|(a, b)| a.abs_diff(*b) <= 1), "{a:?} {b:?}");
}

/// In columns, each column's lines are spaced from its own top.
#[test]
fn columns_space_their_own_lines() {
    let text = "Hl\nxH\nlx\nHl\nxH\nlx";
    let rows = |spacing: f32| -> Vec<i32> {
        let layout = TitleRenderer::new().render_layout(&title(text, spacing, 2), 1920, 1080);
        let glyphs: Vec<&[i32; 4]> = layout.glyph_parents.iter().zip(&layout.glyph_boxes).filter(|(c, _)| !c.is_whitespace()).map(|(_, b)| b).collect();
        glyphs.iter().step_by(2).map(|b| b[3]).collect()
    };
    let (one, wide) = (rows(1.0), rows(1.5));
    assert_eq!(one.len(), 6);
    let pitch = (one[1] - one[0]) as f32;
    assert_eq!(wide[3], wide[0], "the second column starts at the top too: {wide:?}");
    for (k, &y) in wide.iter().enumerate() {
        let want = wide[0] as f32 + (k % 3) as f32 * pitch * 1.5;
        assert!((y as f32 - want).abs() <= 2.0, "row {k}: {wide:?} vs {one:?}");
    }
}
