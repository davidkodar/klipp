//! The GPU stack's frame is the CPU renderer's frame: the same layers,
//! blends and opacities, encoded to display bytes, within a level.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

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

fn picture(seed: f32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[(fx + seed).fract() * 0.8, fy * 0.6, ((fx + fy) * 0.5 + seed).fract() * 0.7, 1.0]);
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

/// Two video layers with a blend mode and opacity, a title, a generator.
fn stack() -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Stack".into(), font_size: 160.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    let mut gen = Track::new("Gen", TrackKind::Video);
    let mut g = clip(ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 80, 20, 255] }), 0.0, 5.0);
    g.blend_mode = klipp_core::BlendMode::Multiply;
    g.builtin_effects = vec![klipp_effects::BuiltinEffect::Opacity { value: 0.4 }];
    gen.clips = vec![g];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    t.blend_mode = klipp_core::BlendMode::Screen;
    t.builtin_effects = vec![klipp_effects::BuiltinEffect::Opacity { value: 0.5 }];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(t.id, picture(0.3));
    src.0.insert(b.id, picture(0.0));
    top.clips = vec![t];
    base.clips = vec![b];
    seq.tracks = vec![text, gen, top, base];
    (seq, src)
}

#[test]
fn the_gpu_stack_is_the_renderers_frame() {
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        eprintln!("skipping: the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "GPU stack differs from the CPU renderer by {d} levels");
}

#[test]
fn the_gpu_stack_declines_what_it_cannot_do_yet() {
    let (mut seq, mut src) = stack();
    let source = seq.tracks[3].clips[0].id;
    seq.tracks[2].clips[0].matte_source = Some(klipp_timeline::MatteBinding {
        source_clip_id: source,
        channel: klipp_nodes::params::MatteChannel::Luminance,
        invert: false,
        feather_px: 0.0,
        choke_px: 0.0,
    });
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none());
}
