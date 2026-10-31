//! A title's backing is a filled box behind its words, at each corner of
//! the frame: inside the box the picture is darkened by the backing's
//! alpha and nothing else, outside it the picture is as it was. With
//! contrast protection on (a new title's `Auto`), the protection's halo
//! counted the dark box as text, read the words as dark and lightened
//! the picture into bright streaks round the box. The CPU path and the
//! GPU stack draw it alike. A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, ContrastProtect, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 960;
const H: u32 = 540;
/// The backing's alpha, 0.6 as the title op's `backing: 0.6` makes it.
const ALPHA: u8 = 153;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A busy picture: large bright and dark squares, which is what made the
/// protection's halo strong.
fn busy() -> Arc<Vec<f32>> {
    Arc::new((0..H).flat_map(|y| (0..W).flat_map(move |x| {
        let v = if (x / 19 + y / 12) % 2 == 0 { 0.85 } else { 0.04 };
        [v, v * 0.85, v * 0.7, 1.0]
    })).collect())
}

struct Busy(Arc<Vec<f32>>);

impl FrameSource for Busy {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: W, height: H, pts: None })
    }
}

/// White words at (`x`, `y`) on a black backing, protection as a new
/// title has it, over the busy picture; or the picture alone.
fn seq(at: Option<(f32, f32)>) -> Sequence {
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut tracks = vec![v1];
    if let Some((x, y)) = at {
        let mut td = TitleData::single_text(TextBox { text: "8\u{d7} speed".into(), font_size: 30.0, color: [255, 255, 255, 255], ..Default::default() });
        td.objects[0].transform.center.x = x;
        td.objects[0].transform.center.y = y;
        td.background = TitleBackground::Color { color: [0, 0, 0, ALPHA], padding: 12.0, lines: None };
        td.protect = ContrastProtect::Auto;
        let mut t = Track::new("T", TrackKind::Video);
        t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
        tracks.insert(0, t);
    }
    let mut s = Sequence::new_default();
    s.resolution = klipp_core::Resolution { width: W, height: H };
    s.tracks = tracks;
    s
}

/// Checks a frame drawn with the title against the picture alone: what
/// changed is one box; the words are where it is much brighter than the
/// backing; and round them, the backing's margin, every pixel is the
/// picture darkened by the backing and nothing else (no halo darker or
/// lighter, no hole). Returns the box and the words' extent.
fn check(name: &str, got: &[u8], picture: &[u8], darkened: &[u8]) -> ([usize; 4], [usize; 4]) {
    let (w, h) = (W as usize, H as usize);
    let px = |buf: &[u8], i: usize| [buf[i * 4] as i32, buf[i * 4 + 1] as i32, buf[i * 4 + 2] as i32];
    let near = |a: [i32; 3], b: [i32; 3]| a.iter().zip(&b).all(|(a, b)| (a - b).abs() <= 3);
    let extent = |keep: &dyn Fn(usize) -> bool| {
        let mut e = [usize::MAX, usize::MAX, 0, 0];
        for i in (0..w * h).filter(|i| keep(*i)) {
            let (x, y) = (i % w, i / w);
            e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
        }
        e
    };
    let boxed = extent(&|i| !near(px(got, i), px(picture, i)));
    assert!(boxed[0] <= boxed[2], "{name}: the title drew nothing");
    let in_box = |i: usize| (boxed[0]..=boxed[2]).contains(&(i % w)) && (boxed[1]..=boxed[3]).contains(&(i / w));
    let words = extent(&|i| in_box(i) && px(got, i).iter().zip(&px(darkened, i)).all(|(g, d)| g - d > 40));
    assert!(words[0] <= words[2], "{name}: no words");
    assert!(words[0] > boxed[0] && words[1] > boxed[1] && words[2] < boxed[2] && words[3] < boxed[3], "{name}: words {words:?} not inside the box {boxed:?}");
    // The margin: the box less the words and their soft edge.
    let soft = 3;
    for y in boxed[1]..=boxed[3] {
        for x in boxed[0]..=boxed[2] {
            let by_words = x + soft >= words[0] && x <= words[2] + soft && y + soft >= words[1] && y <= words[3] + soft;
            if by_words {
                continue;
            }
            let i = y * w + x;
            assert!(near(px(got, i), px(darkened, i)), "{name}: pixel {x},{y} in the box's margin is {:?}, the backing over the picture is {:?}", px(got, i), px(darkened, i));
        }
    }
    (boxed, words)
}

#[test]
fn a_backing_is_a_filled_box_behind_the_words_at_each_corner() {
    let picture_pixels = busy();
    let darkened: Arc<Vec<f32>> = Arc::new(picture_pixels.chunks(4).flat_map(|p| {
        let k = 1.0 - ALPHA as f32 / 255.0;
        [p[0] * k, p[1] * k, p[2] * k, 1.0]
    }).collect());
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let to_u8 = klipp_effects::linear_f32_rgba_to_srgb_u8;
    let picture = to_u8(&cpu.render(&seq(None), tc(1.0), W, H, &mut Busy(Arc::clone(&picture_pixels)), RenderOptions::default()));
    let darkened = to_u8(&darkened);
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let has_gpu = gpu.executor().gpu_pipeline_or_init().is_some();
    assert!(has_gpu || !required(), "GPU required");
    if !has_gpu {
        eprintln!("UNVERIFIED on the GPU: no adapter");
    }
    // The four corners, inside title-safe, and the middle.
    for (x, y) in [(0.12, 0.07), (0.88, 0.07), (0.12, 0.93), (0.88, 0.93), (0.5, 0.5)] {
        let s = seq(Some((x, y)));
        let got = to_u8(&cpu.render(&s, tc(1.0), W, H, &mut Busy(Arc::clone(&picture_pixels)), RenderOptions::default()));
        let (boxed, words) = check(&format!("CPU at {x},{y}"), &got, &picture, &darkened);
        // The backing's margin, 12 pixels at 1080p, round the words.
        let pad = 12.0 * H as f32 / 1080.0;
        let margin = [words[0] - boxed[0], words[1] - boxed[1], boxed[2] - words[2], boxed[3] - words[3]];
        assert!(margin.iter().all(|m| *m as f32 >= pad - 1.5), "CPU at {x},{y}: box {boxed:?} round words {words:?}, margins {margin:?} under {pad}");
        if has_gpu {
            let (stack, _) = gpu.render_display_u8_on_gpu(&s, tc(1.0), W, H, &mut Busy(Arc::clone(&picture_pixels)), RenderOptions::default()).expect("the GPU stack draws a backed title");
            let (gpu_box, _) = check(&format!("GPU stack at {x},{y}"), &stack, &picture, &darkened);
            assert!(gpu_box.iter().zip(&boxed).all(|(a, b)| a.abs_diff(*b) <= 1), "GPU box {gpu_box:?}, CPU box {boxed:?}");
        }
    }
}
