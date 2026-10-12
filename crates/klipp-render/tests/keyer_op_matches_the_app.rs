//! #1170: a clip keyed by the `effect` op is keyed as the app keys it.
//! The op's chroma key, and the app's Chroma Key added as its effects
//! browser adds it (the same defaults), render the same alpha on the
//! same picture: the green keyed out, the rest kept.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_document::ops::{resolve, Op};
use klipp_timeline::{Clip, Sequence};
use std::sync::Arc;

const W: u32 = 32;
const H: u32 = 16;

/// The left half pure green, the right half red, in linear light.
struct GreenAndRed(Arc<Vec<f32>>);

impl GreenAndRed {
    fn new() -> Self {
        let mut px = Vec::with_capacity((W * H * 4) as usize);
        for _y in 0..H {
            for x in 0..W {
                px.extend_from_slice(if x < W / 2 { &[0.0, 1.0, 0.0, 1.0] } else { &[1.0, 0.0, 0.0, 1.0] });
            }
        }
        Self(Arc::new(px))
    }
}

impl FrameSource for GreenAndRed {
    fn frame(&mut self, _clip: &Clip, _secs: f64, _w: u32, _h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(self.0.clone()), width: W, height: H, pts: None })
    }
}

fn alpha(seq: &Sequence) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let img = r.render(seq, klipp_core::Timecode::from_secs_f64(1.0, 25), W, H, &mut GreenAndRed::new(), RenderOptions { keep_alpha: true, ..Default::default() });
    img.chunks_exact(4).map(|p| p[3]).collect()
}

#[test]
fn a_chroma_key_by_op_renders_as_the_apps() {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/keyer-fixture.mov".into(), None);
    item.video_codec = "h264".into();
    let media = pool.add(item);
    let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let clip = Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let clip_id = clip.id;
    seq.tracks[v].clips.push(clip);

    // By the op, at its defaults.
    let ops: Vec<Op> = serde_json::from_value(serde_json::json!([{ "op": "effect", "clip_id": clip_id.0.to_string(), "kind": "green_screen" }])).unwrap();
    let plan = resolve(&seq, &pool, &ops).unwrap();
    let (mut seqs, mut p, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
    klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps }.apply_forward(&plan.steps[0].edit);
    let by_op = seqs.remove(0);

    // As the app adds its Chroma Key: the effects browser's defaults.
    let app_key = klipp_effects::BuiltinEffect::ChromaKey {
        key_color: [0, 255, 0],
        tolerance: 0.2,
        soft_edge: 0.1,
        spill_method: klipp_effects::SpillMethod::ChannelClamp,
        spill_strength: 0.6,
    };
    let mut in_app = seq.clone();
    let c = in_app.tracks[v].clips.iter_mut().find(|c| c.id == clip_id).unwrap();
    c.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&app_key)]).expect("an effects edit").expect("applied");

    let (a_op, a_app) = (alpha(&by_op), alpha(&in_app));
    assert_eq!(a_op, a_app, "the op's key is the app's");
    let at = |x: u32, y: u32| a_op[(y * W + x) as usize];
    assert!(at(4, 8) < 0.05, "the green is keyed out: alpha {}", at(4, 8));
    assert!(at(W - 4, 8) > 0.95, "the red is kept: alpha {}", at(W - 4, 8));
}
