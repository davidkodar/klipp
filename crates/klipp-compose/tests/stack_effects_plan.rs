//! On the GPU stack, a graded layer's effects and a graded adjustment
//! layer's run as nodes of the grade's plan (#1089, phase 2b M2, step
//! 10b), not as a pass over the layer after it, and the frame is the
//! legacy route's byte for byte. The render counter says the plan route
//! was taken, so a case can't pass by both falling back.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, params::NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

// The legacy route's frames, stored before step 11d deleted it: each case
// is held to them where it was held to a live legacy render.
#[path = "support/legacy_goldens.rs"]
mod legacy_goldens;
use legacy_goldens::{legacy_bytes, matches_bytes, scope};

const W: u32 = 160;
const H: u32 = 90;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

#[derive(Default)]
struct Pictures(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Pictures {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture(seed: f32, cutout: bool) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let inside = !cutout || ((40..W - 40).contains(&x) && (20..H - 20).contains(&y));
            let a = if inside { 1.0 } else { 0.0 };
            v.extend_from_slice(&[((fx + seed).fract() * 0.8) * a, fy * 0.6 * a, ((fx + fy) * 0.5) * 0.7 * a, a]);
        }
    }
    Arc::new(v)
}

fn exposure_grade(stops: f32) -> NodeGraph {
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn media(src: &mut Pictures, seed: f32, cutout: bool) -> Clip {
    let c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) };
    src.0.insert(c.id, picture(seed, cutout));
    c
}

/// A base layer and, over it, a graded layer with effects (a spill
/// group among them, cast past its cut-out edge).
fn layered() -> (Sequence, Pictures) {
    let mut src = Pictures::default();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut base = Track::new("V1", TrackKind::Video);
    base.clips = vec![media(&mut src, 0.3, false)];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut c = media(&mut src, 0.0, true);
    c.set_grade(exposure_grade(0.4));
    let fx = vec![
        E::Gamma { gamma: 1.2 },
        E::GaussianBlur { radius: 1.5 },
        E::DropShadow { offset_x: 0.03, offset_y: 0.02, blur: 0.01, color: [10, 20, 30], opacity: 0.8 },
        E::OuterGlow { radius: 0.03, intensity: 0.6, color: [250, 200, 40] },
    ];
    let n = fx.len();
    c.set_effect_list(fx, vec![true; n]);
    top.clips = vec![c];
    seq.tracks = vec![top, base];
    (seq, src)
}

/// The frame on the GPU stack, and how many clips ran their effects in
/// the grade's plan; `None` without a GPU stack.
fn render(seq: &Sequence, src: &mut Pictures, legacy: bool) -> Option<(Vec<u8>, u32)> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
    let (bytes, stats) = r.render_display_u8_on_gpu(seq, tc(1.0), W, H, src, RenderOptions::default())?;
    assert_eq!(stats.effects_fallback_lists, 0, "every plan list came from the picture");
    Some((bytes, stats.effects_followed_grade))
}

fn both(name: &str, seq: &Sequence, src: &mut Pictures, plan_clips: u32) {
    let Some((new, followed)) = render(seq, src, false) else {
        assert!(!required(), "{name}: required, but the GPU stack declined");
        eprintln!("UNVERIFIED: {name}: no GPU stack");
        return;
    };
    let legacy = legacy_bytes(|| {
        let (frame, legacy_followed) = render(seq, src, true).expect("the legacy route takes the stack too");
        assert_eq!(legacy_followed, 0, "{name}: the legacy route never splices");
        frame
    });
    assert_eq!(followed, plan_clips, "{name}: the plan route was taken");
    if !matches_bytes(&new, &legacy) {
        let worst = new.iter().zip(&legacy).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
        panic!("{name}: the plan route differs from the legacy one (worst {worst} levels)");
    }
}

#[test]
fn a_graded_layers_effects_run_in_its_grades_plan_on_the_stack() {
    let _goldens = scope("stack_effects_plan.a_graded_layers_effects_run_in_its_grades_plan_on_the_stack");
    let (seq, mut src) = layered();
    both("graded layer", &seq, &mut src, 1);
}

#[test]
fn a_graded_adjustment_layers_effects_run_in_its_grades_plan() {
    let _goldens = scope("stack_effects_plan.a_graded_adjustment_layers_effects_run_in_its_grades_plan");
    let (mut seq, mut src) = layered();
    let mut adj = Track::new("Adj", TrackKind::Video);
    let mut a = Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(4.0)) };
    a.set_grade(exposure_grade(-0.5));
    a.set_effect_list(vec![E::GaussianBlur { radius: 2.0 }, E::Vibrance { amount: 0.3 }], vec![true; 2]);
    adj.clips = vec![a];
    seq.tracks.insert(0, adj);
    both("adjustment layer", &seq, &mut src, 2);
}

#[test]
fn a_failed_grade_on_the_stack_never_drops_the_layers_effects() {
    // A grade whose run fails (a cycle) gives back its input with nothing
    // applied. A layer whose effects were spliced into it declines the
    // stack frame, so the CPU path draws it with its effects; it is never
    // placed without them.
    let (mut seq, mut src) = layered();
    let c = &mut seq.tracks[0].clips[0];
    let mut g = c.grade().clone();
    let exposure = g.nodes.iter().find(|n| n.kind == NodeKind::Exposure).unwrap().clone();
    let other = Node { id: NodeId::new_v4(), ..exposure.clone() };
    g.edges.retain(|e| e.to_node != exposure.id);
    g.edges.push(Edge { from_node: other.id, from_port: 0, to_node: exposure.id, to_port: 0 });
    g.edges.push(Edge { from_node: exposure.id, from_port: 0, to_node: other.id, to_port: 0 });
    g.nodes.push(other);
    assert!(g.topo_sort().is_err(), "the case is a cycle");
    c.set_grade(g);
    match render(&seq, &mut src, false) {
        None => {}
        Some((_, followed)) => assert_eq!(followed, 0, "a failed run is never taken as done"),
    }
}

/// Step 11d: a title on the stack is placed in its slot on the GPU, and
/// its effects run as a plan of their own over the slot (not the legacy
/// pass, which is gone): the stack draws it, counts it, and draws what
/// the renderer's own path draws for the same frame, within a level. The
/// golden corpus holds the same kind of layer ("rule/title over
/// footage") to frames recorded before M2.
#[test]
fn a_titles_effects_run_as_a_plan_over_its_slot_on_the_stack() {
    let mut src = Pictures::default();
    let base = media(&mut src, 0.3, false);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Stack".into(), font_size: 48.0, ..Default::default() });
    let mut title = Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) };
    let fx = vec![E::GaussianBlur { radius: 1.5 }, E::DropShadow { offset_x: 0.02, offset_y: 0.03, blur: 0.01, color: [0, 0, 0], opacity: 0.8 }, E::Gamma { gamma: 1.2 }];
    let n = fx.len();
    title.set_effect_list(fx, vec![true; n]);
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![title];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![base];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((stack, stats)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    assert_eq!(stats.effects_on_layers, 1, "the title's effects ran as a plan over its slot");
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    eprintln!("title on the stack vs the CPU path: worst {worst} levels");
    assert!(worst <= 2, "the stack's title differs from the CPU path by {worst} levels");
}
