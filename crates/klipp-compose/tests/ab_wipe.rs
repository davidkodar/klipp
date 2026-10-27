//! The A/B wipe of a node's input against its output (#2758): the
//! wipe's left half is the frame with the node bypassed
//! (`Clip::toggle_picture_node`), any node of the picture, not only an
//! effect slot. A merge bypassed shows what it merges onto, the clip
//! alone; a node on the merge's branch bypassed shows the branch without
//! it. Each half is drawn on the GPU display stack, as the viewer draws
//! it, with nothing falling back, within a level of the picture that
//! never had the node.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_core::MediaId;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const ITEM: &str = "4e8b2c71-9a3d-4f5e-8c1b-6d2a7e9f0b13";

/// The clip's chart, and the item: a red square on green.
struct Sources;

impl FrameSource for Sources {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.8 * fx, 0.1 + 0.6 * fy, 0.6 * (1.0 - fx), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, _secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        if media != ITEM {
            return Err(SideMissing::File(format!("{media}.mov")));
        }
        Ok((0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let square = (36..60).contains(&x) && (15..39).contains(&y);
                if square { [0.8, 0.05, 0.05, 1.0] } else { [0.02, 0.75, 0.02, 1.0] }
            })
            .collect())
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn effect_node(e: &E) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

fn key() -> E {
    E::ChromaKey { key_color: [5, 190, 5], tolerance: 0.25, soft_edge: 0.05, spill_method: Default::default(), spill_strength: 0.0 }
}

fn shift() -> E {
    E::Transform { position_x: 14.0, position_y: -6.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn plain() -> Clip {
    Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }
}

/// A clip with a Normal merge after its display step, its `fg` the item
/// through each of `branch` beside the chain; with the merge's path and
/// each branch node's.
fn merged(mut c: Clip, branch: &[E]) -> (Clip, NodePath, Vec<NodePath>) {
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let m = effect_node(&E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 });
    let item = effect_node(&E::MediaSource { media: ITEM.into(), offset: 0.0 });
    let mut ops = vec![
        GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(item.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(item.id), to_port: "in".into() },
    ];
    let mut last = item.id;
    let mut nodes = Vec::new();
    for e in branch {
        let n = effect_node(e);
        ops.extend([
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(n.clone()) },
            GraphOp::Connect { from: at(last), from_port: "out".into(), to: at(n.id), to_port: "in".into() },
        ]);
        last = n.id;
        nodes.push(at(n.id));
    }
    ops.push(GraphOp::Connect { from: at(last), from_port: "out".into(), to: at(m.id), to_port: "fg".into() });
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    (c, at(m.id), nodes)
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

/// `seq` drawn on the GPU display stack, as the viewer draws a frame,
/// and nothing fell back; `None` when the stack declined and no GPU is
/// required.
fn stack(seq: &Sequence) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Sources, RenderOptions::default());
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "nothing falls back: {:?}", fell.events);
    Some(bytes)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The same clip with `edit` made: the wipe's halves differ only by it.
fn with(c: &Clip, edit: impl FnOnce(&mut Clip) -> bool) -> Clip {
    let mut c = c.clone();
    assert!(edit(&mut c), "the clip holds the node");
    c
}

#[test]
fn a_merge_bypassed_shows_what_it_merges_onto() {
    let base = plain();
    let (clip, merge, _) = merged(base.clone(), &[key(), shift()]);
    let Some(output) = stack(&sequence(clip.clone())) else { return };
    let Some(input) = stack(&sequence(with(&clip, |c| c.toggle_picture_node(&merge)))) else { return };
    let Some(alone) = stack(&sequence(base)) else { return };
    assert!(worst(&input, &alone) <= 1, "the merge's input is the clip alone: {} levels off", worst(&input, &alone));
    assert!(worst(&output, &input) > 100, "the merge lays the square over it");
    // Toggled again, the output.
    let back = with(&with(&clip, |c| c.toggle_picture_node(&merge)), |c| c.toggle_picture_node(&merge));
    let Some(again) = stack(&sequence(back)) else { return };
    assert!(worst(&again, &output) <= 1);
}

#[test]
fn a_branch_node_bypassed_shows_the_branch_without_it() {
    let base = plain();
    let (clip, _, branch) = merged(base.clone(), &[key(), shift()]);
    let Some(output) = stack(&sequence(clip.clone())) else { return };
    let Some(input) = stack(&sequence(with(&clip, |c| c.toggle_picture_node(&branch[1])))) else { return };
    // The branch without the move: keyed, the square where it was.
    let (unmoved, _, _) = merged(base, &[key()]);
    let Some(want) = stack(&sequence(unmoved)) else { return };
    assert!(worst(&input, &want) <= 1, "the move's input is the keyed item unmoved: {} levels off", worst(&input, &want));
    assert!(worst(&output, &input) > 100, "the move moves the square");
}

/// A path the clip doesn't hold leaves it as it is.
#[test]
fn a_node_the_clip_does_not_hold_is_not_toggled() {
    let (mut clip, _, _) = merged(plain(), &[key()]);
    let elsewhere = NodePath::new(Root::Clip { clip: clip.id.0 }, [klipp_graph_format::NodeId::new_v4()]);
    assert!(!clip.toggle_picture_node(&elsewhere));
    let other_clip = NodePath::new(Root::Clip { clip: klipp_graph_format::NodeId::new_v4() }, [klipp_graph_format::NodeId::new_v4()]);
    assert!(!clip.toggle_picture_node(&other_clip));
}
