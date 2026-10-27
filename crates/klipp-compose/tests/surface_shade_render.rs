//! A layer pinned to a surface takes the surface's shade across it and
//! its texture (#2751): over a wall lit on one side and in shade on the
//! other, a flat grey layer is lighter over the lit side and darker over
//! the shaded one, on the CPU route and the GPU stack alike.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, SurfacePin, Track, TrackKind};

const W: u32 = 128;
const H: u32 = 72;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// The wall: lit on its left, in shade on its right, with a fine grain;
/// any other clip a flat grey.
struct Wall(ClipId);

impl FrameSource for Wall {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let wall = clip.id == self.0;
        let px = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    if !wall {
                        return [0.5, 0.5, 0.5, 1.0];
                    }
                    let lit = if x < width / 2 { 0.6 } else { 0.15 };
                    let v = lit * if (x / 2 + y / 2) % 2 == 0 { 1.15 } else { 0.85 };
                    [v, v, v, 1.0]
                })
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width, height, pts: None })
    }
}

fn media() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) }
}

/// The grey layer, pinned with `shade` and `texture`, over the wall.
fn scene(shade: f32, texture: f32) -> (Sequence, ClipId) {
    let wall = media();
    let mut layer = media();
    layer.surface_pin = Some(SurfacePin { host: wall.id, surface: klipp_core::ClipId::new().0, picture_size: None, fit: Default::default(), inset: 0.0, on_loss: Default::default(), light: 0.0, shade, texture });
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = [("V2", &layer), ("V1", &wall)]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = vec![clip.clone()];
            t
        })
        .into();
    (seq, wall.id)
}

/// The mean of a block on each side of the wall, CPU then GPU.
fn render(shade: f32, texture: f32) -> [[f32; 2]; 2] {
    let (seq, wall) = scene(shade, texture);
    let mut src = Wall(wall);
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        let mean = |xs: std::ops::Range<u32>| {
            let mut sum = 0.0;
            for y in 20..52 {
                for x in xs.clone() {
                    sum += img[((y * W + x) * 4) as usize];
                }
            }
            sum / (32 * xs.len()) as f32
        };
        [mean(10..50), mean(78..118)]
    })
}

#[test]
fn a_pinned_layer_takes_the_surfaces_shade_on_both_routes() {
    let [[flat_lit, flat_dark], _] = render(0.0, 0.0);
    assert!((flat_lit - flat_dark).abs() < 1e-3, "without it, the grey is even: {flat_lit} {flat_dark}");
    let [cpu, gpu] = render(1.0, 0.5);
    for (path, [lit, dark]) in [("CPU", cpu), ("GPU", gpu)] {
        assert!(lit > flat_lit * 1.1 && dark < flat_dark * 0.75, "{path}: lighter where lit, darker in shade: {lit} {dark} (flat {flat_lit})");
    }
    assert!((cpu[0] - gpu[0]).abs() < 4e-3 && (cpu[1] - gpu[1]).abs() < 4e-3, "CPU and GPU agree: {cpu:?} vs {gpu:?}");
    // And the GPU's is the stack's own, not a fallback to the CPU.
    let (seq, wall) = scene(1.0, 0.5);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let drawn = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Wall(wall), RenderOptions::default());
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    assert!(drawn.is_some() || !required, "required, but the GPU stack declined the pinned layer");
}
