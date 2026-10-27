//! Animated captions (#2710) as drawn: at the second word's moment,
//! highlight colours that word alone, karaoke the words said so far, pop
//! makes that word larger, and typewriter draws only the words said.

use klipp_timeline::caption_motion::{animated_caption, CaptionScreen, TimedWord};
use klipp_timeline::{CaptionAnimation, CaptionMotion, TitleData};
use klipp_title::TitleRenderer;

const W: u32 = 960;
const H: u32 = 540;
const RED: [u8; 4] = [255, 0, 0, 255];

fn screen() -> CaptionScreen {
    let words = [("ONE", 0.0), ("TWO", 0.5), ("SIX", 1.0)].map(|(t, s)| TimedWord { text: t.into(), start: s, end: s + 0.4 }).to_vec();
    CaptionScreen { words, start: 0.0, end: 1.5 }
}

fn caption(style: CaptionMotion) -> TitleData {
    animated_caption(&screen(), &CaptionAnimation { style, active_color: RED, outline: 0.0, ..Default::default() }, W, H)
}

fn draw(td: &TitleData, t: f32) -> Vec<u8> {
    TitleRenderer::new().render_with_window(td, W, H, None, t, Some(1.5))
}

/// The columns each word's ink takes, left to right, from the caption
/// drawn with its animator off: runs of inked columns split at the two
/// widest gaps.
fn word_spans() -> Vec<(u32, u32)> {
    let mut td = caption(CaptionMotion::Highlight);
    td.objects[0].fx_mut().animators.clear();
    let px = draw(&td, 0.7);
    let inked: Vec<u32> = (0..W).filter(|&x| (0..H).any(|y| px[((y * W + x) * 4 + 3) as usize] > 128)).collect();
    let mut gaps: Vec<(u32, usize)> = inked.windows(2).enumerate().map(|(i, p)| (p[1] - p[0], i)).collect();
    gaps.sort_by_key(|g| std::cmp::Reverse(g.0));
    let mut cuts: Vec<usize> = gaps[..2].iter().map(|g| g.1).collect();
    cuts.sort();
    vec![(inked[0], inked[cuts[0]]), (inked[cuts[0] + 1], inked[cuts[1]]), (inked[cuts[1] + 1], *inked.last().unwrap())]
}

/// Of each word, how many pixels are red, and how many are inked at all.
fn per_word(px: &[u8], spans: &[(u32, u32)]) -> Vec<(usize, usize)> {
    spans.iter().map(|&(x0, x1)| {
        let mut red = 0;
        let mut ink = 0;
        for y in 0..H {
            for x in x0..=x1 {
                let p = &px[((y * W + x) * 4) as usize..((y * W + x) * 4 + 4) as usize];
                if p[3] > 128 {
                    ink += 1;
                    if p[0] > 200 && p[1] < 60 && p[2] < 60 {
                        red += 1;
                    }
                }
            }
        }
        (red, ink)
    }).collect()
}

#[test]
fn the_spoken_word_is_shown_at_its_time() {
    let spans = word_spans();
    let red = |style| per_word(&draw(&caption(style), 0.7), &spans).iter().map(|&(r, i)| r * 2 > i).collect::<Vec<_>>();
    assert_eq!(red(CaptionMotion::Highlight), [false, true, false], "highlight: the second word alone");
    assert_eq!(red(CaptionMotion::Karaoke), [true, true, false], "karaoke: the words said so far");
    assert_eq!(red(CaptionMotion::Pop), [false, true, false], "pop: the second word coloured");

    // Pop: that word drawn larger than when it is not being said.
    let ink = |td: &TitleData, t: f32| per_word(&draw(td, t), &spans)[1].1;
    let pop = caption(CaptionMotion::Pop);
    assert!(ink(&pop, 0.7) as f32 > ink(&pop, 0.2) as f32 * 1.2, "pop: {} vs {}", ink(&pop, 0.7), ink(&pop, 0.2));

    // Typewriter: before the third word, it is not drawn.
    let typed = per_word(&draw(&caption(CaptionMotion::Typewriter), 0.7), &spans);
    assert!(typed[0].1 > 100 && typed[1].1 > 100 && typed[2].1 == 0, "{typed:?}");
}
