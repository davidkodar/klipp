//! Where titles still raster on the CPU while they play (#2335): every
//! built-in template, every motion and loop, and the layer styles, each
//! played for three seconds at 24 fps on the GPU stack at 1080p, with
//! each frame's `Fallback` events counted and the frame's time charged
//! to them. Ranked by the time it costs, frames × cost, so what happens
//! every frame of a common title comes first.
//!
//! A measurement, not a gate: `cargo test -p klipp-compose --test
//! title_fallback_tally -- --ignored --nocapture`. Its own binary as it
//! times itself. The time is the frame's work on this thread, which a
//! CPU raster is; GPU passes run on after it.

use std::collections::BTreeMap;
use std::time::Instant;

use klipp_compose::{DisplayDelivery, FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::title_motion::{Motion, MotionKind, TitleLoop};
use klipp_core::{Interpolation, Keyframe, RationalTime};
use klipp_effects::residency::{self, Work};
use klipp_timeline::*;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 24;
const SECONDS: f64 = 3.0;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn text() -> TextBox {
    TextBox { text: "Playing title".into(), font_size: 150.0, ..Default::default() }
}

fn styles() -> LayerFx {
    LayerFx {
        glows: vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 15.0, radius_keyframes: vec![key(0.0, 8.0), key(2.0, 24.0)], intensity: 0.9, intensity_keyframes: vec![] }],
        shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 4.0, offset_y: 6.0, blur: 5.0, offset_x_keyframes: vec![], offset_y_keyframes: vec![], blur_keyframes: vec![] }],
        outlines: vec![TextOutline { enabled: true, color: [240, 245, 255, 230], width: 3.5, width_keyframes: vec![] }],
        ..Default::default()
    }
}

/// What is played: the templates as a user adds them, then each motion,
/// loop and style alone on a plain text.
fn corpus() -> Vec<(String, TitleData)> {
    let mut out: Vec<(String, TitleData)> = title_library::TITLES.iter().map(|t| (format!("template {}", t.name), t.build())).collect();
    for m in MotionKind::ALL {
        let fx = LayerFx { motion: TitleMotion::enter(Motion::new(m).duration(1.0)), ..Default::default() };
        out.push((format!("motion {m:?}"), TitleData::single_text_fx(text(), fx)));
    }
    for (name, l) in TitleLoop::presets() {
        let mut t = TitleData::single_text(text());
        t.objects[0].fx_mut().loops = vec![l];
        out.push((format!("loop {name}"), t));
    }
    out.push(("styles flat".into(), TitleData::single_text_fx(text(), styles())));
    out.push(("styles rising".into(), TitleData::single_text_fx(text(), LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)), ..styles() })));
    let mut solid = text();
    let mut d = Text3d::default();
    d.keys.rotate_y = vec![key(0.0, -25.0), key(2.0, 25.0)];
    solid.three_d = Some(d);
    out.push(("3d keyed".into(), TitleData::single_text_fx(solid, styles())));
    let mut keyed = text();
    keyed.font_size_keyframes = vec![key(0.0, 140.0), key(2.0, 180.0)];
    out.push(("keyed size + styles".into(), TitleData::single_text_fx(keyed, styles())));
    let mut tracking = text();
    tracking.tracking_keyframes = vec![key(0.0, 0.0), key(2.0, 8.0)];
    out.push(("keyed tracking".into(), TitleData::single_text(tracking)));
    let mut glitch = text();
    glitch.glitch_intensity = 0.7;
    out.push(("glitch".into(), TitleData::single_text(glitch)));
    let mut chroma = text();
    chroma.chromatic_aberration_px = 6.0;
    out.push(("chromatic".into(), TitleData::single_text(chroma)));
    // Styles that stay as they are, with the layer moving.
    let still_styles = || LayerFx { glows: vec![TextGlow { radius_keyframes: vec![], ..styles().glows[0].clone() }], ..styles() };
    let mut moving = TitleData::single_text_fx(text(), still_styles());
    moving.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.3), key(3.0, 0.7)];
    out.push(("keyed position + still styles".into(), moving));
    out.push(("still styles rising".into(), TitleData::single_text_fx(text(), LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(1.0)), ..still_styles() })));
    let mut plain = TitleData::single_text(text());
    plain.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.3), key(3.0, 0.7)];
    out.push(("keyed position".into(), plain));
    let mut scaled = TitleData::single_text(text());
    scaled.objects[0].transform.size_x_keyframes = vec![key(0.0, 0.6), key(3.0, 0.9)];
    out.push(("keyed box size".into(), scaled));
    let mut turning = TitleData::single_text(text());
    turning.objects[0].transform.rotation_keyframes = vec![key(0.0, -10.0), key(3.0, 10.0)];
    out.push(("keyed rotation".into(), turning));
    let mut faded = TitleData::single_text(text());
    faded.objects[0].opacity_keyframes = vec![key(0.0, 0.0), key(1.0, 1.0)];
    out.push(("keyed opacity".into(), faded));
    out.push(("styles fading".into(), TitleData::single_text_fx(text(), LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Fade).duration(1.0)), ..styles() })));
    let mut moving = TitleData::single_text_fx(text(), styles());
    moving.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.3), key(3.0, 0.7)];
    out.push(("keyed position + styles".into(), moving));
    out
}

#[derive(Default)]
struct Tally {
    frames: u64,
    ms: f64,
    fixtures: BTreeMap<String, u64>,
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn tally_title_fallbacks_while_playing() {
    let mut by_reason: BTreeMap<(String, String), Tally> = BTreeMap::new();
    let (mut all_frames, mut all_ms, mut fallback_frames) = (0u64, 0.0f64, 0u64);
    for (name, title) in corpus() {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut track = Track::new("Text", TrackKind::Text);
        track.clips = vec![Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, FPS), RationalTime::from_secs_f64(SECONDS + 1.0, FPS)) }];
        seq.tracks = vec![track];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        if r.executor().gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            eprintln!("skipping: no GPU");
            return;
        }
        for f in 0..(SECONDS * FPS as f64) as u64 {
            let at = RationalTime::from_secs_f64(f as f64 / FPS as f64, FPS);
            let t = Instant::now();
            let placed = r.render_display_on_gpu(&seq, at, W, H, &mut NoMedia, RenderOptions::default(), DisplayDelivery { texture: None, bytes: false });
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            all_frames += 1;
            all_ms += ms;
            let events = residency::last_frame().events;
            let mut reasons: Vec<(String, String)> = events.iter().filter(|e| e.work == Work::Fallback).map(|e| (e.feature.to_string(), e.reason.to_string())).collect();
            if placed.is_none() {
                reasons.push(("stack".into(), "the GPU stack declined the frame".into()));
            }
            reasons.sort();
            reasons.dedup();
            if !reasons.is_empty() {
                fallback_frames += 1;
            }
            for k in reasons {
                let e = by_reason.entry(k).or_default();
                e.frames += 1;
                e.ms += ms;
                *e.fixtures.entry(name.clone()).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<_> = by_reason.into_iter().collect();
    ranked.sort_by(|a, b| b.1.ms.total_cmp(&a.1.ms));
    println!("TALLY {all_frames} frames, {fallback_frames} with a fallback, {:.0} ms in all, {W}x{H}", all_ms);
    println!("TALLY | rank | feature | reason | frames | ms (frames × cost) | ms/frame | fixtures (frames) |");
    for (i, ((feature, reason), t)) in ranked.iter().enumerate() {
        let mut fx: Vec<_> = t.fixtures.iter().collect();
        fx.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = fx.iter().take(6).map(|(n, c)| format!("{n} ({c})")).collect();
        let more = if fx.len() > 6 { format!(", +{} more", fx.len() - 6) } else { String::new() };
        println!("TALLY | {} | {feature} | {reason} | {} | {:.0} | {:.1} | {}{more} |", i + 1, t.frames, t.ms, t.ms / t.frames as f64, top.join(", "));
    }
}
