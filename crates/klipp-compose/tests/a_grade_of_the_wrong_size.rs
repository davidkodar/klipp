//! A grade run that gives a picture of the wrong size (#1913: an empty
//! one, where its result was a node with no picture) is a failed run on
//! the CPU: the frame is still the whole picture, drawn without the
//! grade, and the run is declined, never handed on to crash whatever
//! shows it.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_timeline::{Clip, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

struct Grey;

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = [0.3f32, 0.3, 0.3, 1.0].repeat((w * h) as usize);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

#[test]
fn a_grade_that_gives_no_picture_is_drawn_without_it() {
    let mut clip = Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corrector && n.kind == NodeKind::Corrector).unwrap().params.exposure = Some(1.0);
    clip.set_grade(g);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    let draw = |seq: &Sequence| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        r.render_inspected(seq, tc(0.5), W, H, &mut Grey, RenderOptions::default(), Inspect::default())
    };
    let (graded, _) = draw(&seq);
    let ungraded = {
        let mut plain = seq.clone();
        plain.tracks[0].clips[0].set_grade(NodeGraph::default());
        draw(&plain).0
    };
    assert_ne!(graded, ungraded, "the grade does something");

    let (out, found) = {
        let _fault = klipp_compose::fault::empty_grade(id);
        draw(&seq)
    };
    assert_eq!(out.len(), (W * H * 4) as usize, "the whole frame");
    assert_eq!(out, ungraded, "drawn without the grade");
    assert!(
        found.declines.iter().any(|d| d.clip == Some(id) && d.outcome == klipp_nodes::DeclineOutcome::RenderFailed),
        "the run is declined: {:?}",
        found.declines
    );
}
