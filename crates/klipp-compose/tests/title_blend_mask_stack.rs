//! A title whose layers blend and mask one another (#1513) is the same
//! picture in the viewer and in an export: the GPU stack, which places
//! the title's resting render or takes it rastered, against the CPU
//! renderer an export runs, at rest and while a layer moves.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_motion::{Motion, MotionKind};
use klipp_core::{BlendMode, ClipId, RationalTime, Timecode};
use klipp_timeline::*;

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
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

fn rect(td: &mut TitleData, fill: [u8; 4], centre: (f32, f32), size: (f32, f32)) -> TitleObjectId {
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: centre.0, y: centre.1 };
    xf.size = TitlePosition { x: size.0, y: size.1 };
    td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some(fill), gradient: None, stroke: None, fit: None }), xf)
}

/// A band, a word screened over it that rises in, and a stripe shown
/// only inside the word, multiplied.
fn title() -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    rect(&mut td, [30, 60, 200, 230], (0.5, 0.5), (0.9, 0.5));
    let mut xf = default_text_transform();
    xf.size = TitlePosition { x: 1.0, y: 1.0 };
    let word = td.add_object(TitleObjectContent::TextBox(TextBox { text: "Blend".into(), font_size: 260.0, color: [240, 200, 60, 255], ..Default::default() }), xf);
    td.object_mut(word).unwrap().blend = BlendMode::Screen;
    td.object_mut(word).unwrap().fx_mut().motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8));
    let stripe = rect(&mut td, [255, 120, 40, 255], (0.5, 0.45), (1.0, 0.15));
    td.object_mut(stripe).unwrap().blend = BlendMode::Multiply;
    td.object_mut(stripe).unwrap().mask = Some(LayerMask { source: word, kind: LayerMaskKind::Alpha });
    td
}

/// How far the viewer may be from the export at `t` for the rising word
/// here. While it moves, the GPU draws the posed glyphs' coverage from
/// their outlines and the CPU scene from their bitmaps; at a partial alpha
/// the two land a code apart, which shows as two levels on a few pixels
/// over the picture. At rest both draw the bitmaps (#2203).
fn rising_or_rest(t: f64) -> i32 {
    if t < 0.8 { 2 } else { 1 }
}

#[test]
fn a_blended_masked_title_is_the_same_in_the_viewer_and_the_export() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(title()), 0.0, 4.0)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(b.id, picture());
    base.clips = vec![b];
    seq.tracks = vec![text, base];

    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    // Rising (rastered), then at rest (the resting render placed).
    for t in [0.2, 0.5, 2.0, 3.0] {
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
            eprintln!("skipping: the GPU stack declined (no usable GPU?)");
            return;
        };
        let cpu = display(&r.render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= rising_or_rest(t), "at {t} s the viewer differs from the export by {d} levels");
        // And the frame is the mixed one, not the layers laid flat.
        let mut flat = seq.clone();
        if let ClipKind::Title(td) = &mut flat.tracks[0].clips[0].kind {
            for o in &mut td.objects {
                o.blend = BlendMode::Normal;
                o.mask = None;
            }
        }
        let flat = display(&r.render(&flat, tc(t), W, H, &mut src, RenderOptions::default()));
        assert!(worst(&cpu, &flat) > 40, "at {t} s the blends and the mask show");
    }
}

/// A word screened over a band as it rises in, with no mask: the same in
/// the viewer and the export, and not cut at its resting box on the way
/// up — a lone screened word, over nothing of its title, is the same
/// word laid normally, which is placed whole.
#[test]
fn a_screened_word_rising_over_a_band_is_whole_in_the_viewer_and_the_export() {
    let scene = |band: bool, blend: BlendMode| {
        let mut td = title();
        td.objects.remove(2);
        if !band {
            td.objects.remove(0);
        }
        td.objects.iter_mut().find(|o| matches!(o.content, TitleObjectContent::TextBox(_))).unwrap().blend = blend;
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut text = Track::new("Text", TrackKind::Text);
        text.clips = vec![clip(ClipKind::Title(td), 0.0, 4.0)];
        let mut base = Track::new("V1", TrackKind::Video);
        let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
        let mut src = Flat::default();
        src.0.insert(b.id, picture());
        base.clips = vec![b];
        seq.tracks = vec![text, base];
        (seq, src)
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    for t in [0.2, 0.5, 2.0] {
        let (seq, mut src) = scene(true, BlendMode::Screen);
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
            eprintln!("skipping: the GPU stack declined (no usable GPU?)");
            return;
        };
        let cpu = display(&r.render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= rising_or_rest(t), "at {t} s the viewer differs from the export by {d} levels");
        let (screened, mut src) = scene(false, BlendMode::Screen);
        let screened = display(&r.render(&screened, tc(t), W, H, &mut src, RenderOptions::default()));
        let (normal, mut src) = scene(false, BlendMode::Normal);
        let normal = display(&r.render(&normal, tc(t), W, H, &mut src, RenderOptions::default()));
        let d = worst(&screened, &normal);
        assert!(d <= 1, "at {t} s the rising word screened over nothing differs from it laid normally by {d} levels");
    }
}
