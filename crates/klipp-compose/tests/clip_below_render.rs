//! Clip Below (#1746): a node beside a clip's picture chain that draws
//! what the tracks under the clip's composite to, seen through the clip's
//! placement so it lines up with what is under the clip on screen. The
//! clip is drawn in the stacking loop over that composite, as an
//! adjustment layer is; every other clip is drawn as before, to the bit.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

/// Each media item's picture, smooth so a resample is close: the
/// background a ramp, a clip on top another, and one item black.
struct Sources {
    black: MediaId,
    top: MediaId,
    /// An item whose frames are HDR (PQ codes): a scene-referred clip.
    hdr: Option<MediaId>,
}

impl FrameSource for Sources {
    fn frame(&mut self, c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let id = c.media_id()?;
        if Some(id) == self.hdr {
            let codes = klipp_color::hdr::SceneCodes::Packed10((0..w * h).map(|i| klipp_color::hdr::pack_rgb10(300 + i % 200, 400, 500)).collect());
            let pixels = SourcePixels::SceneReferred { codes: Arc::new(codes), transfer: klipp_color::hdr::HdrTransfer::Pq, source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
            return Some(SourceFrame { pixels, width: w, height: h, pts: None });
        }
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&match id {
                    id if id == self.black => [0.0, 0.0, 0.0, 1.0],
                    id if id == self.top => [0.7 - 0.4 * fy, 0.2 + 0.5 * fx, 0.35, 1.0],
                    _ => [0.1 + 0.8 * fx, 0.1 + 0.6 * fy, 0.6 * (1.0 - fx), 1.0],
                });
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn media(id: MediaId, from: f64, to: f64) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(from), tc(to)) };
    c.source_in = tc(0.0);
    c.source_out = tc(to - from);
    c
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

/// `c` with a Merge of `mode` at `opacity` after its display step and,
/// when `below`, a Clip Below beside the chain wired into its `fg`: as the
/// Picture view's Sources menu makes it.
fn merged(mut c: Clip, mode: BlendMode, opacity: f32, below: bool) -> Clip {
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let m = effect_node(&E::Merge { blend_mode: mode, opacity });
    let mut ops = vec![GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) }];
    if below {
        let b = effect_node(&E::ClipBelow {});
        ops.extend([
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(b.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(b.id), to_port: "in".into() },
            GraphOp::Connect { from: at(b.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
        ]);
    }
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    c
}

fn placed(c: Clip, scale_x: f32, scale_y: f32, rotation: f32, x: f32, y: f32) -> Clip {
    TestClip::of(c)
        .effect(E::Transform { position_x: x, position_y: y, scale_x, scale_y, rotation, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None })
        .build()
}

/// Tracks bottom to top.
fn sequence(tracks: Vec<Vec<Clip>>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks
        .into_iter()
        .enumerate()
        .rev()
        .map(|(i, clips)| {
            let mut t = Track::new(&format!("V{}", i + 1), TrackKind::Video);
            t.clips = clips;
            t
        })
        .collect();
    seq
}

fn render(gpu: bool, seq: &Sequence, src: &mut Sources, at: f64) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(at), W, H, src, RenderOptions::default(), Inspect::default())
}

fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn sources() -> Sources {
    Sources { black: MediaId::new(), top: MediaId::new(), hdr: None }
}

fn gpu_routes() -> Vec<bool> {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    if ok { vec![false, true] } else { vec![false] }
}

/// A clip on top merged by Difference with what is below it shows the
/// difference of its picture and the background's, as the formula gives.
#[test]
fn clip_below_merged_by_difference_draws_the_formula() {
    let mut src = sources();
    let ground = MediaId::new();
    let bottom = || media(ground, 0.0, 2.0);
    let top = media(src.top, 0.0, 2.0);
    let (background, _) = render(false, &sequence(vec![vec![bottom()]]), &mut src, 1.0);
    let (alone, _) = render(false, &sequence(vec![vec![top.clone()]]), &mut src, 1.0);
    let want: Vec<f32> = background.chunks_exact(4).zip(alone.chunks_exact(4)).flat_map(|(b, t)| [(t[0] - b[0]).abs(), (t[1] - b[1]).abs(), (t[2] - b[2]).abs(), t[3]]).collect();
    let seq = sequence(vec![vec![bottom()], vec![merged(top, BlendMode::Difference, 1.0, true)]]);
    for gpu in gpu_routes() {
        let (out, found) = render(gpu, &seq, &mut src, 1.0);
        assert!(found.declines.is_empty(), "gpu {gpu}: {:?}", found.declines);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {gpu}: difference against the formula: worst {worst} levels");
        assert!(worst <= 1, "gpu {gpu}: {worst} levels from the formula");
    }
}

/// A clip placed small and turned, showing what is below it whole, leaves
/// the frame the background's: what it sees lines up with what is under
/// it on screen.
#[test]
fn a_placed_clip_sees_what_is_under_it() {
    let mut src = sources();
    let ground = MediaId::new();
    let (background, _) = render(false, &sequence(vec![vec![media(ground, 0.0, 2.0)]]), &mut src, 1.0);
    for (sx, sy, rot, x, y) in [(0.5, 0.5, 0.0, 10.0, -6.0), (0.6, 0.45, 25.0, -8.0, 4.0)] {
        let top = merged(placed(media(src.top, 0.0, 2.0), sx, sy, rot, x, y), BlendMode::Normal, 1.0, true);
        let (alone, _) = render(false, &sequence(vec![vec![media(ground, 0.0, 2.0)], vec![placed(media(src.top, 0.0, 2.0), sx, sy, rot, x, y)]]), &mut src, 1.0);
        assert!(worst_levels(&alone, &background) > 20, "the clip on top changes the frame unmerged");
        // Where the placed clip covers the frame whole or not at all: its
        // edge is a blend of its picture's and what is outside its frame,
        // as any placed picture's is. And not the frame's outer pixels,
        // where what is below is read half past the frame's edge.
        let place = E::Transform { position_x: x, position_y: y, scale_x: sx, scale_y: sy, rotation: rot, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let coverage = klipp_effects::apply_effects(&[place], vec![1.0; (W * H * 4) as usize], W, H);
        let inner = |i: usize| (1..W - 1).contains(&(i as u32 % W)) && (1..H - 1).contains(&(i as u32 / W));
        let whole = |i: usize| inner(i) && (coverage[i * 4 + 3] >= 0.999 || coverage[i * 4 + 3] <= 0.001);
        let seq = sequence(vec![vec![media(ground, 0.0, 2.0)], vec![top]]);
        for gpu in gpu_routes() {
            let (out, found) = render(gpu, &seq, &mut src, 1.0);
            assert!(found.declines.is_empty(), "gpu {gpu}: {:?}", found.declines);
            let keep = |p: &[f32]| p.chunks_exact(4).enumerate().filter(|(i, _)| whole(*i)).flat_map(|(_, p)| p.to_vec()).collect::<Vec<f32>>();
            let worst = worst_levels(&keep(&out), &keep(&background));
            eprintln!("scale {sx}x{sy}, turned {rot}: gpu {gpu}: against the background: worst {worst} levels");
            assert!(worst <= 3, "scale {sx}x{sy}, turned {rot}, gpu {gpu}: {worst} levels from the background");
        }
    }
}

/// Only the middle of three tracks reads what is below it, by a merge at
/// no strength: the frame is the same, to the bit, as the same stack with
/// no Clip Below, so drawing it in the stacking loop moves nothing else.
#[test]
fn a_clip_drawn_over_what_is_below_moves_no_other_clip() {
    let mut src = sources();
    let (ground, over, mid) = (MediaId::new(), MediaId::new(), src.top);
    let stack = |below: bool| {
        let middle = merged(placed(media(mid, 0.0, 2.0), 0.7, 0.7, 0.0, 6.0, 3.0), BlendMode::Normal, 0.0, below);
        let mut top = placed(media(over, 0.0, 2.0), 0.3, 0.3, 0.0, -20.0, -10.0);
        top.blend_mode = BlendMode::Screen;
        sequence(vec![vec![media(ground, 0.0, 2.0)], vec![middle], vec![top]])
    };
    for gpu in gpu_routes() {
        let (with, _) = render(gpu, &stack(true), &mut src, 1.0);
        let (without, _) = render(gpu, &stack(false), &mut src, 1.0);
        assert!(with.iter().zip(&without).all(|(a, b)| a.to_bits() == b.to_bits()), "gpu {gpu}: the frame moved");
    }
}

/// A dissolve out of a clip that reads what is below it: the clip, by
/// Difference with the same picture below it, is black, so the dissolve
/// is the one out of a black clip.
#[test]
fn a_dissolve_out_of_a_clip_over_what_is_below_draws_it_there() {
    let mut src = sources();
    let (ground, other) = (MediaId::new(), src.top);
    let dissolve = |a: Clip| {
        let b = media(other, 1.0, 2.0);
        let t = klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(0.5),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        };
        let mut seq = sequence(vec![vec![media(ground, 0.0, 2.0)], vec![a, b]]);
        seq.tracks[0].transitions.push(t);
        seq
    };
    let reading = dissolve(merged(media(ground, 0.0, 1.0), BlendMode::Difference, 1.0, true));
    let black = dissolve(media(src.black, 0.0, 1.0));
    for gpu in gpu_routes() {
        let (out, found) = render(gpu, &reading, &mut src, 1.0);
        assert!(found.declines.is_empty(), "gpu {gpu}: {:?}", found.declines);
        let (want, _) = render(gpu, &black, &mut src, 1.0);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {gpu}: dissolve against one out of black: worst {worst} levels");
        assert!(worst <= 1, "gpu {gpu}: {worst} levels from the dissolve out of black");
    }
}

/// A clip on the bottom track has nothing below it: what it sees is
/// clear, so a Normal merge of it leaves the clip's picture.
#[test]
fn nothing_is_below_the_bottom_track() {
    let mut src = sources();
    let top = media(src.top, 0.0, 2.0);
    let (alone, _) = render(false, &sequence(vec![vec![top.clone()]]), &mut src, 1.0);
    let (out, found) = render(false, &sequence(vec![vec![merged(top, BlendMode::Normal, 1.0, true)]]), &mut src, 1.0);
    assert!(found.declines.is_empty(), "{:?}", found.declines);
    assert_eq!(worst_levels(&out, &alone), 0);
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

/// The GPU display stack lays a clip that reads what is below it over
/// what is there (#2758): by each blend mode, its frame is the CPU's
/// within a level, with nothing falling back. It declined such a frame,
/// which the timeline render drew on the CPU.
#[test]
fn the_gpu_stack_lays_a_clip_over_what_is_below_it() {
    if !gpu_routes().contains(&true) {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let mut src = sources();
    let ground = MediaId::new();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let plain = display(&render(false, &sequence(vec![vec![media(ground, 0.0, 2.0)], vec![media(src.top, 0.0, 2.0)]]), &mut src, 1.0).0);
    for mode in [
        BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay, BlendMode::SoftLight, BlendMode::HardLight,
        BlendMode::Difference, BlendMode::Exclusion, BlendMode::ColorDodge, BlendMode::ColorBurn, BlendMode::Darken, BlendMode::Lighten,
    ] {
        let seq = sequence(vec![vec![media(ground, 0.0, 2.0)], vec![merged(media(src.top, 0.0, 2.0), mode, 0.8, true)]]);
        let (cpu, found) = render(false, &seq, &mut src, 1.0);
        assert!(found.declines.is_empty(), "{mode:?}: {:?}", found.declines);
        let cpu = display(&cpu);
        let frame = klipp_effects::residency::frame("test", 1.0);
        let drawn = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        drop(frame);
        let Some((stack, _)) = drawn else { panic!("{mode:?}: the stack declined: {:?}", r.last_decline()) };
        let fell = klipp_effects::residency::last_frame();
        assert_eq!(fell.fallback_count(), 0, "{mode:?}: nothing falls back: {:?}", fell.events);
        let off = cpu.iter().zip(&stack).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(off <= 1, "{mode:?}: the stack draws {off} levels from the CPU");
        if mode != BlendMode::Normal {
            let changed = cpu.iter().zip(&plain).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(changed > 3, "{mode:?}: what is below changed the picture");
        }
    }
}

/// One whose below is seen through a move or a tilt is still the
/// timeline render's: the stack declines it, named, and the count of its
/// declines says how often.
#[test]
fn the_gpu_stack_declines_a_moved_clip_below_named() {
    let mut src = sources();
    let ground = MediaId::new();
    let top = merged(placed(media(src.top, 0.0, 2.0), 0.6, 0.6, 10.0, 12.0, 4.0), BlendMode::Difference, 1.0, true);
    let seq = sequence(vec![vec![media(ground, 0.0, 2.0)], vec![top]]);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    if !gpu_routes().contains(&true) {
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    r.set_gpu_enabled(true);
    let before = klipp_compose::GPU_STACK_DECLINES.load(std::sync::atomic::Ordering::Relaxed);
    assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none());
    assert_eq!(r.last_decline(), Some("clip below, moved or tilted"));
    assert!(klipp_compose::GPU_STACK_DECLINES.load(std::sync::atomic::Ordering::Relaxed) > before);
}

/// A precomposition that reads what is below it, whose scene holds a clip
/// that reads what is below it too: each sees its own. The scene's loop,
/// drawn inside the outer clip, leaves the outer clip's picture below as
/// it found it.
#[test]
fn a_precomposition_and_its_scene_each_see_their_own_below() {
    let mut src = sources();
    let ground = MediaId::new();
    let bottom = || media(ground, 0.0, 2.0);
    let top = media(src.top, 0.0, 2.0);
    let (background, _) = render(false, &sequence(vec![vec![bottom()]]), &mut src, 1.0);
    let (alone, _) = render(false, &sequence(vec![vec![top.clone()]]), &mut src, 1.0);
    let want: Vec<f32> = background.chunks_exact(4).zip(alone.chunks_exact(4)).flat_map(|(b, t)| [(t[0] - b[0]).abs(), (t[1] - b[1]).abs(), (t[2] - b[2]).abs(), t[3]]).collect();
    // The scene: the top clip over nothing, a Normal merge of what is below
    // it (clear there), which leaves it as it is.
    let scene = sequence(vec![vec![merged(top, BlendMode::Normal, 1.0, true)]]);
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(2.0));
    pre.source_out = tc(2.0);
    let outer = sequence(vec![vec![bottom()], vec![merged(pre, BlendMode::Difference, 1.0, true)]]);
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), scene]);
    for gpu in gpu_routes() {
        let (out, found) = render(gpu, &flat, &mut src, 1.0);
        assert!(found.declines.is_empty(), "gpu {gpu}: {:?}", found.declines);
        let worst = worst_levels(&out, &want);
        eprintln!("gpu {gpu}: nested, the precomposition by Difference against the formula: worst {worst} levels");
        assert!(worst <= 1, "gpu {gpu}: {worst} levels from the formula");
    }
}

/// What is below a clip is the composite, display-referred (#1867): an SDR
/// clip's picture is display-referred too, so it mixes; an HDR clip's
/// picture after its display step (where the Picture view puts a Merge)
/// mixes; before its display step a clip's picture is scene-referred, and
/// wiring what is below into a Merge there is refused as the edit is
/// made, saying why, the picture left as it was.
#[test]
fn what_is_below_is_display_referred() {
    let mut src = sources();
    let (ground, hdr) = (MediaId::new(), MediaId::new());
    src.hdr = Some(hdr);
    let bottom = || media(ground, 0.0, 2.0);
    // A Merge of what is below, after the display step or, `before`, just
    // after the clip's grade.
    let merged_at = |c: Clip, before: bool| -> Result<Clip, String> {
        let mut c = c;
        let picture = c.picture_v2().unwrap().picture.clone();
        let clip_id = c.id.0;
        let at = |id| NodePath::new(Root::Clip { clip: clip_id }, [id]);
        let anchor = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == if before { lt::GROUP } else { lt::SCENE_TO_DISPLAY }).unwrap().id;
        let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
        let (m, b) = (effect_node(&E::Merge { blend_mode: BlendMode::Screen, opacity: 0.5 }), effect_node(&E::ClipBelow {}));
        c.apply_picture_ops(&[
            GraphOp::AddNode { after: at(anchor), node: Box::new(m.clone()) },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(b.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(b.id), to_port: "in".into() },
            GraphOp::Connect { from: at(b.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
        ])?;
        Ok(c)
    };
    // Before the display step: refused as it is made, the picture as it was.
    let hdr_clip = media(hdr, 0.0, 2.0);
    let why = merged_at(hdr_clip.clone(), true).unwrap_err();
    assert!(why.contains("mix a scene-referred picture with a display-referred one"), "{why}");
    for gpu in gpu_routes() {
        // An SDR clip: it mixes.
        let sdr = merged_at(media(src.top, 0.0, 2.0), false).unwrap();
        let (_, found) = render(gpu, &sequence(vec![vec![bottom()], vec![sdr]]), &mut src, 1.0);
        assert!(found.declines.is_empty(), "gpu {gpu}: SDR: {:?}", found.declines);
        // An HDR clip after its display step: it mixes, and draws.
        let after = merged_at(hdr_clip.clone(), false).unwrap();
        let (plain, _) = render(gpu, &sequence(vec![vec![bottom()], vec![hdr_clip.clone()]]), &mut src, 1.0);
        let (out, found) = render(gpu, &sequence(vec![vec![bottom()], vec![after]]), &mut src, 1.0);
        assert!(found.declines.is_empty(), "gpu {gpu}: HDR after: {:?}", found.declines);
        assert!(worst_levels(&out, &plain) > 5, "gpu {gpu}: HDR after: the merge draws");
    }
}
