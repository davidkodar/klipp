//! #3237: a backing drawn per line moves with its lines. With a line
//! stagger the second line's box is not there until its line comes in;
//! rising, the boxes keep their place round the moving text; a
//! typewriter's box grows with the letters shown. The GPU stack draws it
//! as the CPU does, within a level. A missing GPU is reported as
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, LineBoxes, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 960;
const H: u32 = 540;

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

/// White `text` entering by `enter`, on a near-black backing per line,
/// over grey.
fn seq(text: &str, enter: Option<Motion>) -> Sequence {
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(6.0), tc(0.0), tc(6.0))];
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 60.0, color: [255, 255, 255, 255], ..Default::default() });
    td.background = TitleBackground::Color { color: [10, 10, 10, 255], padding: 16.0, lines: Some(LineBoxes::default()) };
    td.protect = klipp_timeline::ContrastProtect::Off;
    td.objects[0].fx_mut().motion.enter = enter;
    let mut t = Track::new("T", TrackKind::Video);
    t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(6.0)) }];
    let mut s = Sequence::new_default();
    s.resolution = klipp_core::Resolution { width: W, height: H };
    s.tracks = vec![t, v1];
    s
}

struct Renderers {
    cpu: Renderer,
    gpu: Option<Renderer>,
    picture: Arc<Vec<f32>>,
}

impl Renderers {
    fn new() -> Self {
        let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        cpu.set_gpu_enabled(false);
        let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        gpu.set_gpu_enabled(true);
        let has_gpu = gpu.executor().gpu_pipeline_or_init().is_some();
        assert!(has_gpu || !required(), "GPU required");
        if !has_gpu {
            eprintln!("UNVERIFIED on the GPU: no adapter");
        }
        Self { cpu, gpu: has_gpu.then_some(gpu), picture: Arc::new((0..W * H).flat_map(|_| [0.2f32, 0.2, 0.2, 1.0]).collect()) }
    }

    /// The frame at `secs` on the CPU, checked against the GPU stack's:
    /// every backing pixel away from the letters within a level.
    fn at(&mut self, name: &str, s: &Sequence, secs: f64) -> Vec<u8> {
        let cpu = klipp_effects::linear_f32_rgba_to_srgb_u8(&self.cpu.render(s, tc(secs), W, H, &mut Grey(Arc::clone(&self.picture)), RenderOptions::default()));
        if let Some(gpu) = self.gpu.as_mut() {
            let (stack, _) = gpu.render_display_u8_on_gpu(s, tc(secs), W, H, &mut Grey(Arc::clone(&self.picture)), RenderOptions::default()).expect("the GPU stack draws the title");
            let w = W as usize;
            let near_letters = |i: usize| {
                let (x, y) = ((i % w) as i64, (i / w) as i64);
                (-4..=4).any(|dy| (-4..=4).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0 && ny >= 0 && nx < W as i64 && ny < H as i64 && [&cpu, &stack].iter().any(|b| b[(ny as usize * w + nx as usize) * 4] > 60)
                }))
            };
            let off: Vec<usize> = (0..w * H as usize).filter(|&i| !near_letters(i) && (0..3).any(|c| cpu[i * 4 + c].abs_diff(stack[i * 4 + c]) > 1)).collect();
            assert!(off.is_empty(), "{name}: {} backing pixels differ by more than a level between the CPU and the GPU stack, the first at {:?}", off.len(), off.first().map(|i| (i % w, i / w)));
        }
        cpu
    }
}

/// The rows the backing darkens, as `(first, last)` runs, and the
/// columns it reaches across them.
fn dark_rows(buf: &[u8]) -> (Vec<(usize, usize)>, Option<(usize, usize)>) {
    let w = W as usize;
    // Darker than the grey picture (124): the backing, faded or not.
    let dark = |i: usize| buf[i * 4] < 100;
    let rows: Vec<usize> = (0..H as usize).filter(|&y| (0..w).any(|x| dark(y * w + x))).collect();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for y in rows {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == y => *end = y,
            _ => runs.push((y, y)),
        }
    }
    let xs: Vec<usize> = (0..w).filter(|&x| (0..H as usize).any(|y| dark(y * w + x))).collect();
    (runs, xs.first().zip(xs.last()).map(|(a, b)| (*a, *b)))
}

/// The rows the white letters reach.
fn ink_rows(buf: &[u8]) -> (usize, usize) {
    let w = W as usize;
    let rows: Vec<usize> = (0..H as usize).filter(|&y| (0..w).any(|x| buf[(y * w + x) * 4] > 200)).collect();
    (rows[0], *rows.last().unwrap())
}

#[test]
fn a_line_staggers_box_comes_in_with_its_line() {
    let mut r = Renderers::new();
    let s = seq("First line\nSecond line", Some(Motion::new(MotionKind::Fade).duration(0.2).stagger(StaggerUnit::Line, 2.0)));
    let (one, _) = dark_rows(&r.at("first in", &s, 1.0));
    let (both, _) = dark_rows(&r.at("both in", &s, 3.0));
    assert_eq!(both.len(), 1, "both in: one joined shape: {both:?}");
    assert_eq!(one.len(), 1, "the first line in: one box: {one:?}");
    assert_eq!(one[0].0, both[0].0, "the first line's box where it will stay");
    assert!(one[0].1 + 30 < both[0].1, "the second line's box not there yet: {one:?} against {both:?}");
}

#[test]
fn a_rising_lines_box_keeps_its_place_round_it() {
    let mut r = Renderers::new();
    let rest = r.at("at rest", &seq("Rising", None), 1.0);
    let rising = r.at("rising", &seq("Rising", Some(Motion::new(MotionKind::Rise).duration(1.0))), 0.3);
    let (box_rest, ink_rest) = (dark_rows(&rest).0[0], ink_rows(&rest));
    let (box_rising, ink_rising) = (dark_rows(&rising).0[0], ink_rows(&rising));
    assert!(ink_rising.0 > ink_rest.0 + 3, "the text is still rising: its top at {} against {} at rest", ink_rising.0, ink_rest.0);
    let gap = |b: (usize, usize), i: (usize, usize)| i.0 as i64 - b.0 as i64;
    assert!((gap(box_rising, ink_rising) - gap(box_rest, ink_rest)).abs() <= 1, "the box over the text as at rest: box {box_rising:?} ink {ink_rising:?}, at rest box {box_rest:?} ink {ink_rest:?}");
}

#[test]
fn a_typewriters_box_grows_with_its_letters() {
    let mut r = Renderers::new();
    let s = seq("Typed one by one", Some(Motion::new(MotionKind::Fade).duration(0.01).stagger(StaggerUnit::Char, 0.1)));
    let (_, early) = dark_rows(&r.at("early", &s, 0.45));
    let (_, done) = dark_rows(&r.at("done", &s, 3.0));
    let (early, done) = (early.expect("a box"), done.expect("a box"));
    assert_eq!(early.0, done.0, "it starts where the line starts");
    assert!(early.1 + 100 < done.1, "a few letters in, the box is short: to {} against {}", early.1, done.1);
}
