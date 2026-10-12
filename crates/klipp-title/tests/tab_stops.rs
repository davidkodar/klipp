//! Tab stops (#1721): the text after a tab starts, centres, ends or puts
//! its decimal point on its stop, measured from the start indent; past
//! the stops set, one falls every two ems; a hanging first line's tab
//! goes to the indent, where the lines under it start.
use klipp_timeline::{Paragraph, TabAlign, TabStop, TextAlignment, TextBox, TitleData, TitlePosition};
use klipp_title::{CharPosition, TitleRenderer};

const W: u32 = 1920;
const H: u32 = 1080;

fn boxed(text: &str, p: Paragraph) -> TitleData {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 40.0, alignment: TextAlignment::Left, paragraph: p, ..Default::default() });
    td.objects[0].transform.size = TitlePosition { x: 0.6, y: 0.9 };
    td
}

fn chars(td: &TitleData) -> Vec<CharPosition> {
    TitleRenderer::new().compute_char_positions(td, W, H)
}

/// The caret position of the `n`th `c` in the text, and its advance.
fn at(td: &TitleData, cs: &[CharPosition], c: char, n: usize) -> (f32, f32) {
    let text = &td.primary_text().unwrap().text;
    let byte = text.match_indices(c).nth(n).unwrap_or_else(|| panic!("no {c} {n}")).0;
    let p = cs.iter().find(|p| p.byte_offset == byte).unwrap_or_else(|| panic!("no caret at {byte}"));
    (p.x, p.advance)
}

fn stops(tabs: &[(f32, TabAlign)]) -> Paragraph {
    Paragraph { tabs: tabs.iter().map(|&(at, align)| TabStop { at, align }).collect(), ..Default::default() }
}

// Distances are taken between the same letter, so its side bearing
// falls out: a letter's caret is where its ink starts.

#[test]
fn text_after_a_tab_lands_on_its_stop() {
    let left = boxed("a\tX\tX", stops(&[(300.0, TabAlign::Left), (700.0, TabAlign::Left)]));
    let cs = chars(&left);
    let (x1, adv) = at(&left, &cs, 'X', 0);
    assert!((at(&left, &cs, 'X', 1).0 - x1 - 400.0).abs() < 1.0, "left stops 400 px apart");
    assert!((x1 - at(&left, &cs, 'a', 0).0 - 300.0).abs() < 4.0, "the first on 300 px");

    let right = boxed("a\tX\tXX", stops(&[(300.0, TabAlign::Left), (700.0, TabAlign::Right)]));
    let cs = chars(&right);
    let x1 = at(&right, &cs, 'X', 0).0;
    let last = at(&right, &cs, 'X', 2).0;
    // The stop takes the field's shaped width; the caret's advance is a
    // pixel or two off it.
    assert!((last + adv - x1 - 400.0).abs() < 3.0, "XX ends on the 700 px stop: {}", last + adv - x1);

    let centre = boxed("a\tX\tXX", stops(&[(300.0, TabAlign::Left), (700.0, TabAlign::Center)]));
    let cs = chars(&centre);
    let x1 = at(&centre, &cs, 'X', 0).0;
    let mid = at(&centre, &cs, 'X', 1).0;
    assert!((mid - x1 - (400.0 - adv)).abs() < 3.0, "XX is centred on the 700 px stop: {}", mid - x1);
}

#[test]
fn decimal_stops_line_the_points_up() {
    let td = boxed("a\t3.5\nbb\t123.25\nc\t7", stops(&[(500.0, TabAlign::Decimal)]));
    let cs = chars(&td);
    let p1 = at(&td, &cs, '.', 0).0;
    let p2 = at(&td, &cs, '.', 1).0;
    assert!((p1 - p2).abs() < 1.0, "the points line up: {p1} vs {p2}");
    assert!((p1 - at(&td, &cs, 'a', 0).0 - 500.0).abs() < 4.0, "on the stop: {}", p1 - at(&td, &cs, 'a', 0).0);
    let (seven, adv) = at(&td, &cs, '7', 0);
    assert!((seven + adv - p1).abs() < 4.0, "a number without a point ends on it: {} vs {p1}", seven + adv);
}

#[test]
fn past_the_stops_set_a_stop_falls_every_two_ems() {
    // 40 at 1080p is 40 px: a stop every 80 px.
    let td = boxed("\tX\tX", Paragraph::default());
    let cs = chars(&td);
    assert!((at(&td, &cs, 'X', 1).0 - at(&td, &cs, 'X', 0).0 - 80.0).abs() < 1.0);
    // A set stop at 100, then the next step past it, 160.
    let td = boxed("\tX\tX", stops(&[(100.0, TabAlign::Left)]));
    let cs = chars(&td);
    assert!((at(&td, &cs, 'X', 1).0 - at(&td, &cs, 'X', 0).0 - 60.0).abs() < 1.0, "{}", at(&td, &cs, 'X', 1).0 - at(&td, &cs, 'X', 0).0);
}

#[test]
fn a_hanging_list_item_lines_its_text_up_under_the_tab() {
    let item = "1.\tA list item long enough to run on to a second line under the first one";
    let p = Paragraph { indent_start: 60.0, indent_first: -60.0, ..Default::default() };
    let td = boxed(item, p);
    let cs = chars(&td);
    let number = at(&td, &cs, '1', 0).0;
    let text = at(&td, &cs, 'A', 0).0;
    assert!((text - number - 60.0).abs() < 1.5, "the item's text starts at the indent: {}", text - number);
    // The second line starts there too.
    let mut last_x = f32::MAX;
    let second = cs.iter().filter(|c| c.advance > 0.0).find(|c| {
        let back = c.x < last_x - 1.0 && last_x != f32::MAX;
        last_x = c.x;
        back
    });
    let second = second.expect("a second line");
    assert!((second.x - text).abs() < 3.0, "under the first line's text: {} vs {text}", second.x);
    // Nothing is drawn for the tab itself.
    let ink = TitleRenderer::new().render_layout(&td, W, H);
    assert!(ink.glyph_count > 0);
}

/// A tab draws nothing of its own: two letters either side of one are
/// the ink of two letters.
#[test]
fn a_tab_draws_nothing() {
    let mut r = TitleRenderer::new();
    let mut ink = |text: &str| r.render(&boxed(text, Paragraph::default()), W, H).chunks_exact(4).map(|p| p[3] as u64).sum::<u64>();
    let (one, two) = (ink("X"), ink("X\tX"));
    assert!(one > 0 && (two as f64 / one as f64 - 2.0).abs() < 0.02, "{two} vs 2 x {one}");
}
