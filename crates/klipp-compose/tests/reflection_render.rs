//! A reflection's rows below its waterline mix with the track below by
//! its own blend (#2707), while the rows above keep the clip's: on the
//! CPU route and the GPU stack alike.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 20;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// Every media clip a flat grey.
#[derive(Default)]
struct Grey(HashMap<ClipId, f32>);

/// A picture with something for a ripple to move: diagonal stripes over
/// a grey, the same every frame.
struct Stripes;

impl FrameSource for Stripes {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let _ = clip;
        let px = (0..height).flat_map(|y| (0..width).flat_map(move |x| { let v = if (x / 3 + y / 2) % 2 == 0 { 0.8 } else { 0.1 }; [v, v * 0.7, 0.2, 1.0] })).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width, height, pts: None })
    }
}

impl FrameSource for Grey {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let v = *self.0.get(&clip.id)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

fn media() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) }
}

/// A still, crisp reflection of the whole layer at the frame's middle,
/// its own pixels kept below the line, mixed by `blend`.
fn reflection(blend: u32) -> BuiltinEffect {
    BuiltinEffect::Reflection {
        waterline: 0.5, gap: 0.0, ripple_amount: 0.0, ripple_size: 60.0, ripple_speed: 0.0, breakup: 0.0, smear: 0.0, fade: 0.0, opacity: 1.0, darken: 0.0, contact: 0.0,
        blend, tint: [0, 0, 0], tint_amount: 0.0, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 0.0, ripple_from: 0, seed: 0, time: 0.0,
    }
}

/// A pixel in the top half and one in the bottom half, the reflected
/// clip over a lighter one, CPU then GPU.
fn render(blend: u32) -> [[[f32; 4]; 2]; 2] {
    let top = klipp_timeline::test_clip::TestClip::of(media()).effects([reflection(blend)]).build();
    let below = media();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = [("V2", &top), ("V1", &below)]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = vec![clip.clone()];
            t
        })
        .into();
    let mut src = Grey::default();
    src.0.insert(top.id, 0.18);
    src.0.insert(below.id, 0.6);
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        [(W / 2, 3), (W / 2, H - 3)].map(|(x, y)| {
            let i = ((y * W + x) * 4) as usize;
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        })
    })
}

#[test]
fn the_rows_below_the_waterline_blend_by_the_reflections_mode() {
    let close = |a: [f32; 4], b: [f32; 4]| (0..3).all(|c| (a[c] - b[c]).abs() <= 3e-3);
    let [[normal_top, normal_bottom], _] = render(klipp_effects::reflection::NORMAL);
    assert!(close(normal_top, normal_bottom), "all normal, the opaque clip covers the frame: {normal_top:?} vs {normal_bottom:?}");
    let [cpu, gpu] = render(klipp_effects::reflection::SCREEN);
    for (path, [top, bottom]) in [("CPU", cpu), ("GPU", gpu)] {
        assert!(close(top, normal_top), "{path}: above the line, the clip as it is: {top:?} vs {normal_top:?}");
        assert!(bottom[0] > normal_bottom[0] + 0.05 && bottom[0] > 0.6, "{path}: below it, screened over the lighter track: {bottom:?}");
    }
    assert!(close(cpu[1], gpu[1]), "CPU and GPU agree below the line: {:?} vs {:?}", cpu[1], gpu[1]);
}

/// A sequence of `top` over a lower picture.
fn over(top: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v2 = Track::new("V2", if matches!(top.kind, ClipKind::Title(_)) { TrackKind::Text } else { TrackKind::Video });
    v2.clips = vec![top];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![media()];
    seq.tracks = vec![v2, v1];
    seq
}

fn frame(seq: &Sequence, secs: f64, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(seq, tc(secs), W, H, &mut Stripes, RenderOptions::default())
}

fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

/// The ripple is the timeline's position and the seed, nothing else
/// (#2707): frame N, then N+10, then N again draws N's pixels exactly,
/// on the GPU and the CPU, so scrubbing back finds the same frame; the
/// two routes agree; and from one frame to the next it moves a little,
/// never jumping, while over many frames it moves more.
#[test]
fn the_ripple_is_the_timeline_position_and_moves_smoothly() {
    let fx = klipp_effects::reflection::surface("rippled_water", 0.5).unwrap();
    let seq = over(klipp_timeline::test_clip::TestClip::of(media()).effects([fx]).build());
    let step = 1.0 / 24.0;
    let n = 1.0;
    for gpu in [false, true] {
        let first = frame(&seq, n, gpu);
        let later = frame(&seq, n + 10.0 * step, gpu);
        let again = frame(&seq, n, gpu);
        assert_eq!(first, again, "gpu {gpu}: frame N again is frame N");
        assert!(mean_diff(&first, &later) > 1e-4, "gpu {gpu}: it moves over ten frames");
    }
    for k in [0.0, 3.0, 10.0] {
        let (cpu, gpu) = (frame(&seq, n + k * step, false), frame(&seq, n + k * step, true));
        assert!(mean_diff(&cpu, &gpu) < 2e-3, "frame {k}: CPU and GPU agree: {}", mean_diff(&cpu, &gpu));
    }
    // Smooth: each step small, and none far above the others.
    let frames: Vec<Vec<f32>> = (0..12).map(|k| frame(&seq, n + k as f64 * step, true)).collect();
    let steps: Vec<f32> = frames.windows(2).map(|w| mean_diff(&w[0], &w[1])).collect();
    let typical = steps.iter().sum::<f32>() / steps.len() as f32;
    let span = mean_diff(&frames[0], &frames[11]);
    assert!(steps.iter().all(|s| *s <= typical * 2.5 + 1e-5), "no jumps: {steps:?}");
    assert!(span > typical * 1.5, "it travels further over many frames than one: {span} vs {typical}");
}

/// The reflection runs on any picture layer: a media clip, a generator
/// and a title, each changing the rows below its line, on the GPU stack
/// as on the CPU.
#[test]
fn it_reflects_a_media_clip_a_generator_and_a_title() {
    let fx = klipp_effects::reflection::surface("calm_water", 0.5).unwrap();
    let generator = Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [230, 160, 40, 255] }), ..Clip::gap(tc(0.0), tc(4.0)) };
    let title = Clip {
        kind: ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Klipp".into(), font_size: 600.0, ..Default::default() })),
        ..Clip::gap(tc(0.0), tc(4.0))
    };
    let below = |px: &[f32]| px[(W * H * 2) as usize..].to_vec();
    for (name, clip) in [("media", media()), ("generator", generator), ("title", title)] {
        let plain = over(clip.clone());
        let reflected = over(klipp_timeline::test_clip::TestClip::of(clip).effects([fx.clone()]).build());
        for gpu in [false, true] {
            let (a, b) = (frame(&plain, 1.0, gpu), frame(&reflected, 1.0, gpu));
            assert!(mean_diff(&below(&a), &below(&b)) > 1e-3, "{name}, gpu {gpu}: the rows below the line change");
        }
        let (cpu, gpu) = (frame(&reflected, 1.0, false), frame(&reflected, 1.0, true));
        assert!(mean_diff(&cpu, &gpu) < 3e-3, "{name}: CPU and GPU agree: {}", mean_diff(&cpu, &gpu));
    }
}

/// At half the sequence's size (a reduced preview), the split row takes
/// the gap at the render's size, as the reflection does: a gap of 16
/// canvas pixels is 8 rendered, and the rows from the water down blend
/// by the reflection's mode on both routes.
#[test]
fn the_split_takes_the_gap_at_the_renders_size() {
    let fx = BuiltinEffect::Reflection {
        waterline: 0.25, gap: 16.0, ripple_amount: 0.0, ripple_size: 60.0, ripple_speed: 0.0, breakup: 0.0, smear: 0.0, fade: 0.0, opacity: 1.0, darken: 0.0, contact: 0.0,
        blend: klipp_effects::reflection::SCREEN, tint: [0, 0, 0], tint_amount: 0.0, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 0.0, ripple_from: 0, seed: 0, time: 0.0,
    };
    let top = klipp_timeline::test_clip::TestClip::of(media()).effects([fx]).build();
    let below = media();
    let mut seq = Sequence::new_default();
    // The sequence twice the render's size.
    seq.resolution = klipp_core::Resolution { width: W * 2, height: H * 2 };
    seq.tracks = [("V2", &top), ("V1", &below)]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = vec![clip.clone()];
            t
        })
        .into();
    let mut src = Grey::default();
    src.0.insert(top.id, 0.18);
    src.0.insert(below.id, 0.6);
    // The water: a quarter down (5) and 8 rendered rows lower.
    let water = (H as f32 * 0.25 + 8.0) as u32;
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        let red = |y: u32| img[((y * W + W / 2) * 4) as usize];
        assert!((red(water - 1) - red(2)).abs() < 3e-3, "gpu {gpu}: above the water, the clip as it is: {} vs {}", red(water - 1), red(2));
        assert!(red(water) > red(2) + 0.05, "gpu {gpu}: from the water down, screened: {} vs {}", red(water), red(2));
    }
}
