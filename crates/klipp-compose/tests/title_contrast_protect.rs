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

/// A bright picture with stripes in it, or a dark one.
struct Backdrop(Arc<Vec<f32>>);

impl Backdrop {
    fn new(bright: bool) -> Self {
        let v = (0..H)
            .flat_map(|y| (0..W).flat_map(move |x| {
                let v = if bright { if (x / 6 + y / 9) % 2 == 0 { 0.92 } else { 0.7 } } else { 0.015 };
                [v, v, v * 0.95, 1.0]
            }))
            .collect();
        Self(Arc::new(v))
    }
}

impl FrameSource for Backdrop {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: W, height: H, pts: None })
    }
}

fn seq(protect: ContrastProtect) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: "Readable".into(), font_size: 400.0, color: [255, 255, 255, 255], ..Default::default() });
    td.protect = protect;
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];
    seq
}

/// The frame on the GPU stack, checked against the CPU path; `None`
/// when the stack declined it.
fn render(protect: ContrastProtect, bright: bool) -> Option<Vec<u8>> {
    let seq = seq(protect);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (stack, _) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Backdrop::new(bright), RenderOptions::default())?;
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.0), W, H, &mut Backdrop::new(bright), RenderOptions::default()));
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
