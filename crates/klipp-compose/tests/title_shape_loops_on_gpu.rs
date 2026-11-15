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

/// A logo mark: a closed, filled and stroked path, with `fx`.
fn mark(fx: LayerFx) -> TitleData {
    use klipp_core::vector_path::{PathContour, PathPose, PathVertex};
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let v = |x: f32, y: f32| PathVertex { point: [x, y], incoming: [x, y], outgoing: [x, y] };
    let curve = PathVertex { point: [0.9, 0.55], incoming: [0.95, 0.3], outgoing: [0.85, 0.9] };
    let pose = PathPose { stroke_glow: None, fill: [0.85, 0.4, 0.15, 1.0], stroke: [1.0, 0.95, 0.8, 1.0], stroke_width: 5.0, contours: vec![PathContour { closed: true, vertices: vec![v(0.1, 0.2), v(0.55, 0.05), curve, v(0.4, 0.95), v(0.05, 0.7)] }] };
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: 0.3, y: 0.4 };
    xf.size = TitlePosition { x: 0.22, y: 0.3 };
    let id = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(VectorPath { trim: None, pose, keyframes: vec![] }), fill: None, gradient: None, stroke: None, fit: None }), xf);
    *td.object_mut(id).unwrap().fx_mut() = fx;
    td
}

/// `title` over six seconds against the CPU at `times`: its CPU raster
/// fallbacks, its worst level apart and its GPU frame times, as failures.
fn against_the_cpu(case: &str, title: TitleData, times: &[f64], failures: &mut Vec<String>) {
    against_the_cpu_in(klipp_core::WorkingColorspace::Rec709Linear, Expect::GPU, case, title, times, failures)
}

/// What a case must show: at most `levels` apart from the CPU, and drawn
/// with no CPU raster unless `raster` allows it.
#[derive(Clone, Copy)]
struct Expect {
    levels: u8,
    raster: bool,
}

impl Expect {
    const GPU: Expect = Expect { levels: 1, raster: false };
    /// An image layer: its sampled picture's alpha division on the GPU is
    /// within 2.5 ULP, so a channel on a half may round one byte the other
    /// way, two levels laid over what is below (the image layer's own
    /// tolerance, #2342).
    const IMAGE: Expect = Expect { levels: 2, raster: false };
    /// Kept with the title's raster: the same picture, however drawn.
    const RASTER: Expect = Expect { levels: 1, raster: true };
}

/// [`against_the_cpu`] with both renderers working in `ws`, held to `expect`.
fn against_the_cpu_in(ws: klipp_core::WorkingColorspace, expect: Expect, case: &str, title: TitleData, times: &[f64], failures: &mut Vec<String>) {
    let (w, h) = (640, 360);
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("title", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(6.0, 30)) });
    seq.tracks = vec![track];
    let (mut gpu, mut cpu) = (renderer(true), renderer(false));
    gpu.set_working_colorspace(ws);
    cpu.set_working_colorspace(ws);
    let (mut fallbacks, mut worst, mut ms, mut reasons) = (0usize, 0u8, Vec::new(), Vec::<String>::new());
    for &t in times {
        let at = RationalTime::from_secs_f64(t, 30);
        let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
        let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
        let started = Instant::now();
        let Some((got, _)) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()) else {
            failures.push(format!("{case} at {t}: the stack declined the frame"));
            continue;
        };
        ms.push(started.elapsed().as_secs_f64() * 1000.0);
        for e in klipp_effects::residency::last_frame().events.iter().filter(|e| e.work == Work::Fallback) {
            fallbacks += 1;
            let why = format!("{}: {}", e.feature, e.reason);
            if !reasons.contains(&why) {
                reasons.push(why);
            }
        }
        worst = worst.max(got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0));
    }
    ms.sort_by(f64::total_cmp);
    eprintln!("{case:<24} fallbacks {fallbacks}  worst {worst} levels  GPU frame ms median {:.2} max {:.2}", ms.get(ms.len() / 2).copied().unwrap_or(0.0), ms.last().copied().unwrap_or(0.0));
    if fallbacks > 0 && !expect.raster {
        failures.push(format!("{case}: {fallbacks} CPU raster fallbacks ({})", reasons.join("; ")));
    }
    if worst > expect.levels {
        failures.push(format!("{case}: the GPU differs from the CPU by {worst} levels"));
    }
}

fn gpu_or_skip() -> bool {
    let mut probe = renderer(true);
    if probe.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return false;
    }
    true
}

fn glow() -> TextGlow {
    TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 12.0, radius_keyframes: vec![], intensity: 0.9, intensity_keyframes: vec![] }
}

/// A path shape with a moving loop is posed on the GPU as a rectangle is:
/// moved by the loop, by its own painted width and height.
#[test]
fn a_moving_loop_poses_a_path_on_the_gpu() {
    if !gpu_or_skip() {
        return;
    }
    let mut failures = Vec::new();
    for (name, l) in TitleLoop::presets().into_iter().filter(|(_, l)| moves(l)) {
        for (rising, glowing) in [(false, false), (true, false), (false, true)] {
            let motion = if rising { TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)) } else { TitleMotion::default() };
            let glows = if glowing { vec![glow()] } else { Vec::new() };
            let case = format!("path {name}{}{}", if rising { " rising" } else { "" }, if glowing { " glowing" } else { "" });
            against_the_cpu(&case, mark(LayerFx { loops: vec![l.clone()], motion, glows, ..Default::default() }), &[0.3, 0.7, 1.3, 2.2, 3.1, 4.6], &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A path shape's stack, a glow, a shadow or an outline, is dressed on
/// the GPU round its painted coverage, as a rectangle's is.
#[test]
fn a_styled_path_is_dressed_on_the_gpu() {
    if !gpu_or_skip() {
        return;
    }
    let shadow = TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 6.0, offset_x_keyframes: vec![], offset_y: 8.0, offset_y_keyframes: vec![], blur: 6.0, blur_keyframes: vec![] };
    let outline = TextOutline { enabled: true, color: [20, 20, 20, 255], width: 4.0, width_keyframes: vec![] };
    let mut failures = Vec::new();
    for style in ["glow", "shadow", "outline"] {
        for rising in [false, true] {
            let mut fx = LayerFx::default();
            match style {
                "glow" => fx.glows = vec![glow()],
                "shadow" => fx.shadows = vec![shadow.clone()],
                _ => fx.outlines = vec![outline.clone()],
            }
            if rising {
                fx.motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0));
            }
            against_the_cpu(&format!("path {style}{}", if rising { " rising" } else { "" }), mark(fx), &[0.3, 0.7, 2.2], &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A dressed shape's entrance or exit travels by its own painted height
/// and turns about its own painted box, as the CPU poses it: its glow or
/// shadow goes with it, not into the measure.
#[test]
fn a_dressed_shape_moves_by_its_painted_box() {
    if !gpu_or_skip() {
        return;
    }
    let shadow = TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 6.0, offset_x_keyframes: vec![], offset_y: 8.0, offset_y_keyframes: vec![], blur: 6.0, blur_keyframes: vec![] };
    let mut failures = Vec::new();
    for kind in [MotionKind::Rise, MotionKind::SlideLeft, MotionKind::ScaleUp, MotionKind::Pop, MotionKind::MaskRise] {
        for (style, path) in [("glow", false), ("shadow", false), ("glow", true), ("shadow", true)] {
            let mut fx = LayerFx { motion: TitleMotion::enter(Motion::new(kind).duration(1.0)), ..Default::default() };
            if style == "glow" {
                fx.glows = vec![glow()];
            } else {
                fx.shadows = vec![shadow.clone()];
            }
            let title = if path { mark(fx) } else { bar(fx) };
            against_the_cpu(&format!("{} {style} {kind:?}", if path { "path" } else { "bar" }), title, &[0.2, 0.45, 0.7, 2.0], &mut failures);
        }
    }
    // A mask with a loop: the title is never still, so the CPU keeps the
    // mask's rows after the reveal too; with an exit to come, before it.
    let float = TitleLoop::presets().into_iter().find(|(_, l)| matches!(l, TitleLoop::Float { .. })).unwrap().1;
    for (looped, motion) in [(true, TitleMotion::enter(Motion::new(MotionKind::MaskRise).duration(1.0))), (false, TitleMotion::both(Motion::new(MotionKind::Rise).duration(1.0), Motion::new(MotionKind::MaskDrop).duration(1.0)))] {
        for path in [false, true] {
            let fx = LayerFx { motion, glows: vec![glow()], loops: if looped { vec![float.clone()] } else { Vec::new() }, ..Default::default() };
            let title = if path { mark(fx) } else { bar(fx) };
            against_the_cpu(&format!("{} glow mask{}", if path { "path" } else { "bar" }, if looped { " floating" } else { " exit" }), title, &[0.2, 0.7, 2.0, 3.0, 5.3, 5.8], &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A shape with a moving loop that also comes in blurred (a blur, a focus
/// or a soft rise) is posed on the GPU with its blur, moved by the loop.
#[test]
fn a_moving_loop_under_a_blurred_entrance_poses_on_the_gpu() {
    if !gpu_or_skip() {
        return;
    }
    let mut failures = Vec::new();
    for (name, l) in TitleLoop::presets().into_iter().filter(|(_, l)| moves(l)) {
        for kind in [MotionKind::Blur, MotionKind::FocusIn, MotionKind::SoftRise] {
            for path in [false, true] {
                let fx = LayerFx { loops: vec![l.clone()], motion: TitleMotion::enter(Motion::new(kind).duration(1.0)), ..Default::default() };
                let title = if path { mark(fx) } else { bar(fx) };
                against_the_cpu(&format!("{} {name} {kind:?}", if path { "path" } else { "bar" }), title, &[0.2, 0.45, 0.7, 2.2], &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A glow seen through a translucent shape is drawn on the GPU as the CPU
/// mixes it: a path's in any working space, a rectangle's in Rec.709. A
/// rectangle's wider than Rec.709 stays with the title's raster: drawn
/// here it parts from the CPU by two levels along a row of its edge.
#[test]
fn a_glow_through_a_translucent_shape_matches_the_cpu() {
    if !gpu_or_skip() {
        return;
    }
    let mut failures = Vec::new();
    for ws in [klipp_core::WorkingColorspace::Bt2020Linear, klipp_core::WorkingColorspace::AcesCG, klipp_core::WorkingColorspace::Rec709Linear] {
        for path in [false, true] {
            for (rising, looping) in [(false, false), (true, false), (false, true)] {
                let fx = LayerFx {
                    glows: vec![glow()],
                    motion: if rising { TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)) } else { TitleMotion::default() },
                    loops: if looping { vec![TitleLoop::presets().into_iter().find(|(_, l)| matches!(l, TitleLoop::Float { .. })).unwrap().1] } else { Vec::new() },
                    ..Default::default()
                };
                let mut title = if path { mark(fx) } else { bar(fx) };
                if let TitleObjectContent::Shape(shape) = &mut title.objects[0].content {
                    match &mut shape.kind {
                        ShapeKind::Path(p) => p.pose.fill[3] = 0.6,
                        _ => shape.fill = Some([40, 120, 200, 150]),
                    }
                }
                let case = format!("{ws:?} {} translucent glow{}{}", if path { "path" } else { "bar" }, if rising { " rising" } else { "" }, if looping { " floating" } else { "" });
                let expect = if path || ws == klipp_core::WorkingColorspace::Rec709Linear { Expect::GPU } else { Expect::RASTER };
                against_the_cpu_in(ws, expect, &case, title, &[0.3, 0.7, 2.2], &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A picture whose clear margins leave its painted box inside its placed
/// one: a soft disc, fading to nothing well inside its edges.
fn badge(fx: LayerFx) -> TitleData {
    let path = std::env::temp_dir().join(format!("klipp-loop-badge-{}.png", std::process::id()));
    if !path.exists() {
        let (w, h) = (96u32, 64u32);
        image::RgbaImage::from_fn(w, h, |x, y| {
            let (dx, dy) = ((x as f32 + 0.5) / w as f32 - 0.5, (y as f32 + 0.5) / h as f32 - 0.5);
            let a = ((0.36 - (dx * dx + dy * dy).sqrt()) * 9.0).clamp(0.0, 1.0);
            if a == 0.0 { image::Rgba([0, 255, 0, 0]) } else { image::Rgba([230, (x * 2) as u8, (y * 3) as u8, (a * 255.0).round() as u8]) }
        })
        .save(&path)
        .unwrap();
    }
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: 0.6, y: 0.45 };
    xf.size = TitlePosition { x: 0.3, y: 0.35 };
    let id = td.add_object(TitleObjectContent::Image(ImageLayer { path: path.to_string_lossy().into_owned(), fit: ImageFit::Contain }), xf);
    *td.object_mut(id).unwrap().fx_mut() = fx;
    td
}

/// An image layer with a moving loop is posed on the GPU, moved by the
/// loop by its painted box (its clear margins left out), as the CPU moves it.
#[test]
fn a_moving_loop_poses_an_image_on_the_gpu() {
    if !gpu_or_skip() {
        return;
    }
    let mut failures = Vec::new();
    for (name, l) in TitleLoop::presets().into_iter().filter(|(_, l)| moves(l)) {
        for rising in [false, true] {
            let motion = if rising { TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)) } else { TitleMotion::default() };
            let case = format!("image {name}{}", if rising { " rising" } else { "" });
            against_the_cpu_in(klipp_core::WorkingColorspace::Rec709Linear, Expect::IMAGE, &case, badge(LayerFx { loops: vec![l.clone()], motion, ..Default::default() }), &[0.3, 0.7, 1.3, 2.2, 3.1, 4.6], &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
