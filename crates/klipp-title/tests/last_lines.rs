//! Fewer one-word last lines (#1721): a paragraph whose wrapped last
//! line has fewer words than the box asks for takes words from the line
//! above; a box that asks for none breaks as before.
use klipp_timeline::{Paragraph, TextAlignment, TextBox, TitleData, TitlePosition};
use klipp_title::{CharPosition, TitleRenderer};

const W: u32 = 1920;
const H: u32 = 1080;
const TEXT: &str = "A paragraph of plain words that wraps over a few lines and ends on its own";

fn boxed(text: &str, width: f32, min_last_words: u8) -> TitleData {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 48.0, alignment: TextAlignment::Left, paragraph: Paragraph { min_last_words, ..Default::default() }, ..Default::default() });
    td.objects[0].transform.size = TitlePosition { x: width, y: 0.9 };
    td
}

/// Each line's words, from the caret: a line starts where it steps back.
fn lines(td: &TitleData) -> Vec<Vec<String>> {
    let text = td.primary_text().unwrap().text.clone();
    let chars: Vec<CharPosition> = TitleRenderer::new().compute_char_positions(td, W, H);
    let mut out: Vec<String> = Vec::new();
    let mut last_x = f32::MAX;
    for c in &chars {
        // The caret past the text's end has no character.
        let Some(ch) = text.get(c.byte_offset..).and_then(|t| t.chars().next()) else { continue };
        if c.x < last_x - 1.0 && !ch.is_whitespace() {
            out.push(String::new());
        }
        if let Some(l) = out.last_mut() {
            l.push(ch);
        }
        if !ch.is_whitespace() {
            last_x = c.x;
        }
    }
    out.into_iter().map(|l| l.split_whitespace().map(str::to_string).collect()).collect()
}

#[test]
fn a_one_word_last_line_takes_a_word_from_the_line_above() {
    // A measure that leaves the paragraph's last word alone on its line.
    let width = (20..60).map(|w| w as f32 / 100.0).find(|&w| {
        let l = lines(&boxed(TEXT, w, 0));
        l.len() >= 3 && l.last().map(Vec::len) == Some(1)
    }).expect("some measure leaves one word last");
    let plain = lines(&boxed(TEXT, width, 0));
    let balanced = lines(&boxed(TEXT, width, 2));
    eprintln!("{plain:?}\n{balanced:?}");
    assert_eq!(balanced.last().unwrap().len(), 2, "two words on the last line");
    assert_eq!(balanced.len(), plain.len(), "on as many lines");
    assert_eq!(balanced[..balanced.len() - 2], plain[..plain.len() - 2], "the lines above it are as they were");
    let n = plain.len();
    assert_eq!(balanced[n - 2].len(), plain[n - 2].len() - 1, "the word came from the line above");
    assert_eq!(balanced.concat(), plain.concat(), "the same words in the same order");
}

#[test]
fn each_paragraph_is_balanced_and_a_box_asking_for_none_breaks_as_before() {
    let width = (20..60).map(|w| w as f32 / 100.0).find(|&w| lines(&boxed(TEXT, w, 0)).last().map(Vec::len) == Some(1)).unwrap();
    let two = format!("{TEXT}\n{TEXT}");
    let balanced = lines(&boxed(&two, width, 2));
    let per = lines(&boxed(TEXT, width, 2)).len();
    assert_eq!(balanced[per - 1].len(), 2, "the first paragraph's last line");
    assert_eq!(balanced.last().unwrap().len(), 2, "and the second's");
    let mut r = TitleRenderer::new();
    assert_eq!(r.render(&boxed(TEXT, width, 0), W, H), r.render(&boxed(TEXT, width, 1), W, H), "1 asks for nothing");
    let saved = serde_json::to_string(&boxed(TEXT, width, 0)).unwrap();
    assert!(!saved.contains("min_last_words"));
}
