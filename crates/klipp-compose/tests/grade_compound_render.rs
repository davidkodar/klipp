//! A grade in a picture's chain after its display step (#1746): a
//! `klipp.grade_compound` holding a grade graph, run by the executor as a
//! compound node on the picture it reads. On an SDR clip, whose display
//! step leaves the picture as it is, a compound holding a grade draws what
//! the clip's own grade holding it does, on the CPU and the GPU.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.6 * fx, 0.1 + 0.5 * fy, 0.5 * (1.0 - fx) + 0.05, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }
}

/// A grade of one Corrector: an exposure lift and less saturation.
fn grade() -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    let c = g.ensure_corrector();
    let n = g.nodes.iter_mut().find(|n| n.id == c).unwrap();
    n.params.exposure = Some(0.7);
    n.params.saturation = Some(0.4);
    g
}

/// The chart clip with `grade` as a compound after its display step.
fn with_compound() -> Clip {
    let mut c = clip();
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let node = klipp_graph_lift::grade_compound::compound_of(&grade(), klipp_graph_format::NodeId::new_v4()).unwrap();
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(display), node: Box::new(node) }]).unwrap_or_else(|e| panic!("{e}"));
    c
}

fn render(gpu: bool, c: &Clip) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![c.clone()];
    seq.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default())
}

fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn a_compound_after_an_sdr_clips_display_step_draws_its_grade() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    let compound = with_compound();
    assert!(compound.is_picture_truth(), "only the picture holds a compound");
    let mut graded = clip();
    graded.set_grade(grade());
    let (plain, _) = render(false, &clip());
    let (want, _) = render(false, &graded);
    assert!(worst_levels(&want, &plain) > 20, "the grade changes the picture");
    for on_gpu in [false, true].into_iter().filter(|g| !g || gpu) {
        let (out, found) = render(on_gpu, &compound);
        assert!(found.declines.is_empty(), "gpu {on_gpu}: {:?}", found.declines);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {on_gpu}: the compound against the clip's own grade: worst {worst} levels");
        assert!(worst <= 1, "gpu {on_gpu}: {worst} levels from the clip's own grade");
    }
}

/// A compound's Corrector edited by path, through the compound, as the
/// Picture view edits a node it has entered: the edit is in the picture
/// and in the frame, one step undone by the edit's inverse.
#[test]
fn a_compounds_corrector_is_edited_by_path() {
    let mut c = clip();
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip_id = c.id.0;
    let at = |ids: &[klipp_graph_format::NodeId]| NodePath::new(Root::Clip { clip: clip_id }, ids.iter().copied());
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let node = klipp_graph_lift::grade_compound::new_compound(klipp_graph_format::NodeId::new_v4()).unwrap();
    let corrector = node.group.as_ref().unwrap().graph.nodes.iter().find(|n| klipp_graph_lift::structure::kind_of(n) == "Corrector").unwrap().id;
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(&[display]), node: Box::new(node.clone()) }]).unwrap_or_else(|e| panic!("{e}"));
    let (plain, _) = render(false, &clip());
    let (unedited, _) = render(false, &c);
    assert_eq!(worst_levels(&unedited, &plain), 0, "a new compound's Corrector leaves the picture as it is");
    c.apply_picture_ops(&[GraphOp::SetParam { path: at(&[node.id, corrector]), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.8) }]).unwrap_or_else(|e| panic!("{e}"));
    let (edited, found) = render(false, &c);
    assert!(found.declines.is_empty(), "{:?}", found.declines);
    assert!(worst_levels(&edited, &plain) > 20, "the edit is in the frame");
}

fn effect_node(e: &klipp_effects::BuiltinEffect) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

/// A compound after a Merge grades the merged picture, and a key inside it
/// (its Corrector's qualifier, on the highlights) reads that picture, its
/// own input: it draws what the executor running its grade on the merged
/// frame does, on the CPU and the GPU.
#[test]
fn a_compound_after_a_merge_grades_and_keys_the_merged_picture() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    let mut keyed = klipp_nodes::NodeGraph::default();
    let corrector = keyed.ensure_corrector();
    let n = keyed.nodes.iter_mut().find(|n| n.id == corrector).unwrap();
    n.params.saturation = Some(0.5);
    n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, lum_low: 0.45, lum_high: 1.0, lum_soft: 0.05, exposure: 1.2, ..Default::default() });
    let compound = klipp_graph_lift::grade_compound::compound_of(&keyed, klipp_graph_format::NodeId::new_v4()).unwrap();
    let run = klipp_timeline::lowered_grade(&compound.group.as_ref().unwrap().graph).unwrap();

    // The chart with a gradient screened over it, and then the compound.
    let merged = |with_compound: bool| {
        let mut c = clip();
        let picture = c.picture_v2().unwrap().picture.clone();
        let clip_id = c.id.0;
        let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
        let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
        let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
        let merge = effect_node(&klipp_effects::BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Screen, opacity: 0.6 });
        let gradient = effect_node(&klipp_effects::BuiltinEffect::Generator { shape: 1, color_a: [20, 40, 220, 255], color_b: [250, 230, 200, 255], angle: 20.0, center_x: 0.5, center_y: 0.5, radius: 0.5 });
        let mut ops = vec![
            GraphOp::AddNode { after: at(display), node: Box::new(merge.clone()) },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(gradient.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(gradient.id), to_port: "in".into() },
            GraphOp::Connect { from: at(gradient.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
        ];
        if with_compound {
            ops.push(GraphOp::AddNode { after: at(merge.id), node: Box::new(compound.clone()) });
        }
        c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
        c
    };
    let (merged_frame, _) = render(false, &merged(false));
    let mut ex = klipp_nodes::GraphExecutor::new_cpu_only();
    ex.set_source_domain(klipp_nodes::domains::PictureDomain::Display);
    let (want, _) = ex.execute_linear(&merged_frame, &run, W, H, 1.0, 24, None, None).unwrap();
    assert!(worst_levels(&want, &merged_frame) > 10, "the grade changes the merged picture");
    for on_gpu in [false, true].into_iter().filter(|g| !g || gpu) {
        let (out, found) = render(on_gpu, &merged(true));
        assert!(found.declines.is_empty(), "gpu {on_gpu}: {:?}", found.declines);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {on_gpu}: after a merge, against its grade run on the merged frame: worst {worst} levels");
        assert!(worst <= 1, "gpu {on_gpu}: {worst} levels");
    }
}

/// A compound at half mix (its node's own mix) draws half way from the
/// picture it read to what its grade makes, on the CPU and the GPU.
#[test]
fn a_compounds_mix_takes_its_grade_part_way() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    let full = with_compound();
    let mut half = full.clone();
    let compound = half.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::grade_compound::is_compound(n)).unwrap().id;
    half.apply_picture_ops(&[GraphOp::SetMix { path: NodePath::new(Root::Clip { clip: half.id.0 }, [compound]), mix: Some(0.5) }]).unwrap_or_else(|e| panic!("{e}"));
    let (plain, _) = render(false, &clip());
    let (graded, _) = render(false, &full);
    let want: Vec<f32> = plain.iter().zip(&graded).map(|(p, g)| p + (g - p) * 0.5).collect();
    for on_gpu in [false, true].into_iter().filter(|g| !g || gpu) {
        let (out, found) = render(on_gpu, &half);
        assert!(found.declines.is_empty(), "gpu {on_gpu}: {:?}", found.declines);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {on_gpu}: half mix against half way: worst {worst} levels");
        assert!(worst <= 1, "gpu {on_gpu}: {worst} levels");
    }
}

/// A key on the Corrector inside a compound animates the frame: at each
/// key's time the compound draws what the clip's own grade at that value
/// does.
#[test]
fn a_key_inside_a_compound_animates_it() {
    let mut c = clip();
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip_id = c.id.0;
    let at = |ids: &[klipp_graph_format::NodeId]| NodePath::new(Root::Clip { clip: clip_id }, ids.iter().copied());
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let node = klipp_graph_lift::grade_compound::new_compound(klipp_graph_format::NodeId::new_v4()).unwrap();
    let corrector = node.group.as_ref().unwrap().graph.nodes.iter().find(|n| klipp_graph_lift::structure::kind_of(n) == "Corrector").unwrap().id;
    let key = |t: f64, v: f32| serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(&[display]), node: Box::new(node.clone()) },
        GraphOp::AddKeyAt { path: at(&[node.id, corrector]), key: "exposure".into(), keyframe: key(0.0, 0.0) },
        GraphOp::AddKeyAt { path: at(&[node.id, corrector]), key: "exposure".into(), keyframe: key(2.0, 1.0) },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    let render_at = |c: &Clip, secs: f64| {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![c.clone()];
        seq.tracks = vec![track];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        r.render_inspected(&seq, tc(secs), W, H, &mut Chart, RenderOptions::default(), Inspect::default())
    };
    for (secs, exposure) in [(0.5, 0.25f32), (1.5, 0.75)] {
        let mut graded = clip();
        let mut g = klipp_nodes::NodeGraph::default();
        let id = g.ensure_corrector();
        g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(exposure);
        graded.set_grade(g);
        let (want, _) = render_at(&graded, secs);
        let (out, found) = render_at(&c, secs);
        assert!(found.declines.is_empty(), "{:?}", found.declines);
        let worst = worst_levels(&out, &want);
        assert!(worst <= 1, "at {secs} s: {worst} levels from the clip's own grade at exposure {exposure}");
    }
}

/// A compound placed before the display step, where it would never be
/// drawn, is refused as the edit is made, naming it, the picture left as
/// it was.
#[test]
fn a_compound_before_the_display_step_is_refused() {
    let mut c = clip();
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip_id = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
    let before = c.picture_v2().unwrap();
    for anchor in [lt::SOURCE, lt::GROUP] {
        let after = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == anchor).unwrap().id;
        let mut node = klipp_graph_lift::grade_compound::new_compound(klipp_graph_format::NodeId::new_v4()).unwrap();
        node.label = "Look".into();
        let why = c.apply_picture_ops(&[GraphOp::AddNode { after: at(after), node: Box::new(node.clone()) }]).unwrap_err();
        assert!(why.contains("Look") && why.contains("after the display step"), "after {anchor}: {why}");
        assert_eq!(c.picture_v2().unwrap(), before, "after {anchor}: the picture is as it was");
    }
}
