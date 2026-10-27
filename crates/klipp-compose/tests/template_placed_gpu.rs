//! Precompositions on the GPU stack (#2757): a precomposition's scene is
//! composed on the stack and seeds its layer, with no fallback, and draws
//! what the CPU path draws; a composition template placed as one clip
//! draws what its composition draws with the same value set by hand.
//! `precomposition_fps_at_4k` reads the frame rate of a 4K timeline with
//! one precomposition. A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

struct Nothing;

impl FrameSource for Nothing {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

/// A card: a headline on a coloured backing.
fn card(text: &str) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 120.0, ..Default::default() });
    td.background = TitleBackground::Color { color: [30, 90, 200, 255], padding: 24.0 };
    let mut t = Track::new("T1", TrackKind::Text);
    t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(3.0)) }];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![t];
    seq
}

/// `clip` laid in a sequence of its own, flattened with `composition`.
fn outer(clip: Clip, composition: &Sequence) -> Sequence {
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![v];
    klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), composition.clone()])
}

fn on_gpu(seq: &Sequence) -> Option<Vec<u8>> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()).map(|(px, _)| px)
}

fn on_cpu(seq: &Sequence) -> Vec<u8> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render(seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()))
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// A plain precomposition: its scene composed on the stack, no fallback,
/// drawn as the CPU path and the scene on its own draw it.
#[test]
fn a_precomposition_is_composed_on_the_gpu() {
    let scene = card("Launch");
    let flat = outer(Clip::precomposition(scene.id, tc(0.0), tc(3.0)), &scene);
    let Some(gpu) = on_gpu(&flat) else {
        assert!(!required(), "required, but the GPU stack declined the precomposition");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let frame = klipp_effects::residency::last_frame();
    assert_eq!(frame.fallback_count(), 0, "composed on the GPU: {:?}", frame.events);
    let alone = on_gpu(&scene).expect("the scene alone");
    assert!(worst(&gpu, &alone) <= 2, "as the scene draws on its own: {} levels", worst(&gpu, &alone));
    let cpu = on_cpu(&flat);
    assert!(worst(&gpu, &cpu) <= 3, "as the CPU path draws it: {} levels", worst(&gpu, &cpu));
}

/// The frame rate of a 4K timeline with one precomposition on the stack,
/// read over a run of frames after a warm one.
#[test]
fn precomposition_fps_at_4k() {
    let (w, h) = (3840u32, 2160u32);
    let mut scene = card("Launch");
    scene.resolution = klipp_core::Resolution { width: w, height: h };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::precomposition(scene.id, tc(0.0), tc(3.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    seq.tracks = vec![v];
    let flat = klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), scene.clone()]);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    if r.render_display_u8_on_gpu(&flat, tc(0.5), w, h, &mut Nothing, RenderOptions::default()).is_none() {
        assert!(!required(), "required, but the GPU stack declined the 4K precomposition");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    }
    let frames = 24;
    let t = std::time::Instant::now();
    for f in 0..frames {
        let at = tc(0.5 + f as f64 / 24.0);
        assert!(r.render_display_u8_on_gpu(&flat, at, w, h, &mut Nothing, RenderOptions::default()).is_some(), "frame {f} on the stack");
        assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "frame {f} without fallback");
    }
    let fps = frames as f64 / t.elapsed().as_secs_f64();
    eprintln!("4K timeline with one precomposition: {fps:.1} fps (display bytes read back each frame)");
    assert!(fps > 5.0, "{fps:.1} fps");
}

#[test]
fn a_placed_template_draws_its_composition_with_the_value_set() {
    // The template: the card, its headline a parameter.
    let made = card("Launch");
    let instance = klipp_core::TemplateInstance {
        template_id: klipp_core::SequenceId::new().0,
        bindings: [("text".to_string(), "object:0.text".to_string())].into_iter().collect(),
        overrides: [("text".to_string(), klipp_core::TemplateValue::Text("Ship it".into()))].into_iter().collect(),
        media_slots: Default::default(),
    };
    let mut placed = Clip::precomposition(made.id, tc(0.0), tc(3.0));
    placed.kind = ClipKind::TemplatePrecomposition { sequence_id: made.id, template_instance: instance };
    let Some(placed_px) = on_gpu(&outer(placed, &made)) else {
        assert!(!required(), "required, but the GPU stack declined the placed template");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let frame = klipp_effects::residency::last_frame();
    // The same card with the headline typed in, laid in as it is.
    let typed = card("Ship it");
    let reference = on_gpu(&outer(Clip::precomposition(typed.id, tc(0.0), tc(3.0)), &typed)).expect("the stack drew the card");
    let worst = placed_px.iter().zip(&reference).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 2, "the placed template differs from its composition by {worst} levels");
    let blue = placed_px.chunks(4).filter(|p| p[2] > 150 && p[0] < 100).count();
    assert!(blue > 1000, "the card is drawn: {blue} pixels of its backing");
    assert_eq!(frame.fallback_count(), 0, "composed on the GPU: {:?}", frame.events);
}

/// Every media clip a flat orange.
struct Orange;

impl FrameSource for Orange {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let px = [0.8f32, 0.3, 0.05, 1.0].repeat((width * height) as usize);
        Some(SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(px)), width, height, pts: None })
    }
}

/// #2757: a template's media slot, empty, shows its placeholder; filled,
/// the media given; on the GPU stack, no fallback either way.
#[test]
fn a_slot_shows_its_placeholder_until_it_is_filled() {
    let mut scene = Sequence::new_default();
    scene.resolution = klipp_core::Resolution { width: W, height: H };
    scene.tracks[0].clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(3.0), tc(0.0), tc(3.0))];
    let placed = |media: Option<klipp_core::MediaId>| {
        let instance = klipp_core::TemplateInstance {
            template_id: klipp_core::SequenceId::new().0,
            bindings: [("slot_1".to_string(), "clip:0.media".to_string())].into_iter().collect(),
            overrides: Default::default(),
            media_slots: media.map(|m| ("slot_1".to_string(), m.0.to_string())).into_iter().collect(),
        };
        let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
        clip.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
        outer(clip, &scene)
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((empty, _)) = r.render_display_u8_on_gpu(&placed(None), tc(1.0), W, H, &mut Orange, RenderOptions::default()) else {
        assert!(!required(), "required, but the GPU stack declined the slot");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "empty: on the GPU");
    let mid = ((H / 2 * W + W / 2) * 4) as usize;
    let p = klipp_timeline::flatten::SLOT_PLACEHOLDER;
    assert!((0..3).all(|c| (empty[mid + c] as i32 - p[c] as i32).abs() <= 2), "the placeholder: {:?}", &empty[mid..mid + 3]);
    let (filled, _) = r.render_display_u8_on_gpu(&placed(Some(klipp_core::MediaId::new())), tc(1.0), W, H, &mut Orange, RenderOptions::default()).expect("the stack drew the filled slot");
    assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "filled: on the GPU");
    assert!(filled[mid] > 200 && filled[mid + 2] < 90, "the media given: {:?}", &filled[mid..mid + 3]);
}
