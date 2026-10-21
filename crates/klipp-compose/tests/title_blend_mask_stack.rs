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
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None };
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
        { let mut n=0; for (i,(a,b)) in cpu.chunks(4).zip(gpu.chunks(4)).enumerate() { if a.iter().zip(b).any(|(x,y)|(*x as i32-*y as i32).abs()>1) { if n<6 {eprintln!("DIFF t={t} x={} y={} cpu={a:?} gpu={b:?}", i as u32%W, i as u32/W);} n+=1; } } eprintln!("DIFF t={t} count={n}"); }
        let scope = klipp_effects::residency::frame("blend", t); let _ = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()); drop(scope); eprintln!("EVENTS t={t} {:?}", klipp_effects::residency::last_frame().events.iter().map(|e|(e.feature,e.reason)).collect::<Vec<_>>());
        // The viewer composes the objects on the GPU and the export blends
        // them on the CPU (#2148); they round apart by up to two levels at
        // a rising glyph's edge.
        assert!(d <= 2, "at {t} s the viewer differs from the export by {d} levels");
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
