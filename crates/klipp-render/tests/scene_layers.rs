//! A plan's scene (#1926) rendered: two cards that come in staggered and
//! overlap, and a connector drawn on between them. Before its reveal no
//! line is drawn, halfway through about half of it, after it all of it;
//! no card shows before its start, and both show while they overlap.

use std::sync::Arc;
use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::*;
use klipp_timeline::{Clip, EditPlan, Sequence};

const W: u32 = 192;
const H: u32 = 108;

/// The base footage dark, any other picture mid grey.
struct Plates(MediaId);

impl FrameSource for Plates {
    fn frame(&mut self, clip: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = if clip.media_id() == Some(self.0) { 0.05 } else { 0.5 };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((w * h) as usize))), width: w, height: h, pts: None })
    }
}

/// At `secs`: the columns of the middle row the line lights, and whether
/// a card shows left and right of the middle.
fn look(seq: &Sequence, base: MediaId, secs: f64) -> (Option<(u32, u32)>, bool, bool) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let out = r.render(seq, RationalTime::from_secs_f64(secs, 25), W, H, &mut Plates(base), RenderOptions::default());
    let px = |x: u32, y: u32| out[((y * W + x) * 4) as usize];
    let lit: Vec<u32> = (0..W).filter(|&x| px(x, H / 2) > 0.9).collect();
    let card = |xs: std::ops::Range<u32>| xs.clone().any(|x| (0..H).any(|y| (0.3..0.7).contains(&px(x, y))));
    (lit.first().copied().zip(lit.last().copied()), card(0..W / 2), card(W / 2..W))
}

#[test]
fn a_scene_s_cards_come_in_and_its_connector_draws_on() {
    let mut pool = klipp_media::MediaPool::new();
    let mut add = |path: &str| {
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.into(), None);
        item.resolution = Resolution { width: W, height: H };
        item.duration = RationalTime::from_secs_f64(60.0, 25);
        item.video_codec = "h264".into();
        pool.add(item)
    };
    let (base, card) = (add("/tmp/base.mov"), add("/tmp/card.mov"));
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: W, height: H };
    let plan: EditPlan = serde_json::from_value(serde_json::json!({"segments": [
        {"media_id": base, "source_in_secs": 0.0, "source_out_secs": 6.0, "layers": [
            {"kind": "media", "media_id": card, "at_secs": 0.4, "secs": 5.2, "placement": {"x": 0.25, "y": 0.5, "width": 0.3, "height": 0.3}, "enter": {"kind": "rise", "secs": 0.5}},
            {"kind": "media", "media_id": card, "at_secs": 0.8, "secs": 4.8, "placement": {"x": 0.75, "y": 0.5, "width": 0.3, "height": 0.3}, "enter": {"kind": "rise", "secs": 0.5}},
            {"kind": "connector", "from": [0.3, 0.5], "to": [0.7, 0.5], "stroke_width": 40.0, "at_secs": 1.4, "secs": 4.2, "reveal_secs": 0.6}
        ]}
    ]}))
    .unwrap();
    klipp_document::edit_plan::validate(&plan, &pool, None).unwrap();
    let ops = klipp_document::edit_plan::to_ops(&plan, &seq, &pool, None);
    let built = klipp_document::ops::resolve(&seq, &pool, &ops).unwrap();
    let mut seqs = vec![seq];
    let mut fps = seqs[0].fps;
    let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &built.steps {
        view.apply_forward(&step.edit);
    }
    let seq = &seqs[0];

    let (line, left, right) = look(seq, base, 0.3);
    assert!(line.is_none() && !left && !right, "nothing over the picture before the first card: {line:?} {left} {right}");
    // Most of the way through its entrance, before the second starts.
    let (line, left, right) = look(seq, base, 0.76);
    assert!(line.is_none() && left && !right, "the first card alone, coming in: {line:?} {left} {right}");
    let (line, left, right) = look(seq, base, 1.2);
    assert!(line.is_none() && left && right, "both cards, no line yet: {line:?}");
    let full = (0.7 - 0.3) * W as f32;
    let (line, ..) = look(seq, base, 1.7);
    let (a, b) = line.expect("the line has started");
    let drawn = (b - a) as f32 / full;
    assert!(a.abs_diff((0.3 * W as f32) as u32) <= 3 && (0.3..0.7).contains(&drawn), "half drawn from its start: {a}-{b} ({drawn:.2} of it)");
    let (line, left, right) = look(seq, base, 3.0);
    let (a, b) = line.expect("the line is drawn");
    assert!((b - a) as f32 / full > 0.9 && left && right, "all of it, between both cards: {a}-{b}");
}
