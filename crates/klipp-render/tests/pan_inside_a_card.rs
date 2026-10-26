//! A pan on a picture composed as an inset pans inside its card (#1917),
//! measured on rendered frames: the card covers the same part of the
//! frame at every moment of the move as the inset did, the margins round
//! it stay empty, and the picture inside is the pan's: a mark at the
//! picture's centre grows with the push.

use std::sync::Arc;
use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::*;
use klipp_document::ops::{resolve, ClipRef, ComposePose, Op, PanKey};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

const W: u32 = 192;
const H: u32 = 108;

/// A grey picture with a white square at its centre, 1/8 of its width.
struct Marked;

impl FrameSource for Marked {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f32 + 0.5) / w as f32 - 0.5, (y as f32 + 0.5) / h as f32 - 0.5);
                let v = if dx.abs() < 1.0 / 16.0 && dy.abs() < 1.0 / 16.0 { 1.0 } else { 0.2 };
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn apply(seqs: &mut Vec<Sequence>, pool: &mut klipp_media::MediaPool, op: Op) {
    let plan = resolve(&seqs[0], pool, &[op]).expect("a plan");
    let mut fps = seqs[0].fps;
    let mut view = klipp_document::apply::DocumentView { sequences: seqs, active: 0, media_pool: pool, fps: &mut fps };
    for step in &plan.steps {
        view.apply_forward(&step.edit);
    }
}

/// The rendered frame's lit area (the picture over the black behind it): its bounds, and how
/// many pixels are white.
fn measure(seq: &Sequence, secs: f64) -> ((u32, u32, u32, u32), usize) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let out = r.render(seq, RationalTime::from_secs_f64(secs, 25), W, H, &mut Marked, RenderOptions::default());
    let (mut x0, mut y0, mut x1, mut y1, mut white) = (W, H, 0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            let p = &out[((y * W + x) * 4) as usize..][..4];
            if p[0] > 0.05 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            if p[0] > 0.9 {
                white += 1;
            }
        }
    }
    ((x0, y0, x1, y1), white)
}

#[test]
fn a_pan_on_an_inset_keeps_the_card_and_pushes_in_inside_it() {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/inset.mov".into(), None);
    item.resolution = Resolution { width: W, height: H };
    item.duration = RationalTime::from_secs_f64(10.0, 25);
    let media = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: W, height: H };
    let clip = Clip::media(media, RationalTime::from_secs_f64(0.0, 25), RationalTime::from_secs_f64(4.0, 25), RationalTime::from_secs_f64(0.0, 25), RationalTime::from_secs_f64(4.0, 25));
    let id = clip.id;
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut seqs = vec![seq];
    let inset = ComposePose { x: Some(0.5), y: Some(0.46), width: Some(0.91), height: Some(0.91), ..Default::default() };
    apply(&mut seqs, &mut pool, Op::Compose { clip_id: ClipRef::Id(id), poses: vec![inset], flip_x: None, flip_y: None });
    let (card, white_before) = measure(&seqs[0], 1.0);
    assert!(card.0 > 2 && card.2 < W - 3 && card.1 > 0 && card.3 < H - 3, "an inset with margins: {card:?}");

    let ease = klipp_core::keyframe::Ease::InOut;
    apply(&mut seqs, &mut pool, Op::Pan { clip_id: ClipRef::Id(id), keys: vec![PanKey { secs: 0.0, scale: 1.0, x: 0.5, y: 0.5, ease }, PanKey { secs: 2.0, scale: 1.23, x: 0.5, y: 0.5, ease }] });
    for secs in [0.0, 0.6, 1.0, 1.4, 2.0, 3.0] {
        let (now, _) = measure(&seqs[0], secs);
        let near = |a: u32, b: u32| a.abs_diff(b) <= 1;
        assert!(near(now.0, card.0) && near(now.1, card.1) && near(now.2, card.2) && near(now.3, card.3), "at {secs} s the card is {now:?}, the inset {card:?}");
    }
    let (_, white_after) = measure(&seqs[0], 2.0);
    let grown = white_after as f32 / white_before.max(1) as f32;
    assert!((grown - 1.23 * 1.23).abs() < 0.25, "the mark inside grows with the push: {white_before} -> {white_after} ({grown:.2}x)");
}
