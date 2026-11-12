//! A title whose resting render was made ahead on the warm-up thread is
//! the same picture as one rendered cold, and the same as the CPU
//! renderer an export uses: warming a title changes when its raster is
//! made, never what it shows.

#[path = "support/cpu_reference.rs"]
mod cpu_reference;
use cpu_reference::{no_gpu, CpuReference};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_motion::{EdgeFade, FocusBand, RollShape};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, LayerFx, Sequence, TextBox, TitleData, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture() -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[fx * 0.8, fy * 0.6, (fx + fy) * 0.35, 1.0]);
        }
    }
    Arc::new(v)
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(
        linear,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    )
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// `td` over a picture, from 0 s to 5 s.
fn over_a_picture(td: TitleData) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(b.id, picture());
    base.clips = vec![b];
    seq.tracks = vec![text, base];
    (seq, src)
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

struct Drawn {
    bytes: Vec<u8>,
    /// Resting renders the frame made on this thread.
    rasters: u64,
    /// Warmed renders taken in before the frame.
    adopted: usize,
}

/// The frame at `at`, on the GPU stack or the CPU renderer, from a
/// renderer that warmed `warm` first (none: cold), as the engine warms a
/// title ahead of its first frame. `None` when the stack declined.
fn draw(seq: &Sequence, src: &mut Flat, at: f64, warm: Option<&TitleData>, gpu: bool) -> Option<Drawn> {
    let mut r = renderer();
    r.set_gpu_enabled(gpu);
    let mut adopted = 0;
    if let Some(td) = warm {
        r.warm_titles(vec![(td.clone(), W, H, 5.0)]);
        // Each key the warm-up was given comes back; quiet for half a
        // second after the last, it has made them all.
        let until = Instant::now() + Duration::from_secs(20);
        let mut last = Instant::now();
        while Instant::now() < until && last.elapsed() < Duration::from_millis(500) {
            let n = r.adopt_warm_titles();
            if n > 0 {
                adopted += n;
                last = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let before = r.title_renderer().resting_renders();
    let bytes = if gpu {
        r.render_display_u8_on_gpu(seq, tc(at), W, H, src, RenderOptions::default())?.0
    } else {
        display(&r.render(seq, tc(at), W, H, src, RenderOptions::default()))
    };
    Some(Drawn { bytes, rasters: r.title_renderer().resting_renders() - before, adopted })
}

fn faded(focus: bool) -> TitleData {
    TitleData::single_text_fx(
        TextBox { text: "Faded at\nthe edges\nof the\nframe".into(), font_size: 52.0, ..Default::default() },
        LayerFx { loops: if focus { vec![roll()] } else { Vec::new() }, edge_fade: EdgeFade { top: 0.3, bottom: 0.3, ..EdgeFade::NONE }, ..Default::default() },
    )
}

/// A roll standing still, its focus band gone soft.
fn roll() -> klipp_timeline::TitleLoop {
    klipp_timeline::TitleLoop::Scroll {
        direction: klipp_timeline::ScrollDirection::Up,
        speed_px_per_sec: 0.0,
        shape: RollShape { focus: Some(FocusBand { band: 0.3, edge_opacity: 0.3, blur: 8.0 }) },
    }
}

/// `td` warmed and cold at its first frame and a later one, on the GPU
/// stack or the CPU renderer: the warmed frame is the cold one, and both
/// are export's; on the stack, the first frame rasters nothing on the
/// frame's thread.
fn warmed_is_cold(name: &str, td: &TitleData, gpu: bool) {
    let (seq, mut src) = over_a_picture(td.clone());
    let route = if gpu { "GPU stack" } else { "CPU" };
    for at in [0.0, 1.0] {
        let Some(cold) = draw(&seq, &mut src, at, None, gpu) else {
            no_gpu(&format!("{name}: the GPU stack declined (no usable GPU?)"));
            return;
        };
        let warmed = draw(&seq, &mut src, at, Some(td), gpu).expect("the stack drew it cold");
        let export = display(&renderer().cpu_render(&seq, tc(at), W, H, &mut src, RenderOptions::default()));
        eprintln!(
            "{name} ({route}) at {at}s: adopted {}, rasters warmed {} cold {}; warmed vs cold {} levels, warmed vs export {}, cold vs export {}",
            warmed.adopted,
            warmed.rasters,
            cold.rasters,
            worst(&warmed.bytes, &cold.bytes),
            worst(&warmed.bytes, &export),
            worst(&cold.bytes, &export),
        );
        // The warm-up makes the first frame's renders, for the stack
        // playback draws on.
        if gpu && at == 0.0 {
            assert_eq!(warmed.rasters, 0, "{name} ({route}): warmed, the first frame still rastered {} resting renders", warmed.rasters);
        }
        let d = worst(&warmed.bytes, &cold.bytes);
        assert!(d <= 1, "{name} ({route}) at {at}s: the warmed frame differs from the cold one by {d} levels");
        let d = worst(&warmed.bytes, &export);
        assert!(d <= 2, "{name} ({route}) at {at}s: the warmed frame differs from export's by {d} levels");
    }
}

#[test]
fn a_warmed_title_with_an_edge_fade_is_faded_once() {
    for gpu in [true, false] {
        warmed_is_cold("edge fade", &faded(false), gpu);
    }
}

#[test]
fn a_warmed_roll_with_a_focus_band_is_faded_once() {
    for gpu in [true, false] {
        warmed_is_cold("focus band", &faded(true), gpu);
    }
}

/// An edge-faded text with a hidden layer beside it: one layer shown, so
/// its fade is left to where its resting render is placed, warmed or
/// not, and it is faded once.
#[test]
fn a_warmed_title_with_a_hidden_layer_is_faded_once() {
    let mut td = faded(false);
    let mut hidden = TitleData::single_text(TextBox { text: "Hidden".into(), font_size: 40.0, ..Default::default() }).objects.remove(0);
    hidden.visible = false;
    hidden.id = klipp_timeline::TitleObjectId(td.next_object_id);
    td.next_object_id += 1;
    td.objects.push(hidden);
    for gpu in [true, false] {
        warmed_is_cold("hidden layer", &td, gpu);
    }
}

/// Two texts fading in, one a roll with a focus band: drawn layer by
/// layer, each layer its own resting render, the roll's band left to
/// where it is placed. Warmed, those are the renders made.
#[test]
fn a_warmed_title_drawn_layer_by_layer_is_faded_once() {
    let fade_in = klipp_timeline::Motion { kind: klipp_timeline::MotionKind::Fade, duration_secs: 0.5, ..Default::default() };
    let mut td = faded(true);
    let mut second = TitleData::single_text_fx(
        TextBox { text: "Under it".into(), font_size: 40.0, ..Default::default() },
        LayerFx { motion: klipp_timeline::TitleMotion { enter: Some(fade_in), exit: None }, ..Default::default() },
    )
    .objects
    .remove(0);
    second.transform.center.y = 0.85;
    td.objects[0].fx_mut().edge_fade = EdgeFade::NONE;
    td.objects[0].fx_mut().motion.enter = Some(fade_in);
    second.id = klipp_timeline::TitleObjectId(td.next_object_id);
    td.next_object_id += 1;
    td.objects.push(second);
    for gpu in [true, false] {
        warmed_is_cold("layer by layer", &td, gpu);
    }
}

/// Every built-in title template, warmed, rasters nothing at its first
/// frame on the GPU stack, and shows what it shows cold on the stack and
/// on the CPU renderer. Most are a shape and text, or several texts: the stack places
/// each object on its own, so the warm-up makes each object's render,
/// not the whole title's.
#[test]
fn every_warmed_template_rasters_nothing_at_its_first_frame() {
    for t in klipp_timeline::title_library::TITLES.iter() {
        let td = t.data();
        for gpu in [true, false] {
            warmed_is_cold(&t.name, &td, gpu);
        }
    }
}
