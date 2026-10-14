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

/// #1913's own graph: a grade holding a group (a Compound node) that ends
/// in a Mask with no shapes yet, as a Mask just wired in is. The group
/// passes the picture through: the grade around it still draws, with no
/// failed run, and the frame is whole.
#[test]
fn a_group_ending_in_a_new_mask_draws_the_grade_around_it() {
    use klipp_nodes::{Edge, Node, NodeId, NodeParams};
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let chain = |nodes: Vec<Node>| {
        let mut all = vec![node(NodeKind::Source, NodeParams::default())];
        all.extend(nodes);
        all.push(node(NodeKind::Output, NodeParams::default()));
        let edges = all.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
        NodeGraph { output_node_id: all.last().unwrap().id, nodes: all, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
    };
    let exposure = || node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() });
    let group = node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain(vec![node(NodeKind::Mask, NodeParams::default())])), ..Default::default() });
    let draw = |grade: NodeGraph| {
        let mut clip = Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        clip.set_grade(grade);
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut track = Track::new("V", TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        r.render_inspected(&seq, tc(0.5), W, H, &mut Grey, RenderOptions::default(), Inspect::default())
    };
    let (with_group, found) = draw(chain(vec![exposure(), group]));
    let (without, _) = draw(chain(vec![exposure()]));
    assert_eq!(with_group.len(), (W * H * 4) as usize);
    assert!(!found.declines.iter().any(|d| d.outcome == klipp_nodes::DeclineOutcome::RenderFailed), "no failed run: {:?}", found.declines);
    assert_eq!(with_group, without, "the grade around the group still draws");
}
