//! An animated caption (#2710) on the GPU stack: the word being spoken
//! takes its colour, or its box, there as on the CPU path, with no
//! fallback. A
//! missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::caption_motion::{animated_caption, CaptionScreen, TimedWord};
use klipp_timeline::{CaptionAnimation, CaptionMotion, Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

struct Grey(Arc<Vec<f32>>);

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: W, height: H, pts: None })
    }
}

fn seq(style: CaptionMotion) -> Sequence {
    let words = [("ONE", 0.0), ("TWO", 0.5), ("SIX", 1.0)].map(|(t, s)| TimedWord { text: t.into(), start: s, end: s + 0.4 }).to_vec();
    let screen = CaptionScreen { words, start: 0.0, end: 1.5 };
    let td = animated_caption(&screen, &CaptionAnimation { style, active_color: [255, 0, 0, 255], ..Default::default() }, W, H);
    let mut text = Track::new("S1", TrackKind::Subtitle);
    text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(1.0), tc(2.5)) }];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];
    seq
}

/// Red pixels in the frame at `secs`, drawn on the GPU stack and checked
/// against the CPU path; `None` when the stack declined it.
fn red(style: CaptionMotion, secs: f64) -> Option<usize> {
    let seq = seq(style);
    let grey = || Grey(Arc::new([0.2, 0.2, 0.2, 1.0].repeat((W * H) as usize)));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (stack, _) = r.render_display_u8_on_gpu(&seq, tc(secs), W, H, &mut grey(), RenderOptions::default())?;
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(secs), W, H, &mut grey(), RenderOptions::default()));
    let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 3, "{style:?} at {secs}: the stack differs from the CPU path by {worst} levels");
    Some(stack.chunks(4).filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60).count())
}

#[test]
fn the_spoken_word_takes_its_colour_on_the_gpu() {
    let Some(before) = red(CaptionMotion::Karaoke, 1.2) else {
        assert!(!required(), "required, but the GPU stack declined the caption");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let after = red(CaptionMotion::Karaoke, 1.7).unwrap();
    assert!(before > 50 && after > before * 3 / 2, "karaoke fills as the words are said: {before} then {after}");
    assert!(red(CaptionMotion::Pop, 1.7).unwrap() > 50, "pop colours the spoken word");
    // A box behind the spoken word is larger than the word's own ink.
    assert!(red(CaptionMotion::Box, 1.7).unwrap() > red(CaptionMotion::Highlight, 1.7).unwrap(), "box: the spoken word's box");
}

/// Lit pixels round a Glow Caps caption (#3304) at `glow`, drawn on the
/// GPU stack and checked against the CPU path; `None` when the stack
/// declined it. Set in the bundled face: Montserrat is fetched on use,
/// and the glow is what this measures.
fn glow_lit(glow: f32) -> Option<usize> {
    let look = klipp_timeline::caption_looks::by_id("glow_caps").expect("the look");
    let anim = CaptionAnimation { font: None, ..look.animation(look.colour(None).unwrap(), glow) };
    let words = [("glow", 0.0), ("caps", 0.3)].map(|(t, s)| TimedWord { text: t.into(), start: s, end: s + 0.3 }).to_vec();
    let td = animated_caption(&CaptionScreen { words, start: 0.0, end: 1.0 }, &anim, W, H);
    let mut seq = seq(CaptionMotion::Pop);
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(1.0), tc(2.0)) }];
    let dark = || Grey(Arc::new([0.0, 0.0, 0.0, 1.0].repeat((W * H) as usize)));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (stack, _) = r.render_display_u8_on_gpu(&seq, tc(1.5), W, H, &mut dark(), RenderOptions::default())?;
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.5), W, H, &mut dark(), RenderOptions::default()));
    let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 3, "glow {glow}: the stack differs from the CPU path by {worst} levels");
    Some(stack.chunks(4).filter(|p| p[2] > 24).count())
}

#[test]
fn a_glow_caps_caption_glows_on_both_routes() {
    let Some(bare) = glow_lit(0.0) else {
        assert!(!required(), "required, but the GPU stack declined the caption");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let glowing = glow_lit(1.0).unwrap();
    assert!(bare > 50, "the words drawn: {bare}");
    assert!(glowing > bare * 3 / 2, "the glow lights round the words: {bare} bare, {glowing} glowing");
}
