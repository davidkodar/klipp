//! A disabled Transform leaves automatic placement on CPU and GPU (#1207).

use std::sync::Arc;
use klipp_compose::{eval, FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, VideoParam};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
fn tc(s: f64) -> RationalTime { RationalTime::from_secs_f64(s, 24) }

fn transform(x: f32) -> E {
    E::Transform { position_x: x, position_y: 3.0, scale_x: 0.6, scale_y: 0.6,
        rotation: 12.0, anchor_x: 0.2, anchor_y: 0.3,
        rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn clip() -> Clip {
    Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }
}

#[test]
fn disabled_defaults_leave_clip_animation_and_the_next_enabled_transform() {
    let mut c = clip();
    c.edit_video_param_keyframes(VideoParam::PositionX, |k| klipp_core::keyframe::upsert_keyframe(k, 0.0, 4.0));
    let evaluate = |c: &Clip| eval::evaluate_clip_own_transform(c, 1.0, &eval::eval_context(c, tc(1.0), 24.0));
    let automatic = evaluate(&c);
    assert_eq!(automatic, (4.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));
    c = TestClip::of(c).effect(transform(20.0)).off().build();
    assert_eq!(evaluate(&c), automatic);
    c.set_effect_list(vec![transform(20.0), transform(8.0)], vec![false, true]);
    let second = evaluate(&c);
    c.set_effect_list(vec![transform(8.0)], vec![]);
    assert_eq!(evaluate(&c), second, "missing flags mean enabled");
    assert_eq!(second, (4.0, 3.0, 0.6, 0.6, 12.0, 0.2, 0.3));
}

struct Chart;
impl FrameSource for Chart {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut pixels = Vec::new();
        for y in 0..h { for x in 0..w {
            pixels.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.2, 1.0]);
        }}
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

fn sequence() -> Sequence {
    let mut c = clip();
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, params, label: String::new(),
        enabled: true, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.4), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i+1].id, to_port: 0 }).collect();
    c.set_grade(NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(),
        working_space_override: None, frames: vec![], sticky_notes: vec![] });
    // A real effect after the grade makes the GPU plan route observable.
    c = TestClip::of(c).effects([E::Gamma { gamma: 1.2 }, transform(20.0)]).build();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    let (image, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_fallback_lists, 0);
    if gpu { assert_eq!(found.stats.effects_followed_grade, 1, "exercise the plan route"); }
    image
}

#[test]
fn toggling_a_transform_changes_pixels_and_disabled_matches_absent() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required GPU unavailable");
    let enabled = sequence();
    let mut disabled = enabled.clone();
    let c = &mut disabled.tracks[0].clips[0];
    let id = c.effect_entries().nth(1).unwrap().0;
    assert!(c.set_effect_enabled(id, false));
    let mut absent = enabled.clone();
    absent.tracks[0].clips[0].set_effect_list(vec![E::Gamma { gamma: 1.2 }], vec![true]);
    for use_gpu in [false, true].into_iter().filter(|v| !*v || gpu) {
        let on = render(&enabled, use_gpu);
        let off = render(&disabled, use_gpu);
        let without = render(&absent, use_gpu);
        assert_eq!(off, without, "disabled is automatic placement: gpu={use_gpu}");
        assert!(on.iter().zip(&off).filter(|(a,b)| (*a-*b).abs() > 0.01).count() > (W*H) as usize,
            "the toggle visibly changes the chart");
    }
}

#[test]
fn automatic_picture_truth_keeps_its_own_placement_properties() {
    let mut c = clip();
    let mut picture = (*c.picture_v2().unwrap()).clone();
    picture.picture.requires.push(klipp_graph_format::lifted::PICTURE_TRUTH_CAPABILITY.into());
    picture.picture.placement = klipp_graph_format::lifted::PlacementProps {
        pos_x: Some(7.0), pos_y: Some(8.0), scale: Some(0.7), rotation: Some(9.0),
        anchor_x: Some(0.2), anchor_y: Some(0.3), tilt_x: Some(12.0),
        tilt_y: Some(13.0), perspective: Some(2.0), focal_ref: None, aspect: None,
        ..Default::default()
    };
    c.adopt_stored_picture(picture).unwrap();
    let ctx = eval::eval_context(&c, tc(1.0), 24.0);
    assert_eq!(eval::evaluate_clip_own_transform(&c, 1.0, &ctx), (7.0, 8.0, 0.7, 0.7, 9.0, 0.2, 0.3));
    let resolved = klipp_compose::resolve::resolve_effects(&c, &c.picture_v2().unwrap().picture, tc(1.0), 24.0, None, None).unwrap();
    assert!(resolved.iter().any(|effect| matches!(effect, E::Transform { rotation_x, rotation_y, perspective, .. }
        if (*rotation_x, *rotation_y, *perspective) == (12.0, 13.0, 2.0))));
}
