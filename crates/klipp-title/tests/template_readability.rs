//! Measured sweep of the built-in title templates' readability (#2734):
//! each one, as it rests, in 16:9, 9:16 and 1:1, over a bright, a dark and
//! a busy frame. Measured per text layer: the contrast (WCAG ratio) of its
//! glyphs against what is round them as composited; that every text stays
//! inside title-safe (5 % in from each edge); and the smallest text's size
//! (in pixels of a 1080-line frame). Report: `target/sweep/templates.txt`.

use klipp_testing::sweep::Sweep;
use klipp_timeline::title_library::TITLES;
use klipp_timeline::{TitleData, TitleObjectContent};
use klipp_title::TitleRenderer;

/// Items known to fail, and the issue that holds each: generated from the
/// sweep's first run, each to come off as #2734's work fixes it.
const KNOWN: &[(&str, u32)] = &[
    ("Clean Line 16:9 bright", 2734),
    ("Clean Line 16:9 busy", 2734),
    ("Clean Line 9:16 bright", 2734),
    ("Clean Line 9:16 busy", 2734),
    ("Clean Line 1:1 bright", 2734),
    ("Clean Line 1:1 busy", 2734),
    ("Accent Block 16:9 bright", 2734),
    ("Accent Block 16:9 busy", 2734),
    ("Accent Block 9:16 bright", 2734),
    ("Accent Block 9:16 busy", 2734),
    ("Accent Block 1:1 bright", 2734),
    ("Accent Block 1:1 busy", 2734),
    ("Pill 16:9 bright", 2734),
    ("Pill 16:9 busy", 2734),
    ("Pill 9:16 bright", 2734),
    ("Pill 9:16 busy", 2734),
    ("Pill 1:1 bright", 2734),
    ("Pill 1:1 busy", 2734),
    ("Stacked 16:9 bright", 2734),
    ("Stacked 16:9 busy", 2734),
    ("Stacked 9:16 bright", 2734),
    ("Stacked 9:16 busy", 2734),
    ("Stacked 1:1 bright", 2734),
    ("Stacked 1:1 busy", 2734),
    ("Documentary 16:9 bright", 2734),
    ("Documentary 16:9 busy", 2734),
    ("Documentary 9:16 bright", 2734),
    ("Documentary 9:16 busy", 2734),
    ("Documentary 1:1 bright", 2734),
    ("Documentary 1:1 busy", 2734),
    ("Location 16:9 bright", 2734),
    ("Location 16:9 busy", 2734),
    ("Location 9:16 bright", 2734),
    ("Location 9:16 busy", 2734),
    ("Location 1:1 bright", 2734),
    ("Location 1:1 busy", 2734),
    ("Cinematic 16:9 bright", 2734),
    ("Cinematic 9:16 bright", 2734),
    ("Cinematic 1:1 bright", 2734),
    ("Impact 16:9 bright", 2734),
    ("Impact 16:9 busy", 2734),
    ("Impact 9:16 bright", 2734),
    ("Impact 9:16 busy", 2734),
    ("Impact 1:1 bright", 2734),
    ("Impact 1:1 busy", 2734),
    ("Editorial 16:9 bright", 2734),
    ("Editorial 16:9 busy", 2734),
    ("Editorial 9:16 bright", 2734),
    ("Editorial 9:16 busy", 2734),
    ("Editorial 1:1 bright", 2734),
    ("Editorial 1:1 busy", 2734),
    ("Minimal 16:9 bright", 2734),
    ("Minimal 16:9 busy", 2734),
    ("Minimal 9:16 bright", 2734),
    ("Minimal 9:16 busy", 2734),
    ("Minimal 1:1 bright", 2734),
    ("Minimal 1:1 busy", 2734),
    ("Chapter 16:9 bright", 2734),
    ("Chapter 16:9 busy", 2734),
    ("Chapter 9:16 bright", 2734),
    ("Chapter 9:16 busy", 2734),
    ("Chapter 1:1 bright", 2734),
    ("Chapter 1:1 busy", 2734),
    ("Chapter Number 16:9 bright", 2734),
    ("Chapter Number 16:9 busy", 2734),
    ("Chapter Number 9:16 bright", 2734),
    ("Chapter Number 9:16 busy", 2734),
    ("Chapter Number 1:1 bright", 2734),
    ("Chapter Number 1:1 busy", 2734),
    ("Statement 16:9 bright", 2734),
    ("Statement 16:9 busy", 2734),
    ("Statement 9:16 bright", 2734),
    ("Statement 9:16 busy", 2734),
    ("Statement 1:1 bright", 2734),
    ("Statement 1:1 busy", 2734),
    ("Quote 16:9 bright", 2734),
    ("Quote 16:9 busy", 2734),
    ("Quote 9:16 bright", 2734),
    ("Quote 9:16 busy", 2734),
    ("Quote 1:1 bright", 2734),
    ("Quote 1:1 busy", 2734),
    ("Subtitle 16:9 bright", 2734),
    ("Subtitle 9:16 bright", 2734),
    ("Subtitle 1:1 bright", 2734),
    ("Callout 16:9 bright", 2734),
    ("Callout 9:16 bright", 2734),
    ("Callout 1:1 bright", 2734),
    ("Credits Roll 16:9 bright", 2734),
    ("Credits Roll 16:9 busy", 2734),
    ("Credits Roll 9:16 bright", 2734),
    ("Credits Roll 9:16 busy", 2734),
    ("Credits Roll 1:1 bright", 2734),
    ("Credits Roll 1:1 busy", 2734),
    ("End Credits 16:9 bright", 2734),
    ("End Credits 16:9 busy", 2734),
    ("End Credits 9:16 bright", 2734),
    ("End Credits 9:16 busy", 2734),
    ("End Credits 1:1 bright", 2734),
    ("End Credits 1:1 busy", 2734),
    ("End Card 16:9 bright", 2734),
    ("End Card 9:16 bright", 2734),
    ("End Card 1:1 bright", 2734),
    ("Cast 16:9 bright", 2734),
    ("Cast 16:9 busy", 2734),
    ("Cast 9:16 bright", 2734),
    ("Cast 9:16 busy", 2734),
    ("Cast 1:1 bright", 2734),
    ("Cast 1:1 busy", 2734),
    ("Thanks for Watching 16:9 bright", 2734),
    ("Thanks for Watching 16:9 busy", 2734),
    ("Thanks for Watching 9:16 bright", 2734),
    ("Thanks for Watching 9:16 busy", 2734),
    ("Thanks for Watching 1:1 bright", 2734),
    ("Thanks for Watching 1:1 busy", 2734),
    ("Up Next 16:9 bright", 2734),
    ("Up Next 16:9 busy", 2734),
    ("Up Next 9:16 bright", 2734),
    ("Up Next 9:16 busy", 2734),
    ("Up Next 1:1 bright", 2734),
    ("Up Next 1:1 busy", 2734),
    ("Hook 16:9 bright", 2734),
    ("Hook 16:9 busy", 2734),
    ("Hook 9:16 bright", 2734),
    ("Hook 9:16 busy", 2734),
    ("Hook 1:1 bright", 2734),
    ("Hook 1:1 busy", 2734),
    ("Bold Caption 16:9 bright", 2734),
    ("Bold Caption 16:9 busy", 2734),
    ("Bold Caption 9:16 bright", 2734),
    ("Bold Caption 9:16 busy", 2734),
    ("Bold Caption 1:1 bright", 2734),
    ("Bold Caption 1:1 busy", 2734),
    ("Question 16:9 bright", 2734),
    ("Question 16:9 busy", 2734),
    ("Question 9:16 bright", 2734),
    ("Question 9:16 busy", 2734),
    ("Question 1:1 bright", 2734),
    ("Question 1:1 busy", 2734),
    ("Tip 16:9 bright", 2734),
    ("Tip 16:9 busy", 2734),
    ("Tip 9:16 bright", 2734),
    ("Tip 9:16 busy", 2734),
    ("Tip 1:1 bright", 2734),
    ("Tip 1:1 busy", 2734),
    ("Slate 9:16 title-safe", 2734),
    ("Slate 1:1 title-safe", 2734),
    ("Timecode 16:9 bright", 2734),
    ("Timecode 16:9 busy", 2734),
    ("Timecode 9:16 bright", 2734),
    ("Timecode 9:16 busy", 2734),
    ("Timecode 1:1 bright", 2734),
    ("Timecode 1:1 busy", 2734),
];

/// Readable: a ratio of at least this between a text's glyphs and what
/// is round them (WCAG's large-text minimum).
const CONTRAST: f32 = 3.0;
/// The smallest text, in pixels of a 1080-line frame.
const MIN_SIZE: f32 = klipp_timeline::title_library::MIN_TEXT_SIZE;

const FORMATS: &[(&str, u32, u32)] = &[("16:9", 960, 540), ("9:16", 540, 960), ("1:1", 540, 540)];

fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn luminance(p: &[u8]) -> f32 {
    0.2126 * srgb_to_linear(p[0]) + 0.7152 * srgb_to_linear(p[1]) + 0.0722 * srgb_to_linear(p[2])
}

/// The three frames, sRGB RGBA: a bright beach under sky and cloud, a dark
/// night with a few lights, and busy detail over the full range.
fn backgrounds(w: u32, h: u32) -> Vec<(&'static str, Vec<u8>)> {
    let make = |f: &dyn Fn(f32, f32, u32, u32) -> [u8; 3]| -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| {
            let c = f(x as f32 / w as f32, y as f32 / h as f32, x, y);
            [c[0], c[1], c[2], 255]
        }).collect()
    };
    let bright = make(&|fx, fy, _, _| {
        let cloud = (-((fx - 0.3).powi(2) * 30.0 + (fy - 0.25).powi(2) * 80.0)).exp() + (-((fx - 0.75).powi(2) * 25.0 + (fy - 0.35).powi(2) * 90.0)).exp();
        if fy < 0.65 {
            let sky = [200.0 + 30.0 * fy, 222.0 + 15.0 * fy, 250.0];
            sky.map(|c| (c + cloud * 60.0).min(255.0) as u8)
        } else {
            [232, 216, 182]
        }
    });
    let dark = make(&|fx, fy, _, _| {
        let lamp = |x: f32, y: f32| (-((fx - x).powi(2) + (fy - y).powi(2)) * 900.0).exp();
        let l = lamp(0.2, 0.45) + lamp(0.6, 0.4) + lamp(0.85, 0.5);
        [(12.0 + 200.0 * l).min(255.0) as u8, (14.0 + 170.0 * l).min(255.0) as u8, (20.0 + 90.0 * l).min(255.0) as u8]
    });
    let busy = make(&|_, _, x, y| {
        let mut v = (x / 3).wrapping_mul(0x9E37_79B1) ^ (y / 3).wrapping_mul(0x85EB_CA6B);
        v ^= v >> 15;
        v = v.wrapping_mul(0x2C1B_3C6D);
        v ^= v >> 12;
        [(v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8]
    });
    vec![("bright", bright), ("dark", dark), ("busy", busy)]
}

/// `fg` (straight alpha) over the opaque `bg`.
fn over(fg: &[u8], bg: &[u8]) -> Vec<u8> {
    fg.chunks(4).zip(bg.chunks(4)).flat_map(|(f, b)| {
        let a = f[3] as f32 / 255.0;
        [0, 1, 2].map(|c| (f[c] as f32 * a + b[c] as f32 * (1.0 - a)).round() as u8).into_iter().chain([255])
    }).collect()
}

/// `mask` grown by `r` pixels (a square), separably.
fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut row = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            row[y * w + x] = (a..=b).any(|i| mask[y * w + i]);
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        for y in 0..h {
            let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
            out[y * w + x] = (a..=b).any(|j| row[j * w + x]);
        }
    }
    out
}

/// The template with only its text layer `k` left: what that text alone draws.
fn only_text(td: &TitleData, k: usize) -> TitleData {
    let mut t = td.clone();
    t.objects = vec![td.objects[k].clone()];
    t
}

#[test]
fn sweep_templates() {
    let mut sweep = Sweep::new("templates", KNOWN);
    let mut renderer = TitleRenderer::new();
    for template in TITLES {
        let td = template.data();
        let texts: Vec<usize> = td.objects.iter().enumerate().filter(|(_, o)| matches!(o.content, TitleObjectContent::TextBox(_))).map(|(i, _)| i).collect();
        let smallest = td.objects.iter().filter_map(|o| match &o.content {
            TitleObjectContent::TextBox(tb) if !tb.text.trim().is_empty() => Some(tb.font_size),
            _ => None,
        }).fold(f32::MAX, f32::min);
        sweep.check(&format!("{} smallest text", template.name), format!("{smallest:.0} px of 1080"), smallest >= MIN_SIZE);
        for &(format, w, h) in FORMATS {
            let full = renderer.render(&td, w, h);
            let (wu, hu) = (w as usize, h as usize);
            let r = ((w.min(h) as f32) * 0.012).max(3.0) as usize;
            let layers: Vec<Vec<bool>> = texts.iter().map(|&k| renderer.render(&only_text(&td, k), w, h).chunks(4).map(|p| p[3] > 153).collect()).collect();
            // Inside title-safe: no glyph within 5 % of an edge.
            let (mx, my) = (wu / 20, hu / 20);
            let outside = layers.iter().flat_map(|m| m.iter().enumerate()).filter(|(i, on)| **on && {
                let (x, y) = (i % wu, i / wu);
                x < mx || x >= wu - mx || y < my || y >= hu - my
            }).count();
            sweep.check(&format!("{} {format} title-safe", template.name), format!("{outside} glyph pixels outside"), outside == 0);
            for (bg_name, bg) in backgrounds(w, h) {
                let comp = over(&full, &bg);
                let mut worst = f32::MAX;
                for glyphs in &layers {
                    if !glyphs.iter().any(|g| *g) {
                        continue;
                    }
                    let near = dilate(glyphs, wu, hu, r);
                    let (mut lt, mut nt, mut lb, mut nb) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for i in 0..wu * hu {
                        let l = luminance(&comp[i * 4..i * 4 + 4]);
                        if glyphs[i] {
                            lt += l;
                            nt += 1.0;
                        } else if near[i] {
                            lb += l;
                            nb += 1.0;
                        }
                    }
                    let (a, b) = (lt / nt.max(1.0), lb / nb.max(1.0));
                    worst = worst.min((a.max(b) + 0.05) / (a.min(b) + 0.05));
                }
                if worst == f32::MAX {
                    sweep.skip(&format!("{} {format} {bg_name}", template.name), "no text drawn");
                    continue;
                }
                sweep.check(&format!("{} {format} {bg_name}", template.name), format!("contrast {worst:.2}:1"), worst >= CONTRAST);
            }
        }
    }
    sweep.finish();
}
