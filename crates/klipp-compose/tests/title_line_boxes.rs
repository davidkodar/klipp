//! #3237: a title's backing drawn one box per line. Each line's box is
//! as wide as the line, set where its alignment puts it; joined, the
//! boxes are one shape, the corner where a narrow line meets a wider one
//! rounded as its outside corners are (square with a radius of 0); with a
//! gap they stand apart; vertical text has a box per column. The GPU
//! stack draws them as the CPU does, within a level. A missing GPU is
//! reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, LineBoxes, Sequence, TextAlignment, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 960;
const H: u32 = 540;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A flat grey picture: whatever the title changes is the title.
struct Grey(Arc<Vec<f32>>);

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: W, height: H, pts: None })
    }
}

/// White `text` on a near-black backing drawn per line, centred in the
/// frame, over the grey picture; or the picture alone.
fn seq(text: Option<&str>, align: TextAlignment, lines: LineBoxes, vertical: bool) -> Sequence {
    spaced(text, align, lines, vertical, 1.0)
}

/// [`seq`], its lines `spacing` apart.
fn spaced(text: Option<&str>, align: TextAlignment, lines: LineBoxes, vertical: bool, spacing: f32) -> Sequence {
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut tracks = vec![v1];
    if let Some(text) = text {
        let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 60.0, color: [255, 255, 255, 255], alignment: align, vertical_layout: vertical, line_spacing: spacing, ..Default::default() });
        td.background = TitleBackground::Color { color: [10, 10, 10, 255], padding: 16.0, lines: Some(lines) };
        td.protect = klipp_timeline::ContrastProtect::Off;
        let mut t = Track::new("T", TrackKind::Video);
        t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
        tracks.insert(0, t);
    }
    let mut s = Sequence::new_default();
    s.resolution = klipp_core::Resolution { width: W, height: H };
    s.tracks = tracks;
    s
}

/// Whether the pixel at `i` is the backing's near-black.
fn dark(buf: &[u8], i: usize) -> bool {
    buf[i * 4] < 40 && buf[i * 4 + 1] < 40 && buf[i * 4 + 2] < 40
}

/// Whether the pixel at `i` is the grey picture, untouched.
fn grey(buf: &[u8], i: usize, picture: &[u8]) -> bool {
    (0..3).all(|c| buf[i * 4 + c].abs_diff(picture[i * 4 + c]) <= 2)
}

/// Each row's span of pixels the title changed, `(first, last)`.
fn spans(buf: &[u8], picture: &[u8]) -> Vec<Option<(usize, usize)>> {
    let w = W as usize;
    (0..H as usize)
        .map(|y| {
            let xs: Vec<usize> = (0..w).filter(|&x| !grey(buf, y * w + x, picture)).collect();
            Some((*xs.first()?, *xs.last()?))
        })
        .collect()
}

/// The rows where each box is widest and stays so, top first: the span
/// of each run of rows with the same span, longest runs only.
fn boxes(rows: &[Option<(usize, usize)>]) -> Vec<(usize, (usize, usize))> {
    let mut runs: Vec<(usize, usize, (usize, usize))> = Vec::new();
    for (y, s) in rows.iter().enumerate() {
        let Some(s) = s else { continue };
        match runs.last_mut() {
            Some((_, end, span)) if *end + 1 == y && span.0.abs_diff(s.0) <= 1 && span.1.abs_diff(s.1) <= 1 => *end = y,
            _ => runs.push((y, y, *s)),
        }
    }
    runs.into_iter().filter(|(a, b, _)| b - a >= 8).map(|(a, b, s)| ((a + b) / 2, s)).collect()
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
        let picture = Arc::new((0..W * H).flat_map(|_| [0.2f32, 0.2, 0.2, 1.0]).collect());
        Self { cpu, gpu: has_gpu.then_some(gpu), picture }
    }

    fn cpu(&mut self, s: &Sequence) -> Vec<u8> {
        klipp_effects::linear_f32_rgba_to_srgb_u8(&self.cpu.render(s, tc(1.0), W, H, &mut Grey(Arc::clone(&self.picture)), RenderOptions::default()))
    }

    /// The CPU's frame, checked against the GPU stack's when there is one:
    /// every backing pixel (away from the letters) within a level.
    fn both(&mut self, name: &str, s: &Sequence) -> Vec<u8> {
        let cpu = self.cpu(s);
        if let Some(gpu) = self.gpu.as_mut() {
            let (stack, _) = gpu.render_display_u8_on_gpu(s, tc(1.0), W, H, &mut Grey(Arc::clone(&self.picture)), RenderOptions::default()).expect("the GPU stack draws the title");
            let w = W as usize;
            // Away from the letters: no white within 4 pixels.
            let near_letters = |i: usize| {
                let (x, y) = ((i % w) as i64, (i / w) as i64);
                (-4..=4).any(|dy| (-4..=4).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0 && ny >= 0 && nx < W as i64 && ny < H as i64 && cpu[(ny as usize * w + nx as usize) * 4] > 120
                }))
            };
            let off: Vec<usize> = (0..w * H as usize).filter(|&i| !near_letters(i) && (0..3).any(|c| cpu[i * 4 + c].abs_diff(stack[i * 4 + c]) > 1)).collect();
            assert!(off.is_empty(), "{name}: {} backing pixels differ by more than a level between the CPU and the GPU stack, the first at {:?}", off.len(), off.first().map(|i| (i % w, i / w)));
        }
        cpu
    }
}

#[test]
fn each_line_has_its_own_box_set_by_its_alignment() {
    let mut r = Renderers::new();
    let picture = r.cpu(&seq(None, TextAlignment::Center, LineBoxes::default(), false));
    let text = "Short\nA much longer middle line\nEnd";
    for align in [TextAlignment::Left, TextAlignment::Center, TextAlignment::Right] {
        let got = r.both(&format!("{align:?}"), &seq(Some(text), align, LineBoxes::default(), false));
        let b = boxes(&spans(&got, &picture));
        assert_eq!(b.len(), 3, "{align:?}: three boxes, one a line: {b:?}");
        let width = |n: usize| b[n].1 .1 - b[n].1 .0;
        assert!(width(1) > width(0) + 150 && width(0) > width(2), "{align:?}: each as wide as its line: {b:?}");
        let (l, c, rt) = (|n: usize| b[n].1 .0, |n: usize| b[n].1 .0 + b[n].1 .1, |n: usize| b[n].1 .1);
        match align {
            TextAlignment::Left => assert!(l(0).abs_diff(l(1)) <= 2 && l(1).abs_diff(l(2)) <= 2, "left edges together: {b:?}"),
            TextAlignment::Right => assert!(rt(0).abs_diff(rt(1)) <= 2 && rt(1).abs_diff(rt(2)) <= 2, "right edges together: {b:?}"),
            _ => assert!(c(0).abs_diff(c(1)) <= 3 && c(1).abs_diff(c(2)) <= 3, "centred together: {b:?}"),
        }
        // Joined: no grey between the lines down a column every box
        // covers.
        let w = W as usize;
        let x = (b.iter().map(|b| b.1 .0).max().unwrap() + b.iter().map(|b| b.1 .1).min().unwrap()) / 2;
        assert!((b[0].0..=b[2].0).all(|y| !grey(&got, y * w + x, &picture)), "{align:?}: the boxes joined into one shape");
    }
}

#[test]
fn joined_boxes_round_their_inside_corners_and_a_gap_parts_them() {
    let mut r = Renderers::new();
    let picture = r.cpu(&seq(None, TextAlignment::Center, LineBoxes::default(), false));
    let w = W as usize;
    let text = "Hi\nA wider line";
    // Just past the narrow box's edge, inside the wide box's width: the
    // first row the backing covers. A fillet in the inside corner covers
    // rows above the wide box's top edge there; a square corner doesn't.
    let first_covered = |r: &mut Renderers, radius: f32| {
        let got = r.both(&format!("radius {radius}"), &seq(Some(text), TextAlignment::Center, LineBoxes { radius, ..LineBoxes::default() }, false));
        let b = boxes(&spans(&got, &picture));
        assert_eq!(b.len(), 2, "two boxes: {b:?}");
        let x = b[0].1 .1 + 2;
        (0..H as usize).find(|&y| dark(&got, y * w + x)).expect("the wide box under it")
    };
    let (rounded, square) = (first_covered(&mut r, 12.0), first_covered(&mut r, 0.0));
    assert!(rounded + 2 <= square, "rounded: the inside corner filled in above the wide box, from row {rounded}; square from {square}");
    // Lines far apart, joined: the boxes still meet, no grey between.
    let got = r.both("spaced", &spaced(Some(text), TextAlignment::Center, LineBoxes::default(), false, 2.0));
    let b = boxes(&spans(&got, &picture));
    let x = (b[0].1 .0 + b[0].1 .1) / 2;
    assert!((b[0].0..b[1].0).all(|y| !grey(&got, y * w + x, &picture)), "joined across a wide line spacing");
    // Apart by a gap: grey between the lines.
    let got = r.both("gap", &seq(Some(text), TextAlignment::Center, LineBoxes { gap: 10.0, ..LineBoxes::default() }, false));
    let rows = spans(&got, &picture);
    let b = boxes(&rows);
    let x = (b[0].1 .0 + b[0].1 .1) / 2;
    let parted = (b[0].0..b[1].0).filter(|&y| grey(&got, y * w + x, &picture)).count();
    assert!(parted >= 3, "a gap of 10 at 1080p parts them: {parted} grey rows");
}

#[test]
fn vertical_text_has_a_box_per_column() {
    let mut r = Renderers::new();
    let picture = r.cpu(&seq(None, TextAlignment::Center, LineBoxes::default(), false));
    let got = r.both("vertical", &seq(Some("ABCD\nEF"), TextAlignment::Center, LineBoxes { gap: 10.0, ..LineBoxes::default() }, true));
    let w = W as usize;
    // Down the middle of the frame's height, the changed columns fall in
    // two runs: one a column of text.
    let y = (0..H as usize).filter(|&y| (0..w).any(|x| !grey(&got, y * w + x, &picture))).min().expect("the title") + 10;
    let changed: Vec<bool> = (0..w).map(|x| !grey(&got, y * w + x, &picture)).collect();
    let runs = changed.windows(2).filter(|p| !p[0] && p[1]).count() + usize::from(changed[0]);
    assert_eq!(runs, 2, "two column boxes across row {y}");
}
