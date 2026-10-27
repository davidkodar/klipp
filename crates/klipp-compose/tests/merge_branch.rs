//! A merge lays the end of a branch beside the clip's chain (#2758): a
//! media item, keyed and moved by effects beside the chain, its last one
//! wired into the merge's `fg`. The item is a red square on green; keyed,
//! the green goes and the clip shows there, the square moved over it. On
//! the GPU display stack the frame is the CPU's within a level, with
//! nothing falling back; a branch that reads the clip's chain is refused,
//! said.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const ITEM: &str = "7a1c5e92-3f4b-4d8a-b6c0-2e9f1d3a5b47";

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

/// An effect node for `e`, as the Picture view makes one.
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

/// The chart clip with a Normal merge after its display step, and beside
/// the chain the item, then each of `branch`, the last wired into the
/// merge's `fg`. With `from_chain`, the branch's first effect reads the
/// clip's display step instead of the item.
fn clip(branch: &[E], from_chain: bool) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let m = effect_node(&E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 });
    let item = effect_node(&E::MediaSource { media: ITEM.into(), offset: 0.0 });
    let mut ops = vec![
        GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(item.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(item.id), to_port: "in".into() },
    ];
    let mut last = if from_chain { display } else { item.id };
    for e in branch {
        let n = effect_node(e);
        ops.extend([
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(n.clone()) },
            GraphOp::Connect { from: at(last), from_port: "out".into(), to: at(n.id), to_port: "in".into() },
        ]);
        last = n.id;
    }
    ops.push(GraphOp::Connect { from: at(last), from_port: "out".into(), to: at(m.id), to_port: "fg".into() });
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    c
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn cpu(seq: &Sequence) -> (Vec<u8>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (pixels, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Sources, RenderOptions::default(), Inspect::default());
    (display(&pixels), found)
}

fn rgb(frame: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [frame[i], frame[i + 1], frame[i + 2]]
}

#[test]
fn a_keyed_moved_branch_is_laid_over_the_clip_on_the_gpu_stack() {
    let (plain, _) = cpu(&sequence(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }));
    let seq = sequence(clip(&[key(), shift()], false));
    let (want, found) = cpu(&seq);
    assert!(found.declines.is_empty(), "{:?}", found.declines);
    // Keyed: where the green was, the clip.
    assert_eq!(rgb(&want, 8, 45), rgb(&plain, 8, 45), "the green is keyed out");
    // The item keyed and moved by the effects' own reference: where it is
    // whole red, the merge shows it; where it is clear, the clip.
    let item = Sources.media_frame(ITEM, 1.0, WorkingColorspace::Rec709Linear, W, H).unwrap();
    let laid = klipp_effects::apply_effects(&[key(), shift()], item, W, H);
    let at = |x: u32, y: u32| laid[((y * W + x) * 4) as usize..((y * W + x) * 4 + 4) as usize].to_vec();
    let square = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).find(|&(x, y)| { let p = at(x, y); p[3] > 0.999 && p[0] > 0.5 && p[1] < 0.2 }).expect("the square somewhere");
    let red = rgb(&want, square.0, square.1);
    assert!(red[0] > 180 && red[1] < 90, "the square at {square:?}: {red:?}");
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Sources, RenderOptions::default());
    drop(frame);
    let Some((stack, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        return;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "nothing falls back: {:?}", fell.events);
    let off = want.iter().zip(&stack).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(off <= 1, "the stack draws {off} levels from the CPU");
}

/// A branch can't read the clip's chain past its grade: wiring one is
/// refused, said.
#[test]
fn a_branch_reading_the_chain_is_refused_said() {
    let caught = std::panic::catch_unwind(|| clip(&[key()], true));
    let why = caught.expect_err("refused").downcast::<String>().map(|s| *s).unwrap_or_default();
    assert!(why.contains("can read the clip's source, its grade's picture or a node beside the chain"), "said why: {why}");
}

