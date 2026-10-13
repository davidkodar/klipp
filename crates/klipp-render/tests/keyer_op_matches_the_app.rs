//! #1170: a clip keyed by the `effect` op is keyed as the app keys it.
//! The op's chroma key, and the app's Chroma Key added as its effects
//! browser adds it (the same defaults), render the same alpha on the
//! same picture: the green keyed out, the rest kept. #1638: the same for
//! the luma key, on a ramp from black to white.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_document::ops::{resolve, Op};
use klipp_timeline::{Clip, Sequence};
use std::sync::Arc;

const W: u32 = 32;
const H: u32 = 16;

/// A picture in linear light, the same every frame.
struct Picture(Arc<Vec<f32>>);

impl Picture {
    /// The left half pure green, the right half red.
    fn green_and_red() -> Self {
        let mut px = Vec::with_capacity((W * H * 4) as usize);
        for _y in 0..H {
            for x in 0..W {
                px.extend_from_slice(if x < W / 2 { &[0.0, 1.0, 0.0, 1.0] } else { &[1.0, 0.0, 0.0, 1.0] });
            }
        }
        Self(Arc::new(px))
    }
}

impl FrameSource for Picture {
    fn frame(&mut self, _clip: &Clip, _secs: f64, _w: u32, _h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(self.0.clone()), width: W, height: H, pts: None })
    }
}

/// A grey ramp across the frame, black at the left to white at the
/// right, evenly spaced as encoded (as a waveform reads it), in linear
/// light.
fn black_to_white() -> Picture {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for _y in 0..H {
        for x in 0..W {
            let e = x as f32 / (W - 1) as f32;
            let v = if e <= 0.040_45 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) };
            px.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    Picture(Arc::new(px))
}

fn alpha(seq: &Sequence) -> Vec<f32> {
    alpha_of(seq, &mut Picture::green_and_red())
}

fn alpha_of(seq: &Sequence, source: &mut Picture) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let img = r.render(seq, klipp_core::Timecode::from_secs_f64(1.0, 25), W, H, source, RenderOptions { keep_alpha: true, ..Default::default() });
    img.chunks_exact(4).map(|p| p[3]).collect()
}

/// One video clip on a fresh sequence, and its pool.
fn one_clip() -> (Sequence, klipp_media::MediaPool, usize, klipp_core::ClipId) {
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
    (seq, pool, v, clip_id)
}

/// The sequence after `op` (JSON), as the assistant runs it.
fn by_op(seq: &Sequence, pool: &klipp_media::MediaPool, op: serde_json::Value) -> Sequence {
    let ops: Vec<Op> = serde_json::from_value(serde_json::json!([op])).unwrap();
    let plan = resolve(seq, pool, &ops).unwrap();
    let (mut seqs, mut p, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
    klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps }.apply_forward(&plan.steps[0].edit);
    seqs.remove(0)
}

#[test]
fn a_luma_key_by_op_renders_as_the_apps() {
    let (seq, pool, v, clip_id) = one_clip();
    let op = by_op(&seq, &pool, serde_json::json!({ "op": "effect", "clip_id": clip_id.0.to_string(), "kind": "luma_key" }));

    // As the app adds its Luma Key: the effects browser's defaults.
    let app_key = klipp_effects::BuiltinEffect::LumaKey { low: 0.0, high: 0.1, soft: 0.05, invert: false };
    let mut in_app = seq.clone();
    let c = in_app.tracks[v].clips.iter_mut().find(|c| c.id == clip_id).unwrap();
    c.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&app_key)]).expect("an effects edit").expect("applied");

    let (a_op, a_app) = (alpha_of(&op, &mut black_to_white()), alpha_of(&in_app, &mut black_to_white()));
    assert_eq!(a_op, a_app, "the op's key is the app's");
    let at = |x: u32| a_op[(H / 2 * W + x) as usize];
    assert!(at(0) < 0.01, "black is keyed out: alpha {}", at(0));
    assert!(at(W - 1) > 0.99, "white is kept: alpha {}", at(W - 1));
    // The soft edge: 0.1 to 0.15 as encoded is the fourth and fifth
    // columns of 32, part keyed.
    assert!(at(4) > 0.01 && at(4) < 0.99, "the softness ramps it back in: alpha {}", at(4));
    // Control: without the key the ramp is opaque throughout, so the
    // alphas above are the key's.
    assert!(alpha_of(&seq, &mut black_to_white()).iter().all(|a| *a > 0.99));
}

#[test]
fn a_chroma_key_by_op_renders_as_the_apps() {
    let (seq, pool, v, clip_id) = one_clip();

    // By the op, at its defaults.
    let by_op = by_op(&seq, &pool, serde_json::json!({ "op": "effect", "clip_id": clip_id.0.to_string(), "kind": "green_screen" }));

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
