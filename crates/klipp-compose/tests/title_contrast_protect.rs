//! A title's contrast protection (#2734): white text over a bright, busy
//! picture is read at 4.5:1 or near it with protection on, where it was
//! under 1.5:1 without; over a dark picture Auto leaves the picture as
//! it was. The GPU stack draws it, with no fallback, as the CPU path
//! does. `KLIPP_PROTECT_SHEET=<path.png>` writes the protected frame.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, ContrastProtect, Sequence, TextBox, TitleData, Track, TrackKind};

const W: u32 = 480;
const H: u32 = 200;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A bright picture with stripes in it, or a dark one, `W` × `H` unless
/// made at another size.
struct Backdrop(Arc<Vec<f32>>, u32, u32);

impl Backdrop {
    fn new(bright: bool) -> Self {
        let v = (0..H)
            .flat_map(|y| (0..W).flat_map(move |x| {
                let v = if bright { if (x / 6 + y / 9) % 2 == 0 { 0.92 } else { 0.7 } } else { 0.015 };
                [v, v, v * 0.95, 1.0]
            }))
            .collect();
        Self(Arc::new(v), W, H)
    }

    /// Bright grey on the left half, dark blue on the right.
    fn split() -> Self {
        let v = (0..H).flat_map(|_| (0..W).flat_map(|x| if x < W / 2 { [0.85, 0.85, 0.85, 1.0] } else { [0.03, 0.04, 0.3, 1.0] })).collect();
        Self(Arc::new(v), W, H)
    }
}

impl FrameSource for Backdrop {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: self.1, height: self.2, pts: None })
    }
}

fn seq(protect: ContrastProtect) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: "Readable".into(), font_size: 400.0, color: [255, 255, 255, 255], ..Default::default() });
    td.protect = protect;
    seq_of(td, W, H)
}

/// A plain title's white text, 60 px, with an 8 px black stroke round
/// it, on a 1280 × 720 frame: the blur the pass reads the picture with
/// grows with the frame, and at this size the stroke's ring is most of
/// what the cells round the letters hold.
fn seq_stroked(protect: ContrastProtect) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: "Harbour Light".into(), font_size: 60.0, color: [255, 255, 255, 255], ..Default::default() });
    td.protect = protect;
    let outline = serde_json::from_value(serde_json::json!({ "color": [0, 0, 0, 255], "width": 8.0 })).expect("a stroke");
    td.objects[0].fx_mut().outlines.push(outline);
    seq_of(td, 1280, 720)
}

/// `td` over a picture clip on a `w` × `h` sequence.
fn seq_of(td: TitleData, w: u32, h: u32) -> Sequence {
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    seq.tracks = vec![text, v1];
    seq
}

/// The frame on the GPU stack, checked against the CPU path; `None`
/// when the stack declined it.
fn render(protect: ContrastProtect, bright: bool) -> Option<Vec<u8>> {
    render_over(protect, || Backdrop::new(bright))
}

fn render_over(protect: ContrastProtect, backdrop: impl Fn() -> Backdrop) -> Option<Vec<u8>> {
    render_seq(seq(protect), protect, backdrop)
}

fn render_seq(seq: Sequence, protect: ContrastProtect, backdrop: impl Fn() -> Backdrop) -> Option<Vec<u8>> {
    let klipp_core::Resolution { width: w, height: h } = seq.resolution;
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (stack, _) = r.render_display_u8_on_gpu(&seq, tc(1.0), w, h, &mut backdrop(), RenderOptions::default())?;
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.0), w, h, &mut backdrop(), RenderOptions::default()));
    let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 3, "{protect:?}: the stack differs from the CPU path by {worst} levels");
    Some(stack)
}

fn luminance(px: &[u8], i: usize) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(px[i * 4]) + 0.7152 * lin(px[i * 4 + 1]) + 0.0722 * lin(px[i * 4 + 2])
}

/// The text's contrast with the picture round it: the glyphs are the
/// pixels the unprotected frame over the dark picture shows lit, and the
/// picture round them is what lies 2 to 6 pixels from a glyph, at its
/// brightest tenth, where the text is hardest to read.
fn contrast(px: &[u8], glyphs: &[bool]) -> f32 {
    let near = |x: u32, y: u32, r: i32| {
        (-r..=r).any(|dy| (-r..=r).any(|dx| {
            let (gx, gy) = (x as i32 + dx, y as i32 + dy);
            gx >= 0 && gy >= 0 && gx < W as i32 && gy < H as i32 && glyphs[(gy as u32 * W + gx as u32) as usize]
        }))
    };
    let mut behind: Vec<f32> = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(x, y, 1) && near(x, y, 6))
        .map(|(x, y)| luminance(px, (y * W + x) as usize))
        .collect();
    behind.sort_by(f32::total_cmp);
    let bright = behind[behind.len() * 9 / 10];
    (1.0 + 0.05) / (bright + 0.05)
}

#[test]
fn protected_text_reads_over_a_bright_picture() {
    let Some(dark) = render(ContrastProtect::Off, false) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let glyphs: Vec<bool> = (0..(W * H) as usize).map(|i| dark[i * 4] > 200).collect();
    let drawn = glyphs.iter().filter(|g| **g).count();
    assert!(drawn > 1000, "the text is drawn: {drawn} pixels");

    let off = contrast(&render(ContrastProtect::Off, true).unwrap(), &glyphs);
    assert!(off < 1.5, "without protection the text hardly reads: {off:.2}:1");
    let auto_px = render(ContrastProtect::Auto, true).unwrap();
    if let Some(p) = std::env::var_os("KLIPP_PROTECT_SHEET") {
        image::save_buffer(p, &auto_px, W, H, image::ColorType::Rgba8).expect("the sheet is written");
    }
    let auto = contrast(&auto_px, &glyphs);
    assert!(auto >= 4.0, "with protection the text reads: {auto:.2}:1");
    let always = contrast(&render(ContrastProtect::Always, true).unwrap(), &glyphs);
    assert!(always >= auto - 0.05, "always protects at least as much: {always:.2}:1 vs {auto:.2}:1");

    let kept = render(ContrastProtect::Auto, false).unwrap();
    let worst = kept.iter().zip(&dark).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 1, "over a dark picture Auto leaves it as it was ({worst} levels)");
    eprintln!("contrast: off {off:.2}:1, auto {auto:.2}:1, always {always:.2}:1");
}

/// White text across a picture half bright, half dark blue: the shade
/// behind it is even, the same over both halves, as much as the bright
/// half needs. Worked out place by place it darkened only behind the
/// letters over the bright half, an off-centre patch. Measured as the
/// protected frame's luminance over the unprotected one's, just round
/// the letters, on each half.
#[test]
fn a_title_over_bright_and_dark_is_shaded_evenly() {
    let Some(off) = render_over(ContrastProtect::Off, Backdrop::split) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let auto = render_over(ContrastProtect::Auto, Backdrop::split).unwrap();
    // The letters: lit in the unprotected frame over the dark picture.
    let lit = render(ContrastProtect::Off, false).unwrap();
    let glyphs: Vec<bool> = (0..(W * H) as usize).map(|i| lit[i * 4] > 200).collect();
    let near = |x: u32, y: u32, r: i32| {
        (-r..=r).any(|dy| (-r..=r).any(|dx| {
            let (gx, gy) = (x as i32 + dx, y as i32 + dy);
            gx >= 0 && gy >= 0 && gx < W as i32 && gy < H as i32 && glyphs[(gy as u32 * W + gx as u32) as usize]
        }))
    };
    let shade = |left: bool| {
        let ratios: Vec<f32> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, _)| if left { x + 8 < W / 2 } else { x > W / 2 + 8 })
            .filter(|&(x, y)| !near(x, y, 1) && near(x, y, 4))
            .map(|(x, y)| {
                let i = (y * W + x) as usize;
                luminance(&auto, i) / luminance(&off, i).max(1e-4)
            })
            .collect();
        assert!(ratios.len() > 200, "the letters reach both halves: {} pixels round them", ratios.len());
        ratios.iter().sum::<f32>() / ratios.len() as f32
    };
    let (bright, dark) = (shade(true), shade(false));
    eprintln!("shade round the letters: bright half {bright:.3}, dark half {dark:.3}");
    assert!(bright < 0.6, "the bright half is shaded: {bright:.3}");
    assert!((bright - dark).abs() < 0.08, "the shade is even across the title: bright half {bright:.3}, dark half {dark:.3}");
}

/// White text with a black stroke, Readability on Auto, over a warm
/// sunset brown at 1280 × 720: the picture round it is darkened or left,
/// never lightened, on the GPU as on the CPU. Decided cell by cell, the
/// cells the stroke's ring filled read as dark text, and the picture
/// round the letters was lightened into a bright rim about a ragged dark
/// patch.
#[test]
fn a_stroked_title_on_auto_gets_no_bright_rim() {
    let (w, h) = (1280, 720);
    let sunset = || {
        let v = (0..h).flat_map(|_| (0..w).flat_map(|_| [0.45, 0.2, 0.08, 1.0])).collect();
        Backdrop(Arc::new(v), w, h)
    };
    let Some(off) = render_seq(seq_stroked(ContrastProtect::Off), ContrastProtect::Off, sunset) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let auto = render_seq(seq_stroked(ContrastProtect::Auto), ContrastProtect::Auto, sunset).unwrap();
    // Brighter with protection than without, outside the text's own ink
    // (which the two draw alike).
    let lighter = (0..(w * h) as usize).filter(|&i| luminance(&auto, i) > luminance(&off, i) + 0.02 && luminance(&off, i) < 0.5).count();
    assert_eq!(lighter, 0, "{lighter} pixels of the picture lightened round the stroked title");
}

/// A title of a big black block (a logo's place, set here as a run of
/// solid glyphs) above a line of white words, Readability on Auto, over a
/// bright sky at 1280 × 720 (the End Screen template): the white words are
/// light text, so the sky round them is darkened or left, never lightened,
/// on the GPU as on the CPU. Read by the average of all its ink, the
/// block made the title dark text, and the sky round the white words was
/// lightened to their own white.
#[test]
fn white_words_beside_a_black_block_on_auto_keep_the_sky_unlightened() {
    let (w, h) = (1280, 720);
    let sky = || {
        let v = (0..h).flat_map(|_| (0..w).flat_map(|_| [0.55, 0.65, 0.9, 1.0])).collect();
        Backdrop(Arc::new(v), w, h)
    };
    let seq = |protect: ContrastProtect| {
        let mut td = TitleData::single_text(TextBox { text: "Thanks for watching".into(), font_size: 40.0, color: [255, 255, 255, 255], ..Default::default() });
        td.objects[0].transform.center.y = 0.72;
        let mut block = td.objects[0].clone();
        block.id = klipp_timeline::TitleObjectId(td.objects[0].id.0 + 1);
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut block.content {
            tb.text = "\u{2588}\u{2588}\u{2588}\n\u{2588}\u{2588}\u{2588}".into();
            tb.font_size = 160.0;
            tb.color = [0, 0, 0, 255];
        }
        block.transform.center.y = 0.4;
        td.objects.push(block);
        td.protect = protect;
        seq_of(td, w, h)
    };
    let Some(off) = render_seq(seq(ContrastProtect::Off), ContrastProtect::Off, sky) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let auto = render_seq(seq(ContrastProtect::Auto), ContrastProtect::Auto, sky).unwrap();
    // The sky under the words: the band below them, across the frame.
    let band = (h * 78 / 100..h * 82 / 100).flat_map(|y| (0..w).map(move |x| (y * w + x) as usize));
    let lighter = band.filter(|&i| luminance(&auto, i) > luminance(&off, i) + 0.02).count();
    assert_eq!(lighter, 0, "{lighter} pixels of sky under the white words lightened");
    let darker = (h * 74 / 100..h * 80 / 100).flat_map(|y| (0..w).map(move |x| (y * w + x) as usize)).filter(|&i| luminance(&auto, i) + 0.02 < luminance(&off, i)).count();
    assert!(darker > 0, "the sky round the white words is darkened");
}

/// Black letterbox bars across the frame with white words between them
/// (the Opener template), Readability on Auto, over a bright sky at
/// 1280 × 720: the words get their shade, the sky just inside each bar is
/// left as it is, on the GPU as on the CPU. With one halo round all the
/// ink, the bars' edges took the darkening the white words asked for: a
/// dark band along each bar, the sky there at a seventh of its light.
#[test]
fn letterbox_bars_round_white_words_on_auto_leave_the_sky_inside_them() {
    let (w, h) = (1280, 720);
    let sky = || {
        let v = (0..h).flat_map(|_| (0..w).flat_map(|_| [0.55, 0.65, 0.9, 1.0])).collect();
        Backdrop(Arc::new(v), w, h)
    };
    let seq = |protect: ContrastProtect| {
        let mut td = TitleData::single_text(TextBox { text: "THE TITLE".into(), font_size: 100.0, color: [255, 255, 255, 255], ..Default::default() });
        for y in [0.06, 0.94] {
            let mut xf = klipp_timeline::default_text_transform();
            xf.center = klipp_timeline::TitlePosition { x: 0.5, y };
            xf.size = klipp_timeline::TitlePosition { x: 1.0, y: 0.12 };
            td.add_object(
                klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([0, 0, 0, 255]), gradient: None, stroke: None, fit: None }),
                xf,
            );
        }
        td.protect = protect;
        seq_of(td, w, h)
    };
    let Some(off) = render_seq(seq(ContrastProtect::Off), ContrastProtect::Off, sky) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let auto = render_seq(seq(ContrastProtect::Auto), ContrastProtect::Auto, sky).unwrap();
    // The bars' inner edges, found as the first rows from each end the
    // frame without protection shows the sky in.
    let dark_row = |y: u32| luminance(&off, (y * w + w / 2) as usize) < 0.01;
    let top = (0..h).take_while(|&y| dark_row(y)).count() as u32;
    let bottom = h - (0..h).rev().take_while(|&y| dark_row(y)).count() as u32;
    assert!(top > 0 && bottom < h, "the bars are drawn: {top}, {bottom}");
    let band = |ys: std::ops::Range<u32>| ys.flat_map(|y| (0..w).map(move |x| (y * w + x) as usize)).map(|i| luminance(&auto, i) - luminance(&off, i)).fold(0.0f32, f32::min);
    let (above, below) = (band(top..top + 24), band(bottom - 24..bottom));
    assert!(above > -0.02 && below > -0.02, "the sky inside the bars darkened by {above} (top) and {below} (bottom)");
    let words = band(h / 2 - 50..h / 2 + 50);
    assert!(words < -0.1, "the sky round the words is darkened: {words}");
}
