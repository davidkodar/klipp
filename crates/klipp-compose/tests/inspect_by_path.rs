//! Inspection by path (M3 step 16a): what the grade tools ask of the
//! inspected clip (a soloed node, the scope's tap, thumbnails, a matte)
//! named by the node's path in the clip's picture and its output port.
//!
//! A path into a stage of the clip's grade group reads that stage's node
//! as the clip runs it, fed from outside the stage; two nodes of one id
//! (the clip's own and a stage's) are two answers; a matte's output shows
//! the matte; the clip's picture before its grade means what it did; and
//! a path that names nothing is said to be unavailable, the picture left
//! as it is. None of it changes a render that inspects nothing.
//!
//! Each runs with CPU paths and with the GPU where there is one.

use klipp_timeline::GradeAccess;
use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, InspectAt, LinearFrameProviderFn, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::{NodePath, Root};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, GradeGroup, GradeStage, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 32;
const H: u32 = 18;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// Source → `kind` → Output, the middle node of id `id`.
fn graph(kind: NodeKind, params: NodeParams, id: NodeId) -> NodeGraph {
    let node = |id, kind, params| Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![
        node(NodeId::new_v4(), NodeKind::Source, NodeParams::default()),
        node(id, kind, params),
        node(NodeId::new_v4(), NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn exposure(stops: f32, id: NodeId) -> NodeGraph {
    graph(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() }, id)
}

struct Grey(f32);

impl FrameSource for Grey {
    fn frame(&mut self, _clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let v = self.0;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

/// One graded media clip on a sequence, in a grade group whose pre stage
/// is `pre`; the render copy, as the engine renders it.
fn sequence(grade: NodeGraph, pre: Option<NodeGraph>) -> (Sequence, ClipId, GradeGroup) {
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(grade);
    let mut group = GradeGroup::new("Group");
    group.stage_slot(GradeStage::Pre).set(pre);
    clip.grade_group = Some(group.id);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.grade_groups.push(group.clone());
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    (seq.baked_for_render().into_owned(), id, group)
}

fn draw(seq: &Sequence, gpu: bool, inspect: Inspect) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(1.0), W, H, &mut Grey(0.18), RenderOptions::default(), inspect)
}

/// The picture as the viewer and the export render it, read back.
fn plain(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(seq, tc(1.0), W, H, &mut Grey(0.18), RenderOptions::default())
}

fn close(label: &str, a: &[f32], b: &[f32]) {
    let worst = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert_eq!(a.len(), b.len(), "{label}: frames of different sizes");
    assert!(worst <= 1e-4, "{label}: differs by {worst}");
}

fn far(label: &str, a: &[f32], b: &[f32]) {
    let worst = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(!a.is_empty() && a.len() == b.len() && worst > 1e-2, "{label}: the same picture ({} and {} values)", a.len(), b.len());
}

fn stage(group: &GradeGroup, node: NodeId) -> InspectAt {
    InspectAt::out(NodePath::new(Root::GroupStage { group: group.id.0, stage: Stage::Pre }, [node]))
}

#[test]
fn tap_and_solo_leave_export_unchanged() {
    let x = NodeId::new_v4();
    let (seq, clip, group) = sequence(exposure(1.0, x), Some(exposure(-0.5, NodeId::new_v4())));
    let p = seq.grade_groups[0].stage(GradeStage::Pre).unwrap().nodes[1].id;
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let before = r.render(&seq, tc(1.0), W, H, &mut Grey(0.18), RenderOptions::default());
        let inspect = Inspect {
            clip: Some(clip),
            solo: Some(stage(&group, p)),
            scope_tap: Some(klipp_compose::inspect::grade_node(clip, x)),
            thumbs: Some([klipp_compose::inspect::grade_node(clip, x), stage(&group, p)].into_iter().collect()),
            ..Default::default()
        };
        let (_, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Grey(0.18), RenderOptions::default(), inspect);
        assert!(found.unavailable.is_empty(), "gpu {gpu}: {:?}", found.unavailable);
        let after = r.render(&seq, tc(1.0), W, H, &mut Grey(0.18), RenderOptions::default());
        assert_eq!(after, before, "gpu {gpu}: an inspected render leaves the next plain one as it was");
    }
}

#[test]
fn a_nested_tap_sees_its_external_inputs() {
    // The pre stage's node reads the clip's source from outside its stage:
    // soloed, it shows that source through it alone, before the clip's own.
    let p = NodeId::new_v4();
    let (seq, clip, group) = sequence(exposure(1.0, NodeId::new_v4()), Some(exposure(-0.5, p)));
    let (only_the_stage, _, _) = sequence(exposure(-0.5, NodeId::new_v4()), None);
    for gpu in [false, true] {
        let (soloed, found) = draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(stage(&group, p)), ..Default::default() });
        assert!(found.unavailable.is_empty(), "{:?}", found.unavailable);
        close(&format!("gpu {gpu}: the stage's node on the clip's source"), &soloed, &plain(&only_the_stage, gpu));
        far(&format!("gpu {gpu}: not the output"), &soloed, &plain(&seq, gpu));
    }
}

#[test]
fn same_local_id_in_two_groups_gives_two_results() {
    // The clip's own node and the pre stage's share an id.
    let x = NodeId::new_v4();
    let (seq, clip, group) = sequence(exposure(1.0, x), Some(exposure(-0.5, x)));
    for gpu in [false, true] {
        let solo = |at: InspectAt| draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(at), ..Default::default() });
        let (own, a) = solo(klipp_compose::inspect::grade_node(clip, x));
        let (pre, b) = solo(stage(&group, x));
        assert!(a.unavailable.is_empty() && b.unavailable.is_empty(), "{:?} {:?}", a.unavailable, b.unavailable);
        far(&format!("gpu {gpu}: two nodes, two pictures"), &own, &pre);
        close(&format!("gpu {gpu}: the clip's own node ends the grade"), &own, &plain(&seq, gpu));
    }
}

#[test]
fn a_mask_output_tap_uses_the_matte_visualisation() {
    let q = NodeId::new_v4();
    let (seq, clip, _) = sequence(graph(NodeKind::Qualifier, NodeParams::default(), q), None);
    for gpu in [false, true] {
        let (soloed, found) = draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(klipp_compose::inspect::grade_node(clip, q)), ..Default::default() });
        assert!(found.unavailable.is_empty(), "{:?}", found.unavailable);
        let (matte, _) = draw(&seq, gpu, Inspect { clip: Some(clip), matte_view: Some(klipp_compose::inspect::grade_node(clip, q)), ..Default::default() });
        close(&format!("gpu {gpu}: a qualifier soloed shows its matte"), &soloed, &matte);
    }
}

#[test]
fn ungraded_keeps_its_meaning() {
    let x = NodeId::new_v4();
    let (seq, clip, _) = sequence(exposure(1.0, x), Some(exposure(-0.5, NodeId::new_v4())));
    for gpu in [false, true] {
        let (_, plain_found) = draw(&seq, gpu, Inspect { clip: Some(clip), ungraded: true, ..Default::default() });
        let (_, soloed) = draw(&seq, gpu, Inspect { clip: Some(clip), ungraded: true, solo: Some(klipp_compose::inspect::grade_node(clip, x)), ..Default::default() });
        let (a, b) = (plain_found.ungraded.expect("the picture before the grade"), soloed.ungraded.expect("the picture before the grade"));
        close(&format!("gpu {gpu}: before the grade, whatever is soloed"), &a, &b);
    }
}

#[test]
fn a_missing_path_is_unavailable_not_stale() {
    let x = NodeId::new_v4();
    let (seq, clip, group) = sequence(exposure(1.0, x), Some(exposure(-0.5, NodeId::new_v4())));
    let grade = klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, clip.0, "grade", 0);
    let compound = InspectAt::out(NodePath::new(Root::Clip { clip: clip.0 }, [grade, x, NodeId::new_v4()]));
    for gpu in [false, true] {
        for missing in [klipp_compose::inspect::grade_node(clip, NodeId::new_v4()), stage(&group, NodeId::new_v4()), compound.clone()] {
            let (shown, found) = draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(missing.clone()), thumbs: Some([missing.clone()].into_iter().collect()), ..Default::default() });
            assert!(found.unavailable.iter().any(|(at, _)| *at == missing), "gpu {gpu}: {missing:?} said to be unavailable: {:?}", found.unavailable);
            assert!(found.thumbs.is_empty(), "no thumbnail stands in for it");
            close(&format!("gpu {gpu}: the picture, not another node's"), &shown, &plain(&seq, gpu));
        }
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(compound.clone()), ..Default::default() });
        assert!(found.unavailable.iter().any(|(_, why)| why.contains("compound")), "{:?}", found.unavailable);
    }
}

/// A soloed node the GPU would run in a chain with the next (the first
/// of two exposures, a stage's node before the clip's own) is read back:
/// its picture is the one shown, never a chain's empty placeholder.
#[test]
fn a_soloed_node_inside_a_gpu_chain_is_shown() {
    // Source -> E1 -> E2 -> Output, no group: solo E1.
    let mut g = exposure(1.0, NodeId::new_v4());
    let e1 = g.nodes[1].id;
    let e2 = NodeId::new_v4();
    let out = g.output_node_id;
    g.nodes.insert(2, Node { id: e2, kind: NodeKind::Exposure, label: "E2".into(), enabled: true, params: NodeParams { exposure: Some(-0.5), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: vec![] });
    g.edges.retain(|e| e.to_node != out);
    g.edges.push(Edge { from_node: e1, from_port: 0, to_node: e2, to_port: 0 });
    g.edges.push(Edge { from_node: e2, from_port: 0, to_node: out, to_port: 0 });
    let (seq, clip, _) = sequence(g, None);
    let (only_e1, _, _) = sequence(exposure(1.0, NodeId::new_v4()), None);
    for gpu in [false, true] {
        let (shown, found) = draw(&seq, gpu, Inspect { clip: Some(clip), solo: Some(klipp_compose::inspect::grade_node(clip, e1)), ..Default::default() });
        assert!(found.unavailable.is_empty(), "{:?}", found.unavailable);
        close(&format!("gpu {gpu}: E1's picture"), &shown, &plain(&only_e1, gpu));
    }
}

/// Time context (M3 step 16a): a keyframed node inspected the way the
/// engine's thumbnails from a filmstrip frame are (the clip alone, at the
/// playhead) is read at that frame's clip time: its picture is the one
/// its value there gives, at two frames with two values.
#[test]
fn a_keyed_node_tap_has_its_time_context() {
    // Source -> exposure keyed 0 at 0 s, 2 at 2 s -> exposure 0.5 -> Output.
    let e = NodeId::new_v4();
    let mut g = exposure(0.0, e);
    let mut keys = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
    klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 2.0);
    g.nodes[1].keyframes.tracks.push(klipp_nodes::NodeKeyframeTrack { param_name: "exposure".into(), keyframes: keys });
    let (out, after) = (g.output_node_id, NodeId::new_v4());
    g.nodes.insert(2, Node { id: after, kind: NodeKind::Exposure, label: "after".into(), enabled: true, params: NodeParams { exposure: Some(0.5), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: vec![] });
    g.edges.retain(|x| x.to_node != out);
    g.edges.push(Edge { from_node: e, from_port: 0, to_node: after, to_port: 0 });
    g.edges.push(Edge { from_node: after, from_port: 0, to_node: out, to_port: 0 });
    let (seq, clip, _) = sequence(g, None);
    for gpu in [false, true] {
        for (t, stops) in [(0.5, 0.5), (1.5, 1.5)] {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(gpu);
            let inspect = Inspect { clip: Some(clip), only: Some(clip), solo: Some(klipp_compose::inspect::grade_node(clip, e)), ..Default::default() };
            let (shown, found) = r.render_inspected(&seq, tc(t), W, H, &mut Grey(0.18), RenderOptions::default(), inspect);
            assert!(found.unavailable.is_empty(), "{:?}", found.unavailable);
            let (still, _, _) = sequence(exposure(stops, NodeId::new_v4()), None);
            let mut plain_r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            plain_r.set_gpu_enabled(gpu);
            let want = plain_r.render(&still, tc(t), W, H, &mut Grey(0.18), RenderOptions::default());
            close(&format!("gpu {gpu}: the keyed node at {t} s"), &shown, &want);
        }
    }
}

/// An identity grade the tools read inside still runs, so its nodes'
/// thumbnails come back (a plain clip's Source); an identity effect,
/// dropped from the run, shows the picture that feeds it, here the
/// grade's output, the source's (#1314).
#[test]
fn an_identity_grade_still_answers_its_inspection() {
    let clip = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }).effect(klipp_effects::BuiltinEffect::Gamma { gamma: 1.0 }).build();
    let source = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a source").id;
    assert!(clip.grade_is_identity());
    let effect = clip.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let gamma = InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [effect]));
    for gpu in [false, true] {
        let thumbs = [klipp_compose::inspect::grade_node(id, source), gamma.clone()].into_iter().collect();
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(thumbs), ..Default::default() });
        assert!(found.thumbs.contains_key(&klipp_compose::inspect::grade_node(id, source)), "gpu {gpu}: the source's thumbnail: {:?}", found.unavailable);
        assert!(found.unavailable.is_empty(), "gpu {gpu}: {:?}", found.unavailable);
        assert_eq!(found.thumbs.get(&gamma), found.thumbs.get(&klipp_compose::inspect::grade_node(id, source)), "gpu {gpu}: the identity effect shows the grade's output");
        // The scope's tap on it too: the source's picture.
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), scope_tap: Some(klipp_compose::inspect::grade_node(id, source)), ..Default::default() });
        let tapped = found.scope_tap.unwrap_or_else(|| panic!("gpu {gpu}: a tap on the identity grade's source: {:?}", found.unavailable));
        assert!(tapped.chunks_exact(4).all(|p| (p[0] - 0.18).abs() < 1e-3), "gpu {gpu}: the source's grey");
    }
}

/// An effect dropped from the run as an identity (a Gamma of 1) answers
/// its thumbnail, its solo and the scope's tap as what feeds it would
/// (#1314): first in the chain, with the grade's output; after another
/// effect, as that effect is answered (inspecting a clip keeps its
/// effects out of the grade's run, #1274, so both are unavailable then,
/// and never answered by the clip's output).
#[test]
fn an_identity_effect_shows_the_picture_that_feeds_it() {
    use klipp_effects::BuiltinEffect::Gamma;
    let media = || Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    for (chain, identity, feeder) in [(vec![1.0, 1.5], 0, None), (vec![0.8, 1.0, 1.5], 1, Some(0))] {
        let clip = chain.iter().fold(TestClip::of(media()), |c, g| c.effect(Gamma { gamma: *g })).build();
        let id = clip.id;
        let source = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a source").id;
        let effects: Vec<_> = clip.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| klipp_graph_lift::effect_params::tag(n) == Some("gamma")).map(|n| n.id).collect();
        assert_eq!(effects.len(), chain.len());
        let mut seq = Sequence::new_default();
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        let at = |n| InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [n]));
        let (asked, feeding) = (at(effects[identity]), feeder.map_or_else(|| klipp_compose::inspect::grade_node(id, source), |i| at(effects[i])));
        let unavailable = |found: &klipp_compose::Inspection, a: &InspectAt| found.unavailable.iter().any(|(x, _)| x == a);
        for gpu in [false, true] {
            let what = format!("{chain:?} gpu {gpu}");
            let thumbs = [asked.clone(), feeding.clone()].into_iter().collect();
            let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(thumbs), ..Default::default() });
            assert_eq!(unavailable(&found, &asked), unavailable(&found, &feeding), "{what}: {:?}", found.unavailable);
            assert_eq!(found.thumbs.get(&asked), found.thumbs.get(&feeding), "{what}: the thumbnail of what feeds it");
            if feeder.is_none() {
                assert!(found.thumbs.contains_key(&asked), "{what}: first in the chain, the grade's output: {:?}", found.unavailable);
            }
            let solo = |a: &InspectAt| draw(&seq, gpu, Inspect { clip: Some(id), solo: Some(a.clone()), ..Default::default() });
            let ((shown, found), (fed, fed_found)) = (solo(&asked), solo(&feeding));
            assert_eq!(unavailable(&found, &asked), unavailable(&fed_found, &feeding), "{what}: {:?}", found.unavailable);
            if !unavailable(&found, &asked) {
                assert_eq!(shown, fed, "{what}: the solo of what feeds it");
            }
            let tap = |a: &InspectAt| draw(&seq, gpu, Inspect { clip: Some(id), scope_tap: Some(a.clone()), ..Default::default() }).1;
            let (tapped, fed) = (tap(&asked), tap(&feeding));
            assert_eq!(tapped.scope_tap, fed.scope_tap, "{what}: the tap of what feeds it: {:?}", tapped.unavailable);
        }
    }
}

/// A node inside a compound of a clip's grade, and inside a compound in
/// that, answers its thumbnail, a solo and the scope's tap by its full
/// path, on the CPU and the GPU (#1475): nothing is unavailable, each
/// depth's solo shows its own picture, not the clip's.
#[test]
fn a_node_inside_a_compound_is_inspected_by_its_path() {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let chain = |inner: Vec<Node>| {
        let mut nodes = vec![node(NodeKind::Source, NodeParams::default())];
        nodes.extend(inner);
        nodes.push(node(NodeKind::Output, NodeParams::default()));
        let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    };
    let exposure = |stops| node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() });
    let (inner, deep) = (exposure(1.0), exposure(0.5));
    let (inner_id, deep_id) = (inner.id, deep.id);
    let nested = node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain(vec![deep])), ..Default::default() });
    let nested_id = nested.id;
    let outer = node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain(vec![inner, nested])), ..Default::default() });
    let outer_id = outer.id;
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(chain(vec![outer, exposure(-2.0)]));
    let id = clip.id;
    let grade = klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, id.0, "grade", 0);
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let at = |nodes: &[NodeId]| InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, std::iter::once(grade).chain(nodes.iter().copied())));
    for gpu in [false, true] {
        let plain = draw(&seq, gpu, Inspect::default()).0;
        let mut soloed = Vec::new();
        for (what, path) in [("one deep", at(&[outer_id, inner_id])), ("two deep", at(&[outer_id, nested_id, deep_id]))] {
            let what = format!("{what} gpu {gpu}");
            let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some([path.clone()].into_iter().collect()), ..Default::default() });
            assert!(found.unavailable.is_empty(), "{what}: {:?}", found.unavailable);
            assert!(found.thumbs.contains_key(&path), "{what}: a thumbnail");
            let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), scope_tap: Some(path.clone()), ..Default::default() });
            assert!(found.unavailable.is_empty() && found.scope_tap.is_some(), "{what}: a tap: {:?}", found.unavailable);
            let (shown, found) = draw(&seq, gpu, Inspect { clip: Some(id), solo: Some(path.clone()), ..Default::default() });
            assert!(found.unavailable.is_empty(), "{what}: {:?}", found.unavailable);
            assert_ne!(shown, plain, "{what}: the solo isn't the clip's picture");
            soloed.push(shown);
        }
        assert_ne!(soloed[0], soloed[1], "gpu {gpu}: each depth shows its own node");
    }
}

/// The picture's own nodes answer their thumbnails by their paths in the
/// picture, as the Nodes dock asks (#1726): the Source the clip's source,
/// the grade group the graded picture, an effect the picture after it
/// (on the GPU, where the effects run in the grade's plan), Placement the
/// finished frame, as the program viewer shows it.
#[test]
fn the_pictures_own_nodes_answer_their_thumbnails() {
    let (seq, id, _) = sequence(exposure(1.0, NodeId::new_v4()), None);
    let mut seq = seq;
    let clip = seq.tracks[0].clips.iter_mut().find(|c| c.id == id).unwrap();
    *clip = TestClip::of(clip.clone()).effect(klipp_effects::BuiltinEffect::Gamma { gamma: 1.6 }).build();
    let clip = seq.tracks[0].clips.iter().find(|c| c.id == id).unwrap();
    let picture = clip.picture_v2().unwrap();
    let of = |t: &str| picture.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == t).unwrap().id;
    let at = |n| InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [n]));
    let effect = picture.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gamma")).unwrap().id;
    let (source, grade, gamma, placement) = (
        at(of(klipp_graph_format::lifted::types::SOURCE)),
        at(klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, id.0, "grade", 0)),
        at(effect),
        at(of(klipp_graph_format::lifted::types::PLACEMENT)),
    );
    let run_source = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    for gpu in [false, true] {
        let thumbs = [source.clone(), grade.clone(), gamma.clone(), placement.clone(), klipp_compose::inspect::grade_node(id, run_source)].into_iter().collect();
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(thumbs), ..Default::default() });
        let unavailable = |a: &InspectAt| found.unavailable.iter().find(|(x, _)| x == a).map(|(_, why)| why.clone());
        let (s, g) = (found.thumbs.get(&source), found.thumbs.get(&grade));
        assert!(s.is_some() && g.is_some(), "gpu {gpu}: {:?}", found.unavailable);
        assert_eq!(s, found.thumbs.get(&klipp_compose::inspect::grade_node(id, run_source)), "gpu {gpu}: the Source is the grade's source");
        assert_ne!(s, g, "gpu {gpu}: the grade group shows the graded picture");
        // Placement shows the finished frame, as the program viewer does.
        let (frame, _) = draw(&seq, gpu, Inspect::default());
        let finished = klipp_nodes::executor::downsample_to_thumb_u8(&frame, W, H, klipp_nodes::executor::THUMB_W, klipp_nodes::executor::THUMB_H);
        assert_eq!(found.thumbs.get(&placement), Some(&finished), "gpu {gpu}: {:?}", unavailable(&placement));
        // The effect shows the picture after it, on the GPU's chain and on
        // the CPU's kernels alike (#3328).
        let e = found.thumbs.get(&gamma);
        assert!(e.is_some(), "gpu {gpu}: {:?}", unavailable(&gamma));
        assert_ne!(e, g, "gpu {gpu}: the effect shows the picture after it");
    }
}

/// Media at 24 fps whose frames differ: frame `n` a ramp across the frame
/// at one of three uneven levels in turn, so no frame is the mean of its
/// neighbours. Its frames, the next one for a
/// blend and its neighbours for a denoise, each by its media frame.
struct Moving;

const MEDIA_FPS: f64 = 24.0;

fn moving(n: i64) -> Vec<f32> {
    let level = [0.3, 0.4, 0.33][n.rem_euclid(3) as usize];
    (0..W * H).flat_map(|i| {
        let x = (i % W) as f32 / W as f32;
        [level + 0.1 * x, level, level * 0.8, 1.0]
    }).collect()
}

/// The media frame shown at `secs`: the one at or before it.
fn frame_at(secs: f64) -> i64 {
    (secs * MEDIA_FPS + 1e-6).floor() as i64
}

impl FrameSource for Moving {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let n = frame_at(secs);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(moving(n))), width: w, height: h, pts: Some(n as f64 / MEDIA_FPS) })
    }
    fn frame_after(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let n = frame_at(secs) + 1;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(moving(n))), width: w, height: h, pts: Some(n as f64 / MEDIA_FPS) })
    }
    fn temporal_frames_linear(&mut self, clip: &Clip, _: klipp_core::WorkingColorspace) -> Option<LinearFrameProviderFn> {
        let source_in = clip.source_in.to_secs_f64();
        Some(Box::new(move |t| Some((std::sync::Arc::new(moving(((source_in + t) * MEDIA_FPS).round() as i64)), W, H))))
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 10.0))
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(MEDIA_FPS)
    }
}

/// A media clip of `Moving` carrying `effects`, at half speed when
/// `slowed`, graded by `grade` when given; its sequence at 24 fps.
fn moving_clip(effects: &[klipp_effects::BuiltinEffect], slowed: bool, grade: Option<NodeGraph>) -> (Sequence, Clip) {
    let out = if slowed { 1.0 } else { 2.0 };
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(out));
    if slowed {
        c.speed = 0.5;
    }
    let mut built = TestClip::of(c);
    for e in effects {
        built = built.effect(e.clone());
    }
    let mut c = built.build();
    if let Some(g) = grade {
        c.set_grade(g);
    }
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(24, 1);
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![c.clone()];
    seq.tracks = vec![track];
    (seq, c)
}

/// The output of `clip`'s picture node of type `type_id`.
fn picture_node(clip: &Clip, type_id: &str) -> InspectAt {
    let node = clip.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == type_id).unwrap_or_else(|| panic!("a {type_id} node")).id;
    InspectAt::out(NodePath::new(Root::Clip { clip: clip.id.0 }, [node]))
}

fn moving_render(seq: &Sequence, gpu: bool, t: f64, inspect: Inspect) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(t), W, H, &mut Moving, RenderOptions::default(), inspect)
}

/// #1305: a temporal denoise is inspected with its neighbouring frames.
/// Its tap is the picture after it in the normal render (the clip with
/// the denoise and no grade), at frames whose neighbours differ, and its
/// solo shows that picture; the clip's own grade after it doesn't reach
/// either.
#[test]
fn a_temporal_denoise_is_inspected_with_its_neighbours() {
    let denoise = klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 };
    let (graded, clip) = moving_clip(std::slice::from_ref(&denoise), false, Some(exposure(1.0, NodeId::new_v4())));
    let (alone, _) = moving_clip(std::slice::from_ref(&denoise), false, None);
    let (bare, _) = moving_clip(&[], false, None);
    let at = picture_node(&clip, klipp_graph_format::lifted::types::TEMPORAL_DENOISE);
    for gpu in [false, true] {
        for t in [0.5, 0.5 + 1.0 / 24.0, 0.5 + 2.0 / 24.0] {
            let what = format!("gpu {gpu} at {t:.3} s");
            let (shown, found) = moving_render(&graded, gpu, t, Inspect { clip: Some(clip.id), scope_tap: Some(at.clone()), ..Default::default() });
            assert!(found.unavailable.is_empty(), "{what}: {:?}", found.unavailable);
            let tapped = found.scope_tap.unwrap_or_else(|| panic!("{what}: no tap"));
            let after = moving_render(&alone, gpu, t, Inspect::default()).0;
            close(&format!("{what}: the tap is the picture after the denoise"), &tapped, &after);
            // Controls: the denoise changed the frame (its neighbours
            // differ), and the grade after it changes the picture shown.
            far(&format!("{what}: the denoise did something"), &tapped, &moving_render(&bare, gpu, t, Inspect::default()).0);
            far(&format!("{what}: the tap isn't the graded picture"), &tapped, &shown);
            // Run as nodes of their own for the tap, the picture is the
            // one the render draws without it.
            close(&format!("{what}: the picture shown"), &shown, &moving_render(&graded, gpu, t, Inspect::default()).0);
            let (solo, found) = moving_render(&graded, gpu, t, Inspect { clip: Some(clip.id), solo: Some(at.clone()), ..Default::default() });
            assert!(found.unavailable.is_empty(), "{what}: {:?}", found.unavailable);
            close(&format!("{what}: the solo shows the picture after the denoise"), &solo, &after);
        }
    }
}

/// #1305: a slowed clip's frame blend and the denoise after it, each
/// inspected: the blend's tap is the picture after the blend alone, the
/// denoise's the picture after both, at frames between two media frames;
/// a thumbnail of each comes back. At full speed nothing blends, and a
/// request on the blend says it doesn't run.
#[test]
fn a_frame_blend_and_a_denoise_after_it_are_each_inspected() {
    let blend = klipp_effects::BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1 };
    let denoise = klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 };
    let (both, clip) = moving_clip(&[blend.clone(), denoise.clone()], true, Some(exposure(1.0, NodeId::new_v4())));
    let (blend_alone, _) = moving_clip(std::slice::from_ref(&blend), true, None);
    let (both_alone, _) = moving_clip(&[blend.clone(), denoise.clone()], true, None);
    let at_blend = picture_node(&clip, klipp_graph_format::lifted::types::FRAME_BLEND);
    let at_denoise = picture_node(&clip, klipp_graph_format::lifted::types::TEMPORAL_DENOISE);
    for gpu in [false, true] {
        // Odd timeline frames fall halfway between two media frames.
        for t in [11.0 / 24.0, 13.0 / 24.0] {
            let what = format!("gpu {gpu} at {t:.3} s");
            let plain = moving_render(&both, gpu, t, Inspect::default()).0;
            let tap = |at: &InspectAt| {
                let (shown, found) = moving_render(&both, gpu, t, Inspect { clip: Some(clip.id), scope_tap: Some(at.clone()), ..Default::default() });
                assert!(found.unavailable.is_empty(), "{what}: {:?}", found.unavailable);
                close(&format!("{what}: the picture shown"), &shown, &plain);
                found.scope_tap.unwrap_or_else(|| panic!("{what}: no tap"))
            };
            let (blended, denoised) = (tap(&at_blend), tap(&at_denoise));
            close(&format!("{what}: the blend's tap"), &blended, &moving_render(&blend_alone, gpu, t, Inspect::default()).0);
            close(&format!("{what}: the denoise's tap"), &denoised, &moving_render(&both_alone, gpu, t, Inspect::default()).0);
            // Control: the two are told apart, so neither tap is the other
            // node's.
            far(&format!("{what}: the blend and the denoise"), &blended, &denoised);
            let thumbs = Some([at_blend.clone(), at_denoise.clone()].into_iter().collect());
            let (_, found) = moving_render(&both, gpu, t, Inspect { clip: Some(clip.id), thumbs, ..Default::default() });
            assert!(found.thumbs.contains_key(&at_blend) && found.thumbs.contains_key(&at_denoise), "{what}: {:?}", found.unavailable);
        }
    }
    // Sped up the blend doesn't run: said so, never another node's. (At
    // full speed it does, conforming a file of a lower rate, #2479.)
    let (mut fast, _) = moving_clip(&[blend, denoise], false, None);
    {
        let c = &mut fast.tracks[0].clips[0];
        c.speed = 2.0;
        c.source_out = tc(4.0);
    }
    let clip = fast.tracks[0].clips[0].clone();
    let at = picture_node(&clip, klipp_graph_format::lifted::types::FRAME_BLEND);
    let (_, found) = moving_render(&fast, false, 0.5, Inspect { clip: Some(clip.id), scope_tap: Some(at.clone()), ..Default::default() });
    assert!(found.scope_tap.is_none(), "a tap on a blend that doesn't run");
    let why = found.unavailable.iter().find(|(a, _)| *a == at).map(|(_, w)| w.clone()).expect("said to be unavailable");
    assert!(why.contains("doesn't run"), "{why}");
}


/// #3328: each effect of a clip's stack previews the picture after it, in
/// turn (a gamma, then a film grain, on a reversed clip), on the GPU and
/// on the CPU; inspecting them leaves the frame as it is drawn.
#[test]
fn each_effect_of_the_stack_previews_the_picture_after_it() {
    let grain = klipp_effects::BuiltinEffect::FilmGrain { amount: 0.5, size: 1.0, color: 0, shadows: 0.5, seed: 3 };
    let (seq, id, _) = sequence(NodeGraph::default(), None);
    let mut seq = seq;
    let clip = seq.tracks[0].clips.iter_mut().find(|c| c.id == id).unwrap();
    let mut c = TestClip::of(clip.clone()).effects([klipp_effects::BuiltinEffect::Gamma { gamma: 1.6 }, grain]).build();
    c.speed = -1.0;
    *clip = c;
    let seq = seq.baked_for_render().into_owned();
    let clip = seq.tracks[0].clips.iter().find(|c| c.id == id).unwrap();
    let picture = clip.picture_v2().unwrap();
    let at = |n| InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [n]));
    let source = at(picture.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SOURCE).unwrap().id);
    let effects: Vec<InspectAt> = clip.picture_effects().unwrap().iter().map(|e| at(e.node)).collect();
    assert_eq!(effects.len(), 2);
    for gpu in [false, true] {
        let thumbs = effects.iter().cloned().chain([source.clone()]).collect();
        let (shown, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(thumbs), ..Default::default() });
        let (s, first, second) = (found.thumbs.get(&source), found.thumbs.get(&effects[0]), found.thumbs.get(&effects[1]));
        assert!(first.is_some() && second.is_some(), "gpu {gpu}: {:?}", found.unavailable);
        assert_ne!(first, s, "gpu {gpu}: the gamma changes the source");
        assert_ne!(second, first, "gpu {gpu}: the grain changes what the gamma gave");
        close(&format!("gpu {gpu}: the frame inspected"), &shown, &plain(&seq, gpu));
    }
}

/// #3328: a clip with an effect drawn on the placed layer (a Reflection,
/// after the clip's move): the effects before it preview on the CPU and
/// the GPU; the Reflection too, on the CPU as the identity that stands in
/// for it when the clip is measured alone (#2475).
#[test]
fn a_reflection_previews_on_the_cpu_and_the_gpu() {
    let reflection = klipp_effects::BuiltinEffect::Reflection {
        waterline: 0.5, gap: 0.0, ripple_amount: 0.0, ripple_size: 60.0, ripple_speed: 0.0, breakup: 0.0, smear: 0.0, fade: 0.0, opacity: 1.0, darken: 0.0, contact: 0.0,
        blend: 0, tint: [0, 0, 0], tint_amount: 0.0, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 0.0, ripple_from: 0, seed: 0, time: 0.0,
    };
    let (seq, id, _) = sequence(NodeGraph::default(), None);
    let mut seq = seq;
    let clip = seq.tracks[0].clips.iter_mut().find(|c| c.id == id).unwrap();
    *clip = TestClip::of(clip.clone()).effects([klipp_effects::BuiltinEffect::Gamma { gamma: 1.6 }, reflection]).build();
    let seq = seq.baked_for_render().into_owned();
    let clip = seq.tracks[0].clips.iter().find(|c| c.id == id).unwrap();
    let at = |n| InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [n]));
    let effects: Vec<InspectAt> = clip.picture_effects().unwrap().iter().map(|e| at(e.node)).collect();
    for gpu in [false, true] {
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(effects.iter().cloned().collect()), ..Default::default() });
        // Both preview; a full-frame picture leaves the reflection no room
        // to show, and on the CPU the clip measured alone draws none (#2475):
        // its preview is the picture it is given.
        for (i, e) in effects.iter().enumerate() {
            assert!(found.thumbs.contains_key(e), "gpu {gpu}: effect {i}: {:?}", found.unavailable);
        }
    }
}


/// #3328: a title's picture nodes preview as a clip's do, on the GPU and
/// the CPU: its source the title as drawn (white text, which the gamma
/// leaves as it is), its effect, Placement and Output; its display step,
/// a title being display-referred, says it doesn't run.
#[test]
fn a_titles_picture_nodes_preview() {
    let td = klipp_timeline::title_library::builtin("Watermark").expect("a built-in title").data();
    let clip = TestClip::of(Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(2.0)) }).effect(klipp_effects::BuiltinEffect::Gamma { gamma: 1.6 }).build();
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let seq = seq.baked_for_render().into_owned();
    let picture = seq.tracks[0].clips[0].picture_v2().unwrap();
    let ats: Vec<(String, InspectAt)> = picture.picture.graph.nodes.iter().map(|n| (n.type_id.as_str().to_string(), InspectAt::out(NodePath::new(Root::Clip { clip: id.0 }, [n.id])))).collect();
    let of = |t: &str| ats.iter().find(|(x, _)| x == t).map(|(_, a)| a.clone()).unwrap();
    for gpu in [false, true] {
        let (_, found) = draw(&seq, gpu, Inspect { clip: Some(id), only: Some(id), thumbs: Some(ats.iter().map(|(_, a)| a.clone()).collect()), ..Default::default() });
        for (t, a) in &ats {
            let display = t == klipp_graph_format::lifted::types::SCENE_TO_DISPLAY;
            assert_eq!(found.thumbs.contains_key(a), !display, "gpu {gpu}: {t}: {:?}", found.unavailable);
            assert!(!display || found.unavailable.iter().any(|(x, _)| x == a), "gpu {gpu}: the display step says why");
        }
        let source = found.thumbs.get(&of(klipp_graph_format::lifted::types::SOURCE)).unwrap();
        assert!(source.chunks_exact(4).any(|p| p[3] > 0), "gpu {gpu}: the source preview shows the title");
    }
}
