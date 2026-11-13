//! A shape with a loop that moves it (a shake, a float, a sway, a wave, a
//! scroll, a crawl, a drift) is its painted layer posed on the GPU, offset
//! by the loop as the CPU offsets it, by the layer's own painted width and
//! height (#3167): the same picture, with no CPU raster. Moving loops on a
//! shape fell back to rastering the whole title on the CPU every frame.

use std::time::Instant;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::title_motion::{Motion, MotionKind, TitleLoop};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::residency::Work;
use klipp_timeline::*;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn moves(l: &TitleLoop) -> bool {
    matches!(l, TitleLoop::Shake { .. } | TitleLoop::Wave { .. } | TitleLoop::Scroll { .. } | TitleLoop::Crawl { .. } | TitleLoop::Float { .. } | TitleLoop::Sway { .. } | TitleLoop::Drift { .. })
}

/// A lower third's bar: a rounded rectangle, with `fx`.
fn bar(fx: LayerFx) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: 0.35, y: 0.78 };
    xf.size = TitlePosition { x: 0.5, y: 0.12 };
    let id = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 12.0 }, fill: Some([40, 120, 200, 255]), gradient: None, stroke: None, fit: None }), xf);
    *td.object_mut(id).unwrap().fx_mut() = fx;
    td
}

#[test]
fn a_moving_loop_poses_a_shape_on_the_gpu() {
    let (w, h) = (640, 360);
    let loops: Vec<_> = TitleLoop::presets().into_iter().filter(|(_, l)| moves(l)).collect();
    assert!(loops.len() >= 7, "the moving loops are there: {:?}", loops.iter().map(|(n, _)| *n).collect::<Vec<_>>());
    let mut probe = renderer(true);
    if probe.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return;
    }
    let mut failures = Vec::new();
    for (name, l) in loops {
        for (rising, glowing) in [(false, false), (true, false), (false, true)] {
            let motion = if rising { TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)) } else { TitleMotion::default() };
            // A glow round it, dressed before the pose as the CPU dresses it.
            let glows = if glowing { vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 12.0, radius_keyframes: vec![], intensity: 0.9, intensity_keyframes: vec![] }] } else { Vec::new() };
            let title = bar(LayerFx { loops: vec![l.clone()], motion, glows, ..Default::default() });
            let case = format!("{name}{}{}", if rising { " rising" } else { "" }, if glowing { " glowing" } else { "" });
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width: w, height: h };
            let mut track = Track::new("title", TrackKind::Video);
            track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(6.0, 30)) });
            seq.tracks = vec![track];
            let (mut gpu, mut cpu) = (renderer(true), renderer(false));
            let (mut fallbacks, mut worst, mut ms) = (0usize, 0u8, Vec::new());
            for t in [0.3, 0.7, 1.3, 2.2, 3.1, 4.6] {
                let at = RationalTime::from_secs_f64(t, 30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
                let started = Instant::now();
                let Some((got, _)) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()) else {
                    failures.push(format!("{case} at {t}: the stack declined the frame"));
                    continue;
                };
                ms.push(started.elapsed().as_secs_f64() * 1000.0);
                fallbacks += klipp_effects::residency::last_frame().events.iter().filter(|e| e.work == Work::Fallback).count();
                worst = worst.max(got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0));
            }
            ms.sort_by(f64::total_cmp);
            eprintln!("{case:<16} fallbacks {fallbacks}  worst {worst} levels  GPU frame ms median {:.2} max {:.2}", ms.get(ms.len() / 2).copied().unwrap_or(0.0), ms.last().copied().unwrap_or(0.0));
            if fallbacks > 0 {
                failures.push(format!("{case}: {fallbacks} CPU raster fallbacks"));
            }
            if worst > 1 {
                failures.push(format!("{case}: the GPU differs from the CPU by {worst} levels"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The poses there were before a loop could move a shape stay as they
/// were: the loop's offset rides in the pose's spare parameter, zero for
/// every one of these, and a shape moved by a pixel more or less than the
/// CPU moves it is off by most of a level's range at its edges, so any
/// change to how these round is seen here.
#[test]
fn a_shape_without_a_loop_moves_as_it_did() {
    let (w, h) = (640, 360);
    let mut probe = renderer(true);
    if probe.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return;
    }
    let kinds = [MotionKind::Rise, MotionKind::Drop, MotionKind::SlideLeft, MotionKind::SlideRight, MotionKind::FlyTop, MotionKind::Bounce, MotionKind::ScaleUp, MotionKind::Pop, MotionKind::MaskRise, MotionKind::Fade];
    let mut failures = Vec::new();
    for kind in kinds {
        for exit in [false, true] {
            let m = Motion::new(kind).duration(1.0);
            let motion = if exit { TitleMotion { enter: None, exit: Some(m) } } else { TitleMotion::enter(m) };
            let title = bar(LayerFx { motion, ..Default::default() });
            let case = format!("{kind:?} {}", if exit { "exit" } else { "enter" });
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width: w, height: h };
            let mut track = Track::new("title", TrackKind::Video);
            track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(4.0, 30)) });
            seq.tracks = vec![track];
            let (mut gpu, mut cpu) = (renderer(true), renderer(false));
            for t in [0.2, 0.45, 0.7, 2.0, 3.3, 3.55, 3.8] {
                let at = RationalTime::from_secs_f64(t, 30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
                let Some((got, _)) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()) else {
                    failures.push(format!("{case} at {t}: the stack declined the frame"));
                    continue;
                };
                let fallbacks = klipp_effects::residency::last_frame().events.iter().filter(|e| e.work == Work::Fallback).count();
                let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                if fallbacks > 0 || worst > 1 {
                    failures.push(format!("{case} at {t}: {fallbacks} fallbacks, {worst} levels from the CPU"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
