//! Viewing a node of a clip's chain past its grade (#2879): the frame as
//! the chain leaves that node, drawn by cutting the chain there
//! (`Clip::cut_chain_at`: every node after it switched off but the
//! display step, the placement and the output). On the GPU display stack,
//! with nothing falling back, the cut frame is the frame of the same clip
//! built with the chain ending at that node; a node beside the chain is
//! not cut.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

/// A chart with a bright patch, for the glow to spread from.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = (0..w * h)
            .flat_map(|i| {
                let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                let bright = (0.4..0.6).contains(&fx) && (0.4..0.6).contains(&fy);
                if bright { [0.95, 0.9, 0.8, 1.0] } else { [0.1 + 0.3 * fx, 0.15, 0.3 * (1.0 - fy), 1.0] }
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
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

fn glow() -> E {
    E::Glow { radius: 6.0, intensity: 0.8, threshold: 0.5 }
}

/// A clip whose chain past its display step is `chain`, in order; with
/// each one's node id.
fn chained(c: &Clip, chain: &[E]) -> (Clip, Vec<klipp_graph_format::NodeId>) {
    let mut c = c.clone();
    let clip = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip }, [id]);
    let picture = c.picture_v2().unwrap().picture.clone();
    let mut last = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let mut ids = Vec::new();
    for e in chain {
        let n = effect_node(e);
        c.apply_picture_ops(&[GraphOp::AddNode { after: at(last), node: Box::new(n.clone()) }]).unwrap_or_else(|e| panic!("{e}"));
        last = n.id;
        ids.push(n.id);
    }
    (c, ids)
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn stack(seq: &Sequence) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default());
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

#[test]
fn a_node_past_the_grade_viewed_is_the_chain_cut_there() {
    let base = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let (full, ids) = chained(&base, &[E::GaussianBlur { radius: 2.0 }, glow(), E::Invert { mix: 1.0 }]);
    let mut cut = full.clone();
    assert!(cut.cut_chain_at(ids[1]), "the glow is on the chain");
    let (want, _) = chained(&base, &[E::GaussianBlur { radius: 2.0 }, glow()]);
    let Some(shown) = stack(&sequence(cut)) else { return };
    let Some(wanted) = stack(&sequence(want)) else { return };
    let Some(whole) = stack(&sequence(full)) else { return };
    assert!(worst(&shown, &wanted) <= 1, "the glow's picture: {} levels from the chain ending at it", worst(&shown, &wanted));
    assert!(worst(&shown, &whole) > 100, "not the whole chain: the invert after it is off");
}

#[test]
fn only_a_node_on_the_chain_is_cut() {
    let base = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let (mut c, ids) = chained(&base, &[glow()]);
    assert!(!c.clone().cut_chain_at(klipp_graph_format::NodeId::new_v4()), "a node the clip lacks");
    // The last node: nothing after it to switch off.
    assert!(c.cut_chain_at(ids[0]));
}
