//! What the picture sweeps share: one clip on a timeline with ops applied
//! by the document, as the app applies them, rendered on the export's CPU
//! path over a synthetic scene; and the measures taken of it.

#![allow(dead_code)]

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_document::apply::DocumentView;
use klipp_document::ops::{self, Op};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use serde_json::{json, Value};

pub const W: u32 = 160;
pub const H: u32 = 90;

pub fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 25)
}

/// A scene with something for every effect: a sky graded to a bright
/// sun, a face's skin, leaves, a green screen patch, stripes for blurs
/// and sharpening, and a deep shadow. Linear light.
pub struct Scene;

/// The scene's colour at `(x, y)` in `0..1`.
pub fn scene_at(x: f32, y: f32) -> [f32; 3] {
    let sun = (-((x - 0.75).powi(2) + (y - 0.2).powi(2)) * 300.0).exp() * 4.0;
    if y < 0.45 {
        let sky = [0.35 + 0.3 * y, 0.5 + 0.2 * y, 0.85];
        return sky.map(|c| c + sun);
    }
    match (x * 6.0) as u32 {
        0 => [0.55, 0.30, 0.20],
        1 => [0.10, 0.16, 0.03],
        2 => [0.05, 0.60, 0.08],
        3 => if ((x * W as f32) as u32 / 3).is_multiple_of(2) { [0.8; 3] } else { [0.05; 3] },
        4 => [0.02, 0.02, 0.025],
        _ => [0.4, 0.25 + 0.3 * (y - 0.45), 0.1],
    }
}

impl FrameSource for Scene {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let px: Vec<f32> = (0..width * height)
            .flat_map(|i| {
                let c = scene_at((i % width) as f32 / width as f32, (i / width) as f32 / height as f32);
                [c[0], c[1], c[2], 1.0]
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width, height, pts: None })
    }
}

/// A four-second sequence of one picture clip, and its media pool.
pub fn fixture() -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/sweep-scene.mov".into(), None);
    item.duration = tc(4.0);
    item.video_codec = "h264".into();
    let media = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    seq.tracks = vec![v];
    (seq, pool)
}

/// `ops` applied to the fixture's clip (`clip_id` filled in where an op
/// has none), or why they were refused.
pub fn with_ops(ops_json: &[Value]) -> Result<Sequence, String> {
    let (seq, mut pool) = fixture();
    let id = seq.tracks[0].clips[0].id.0.to_string();
    let list: Vec<Value> = ops_json
        .iter()
        .map(|op| {
            let mut op = op.clone();
            if op.get("clip_id").is_none() && op.get("on").is_none() {
                op["clip_id"] = json!(id);
            }
            op
        })
        .collect();
    let ops: Vec<Op> = serde_json::from_value(Value::Array(list)).map_err(|e| e.to_string())?;
    let plan = ops::resolve(&seq, &pool, &ops).map_err(|r| r.why)?;
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    Ok(sequences.remove(0))
}

/// The sequence's frame at `secs` on the export's CPU path, linear RGBA.
pub fn render(seq: &Sequence, secs: f64) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r.render(seq, tc(secs), W, H, &mut Scene, RenderOptions::default())
}

/// Linear light to display, roughly as a viewer shows it.
pub fn display(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

/// The mean difference of two frames as shown, over RGB, 0..1.
pub fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    let (mut s, mut n) = (0.0f64, 0usize);
    for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
        for c in 0..3 {
            s += (display(pa[c]) - display(pb[c])).abs() as f64;
            n += 1;
        }
    }
    (s / n.max(1) as f64) as f32
}

/// Every value a number.
pub fn finite(a: &[f32]) -> bool {
    a.iter().all(|v| v.is_finite())
}

/// The mean of a frame's channel `c` as shown.
pub fn mean_channel(a: &[f32], c: usize) -> f32 {
    a.chunks(4).map(|p| display(p[c])).sum::<f32>() / (a.len() / 4).max(1) as f32
}

/// The share of pixels whose shown colour changed by more than two levels.
pub fn changed_share(a: &[f32], b: &[f32]) -> f32 {
    let moved = a.chunks(4).zip(b.chunks(4)).filter(|(pa, pb)| (0..3).any(|c| (display(pa[c]) - display(pb[c])).abs() > 2.0 / 255.0)).count();
    moved as f32 / (a.len() / 4).max(1) as f32
}

