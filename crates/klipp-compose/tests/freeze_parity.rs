//! A frozen track's layer laid in its place draws the frame the live track
//! draws (#3207): its clip's opacity and blend mode, and the track's
//! opacity, applied live over the frozen layer. Measured with the layer
//! stored as 8-bit sRGB with 8-bit alpha and as half floats, against the
//! live frame, in 8-bit display levels: half floats hold every case within
//! a level; 8-bit does not (up to 6 levels where a graded, blurred clip is
//! laid by Screen at 60%), so frozen frames are stored as half floats.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{BlendMode, RationalTime, Timecode, TransitionAlignment, TransitionId, TransitionKind, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind, Transition};

const W: u32 = 96;
const H: u32 = 64;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn media(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) }
}

fn title(text: &str, from: f64, to: f64) -> Clip {
    let td = TitleData::single_text(TextBox { text: text.into(), font_size: 28.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) }
}

/// A Transform: scaled by `scale`, moved by (`x`, `y`) pixels, turned
/// `degrees`.
fn transform(scale: f32, x: f32, y: f32, degrees: f32) -> BuiltinEffect {
    BuiltinEffect::Transform {
        position_x: x,
        position_y: y,
        scale_x: scale,
        scale_y: scale,
        rotation: degrees,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

/// Each clip its own picture: a ramp turned by the clip's id, so two clips
/// never look alike, with detail for a blur or a transform to move.
struct Pictures;

impl FrameSource for Pictures {
    fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let seed = (c.id.0.as_u128() % 97) as f32 / 97.0;
        let px: Vec<f32> = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                    let checker = if (x / 6 + y / 6) % 2 == 0 { 0.15 } else { 0.0 };
                    [(u * 0.8 + seed * 0.2 + checker).min(1.0), (v * 0.7 + (secs as f32 * 0.1).fract() * 0.3).min(1.0), (0.2 + seed * 0.6 - checker).max(0.0), 1.0]
                })
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r.set_working_colorspace(WorkingColorspace::Rec709Linear);
    r
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The layer as the SDR format stores it: straight alpha, colour encoded
/// to 8-bit sRGB, alpha to 8 bits.
fn as_8bit(layer: &[f32]) -> Vec<f32> {
    layer
        .chunks_exact(4)
        .flat_map(|p| {
            let q = |c: f32| srgb_decode((srgb_encode(c) * 255.0).round() / 255.0);
            [q(p[0]), q(p[1]), q(p[2]), (p[3].clamp(0.0, 1.0) * 255.0).round() / 255.0]
        })
        .collect()
}

/// The layer as half floats.
fn as_f16(layer: &[f32]) -> Vec<f32> {
    layer.iter().map(|v| half::f16::from_f32(*v).to_f32()).collect()
}

/// The worst difference between two 8-bit RGBA frames, colour channels.
fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.chunks_exact(4).zip(b.chunks_exact(4)).flat_map(|(x, y)| (0..3).map(move |k| (x[k] as i32 - y[k] as i32).abs())).max().unwrap_or(0)
}

/// `project[0]` with track `frozen` live and laid from its frozen layer,
/// at each of `times`: the worst difference with the layer stored 8-bit
/// and as half floats.
fn measure(name: &str, project: &[Sequence], frozen: usize, times: &[f64]) -> (i32, i32) {
    let seq = klipp_timeline::flatten::flatten_sequence(&project[0], project);
    let (mut w8, mut w16) = (0, 0);
    for &t in times {
        let mut r = renderer();
        let live = display(&r.render(&seq, tc(t), W, H, &mut Pictures, RenderOptions::default()));
        let layer = r.render_track_layer(&seq, frozen, tc(t), W, H, &mut Pictures).unwrap_or_else(|| panic!("{name} at {t}: a layer to freeze"));
        for (store, worst_so_far) in [(as_8bit(&layer), &mut w8), (as_f16(&layer), &mut w16)] {
            let mut r = renderer();
            r.set_frozen_layers(HashMap::from([(frozen, Arc::new(store))]));
            let laid = display(&r.render(&seq, tc(t), W, H, &mut Pictures, RenderOptions::default()));
            *worst_so_far = (*worst_so_far).max(worst(&live, &laid));
        }
    }
    eprintln!("MEASURE {name}: frozen vs live, worst {w8} levels stored 8-bit, {w16} as half floats");
    (w8, w16)
}

fn over_a_clip(top: Clip) -> Vec<Sequence> {
    vec![sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![media(0.0, 4.0)])])]
}

/// Every kind of content a frozen track can hold, over a live clip.
fn cases() -> Vec<(String, Vec<Sequence>, Vec<f64>)> {
    let mut out: Vec<(String, Vec<Sequence>, Vec<f64>)> = Vec::new();
    let times = vec![0.5, 1.3];

    // Graded and with effects, laid half see-through by Screen.
    let mut graded = TestClip::of(media(0.0, 4.0))
        .effects(vec![BuiltinEffect::GaussianBlur { radius: 2.0 }, BuiltinEffect::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }, BuiltinEffect::Opacity { value: 0.6 }])
        .build();
    graded.blend_mode = BlendMode::Screen;
    out.push(("graded clip with effects, 60% by Screen".into(), over_a_clip(graded), times.clone()));

    // Scaled down and moved: an edge of partial alpha over the clip below.
    let moved = TestClip::of(media(0.0, 4.0)).effects(vec![transform(0.55, 9.0, -5.0, 12.0)]).build();
    out.push(("scaled, turned and moved".into(), over_a_clip(moved), times.clone()));

    // Multiply, its opacity keyed: applied live over the frozen layer.
    let mut keyed = TestClip::of(media(0.0, 4.0)).lane("opacity", 0.0, 0.2).lane("opacity", 2.0, 0.9).build();
    keyed.blend_mode = BlendMode::Multiply;
    out.push(("Multiply, opacity keyed".into(), over_a_clip(keyed), times.clone()));

    // A title, its text's soft edges over the clip.
    out.push(("title".into(), over_a_clip(title("Frozen", 0.0, 4.0)), times.clone()));

    // A generator.
    let generator = Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 80, 20, 200] }), ..Clip::gap(tc(0.0), tc(4.0)) };
    out.push(("generator".into(), over_a_clip(TestClip::of(generator).effects(vec![transform(0.5, 0.0, 0.0, 0.0)]).build()), times.clone()));

    // A cross dissolve between two clips with effects.
    let a = TestClip::of(media(0.0, 2.0)).effects(vec![BuiltinEffect::Invert { mix: 0.5 }]).build();
    let b = TestClip::of(media(2.0, 4.0)).effects(vec![transform(0.7, 0.0, 0.0, 0.0)]).build();
    let mut t = track("V2", TrackKind::Video, vec![a.clone(), b.clone()]);
    t.transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, duration: tc(1.0), clip_a_id: a.id, clip_b_id: b.id, user_params: Default::default(), alignment: TransitionAlignment::Center });
    out.push(("cross dissolve".into(), vec![sequence(vec![t, track("V1", TrackKind::Video, vec![media(0.0, 4.0)])])], vec![1.75, 2.2]));

    // The track's own effects, baked onto its clip for the render.
    let mut with_track_fx = track("V2", TrackKind::Video, vec![TestClip::of(media(0.0, 4.0)).effects(vec![transform(0.6, 4.0, 0.0, 0.0)]).build()]);
    with_track_fx.effects.video_effect_list = klipp_timeline::TrackEffectList::new(vec![BuiltinEffect::Invert { mix: 0.6 }], vec![true]);
    out.push(("track effects".into(), vec![sequence(vec![with_track_fx, track("V1", TrackKind::Video, vec![media(0.0, 4.0)])])], times.clone()));

    // A precomposition: a title over a clip, scaled as one picture.
    let scene = sequence(vec![track("T1", TrackKind::Text, vec![title("Pre", 0.0, 4.0)]), track("V1", TrackKind::Video, vec![media(0.0, 4.0)])]);
    let pre = TestClip::of(Clip::precomposition(scene.id, tc(0.0), tc(4.0))).effects(vec![transform(0.6, -6.0, 3.0, 0.0)]).build();
    let mut project = over_a_clip(pre);
    project.push(scene);
    out.push(("precomposition".into(), project, times));

    out
}

/// Frozen versus live for every kind of content, the frozen layer stored
/// 8-bit and as half floats; the track's opacity is a live control too.
#[test]
fn a_frozen_layer_lays_the_frame_the_live_track_draws() {
    let mut results = Vec::new();
    for (name, mut project, times) in cases() {
        project[0].tracks[0].opacity = 0.8;
        results.push((name.clone(), measure(&name, &project, 0, &times)));
    }
    for (name, (_, w16)) in &results {
        assert!(*w16 <= 1, "{name}: half floats {w16} levels from live");
    }
    let w8 = results.iter().map(|(_, (w8, _))| *w8).max().unwrap_or(0);
    eprintln!("MEASURE worst over every case: {w8} levels stored 8-bit");
}

/// A layer as a picture to compare: its colour premultiplied by its alpha
/// and display-encoded, and its alpha, 8 bits each.
fn layer_picture(layer: &[f32]) -> Vec<u8> {
    let premultiplied: Vec<f32> = layer.chunks_exact(4).flat_map(|p| [p[0] * p[3], p[1] * p[3], p[2] * p[3], 1.0]).collect();
    let mut out = display(&premultiplied);
    for (px, p) in out.chunks_exact_mut(4).zip(layer.chunks_exact(4)) {
        px[3] = (p[3].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    out
}

/// The freeze draws a track's layer on the GPU stack (GPU-first), as the
/// CPU draws it: within a level, colour and alpha, for every kind of
/// content; laid in the frame, it draws the live frame.
#[test]
fn the_gpu_stack_draws_the_layer_the_cpu_draws() {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    gpu.set_working_colorspace(WorkingColorspace::Rec709Linear);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some() || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
    if gpu.executor().gpu_pipeline_or_init().is_none() {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    for (name, mut project, times) in cases() {
        project[0].tracks[0].opacity = 0.8;
        let seq = klipp_timeline::flatten::flatten_sequence(&project[0], &project);
        for t in times {
            let cpu = renderer().render_track_layer(&seq, 0, tc(t), W, H, &mut Pictures).expect("the CPU's layer");
            let on_gpu = gpu.render_track_layer_on_gpu(&seq, 0, tc(t), W, H, &mut Pictures).unwrap_or_else(|| panic!("{name} at {t}: the stack declined: {:?}", gpu.last_decline()));
            let (a, b) = (layer_picture(&cpu), layer_picture(&on_gpu));
            let apart = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
            eprintln!("MEASURE {name} at {t}: GPU layer vs CPU layer, worst {apart} levels");
            assert!(apart <= 1, "{name} at {t}: the GPU's layer {apart} levels from the CPU's");
            let mut r = renderer();
            let live = display(&r.render(&seq, tc(t), W, H, &mut Pictures, RenderOptions::default()));
            r.set_frozen_layers(HashMap::from([(0, Arc::new(as_f16(&on_gpu)))]));
            let laid = display(&r.render(&seq, tc(t), W, H, &mut Pictures, RenderOptions::default()));
            assert!(worst(&live, &laid) <= 1, "{name} at {t}: the GPU's frozen layer laid {} levels from live", worst(&live, &laid));
        }
    }
}
