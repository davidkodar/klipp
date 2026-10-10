//! A4 `effect_nodes_keep_the_legacy_order_rules` (the acceptance spec's A4,
//! phase 2b M2 after steps 11b and 11c): with a clip's effects run as plan
//! nodes, every ordering rule the legacy chain follows still holds, on
//! every path a frame can take: the renderer on the CPU, the renderer with
//! the GPU (where the plan route runs), and the GPU stack.
//!
//! Each case renders one small timeline on the plan route and holds it to
//! the legacy route's frames, stored before step 11d deleted that route
//! (`support/legacy_goldens.rs`), standing in for BASE: every path within
//! a level of an 8-bit picture: the
//! renderer's linear frames once display-encoded, the stack's bytes as
//! they are. The decline set is the same on both routes: the stack takes
//! or declines a frame alike, and the renderer uploads the same layers.
//! The plan route must also have been taken (a counter), so a case can't
//! pass by both routes being the legacy one.
//!
//! A missing GPU leaves the GPU paths unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, params::NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

// The legacy route's frames, stored before step 11d deleted it: each case
// is held to them where it was held to a live legacy render.
#[path = "support/legacy_goldens.rs"]
mod legacy_goldens;
use legacy_goldens::{legacy_bytes, scope};

const W: u32 = 160;
const H: u32 = 90;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Each media clip's own picture: hue across, brightness down, opaque.
#[derive(Default, Clone)]
struct Pictures(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Pictures {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture(seed: f32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let block = if ((fx * 8.0) as u32 + (fy * 4.0) as u32).is_multiple_of(2) { 1.0 } else { 0.6 };
            v.extend_from_slice(&[((fx + seed).fract() * 0.8) * block, (0.1 + fy * 0.6) * block, ((fx + fy) * 0.35 + 0.05) * block, 1.0]);
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

/// A graded media clip carrying `fx`, all enabled unless `enabled` says.
fn media(src: &mut Pictures, seed: f32, fx: Vec<E>, enabled: Option<Vec<bool>>) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) };
    c.set_grade(exposure_grade(0.3));
    let on = enabled.unwrap_or_else(|| vec![true; fx.len()]);
    let mut b = TestClip::of(c);
    for (i, e) in fx.into_iter().enumerate() {
        b = b.effect(e);
        if !on.get(i).copied().unwrap_or(true) {
            b = b.off();
        }
    }
    let c = b.build();
    src.0.insert(c.id, picture(seed));
    c
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

fn track(name: &str, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, TrackKind::Video);
    t.clips = clips;
    t
}

fn transform(x: f32, scale: f32, rotation: f32) -> E {
    E::Transform { position_x: x, position_y: 0.0, scale_x: scale, scale_y: scale, rotation, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn shadow() -> E {
    E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity: 0.8 }
}

fn glow() -> E {
    E::OuterGlow { radius: 0.03, intensity: 0.6, color: [255, 200, 120] }
}

/// One route's frame on every path, and what each path declined.
struct Paths {
    /// The renderer, GPU off.
    cpu: Vec<f32>,
    /// The renderer, GPU on: where the plan route runs.
    gpu: Vec<f32>,
    /// The GPU stack's display bytes, or `None` when it declined.
    stack: Option<Vec<u8>>,
    /// Layers the renderer drew on the CPU and uploaded.
    cpu_layers: u32,
    /// Clips whose effects ran as plans, on the GPU renderer and the stack.
    plans: u32,
}

fn render(seq: &Sequence, src: &Pictures, at: f64, legacy: bool, host: impl Fn() -> klipp_plugin::OfxHost) -> Paths {
    let renderer = |gpu: bool| {
        let mut r = Renderer::new(host(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
        r
    };
    let (cpu, _) = renderer(false).render_inspected(seq, tc(at), W, H, &mut src.clone(), RenderOptions::default(), Inspect::default());
    let (gpu, found) = renderer(true).render_inspected(seq, tc(at), W, H, &mut src.clone(), RenderOptions::default(), Inspect::default());
    let stack = renderer(true).render_display_u8_on_gpu(seq, tc(at), W, H, &mut src.clone(), RenderOptions::default());
    let plans = found.stats.plan_clips() + stack.as_ref().map_or(0, |(_, s)| s.plan_clips());
    Paths { cpu, gpu, stack: stack.map(|(px, _)| px), cpu_layers: found.stats.cpu_layers, plans }
}

/// A linear frame as display bytes, the sRGB encode a viewer applies.
fn encoded(linear: &[f32]) -> Vec<u8> {
    klipp_effects::linear_f32_rgba_to_srgb_u8(linear)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// The rule holds on every path: plan and legacy within a level, the same
/// paths declined, and the plan route taken somewhere it could be.
/// The legacy route's paths as the cases compare them: the renderer's
/// frames display-encoded, the stack's bytes or its decline, and the
/// layers drawn on the CPU.
struct Legacy {
    cpu: Vec<u8>,
    gpu: Vec<u8>,
    stack: Option<Vec<u8>>,
    cpu_layers: u32,
}

/// `Legacy` packed into one stored frame: each part's length, then it.
fn pack(p: &Paths) -> Vec<u8> {
    let mut out = Vec::new();
    for part in [Some(encoded(&p.cpu)), Some(encoded(&p.gpu)), p.stack.clone()] {
        match part {
            Some(b) => {
                out.extend((b.len() as u32).to_le_bytes());
                out.extend(b);
            }
            None => out.extend(u32::MAX.to_le_bytes()),
        }
    }
    out.extend(p.cpu_layers.to_le_bytes());
    out
}

fn unpack(b: &[u8]) -> Legacy {
    let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let mut at = 0;
    let mut parts = Vec::new();
    for _ in 0..3 {
        let n = word(at);
        at += 4;
        if n == u32::MAX {
            parts.push(None);
        } else {
            parts.push(Some(b[at..at + n as usize].to_vec()));
            at += n as usize;
        }
    }
    let cpu_layers = word(at);
    let mut parts = parts.into_iter();
    Legacy { cpu: parts.next().flatten().unwrap(), gpu: parts.next().flatten().unwrap(), stack: parts.next().flatten(), cpu_layers }
}

fn holds(name: &str, seq: &Sequence, src: &Pictures, at: f64, host: impl Fn() -> klipp_plugin::OfxHost + Copy) {
    let plan = render(seq, src, at, false, host);
    let legacy = unpack(&legacy_bytes(|| {
        let l = render(seq, src, at, true, host);
        assert_eq!(l.plans, 0, "{name}: the legacy route runs no plan");
        pack(&l)
    }));
    let cpu = worst(&encoded(&plan.cpu), &legacy.cpu);
    let gpu = worst(&encoded(&plan.gpu), &legacy.gpu);
    eprintln!("{name}: cpu {cpu}, gpu {gpu}, stack {:?}, plans {}", plan.stack.as_ref().zip(legacy.stack.as_ref()).map(|(a, b)| worst(a, b)), plan.plans);
    assert!(cpu <= 1, "{name}: the CPU path differs by {cpu} levels");
    assert!(gpu <= 1, "{name}: the GPU path differs by {gpu} levels");
    assert_eq!(plan.stack.is_some(), legacy.stack.is_some(), "{name}: the stack takes or declines alike");
    if let (Some(a), Some(b)) = (&plan.stack, &legacy.stack) {
        let s = worst(a, b);
        assert!(s <= 1, "{name}: the stack differs by {s} levels");
    }
    assert_eq!(plan.cpu_layers, legacy.cpu_layers, "{name}: the same layers drawn on the CPU");
    if plan.plans == 0 {
        assert!(!required(), "{name}: required, but no path ran the effects as a plan");
        eprintln!("UNVERIFIED: {name}: no GPU route");
    }
    // The case is a real one: its effects change the picture.
    let mut bare = seq.clone();
    for t in &mut bare.tracks {
        for c in &mut t.clips {
            c.set_effect_list(vec![], vec![]);
        }
        t.effects.set_video_effect_list(vec![], vec![]);
    }
    assert!(encoded(&plan.cpu) != encoded(&render(&bare, src, at, false, host).cpu), "{name}: the effects change the picture");
}

fn plain_host() -> klipp_plugin::OfxHost {
    klipp_plugin::OfxHost::new()
}

/// The built-in invert plug-in keeps its host in a process-wide pointer,
/// so the case that loads it runs alone.
static PLUGIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn invert_host() -> klipp_plugin::OfxHost {
    let mut host = klipp_plugin::OfxHost::new();
    let at = host.register_raw_plugin(klipp_plugin::test_plugin::create_test_plugin_raw());
    host.initialise_plugin(at).expect("the invert plug-in loads");
    host
}

/// (a) A plug-in is the opaque stage between the grade and the built-in
/// effects: the effects take the inverted picture.
#[test]
fn a_plugin_runs_before_the_built_in_effects() {
    let _goldens = scope("native_effect_order.a_plugin_runs_before_the_built_in_effects");
    let _turn = PLUGIN.lock().unwrap_or_else(|e| e.into_inner());
    let mut src = Pictures::default();
    let mut c = media(&mut src, 0.0, vec![E::Gamma { gamma: 1.3 }, E::Vignette { amount: 0.6, size: 0.5, softness: 0.4, roundness: 1.0 }], None);
    c.set_ofx_effects(vec![klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() }]);
    holds("plug-in first", &sequence(vec![track("V1", vec![c])]), &src, 1.0, invert_host);
}

/// (b) The automatic placement goes before the first effect that draws
/// outside the picture, so a shadow is cast by the placed picture; with
/// no such effect the placement comes last.
#[test]
fn the_automatic_placement_goes_before_the_first_effect_drawing_outside() {
    let _goldens = scope("native_effect_order.the_automatic_placement_goes_before_the_first_effect_drawing_outside");
    for (name, fx) in [
        ("with a shadow", vec![E::Gamma { gamma: 1.2 }, shadow(), E::Vibrance { amount: 0.3 }]),
        ("without", vec![E::Gamma { gamma: 1.2 }, E::Vibrance { amount: 0.3 }]),
    ] {
        let mut src = Pictures::default();
        let mut c = media(&mut src, 0.2, fx, None);
        c = TestClip::of(c).lane("scale", 0.0, 0.6).build();
        let base = media(&mut src, 0.5, vec![], None);
        holds(&format!("automatic placement, {name}"), &sequence(vec![track("V2", vec![c]), track("V1", vec![base])]), &src, 1.0, plain_host);
    }
}

/// (c) Two enabled Transforms both apply, in order, and a disabled one
/// carrying values does nothing.
#[test]
fn two_transforms_apply_in_order_and_a_disabled_one_does_nothing() {
    let _goldens = scope("native_effect_order.two_transforms_apply_in_order_and_a_disabled_one_does_nothing");
    let mut src = Pictures::default();
    let fx = vec![transform(0.0, 0.8, 0.0), E::Gamma { gamma: 1.1 }, transform(10.0, 1.0, 8.0), transform(40.0, 0.5, 30.0)];
    let c = media(&mut src, 0.1, fx, Some(vec![true, true, true, false]));
    let base = media(&mut src, 0.6, vec![], None);
    holds("two transforms and a disabled one", &sequence(vec![track("V2", vec![c]), track("V1", vec![base])]), &src, 1.0, plain_host);
}

/// (d) A clip whose placement is the identity, under an open camera
/// shutter: it doesn't move, so the shutter leaves it as it is.
#[test]
fn an_identity_placement_under_an_open_shutter_is_left_as_it_is() {
    let _goldens = scope("native_effect_order.an_identity_placement_under_an_open_shutter_is_left_as_it_is");
    let mut src = Pictures::default();
    let c = media(&mut src, 0.3, vec![E::Gamma { gamma: 1.2 }, transform(0.0, 1.0, 0.0), E::Vignette { amount: 0.5, size: 0.6, softness: 0.4, roundness: 1.0 }], None);
    let mut seq = sequence(vec![track("V1", vec![c])]);
    seq.camera.shutter_deg = 180.0;
    seq.camera.shutter_samples = 8;
    holds("identity under a shutter", &seq, &src, 1.0, plain_host);
}

/// (e) A shadow and an outer glow next to each other run as one spill
/// group; separated by a blur, as two.
#[test]
fn spill_effects_group_when_adjacent_and_split_when_separated() {
    let _goldens = scope("native_effect_order.spill_effects_group_when_adjacent_and_split_when_separated");
    for (name, fx) in [
        ("adjacent", vec![E::Gamma { gamma: 1.1 }, shadow(), glow()]),
        ("separated by a blur", vec![E::Gamma { gamma: 1.1 }, shadow(), E::GaussianBlur { radius: 2.0 }, glow()]),
    ] {
        let mut src = Pictures::default();
        let mut c = media(&mut src, 0.4, fx, None);
        c = TestClip::of(c).lane("scale", 0.0, 0.6).build();
        let base = media(&mut src, 0.7, vec![], None);
        holds(&format!("spill, {name}"), &sequence(vec![track("V2", vec![c]), track("V1", vec![base])]), &src, 1.0, plain_host);
    }
}

/// (f) A track's effects run after the clip's own, and each list's
/// Opacity stays its own: the clip's weighs the clip, the track's the
/// track (#1078).
#[test]
fn track_effects_follow_the_clips_own_with_each_opacity_its_own() {
    let _goldens = scope("native_effect_order.track_effects_follow_the_clips_own_with_each_opacity_its_own");
    let mut src = Pictures::default();
    let c = media(&mut src, 0.5, vec![E::Gamma { gamma: 1.2 }, E::Opacity { value: 0.7 }], None);
    let base = media(&mut src, 0.8, vec![], None);
    let mut top = track("V2", vec![c]);
    top.effects.set_video_effect_list(vec![E::Vignette { amount: 0.6, size: 0.5, softness: 0.4, roundness: 1.0 }, E::Opacity { value: 0.5 }], vec![true, true]);
    holds("track effects after the clip's", &sequence(vec![top, track("V1", vec![base])]), &src, 1.0, plain_host);
}

/// (g) An adjustment layer grades what lies beneath it, runs its effects,
/// and is mixed back over it by its opacity.
#[test]
fn an_adjustment_layer_grades_then_runs_its_effects_then_mixes_by_opacity() {
    let _goldens = scope("native_effect_order.an_adjustment_layer_grades_then_runs_its_effects_then_mixes_by_opacity");
    let mut src = Pictures::default();
    let base = media(&mut src, 0.2, vec![], None);
    let mut adj = Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(4.0)) };
    adj.set_grade(exposure_grade(-0.4));
    adj = TestClip::of(adj).effects([E::Vibrance { amount: 0.5 }, E::GaussianBlur { radius: 1.5 }, E::Opacity { value: 0.6 }]).build();
    holds("adjustment layer", &sequence(vec![track("V2", vec![adj]), track("V1", vec![base])]), &src, 1.0, plain_host);
}
