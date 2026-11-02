//! #3261: a title's one box round its whole text stays whole under a
//! staggered motion (by line, word or character), drawn under the posed
//! glyphs where it rests, not cut up with them. The GPU stack draws it as
//! the CPU does, within a level. A missing GPU is reported as unverified;
//! with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

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

/// White `text` entering by `enter`, on one near-black box round it all,
/// over grey.
fn seq(text: &str, enter: Option<Motion>) -> Sequence {
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(6.0), tc(0.0), tc(6.0))];
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 60.0, color: [255, 255, 255, 255], ..Default::default() });
    td.background = TitleBackground::Color { color: [10, 10, 10, 255], padding: 16.0, lines: None };
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

/// Where the box darkens the frame, away from the letters: each pixel's
/// row and column. The same set at rest and part-way through a stagger is
/// a box whole and still.
fn box_pixels(buf: &[u8]) -> Vec<(usize, usize)> {
    let w = W as usize;
    (0..w * H as usize).filter(|&i| buf[i * 4] < 100).map(|i| (i % w, i / w)).collect()
}

/// The box's bounds, and whether it fills them: one rectangle.
fn one_rectangle(px: &[(usize, usize)], buf: &[u8]) -> Option<[usize; 4]> {
    let (x0, x1) = (px.iter().map(|p| p.0).min()?, px.iter().map(|p| p.0).max()?);
    let (y0, y1) = (px.iter().map(|p| p.1).min()?, px.iter().map(|p| p.1).max()?);
    let w = W as usize;
    // Every pixel inside is the box or a letter on it.
    let whole = (y0..=y1).all(|y| (x0..=x1).all(|x| buf[(y * w + x) * 4] < 100 || buf[(y * w + x) * 4] > 60));
    whole.then_some([x0, y0, x1, y1])
}

#[test]
fn a_whole_text_box_stays_whole_under_a_stagger() {
    let mut r = Renderers::new();
    let text = "Two words\nand a line";
    let rest = r.at("at rest", &seq(text, None), 3.0);
    let rest_box = one_rectangle(&box_pixels(&rest), &rest).expect("one box at rest");
    for (unit, at) in [(StaggerUnit::Line, 0.3), (StaggerUnit::Word, 0.65), (StaggerUnit::Char, 0.75)] {
        let name = format!("{unit:?}");
        let s = seq(text, Some(Motion::new(MotionKind::Fade).duration(0.1).stagger(unit, 0.5)));
        let mid = r.at(&name, &s, at);
        let done = r.at(&format!("{name} done"), &s, 5.0);
        let ink = mid.chunks_exact(4).filter(|p| p[0] > 200).count();
        let all_ink = done.chunks_exact(4).filter(|p| p[0] > 200).count();
        assert_eq!(one_rectangle(&box_pixels(&done), &done), Some(rest_box), "{name}: all in, the box as at rest");
        assert!(ink > 0 && ink < all_ink, "{name}: part-way through: {ink} of {all_ink} letter pixels");
        let mid_box = one_rectangle(&box_pixels(&mid), &mid);
        assert_eq!(mid_box, Some(rest_box), "{name}: the box whole and where it rests, not cut up with the letters");
    }
}
