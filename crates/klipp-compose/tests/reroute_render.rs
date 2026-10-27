//! A reroute in a clip's picture (#1746) is a point a wire is routed
//! through: on the chain, or beside it in a merge's picture's way, it
//! passes on what it reads, so a picture with one draws, to the bit, what
//! the same picture without it does. Its ports take the type of what
//! reaches them, so a window's matte is routed through one as a picture
//! is (#1877).

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
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
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
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

/// A grade node of `kind` as the picture holds one, with `params`.
fn grade_node(kind: &str, params: serde_json::Value) -> klipp_graph_format::Node {
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": "klipp.legacy.node",
        "label": "",
        "legacy": { "payload": { "kind": kind, "label": kind, "params": params } },
    }))
    .unwrap()
}

fn reroute() -> klipp_graph_format::Node {
    grade_node("Reroute", serde_json::json!({}))
}

/// The chart clip with a blur masked by a circle window and a merge of a
/// gradient after it; `rerouted` routes the chain, the merge's picture,
/// and the window's matte to the blur's mask, each through a reroute.
fn clip(rerouted: bool) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let blur = effect_node(&E::GaussianBlur { radius: 6.0 });
    let merge = effect_node(&E::Merge { blend_mode: BlendMode::Screen, opacity: 0.6 });
    let gradient = effect_node(&E::Generator { shape: 1, color_a: [20, 40, 220, 255], color_b: [250, 230, 200, 255], angle: 20.0, center_x: 0.5, center_y: 0.5, radius: 0.5 });
    let window = grade_node("Mask", serde_json::json!({ "masks": { "masks": [{ "shape": { "Circle": { "center_x": 0.5, "center_y": 0.5, "radius": 0.3 } }, "feather": 0.05 }] } }));
    let mut ops = vec![
        GraphOp::AddNode { after: at(display), node: Box::new(blur.clone()) },
        GraphOp::AddNode { after: at(blur.id), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(window.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(window.id), to_port: "in".into() },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(gradient.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(gradient.id), to_port: "in".into() },
    ];
    let (to_fg, to_mask) = if rerouted {
        let (on_chain, picture, matte) = (reroute(), reroute(), reroute());
        ops.extend([
            GraphOp::AddNode { after: at(blur.id), node: Box::new(on_chain) },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(picture.clone()) },
            GraphOp::Connect { from: at(gradient.id), from_port: "out".into(), to: at(picture.id), to_port: "in".into() },
            // The matte's reroute: wired to the mask before it is fed, as
            // the other order is (#1877).
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(matte.clone()) },
        ]);
        (picture.id, matte.id)
    } else {
        (gradient.id, window.id)
    };
    ops.push(GraphOp::Connect { from: at(to_mask), from_port: "out".into(), to: at(blur.id), to_port: "mask".into() });
    if to_mask != window.id {
        ops.push(GraphOp::Connect { from: at(window.id), from_port: "out".into(), to: at(to_mask), to_port: "in".into() });
    }
    ops.extend([
        GraphOp::Connect { from: at(to_fg), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
    ]);
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
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

#[test]
fn a_picture_with_reroutes_draws_what_it_does_without_them() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    let (plain_clip, rerouted_clip) = (clip(false), clip(true));
    let reroutes = rerouted_clip.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| klipp_graph_lift::structure::kind_of(n) == "Reroute").count();
    assert_eq!(reroutes, 3, "one on the chain, one in the merge's picture's way, one in the window's");
    for on_gpu in [false, true].into_iter().filter(|g| !g || gpu) {
        let (plain, found) = render(on_gpu, &plain_clip);
        assert!(found.declines.is_empty(), "gpu {on_gpu}: {:?}", found.declines);
        let (rerouted, found) = render(on_gpu, &rerouted_clip);
        assert!(found.declines.is_empty(), "gpu {on_gpu}: {:?}", found.declines);
        assert!(plain.iter().zip(&rerouted).all(|(a, b)| a.to_bits() == b.to_bits()), "gpu {on_gpu}: the reroutes changed the frame");
    }
}

/// #1877: a reroute fed a window's matte may feed a mask, but not a
/// picture's input: wired to both, it is refused at the picture, whichever
/// wire is made last, and the picture is left as it was.
#[test]
fn a_reroute_carrying_a_matte_feeds_no_picture() {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip_id = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let blur = effect_node(&E::GaussianBlur { radius: 6.0 });
    let merge = effect_node(&E::Merge { blend_mode: BlendMode::Screen, opacity: 0.6 });
    let window = grade_node("Mask", serde_json::json!({ "masks": { "masks": [{ "shape": { "Circle": { "center_x": 0.5, "center_y": 0.5, "radius": 0.3 } }, "feather": 0.05 }] } }));
    let route = reroute();
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(blur.clone()) },
        GraphOp::AddNode { after: at(blur.id), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(window.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(window.id), to_port: "in".into() },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(route.clone()) },
        GraphOp::Connect { from: at(window.id), from_port: "out".into(), to: at(route.id), to_port: "in".into() },
        GraphOp::Connect { from: at(route.id), from_port: "out".into(), to: at(blur.id), to_port: "mask".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    let before = c.picture_v2().unwrap();
    // The picture input last.
    let why = c.apply_picture_ops(&[GraphOp::Connect { from: at(route.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() }]).unwrap_err();
    // Said through the reroute, by name, so the wire can be found.
    assert!(why.contains("through Reroute") && why.contains(&route.id.to_string()) && why.contains("fg"), "{why}");
    assert_eq!(c.picture_v2().unwrap(), before, "the picture is as it was");
    // The matte last: the reroute fed after it feeds the picture.
    let mut d = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = d.picture_v2().unwrap().picture.clone();
    let clip_id = d.id.0;
    let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let why = d
        .apply_picture_ops(&[
            GraphOp::AddNode { after: at(display), node: Box::new(blur.clone()) },
            GraphOp::AddNode { after: at(blur.id), node: Box::new(merge.clone()) },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(window.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(window.id), to_port: "in".into() },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(route.clone()) },
            GraphOp::Connect { from: at(route.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
            GraphOp::Connect { from: at(route.id), from_port: "out".into(), to: at(blur.id), to_port: "mask".into() },
            GraphOp::Connect { from: at(window.id), from_port: "out".into(), to: at(route.id), to_port: "in".into() },
        ])
        .unwrap_err();
    assert!(why.contains("through Reroute") && why.contains(&merge.id.to_string()) && why.contains("fg"), "refused at the picture input, through the reroute: {why}");
}

/// A key beside the chain reading the clip's source through a reroute: the
/// reroute's input rewired to a picture mid-chain would give the key one
/// it can't read, so the edit is refused, naming the key, and the picture
/// is left as it was. Rewired back to the source, it is taken.
#[test]
fn rewiring_a_reroute_checks_what_reads_it() {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip_id = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
    let (source, display) = (
        picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id,
        picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id,
    );
    let blur = effect_node(&E::GaussianBlur { radius: 6.0 });
    let key = grade_node("LumaKey", serde_json::json!({ "luma_key": { "enabled": true, "low": 0.6, "high": 1.0, "soft": 0.1, "invert": false } }));
    let route = reroute();
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(blur.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(route.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(route.id), to_port: "in".into() },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(key.clone()) },
        GraphOp::Connect { from: at(route.id), from_port: "out".into(), to: at(key.id), to_port: "in".into() },
        GraphOp::Connect { from: at(key.id), from_port: "out".into(), to: at(blur.id), to_port: "mask".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    let before = c.picture_v2().unwrap();
    let why = c.apply_picture_ops(&[GraphOp::Connect { from: at(blur.id), from_port: "out".into(), to: at(route.id), to_port: "in".into() }]).unwrap_err();
    assert!(why.contains(&key.id.to_string()) && why.contains("can read the clip's source, its grade's picture or a node beside the chain"), "{why}");
    assert_eq!(c.picture_v2().unwrap(), before, "the picture is as it was");
    c.apply_picture_ops(&[GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(route.id), to_port: "in".into() }]).unwrap_or_else(|e| panic!("back to the source: {e}"));
}
