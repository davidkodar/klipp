//! A clip's effects take its grade's result where it sits on the GPU,
//! with no download and upload between them, and draw exactly what the
//! legacy route draws (#1089, phase 2b M2 step 10).
//!
//! Each case renders one graded clip with effects on the new route and
//! compares the linear floats bit for bit with the legacy route's frame,
//! stored before step 11d deleted that route (`support/legacy_goldens.rs`;
//! within one display level on another GPU or driver). The same shaders
//! ran on the same f32 frame either way, so anything but equal is a fault. The render counter says which
//! route a frame took, so a case can't pass by both falling back.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::{self, TestClip};

// The legacy route's frames, stored before step 11d deleted it: each test
// is held to them where it was held to a live legacy render.
#[path = "support/legacy_goldens.rs"]
mod legacy_goldens;
// An analysis effect's analysis, as its media's asset (M4 step 9).
#[path = "support/analysis_assets.rs"]
mod analysis;
use legacy_goldens::{legacy_f32, matches_f32, scope, within_cpu_gpu_bar};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[fx * 0.9, fy * 0.7, (1.0 - fx) * 0.6 + 0.05, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A grade that runs on the GPU: one exposure node.
fn graded(effects: Vec<E>) -> Sequence {
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.4), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let graph = NodeGraph {
        output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(),
        working_space_override: None, frames: vec![], sticky_notes: vec![],
    };
    let mut clip = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    clip.set_grade(graph);
    let clip = TestClip::of(clip).effects(effects).build();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

/// The frame and how many clips took the new route.
fn render(seq: &Sequence, legacy: bool) -> (Vec<f32>, u32) {
    render_with(seq, legacy, false)
}

/// `render`, with every effect's recording refused when `refuse` (as a
/// lost device refuses it).
fn render_with(seq: &Sequence, legacy: bool, refuse: bool) -> (Vec<f32>, u32) {
    render_at(seq, legacy, refuse, 1.0)
}

/// `render_with` at `secs` into the sequence.
fn render_at(seq: &Sequence, legacy: bool, refuse: bool, secs: f64) -> (Vec<f32>, u32) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
    // Both pipelines: the grade's (whose chain the effects follow on) and
    // the renderer's own (its layers and stack).
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(refuse);
    }
    if let Some(g) = r.gpu_pipeline() {
        g.refuse_recording(refuse);
    }
    let (out, found) = r.render_inspected(seq, tc(secs), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
    let chain = r.executor().last_run_chain_stats();
    LAST.with(|l| l.set((found.stats.cpu_layers, chain.effect_breaks, chain.submits)));
    ON_CPU.with(|c| c.set(found.stats.effects_on_cpu));
    (out, found.stats.effects_followed_grade)
}

thread_local! {
    /// The last render's declines (`cpu_layers`), effect breaks and
    /// submits, for the cases that check them.
    static LAST: std::cell::Cell<(u32, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
    /// The last render's effect runs the CPU kernels took after a plan
    /// couldn't apply them.
    static ON_CPU: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// How far apart the two routes may be on aarch64; elsewhere they are
/// bit for bit. Apple's GPU under Asahi compiles the two routes'
/// shaders to round differently: measured up to 2.4e-6 on an M1, a
/// sixth of a 16-bit level.
const AARCH64_ROUNDING: f32 = 1e-5;

fn last() -> (u32, u32, u32) {
    LAST.with(|l| l.get())
}

/// Both routes, bit for bit; `Some(followed)` when the GPU was there.
fn both(name: &str, effects: Vec<E>) -> Option<bool> {
    both_seq(name, graded(effects))
}

/// [`both`] on a sequence of its own.
fn both_seq(name: &str, seq: Sequence) -> Option<bool> {
    let (new, followed) = render(&seq, false);
    let (declines, breaks, submits) = last();
    let legacy = legacy_f32(|| {
        let (frame, legacy_followed) = render(&seq, true);
        assert_eq!(declines, last().0, "{name}: no new declines");
        assert_eq!(legacy_followed, 0, "{name}: the legacy route never follows");
        frame
    });
    if followed == 1 {
        let recorded = test_clip::effects(&seq.tracks[0].clips[0]).iter().all(|e| klipp_effects::gpu::slots_needed(e).is_some());
        if recorded {
            assert_eq!((breaks, submits), (0, 1), "{name}: grade and effects in one submit");
        }
    }
    if new.iter().map(|v| v.to_bits()).ne(legacy.iter().map(|v| v.to_bits())) {
        let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        let nan_apart = new.iter().zip(&legacy).any(|(a, b)| a.is_nan() != b.is_nan());
        if !cfg!(target_arch = "aarch64") || nan_apart || !(worst <= AARCH64_ROUNDING) {
            panic!("{name}: the new route differs from the legacy one (worst {worst})");
        }
    }
    Some(followed == 1)
}

fn colour_family() -> Vec<E> {
    vec![
        E::Invert { mix: 0.3 },
        E::Posterize { levels: 9.0 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 12.0, saturation: 1.1, value: 0.95 },
        E::Levels { in_black: 0.02, in_white: 0.95, gamma: 1.1, out_black: 0.0, out_white: 1.0 },
        E::Vibrance { amount: 0.3 },
    ]
}

#[test]
fn the_colour_family_follows_the_grade_and_draws_the_same() {
    let _goldens = scope("effects_follow_grade.the_colour_family_follows_the_grade_and_draws_the_same");
    let gpu = both("colour family", colour_family()).unwrap();
    if !gpu {
        assert!(!required(), "required, but the effects did not follow the grade on the GPU");
        eprintln!("UNVERIFIED: no GPU route");
    }
}

#[test]
fn an_effect_not_yet_recorded_still_follows_and_draws_the_same() {
    let _goldens = scope("effects_follow_grade.an_effect_not_yet_recorded_still_follows_and_draws_the_same");
    // Blur isn't recorded yet (step 7's later family): it runs in its own
    // pass on the same frame, after the recorded ones.
    let mut fx = colour_family();
    fx.push(E::GaussianBlur { radius: 3.0 });
    fx.push(E::Gamma { gamma: 0.9 });
    let gpu = both("with a blur", fx).unwrap();
    assert!(gpu || !required());
}

/// Step 11b: a clip with no grade runs its effects as a plan too (the
/// source straight to the output, the effects between), and draws the
/// legacy route's picture.
#[test]
fn a_clip_with_no_grade_runs_its_effects_as_a_plan() {
    let _goldens = scope("effects_follow_grade.a_clip_with_no_grade_runs_its_effects_as_a_plan");
    let mut seq = graded(colour_family());
    seq.tracks[0].clips[0].set_grade(NodeGraph::default());
    assert!(seq.tracks[0].clips[0].grade_is_identity());
    let gpu = both_seq("no grade", seq).unwrap();
    assert!(gpu || !required(), "required, but the effects did not run as a plan");
}

/// An injected failure (every effect's recording refused, as a lost
/// device refuses it): the grade's picture is gone with the GPU run, so
/// the frame is graded again and its effects drawn by the CPU kernels,
/// counted, and never drawn without them.
#[test]
fn an_effect_that_could_not_be_recorded_is_drawn_by_the_cpu_kernels() {
    // The stored frames keep the name they were recorded under.
    let _goldens = scope("effects_follow_grade.an_effect_that_could_not_be_recorded_sends_the_frame_the_legacy_way");
    let seq = graded(colour_family());
    let (new, followed) = render_with(&seq, false, true);
    let legacy = legacy_f32(|| render_with(&seq, true, true).0);
    assert_eq!(followed, 0, "nothing followed the grade");
    assert_eq!(ON_CPU.with(|c| c.get()), u32::from(gpu()), "the CPU kernels took the effects, counted");
    assert!(within_cpu_gpu_bar(&new, &legacy), "the legacy route's picture, within the CPU/GPU bar");
    let all = legacy_f32(|| render(&seq, true).0);
    assert!(within_cpu_gpu_bar(&new, &all), "with every effect in it");
}

#[test]
fn a_grade_run_that_fails_never_drops_the_effects() {
    let _goldens = scope("effects_follow_grade.a_grade_run_that_fails_never_drops_the_effects");
    // A grade whose run fails (here a cycle, the executor's error) gives
    // back its input with nothing applied. The spliced effects were in
    // that run, so the CPU kernels draw them.
    let mut seq = graded(colour_family());
    let clip = &mut seq.tracks[0].clips[0];
    // Two exposures feeding each other, the output reading one of them.
    let mut g = clip.grade().clone();
    let exposure = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Exposure).unwrap().clone();
    let other = Node { id: NodeId::new_v4(), ..exposure.clone() };
    g.edges.retain(|e| e.to_node != exposure.id);
    g.edges.push(Edge { from_node: other.id, from_port: 0, to_node: exposure.id, to_port: 0 });
    g.edges.push(Edge { from_node: exposure.id, from_port: 0, to_node: other.id, to_port: 0 });
    g.nodes.push(other);
    assert!(g.topo_sort().is_err(), "the case is a cycle");
    clip.set_grade(g);
    let (new, followed) = render(&seq, false);
    let legacy = legacy_f32(|| render(&seq, true).0);
    assert_eq!(followed, 0, "the failed run is not taken as done");
    assert_eq!(ON_CPU.with(|c| c.get()), u32::from(gpu()), "the CPU kernels took the effects, counted");
    assert!(within_cpu_gpu_bar(&new, &legacy), "the legacy route's picture, effects in it, within the CPU/GPU bar");
    let mut no_fx = seq.clone();
    no_fx.tracks[0].clips[0].set_effect_list(vec![], vec![]);
    let bare = legacy_f32(|| render(&no_fx, true).0);
    assert!(new != bare, "the effects are in the picture");
}

#[test]
fn animated_effects_and_a_fixed_placement_from_the_picture_draw_the_legacy_frames() {
    let _goldens = scope("effects_follow_grade.animated_effects_and_a_fixed_placement_from_the_picture_draw_the_legacy_frames");
    // Step 10c-2: the plan route's effects come from the clip's lifted
    // picture. A keyed parameter, a modifier stack, and a Transform effect
    // placed after a shadow while the clip moves, at times around the keys.
    let shadow = E::DropShadow { offset_x: 0.03, offset_y: 0.02, blur: 0.01, color: [10, 20, 40], opacity: 0.8 };
    let tf = E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.7, scale_y: 0.7, rotation: 4.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 8.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let mut seq = graded(vec![E::Gamma { gamma: 1.0 }, E::Vibrance { amount: 0.2 }, shadow, tf]);
    let clip = &mut seq.tracks[0].clips[0];
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    *clip = TestClip::of(clip.clone()).key(0, "gamma", 0.2, 0.7).key(0, "gamma", 1.0, 1.5).key(0, "gamma", 1.8, 0.9).build();
    clip.edit_param_modifiers(|m| {
        let osc = klipp_core::Modifier::Oscillate { amplitude: 0.3, frequency: 1.3, phase: 0.1, waveform: Default::default(), blend: Default::default(), enabled: true };
        m.insert(klipp_core::ParamId::Effect { effect_index: 1, param_name: "amount".into() }, klipp_core::ModifierStack { modifiers: vec![osc] });
    });
    clip.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, -10.0), key(2.0, 10.0)]);
    let mut seen = Vec::new();
    let counted = klipp_compose::PLAN_EFFECT_CLIPS.load(std::sync::atomic::Ordering::Relaxed);
    for t in [0.1, 0.6, 1.0, 1.4, 1.9] {
        let (new, followed) = render_at(&seq, false, false, t);
        let legacy = legacy_f32(|| render_at(&seq, true, false, t).0);
        assert!(followed == 1 || !required(), "at {t}: the plan route was taken");
        assert!(matches_f32(&new, &legacy), "at {t}: the legacy route's frame");
        seen.push(new);
    }
    assert!(seen.windows(2).all(|w| w[0] != w[1]), "the frames animate");
    // The process-wide count the control socket's `perf` reports (other
    // tests run alongside, so it has grown by at least these five).
    let now = klipp_compose::PLAN_EFFECT_CLIPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(now >= counted + 5 || !required(), "{counted} -> {now}");
}

/// Stabilisation reads its analysis, which reaches the plan's executor as
/// its node's (#1209, step 12; M4 step 9): a graded clip with it takes the
/// plan route and draws its correction as the legacy route does.
#[test]
fn a_graded_clip_with_stabilisation_keeps_its_correction() {
    let _goldens = scope("effects_follow_grade.a_graded_clip_with_stabilisation_keeps_its_correction");
    let seq = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let shifted = || analysed_by(renderer(false), vec![analysis::stabilised_by(0.04, -0.02, 96)]);
    let (new, followed) = draw(&mut shifted(), &seq, 1.0, &mut Moving);
    let legacy = legacy_f32(|| draw(&mut shifted(), &seq, 1.0, &mut Moving).0);
    assert!(matches_f32(&new, &legacy), "the correction is drawn on both routes");
    if gpu() {
        assert!(followed, "a stabilised clip takes the plan route (step 12)");
    }
    // The chart gives no analysis key: the clip without its analysis.
    let uncorrected = legacy_f32(|| draw(&mut shifted(), &seq, 1.0, &mut Chart).0);
    assert!(legacy != uncorrected, "the case's correction moves the picture");
}

/// Whether a GPU is there to take the plan route on.
fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

/// Since step 12 every temporal effect's clip takes the plan route: an
/// analysis-driven one (stabilisation, rolling shutter, deflicker) as a
/// plan node, its per-frame data reaching the executor; one needing the
/// source's neighbouring frames (temporal noise reduction, frame blending)
/// at the head before the grade, never among the effects a plan splices,
/// so the effects after it follow the grade. Each draws the legacy frame.
#[test]
fn temporal_effects_take_the_plan_route() {
    let _goldens = scope("effects_follow_grade.temporal_effects_take_the_plan_route");
    use klipp_effects::registry::Temporal;
    let temporal: Vec<_> = klipp_effects::registry::effect_types().filter(|t| t.desc.temporal != Temporal::None).collect();
    let tags = |kind: Temporal| temporal.iter().filter(|t| t.desc.temporal == kind).map(|t| t.tag).collect::<Vec<_>>();
    assert!(tags(Temporal::AnalysisDriven).contains(&"video_stabilization") && tags(Temporal::AnalysisDriven).contains(&"rolling_shutter_correction"));
    assert!(tags(Temporal::SourceNeighbours).contains(&"temporal_denoise"));
    let on_gpu = gpu();
    for ty in temporal {
        let effect = (ty.sample)();
        let neighbours = ty.desc.temporal == Temporal::SourceNeighbours;
        assert_eq!(klipp_effects::registry::needs_source_neighbours(&effect), neighbours, "{}", ty.tag);
        let followed = both(ty.tag, vec![effect, E::Gamma { gamma: 1.2 }]);
        if on_gpu {
            assert_eq!(followed, Some(true), "{}: the plan route", ty.tag);
        }
    }
    assert!(!klipp_effects::registry::needs_source_neighbours(&E::Gamma { gamma: 1.2 }));
}

/// A stabilised clip's motion is cleared for the clip after it: the plain
/// clip drawn next by the same renderer is the plain clip drawn alone.
#[test]
fn a_stabilised_clip_leaves_no_motion_for_the_next() {
    let seq = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let plain = graded(vec![E::Gamma { gamma: 1.2 }]);
    let new = || analysed_by(renderer(false), vec![analysis::stabilised_by(0.04, -0.02, 96)]);
    let mut r = new();
    r.render(&seq, tc(1.0), W, H, &mut Moving, RenderOptions::default());
    let after = r.render(&plain, tc(1.0), W, H, &mut Chart, RenderOptions::default());
    let alone = new().render(&plain, tc(1.0), W, H, &mut Chart, RenderOptions::default());
    assert!(after.iter().map(|v| v.to_bits()).eq(alone.iter().map(|v| v.to_bits())), "the plain clip isn't warped by the one before it");
}

/// The chart, its media analysed: each media clip keyed to the analyses
/// a renderer's store holds (`analysed_by`). The chart alone gives none.
struct Moving;

impl FrameSource for Moving {
    fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Chart.frame(c, secs, w, h)
    }

    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        analysis::key(clip, kind)
    }
}

/// `r` holding `analyses`, for the clips a [`Moving`] source keys.
fn analysed_by(mut r: Renderer, analyses: Vec<(klipp_analysis::assets::AssetKind, serde_json::Value)>) -> Renderer {
    r.set_asset_store(analysis::store(analyses));
    r
}

/// The analyses [`analysed`]'s clips read: a shaking camera, motion
/// shearing the rows, light that flickers every frame.
fn the_analyses() -> Vec<(klipp_analysis::assets::AssetKind, serde_json::Value)> {
    vec![analysis::stabilised_by(0.03, -0.02, 48), analysis::sheared(48), analysis::flickering(48)]
}

/// The three analysis-driven effects on a graded clip: each drawn from a
/// [`Moving`] source with its analysis ([`the_analyses`]), and the same
/// clip from the chart, without it.
fn analysed() -> Vec<(&'static str, Sequence, Sequence)> {
    let stab = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let rsc = graded(vec![E::RollingShutterCorrection { strength: 1.0, readout_ratio: 0.9 }, E::Gamma { gamma: 1.2 }]);
    let flicker = graded(vec![E::Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.0 }, E::Gamma { gamma: 1.2 }]);
    vec![("stabilise", stab.clone(), stab), ("rolling shutter", rsc.clone(), rsc), ("deflicker", flicker.clone(), flicker)]
}

fn renderer(legacy: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
    analysed_by(r, the_analyses())
}

// A stored frame's draw runs only while its test is recorded again from
// today's route (`KLIPP_RECORD_CURRENT_GOLDEN`), so it draws on that route.

/// `seq` at `secs` on `r` from `src`: the frame and whether it took the
/// plan route.
fn draw(r: &mut Renderer, seq: &Sequence, secs: f64, src: &mut dyn FrameSource) -> (Vec<f32>, bool) {
    let (out, found) = r.render_inspected(seq, tc(secs), W, H, src, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
    (out, found.stats.effects_followed_grade == 1)
}

fn bits(a: &[f32]) -> Vec<u32> {
    a.iter().map(|v| v.to_bits()).collect()
}

/// A graded clip with stabilisation, rolling-shutter correction or
/// deflicker draws on the plan route what the legacy route draws, frame
/// after frame and back again, and its analysis moves the picture.
#[test]
fn analysis_driven_effects_draw_the_same_on_the_plan_route() {
    let _goldens = scope("effects_follow_grade.analysis_driven_effects_draw_the_same_on_the_plan_route");
    let on_gpu = gpu();
    for (name, seq, bare) in analysed() {
        let (mut plan, mut legacy) = (renderer(false), None);
        let mut frames = Vec::new();
        // Forward, then a seek back to a frame the analysis treats differently.
        for secs in [0.5, 0.5 + 1.0 / 24.0, 1.0, 1.5, 0.25] {
            let (a, followed) = draw(&mut plan, &seq, secs, &mut Moving);
            let b = legacy_f32(|| draw(legacy.get_or_insert_with(|| renderer(false)), &seq, secs, &mut Moving).0);
            assert_eq!(followed, on_gpu, "{name} at {secs}: the plan route when there is a GPU");
            // The stored frames of the rolling shutter and the deflicker
            // were drawn from analysis the legacy route measured or the
            // clip held; their assets are their own, held to what they work
            // out to (`analysis_driven_effects_draw_what_their_analysis_computes`).
            // Their stored frames are still read, keeping the others' in order.
            assert!(name != "stabilise" || matches_f32(&a, &b), "{name} at {secs}: the plan route draws the legacy frame");
            let (fresh, _) = draw(&mut renderer(false), &seq, secs, &mut Moving);
            assert_eq!(bits(&a), bits(&fresh), "{name} at {secs}: nothing left from the frames before");
            let (uncorrected, _) = draw(&mut renderer(false), &bare, secs, &mut Chart);
            assert_ne!(bits(&a), bits(&uncorrected), "{name} at {secs}: the analysis moves the picture");
            frames.push(a);
        }
        if name != "rolling shutter" {
            // The source moves evenly, so its correction is the same at every frame.
            assert_ne!(bits(&frames[4]), bits(&frames[3]), "{name}: the seek back draws its own frame");
        }
    }
}

/// The deflicker and rolling-shutter clips draw what their analysis
/// computes: the graded frame with no effects, through the effects' own
/// kernels with the gain or rows worked out from the asset itself
/// (`analysis::flicker_gain`, `analysis::shear_rows`). Their stored
/// frames are held to this, never to whatever rendered.
#[test]
fn analysis_driven_effects_draw_what_their_analysis_computes() {
    for (name, seq, _) in analysed().into_iter().filter(|(n, _, _)| *n != "stabilise") {
        let clip = seq.tracks[0].clips[0].clone();
        let mut graded_only = seq.clone();
        strip_effects(&mut graded_only.tracks[0].clips[0]);
        for secs in [0.5, 1.0, 1.5] {
            let (with, _) = draw(&mut renderer(false), &seq, secs, &mut Moving);
            let (without, _) = draw(&mut renderer(false), &graded_only, secs, &mut Moving);
            let (mut effects, mut motion) = (clip.builtin_effects().to_vec(), klipp_effects::MotionData::default());
            for e in &mut effects {
                match e {
                    E::Deflicker { gain, window_secs, .. } => *gain = analysis::flicker_gain(48, *window_secs as f64, secs),
                    E::RollingShutterCorrection { strength, readout_ratio } => motion.rsc_rows = Some(analysis::shear_rows(H, *readout_ratio, *strength)),
                    _ => {}
                }
            }
            let want = klipp_effects::apply_effects_with_motion(&effects, without, W, H, klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::Rec709Linear), &motion);
            let worst = with.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(with.len() == want.len() && worst < 2e-3, "{name} at {secs}: {worst} from what its analysis computes");
        }
    }
}

/// Analysis that finishes mid-play: the frames before it draw the clip
/// uncorrected, and the first frame after it is corrected as a fresh
/// renderer corrects it, on the plan route, with no frame drawn with old
/// or missing data.
#[test]
fn analysis_arriving_mid_play_reaches_the_next_plan_frame() {
    let _goldens = scope("effects_follow_grade.analysis_arriving_mid_play_reaches_the_next_plan_frame");
    let on_gpu = gpu();
    for (name, seq, bare) in analysed() {
        let mut r = renderer(false);
        // The analysis arriving is the clip's media gaining its key: the
        // chart gives none, the moving source the store's.
        let before = draw(&mut r, &bare, 0.5, &mut Chart);
        let (alone, _) = draw(&mut renderer(false), &bare, 0.5, &mut Chart);
        assert_eq!(bits(&before.0), bits(&alone), "{name}: before the analysis, the clip uncorrected");
        let secs = 0.5 + 1.0 / 24.0;
        let (after, followed) = draw(&mut r, &seq, secs, &mut Moving);
        assert_eq!(followed, on_gpu, "{name}: the plan route");
        let want = legacy_f32(|| draw(&mut renderer(false), &seq, secs, &mut Moving).0);
        // Only the stabilisation's asset is the analysis its stored frame
        // was drawn from (see the plan-route test): the others are held to
        // a fresh renderer's frame.
        let want = if name != "stabilise" { draw(&mut renderer(false), &seq, secs, &mut Moving).0 } else { want };
        assert!(matches_f32(&after, &want), "{name}: the first frame after the analysis is corrected");
        // And the analysis going away (cleared, re-analysing) leaves none behind.
        let (gone, _) = draw(&mut r, &bare, 1.0, &mut Chart);
        let (plain, _) = draw(&mut renderer(false), &bare, 1.0, &mut Chart);
        assert_eq!(bits(&gone), bits(&plain), "{name}: no correction once the analysis is gone");
    }
}

/// A scene-referred source: 10-bit codes ramping down the frame with a
/// hue turning across it, so the codes span black to peak and no two
/// channels agree (the HLG branch weighs them by luma).
struct Scene(klipp_color::hdr::HdrTransfer);

impl FrameSource for Scene {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let codes: Vec<u32> = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let (v, t) = (y * 1023 / h, x * 1023 / w);
                    klipp_color::hdr::pack_rgb10(v, (v * 3 / 4 + t / 4).min(1023), (v / 2 + t / 2).min(1023))
                })
            })
            .collect();
        let pixels = SourcePixels::SceneReferred {
            codes: Arc::new(klipp_color::hdr::SceneCodes::Packed10(codes)),
            transfer: self.0,
            source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        Some(SourceFrame { pixels, width: w, height: h, pts: None })
    }
}

/// `seq` from a scene-referred source of `transfer`, under a delivery of
/// `peak` nits (`None` is SDR): the frame, how many clips took the plan
/// route, and its chain's effect breaks and submits.
fn render_scene(seq: &Sequence, legacy: bool, transfer: klipp_color::hdr::HdrTransfer, peak: Option<f32>) -> (Vec<f32>, u32, (u32, u32)) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
    let opts = RenderOptions { hdr_peak_nits: peak, ..Default::default() };
    let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Scene(transfer), opts, Inspect::default());
    assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
    let chain = r.executor().last_run_chain_stats();
    (out, found.stats.effects_followed_grade, (chain.effect_breaks, chain.submits))
}

/// The transfers a scene-referred source arrives in, one per display
/// branch (`HdrTransfer::display_light_kind`).
fn transfers() -> [(&'static str, klipp_color::hdr::HdrTransfer); 3] {
    use klipp_color::hdr::HdrTransfer;
    [("PQ", HdrTransfer::Pq), ("HLG", HdrTransfer::Hlg), ("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3))]
}

/// Step 11c: a scene-referred clip's display step runs in its plan, on
/// the GPU between the grade and the effects, where the legacy route runs
/// it on the CPU (`display_render`). Each transfer, tone-mapped for an
/// SDR picture and as display light for an HDR delivery, draws the legacy
/// picture within a hundredth of a level: the display step is the same
/// closed form on either side, in f32, so only rounding may differ.
#[test]
fn a_scene_referred_clip_runs_its_display_step_and_effects_as_a_plan() {
    let _goldens = scope("effects_follow_grade.a_scene_referred_clip_runs_its_display_step_and_effects_as_a_plan");
    const BAR: f32 = 1.0 / 25_500.0;
    let mut fx = colour_family();
    fx.push(E::Glow { radius: 6.0, intensity: 0.5, threshold: 0.0 });
    let seq = graded(fx);
    for (name, transfer) in transfers() {
        for peak in [None, Some(1000.0)] {
            let case = format!("{name}, peak {peak:?}");
            let (new, followed, (breaks, submits)) = render_scene(&seq, false, transfer, peak);
            let legacy = legacy_f32(|| {
                let (frame, legacy_followed, _) = render_scene(&seq, true, transfer, peak);
                assert_eq!(legacy_followed, 0, "{case}: the legacy route never follows");
                frame
            });
            if followed == 0 {
                assert!(!required(), "{case}: required, but the display step and effects did not run as a plan");
                eprintln!("UNVERIFIED: no GPU route");
                return;
            }
            assert_eq!((breaks, submits), (0, 1), "{case}: grade, display step and effects in one submit");
            // Display light runs to peak / 203; the bar is relative to it.
            let scale = peak.map_or(1.0, |p| p / 203.0);
            let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            eprintln!("{case}: worst {worst}");
            if legacy_goldens::exact() {
                assert!(worst <= BAR * scale, "{case}: the plan differs from the legacy route by {worst}");
            } else {
                // Recorded on another adapter: one display level of the
                // light relative to peak.
                let rel = |f: &[f32]| f.iter().map(|v| v / scale).collect::<Vec<_>>();
                let levels = legacy_goldens::worst_levels(&rel(&new), &rel(&legacy));
                assert!(levels <= 1.0, "{case}: the plan differs from the legacy route by {levels} levels");
            }
        }
    }
}

/// The display step is the run's own: spliced into the plan for a frame,
/// never into the clip's grade, so a grade saved after an HDR render
/// holds no `SceneToDisplay`.
#[test]
fn a_saved_grade_never_holds_the_display_step() {
    let seq = graded(colour_family());
    let (_, followed, _) = render_scene(&seq, false, klipp_color::hdr::HdrTransfer::Pq, Some(1000.0));
    assert!(followed == 1 || !required());
    let saved = serde_json::to_string(seq.tracks[0].clips[0].grade()).unwrap();
    assert!(saved.contains("Exposure"), "the grade was serialised");
    assert!(!saved.contains("SceneToDisplay"), "the display step stayed out of the saved grade");
}

/// The built-in invert plug-in keeps its host in a process-wide pointer,
/// so the tests that load it take turns.
static PLUGIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A renderer whose plug-in host holds the built-in invert plug-in.
fn renderer_with_invert(legacy: bool) -> Renderer {
    let mut host = klipp_plugin::OfxHost::new();
    let at = host.register_raw_plugin(klipp_plugin::test_plugin::create_test_plugin_raw());
    host.initialise_plugin(at).expect("the invert plug-in loads");
    let mut r = Renderer::new(host, klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
    analysed_by(r, the_analyses())
}

/// `seq`'s first clip with the built-in invert plug-in on.
fn with_invert(mut seq: Sequence) -> Sequence {
    let c = seq.tracks[0].clips[0].clone();
    seq.tracks[0].clips[0] = TestClip::of(c).plugin(klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() }).build();
    seq
}

/// Step 11c-2: a clip with a plug-in runs its grade as a plan, the
/// plug-in on the CPU after it, and its effects as a plan of their own
/// after the plug-in, and draws what the legacy route draws: the same
/// plug-in on the same picture, then the same shaders. A scene-referred
/// clip takes its display step in the grade's plan. The plug-in really
/// runs: the frame differs from the clip without it.
#[test]
fn a_clip_with_a_plugin_runs_its_effects_as_a_plan_after_it() {
    let _goldens = scope("effects_follow_grade.a_clip_with_a_plugin_runs_its_effects_as_a_plan_after_it");
    let _turn = PLUGIN.lock().unwrap_or_else(|e| e.into_inner());
    let mut fx = colour_family();
    fx.push(E::GaussianBlur { radius: 2.0 });
    let seq = with_invert(graded(fx.clone()));
    let mut cases: Vec<(&str, Option<klipp_color::hdr::HdrTransfer>)> = vec![("SDR", None)];
    cases.extend(transfers().iter().map(|(n, t)| (*n, Some(*t))));
    for (name, transfer) in cases {
        let run = |legacy: bool, seq: &Sequence| {
            let mut r = renderer_with_invert(legacy);
            let opts = RenderOptions::default();
            let (out, found) = match transfer {
                Some(t) => r.render_inspected(seq, tc(1.0), W, H, &mut Scene(t), opts, Inspect::default()),
                None => r.render_inspected(seq, tc(1.0), W, H, &mut Chart, opts, Inspect::default()),
            };
            assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
            (out, found.stats.effects_after_plugins, found.stats.effects_followed_grade)
        };
        let (new, after, followed) = run(false, &seq);
        let legacy = legacy_f32(|| {
            let (frame, legacy_after, _) = run(true, &seq);
            assert_eq!(legacy_after, 0, "{name}: the legacy route runs no plan after the plug-in");
            frame
        });
        assert_eq!(followed, 0, "{name}: a plug-in's clip never splices its effects into the grade");
        if after == 0 {
            assert!(!required(), "{name}: required, but the effects after the plug-in did not run as a plan");
            eprintln!("UNVERIFIED: no GPU route");
            return;
        }
        assert_eq!(after, 1, "{name}");
        let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("{name}: plan after the plug-in vs legacy: worst {worst}");
        assert!(matches_f32(&new, &legacy), "{name}: differs from the legacy route by {worst}");
        let (plain, _, _) = run(false, &graded(fx.clone()));
        assert!(plain != new, "{name}: the plug-in changes the picture");
    }
}

/// The CPU kernels take a plug-in's clip whose effects can't be applied
/// after it (a lost device's refusal), counted: never drawn without them.
#[test]
fn effects_after_a_plugin_that_cannot_be_applied_take_the_cpu_kernels() {
    let _goldens = scope("effects_follow_grade.effects_after_a_plugin_that_cannot_be_applied_take_the_legacy_route");
    let _turn = PLUGIN.lock().unwrap_or_else(|e| e.into_inner());
    let seq = with_invert(graded(colour_family()));
    let mut r = renderer_with_invert(false);
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(true);
    }
    let (refused, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_after_plugins, 0, "nothing ran as a plan");
    assert_eq!(found.stats.effects_on_cpu, u32::from(gpu()), "the CPU kernels took the effects, counted");
    assert!(found.stats.effects_on_cpu_kernels >= 1, "and counted as drawn on the CPU (#1274)");
    let legacy = legacy_f32(|| renderer_with_invert(false).render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default()).0);
    assert!(within_cpu_gpu_bar(&refused, &legacy), "the legacy route's picture, effects in it, within the CPU/GPU bar");
}

/// Step 11c-3: a moving clip under an open shutter runs its effects as
/// plans of their own around the sampling (the colour effects before its
/// transform once, the transform at each of eight samples, a vignette and
/// a blur after it once on the average). The shutter really smears: the
/// frame differs from the same clip with it shut.
///
/// It is held to its stored frame bit for bit (within a level on another
/// adapter). That frame was the legacy route's until the samples came to
/// be averaged premultiplied (#2017), which changes the smear at the
/// picture's moving edges on purpose; it is recorded again from today's
/// route with that change.
#[test]
fn a_moving_clip_under_an_open_shutter_runs_its_effects_as_plans() {
    let _goldens = scope("effects_follow_grade.a_moving_clip_under_an_open_shutter_runs_its_effects_as_plans");
    let transform = E::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    };
    let mut fx = colour_family();
    fx.push(transform);
    fx.push(E::Vignette { amount: 0.5, size: 0.6, softness: 0.4, roundness: 1.0 });
    fx.push(E::GaussianBlur { radius: 1.5 });
    let shot = |deg: f32| {
        let mut seq = graded(fx.clone());
        // The clip's own transform moves; its Transform effect in the list
        // is where the resolved one goes, the vignette and blur after it.
        let clip = &mut seq.tracks[0].clips[0];
        *clip = TestClip::of(clip.clone()).lane("pos_x", 0.0, -20.0).lane("pos_x", 2.0, 20.0).build();
        seq.camera.shutter_deg = deg;
        seq.camera.shutter_samples = 8;
        seq
    };
    let run = |seq: &Sequence, legacy: bool| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        assert!(!legacy, "the legacy route is gone (M2 step 11d): its frames are stored, never drawn");
        let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
        assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
        (out, found.stats.effects_shuttered, found.stats.effects_followed_grade)
    };
    let open = shot(180.0);
    let (new, shuttered, followed) = run(&open, false);
    assert_eq!(followed, 0, "a shuttered clip splices nothing into its grade");
    if shuttered == 0 {
        assert!(!required(), "required, but the shuttered effects did not run as plans");
        eprintln!("UNVERIFIED: no GPU route");
        return;
    }
    assert_eq!(shuttered, 1);
    let stored = legacy_f32(|| new.clone());
    let worst = new.iter().zip(&stored).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(matches_f32(&new, &stored), "differs from its stored frame by {worst}");
    let (shut, _, shut_followed) = run(&shot(0.0), false);
    assert_eq!(shut_followed, 1, "with the shutter shut the effects follow the grade as before");
    assert!(shut != new, "the shutter smears the moving clip");
}

/// A stabilised, rolling-shutter-corrected or deflickered clip with a
/// plug-in: its effects run as a plan after the plug-in (11c-2) with the
/// frame's analysis (step 12), and draw what the legacy route draws, the
/// correction showing.
#[test]
fn analysis_driven_effects_after_a_plugin_keep_their_correction() {
    let _goldens = scope("effects_follow_grade.analysis_driven_effects_after_a_plugin_keep_their_correction");
    let _turn = PLUGIN.lock().unwrap_or_else(|e| e.into_inner());
    for (name, seq, bare) in analysed() {
        let (seq, bare) = (with_invert(seq), with_invert(bare));
        let run = |seq: &Sequence, src: &mut dyn FrameSource| {
            let mut r = renderer_with_invert(false);
            let (out, found) = r.render_inspected(seq, tc(1.0), W, H, src, RenderOptions::default(), Inspect::default());
            assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
            (out, found.stats.effects_after_plugins)
        };
        let (new, after) = run(&seq, &mut Moving);
        // Stored from today's route when recorded again (#2823).
        let legacy = legacy_f32(|| new.clone());
        if after == 0 {
            assert!(!required(), "{name}: required, but the effects after the plug-in did not run as a plan");
            eprintln!("UNVERIFIED: no GPU route");
            return;
        }
        assert_eq!(after, 1, "{name}");
        // Only the stabilisation's asset is the analysis its stored frame
        // was drawn from (see the plan-route test).
        assert!(name != "stabilise" || matches_f32(&new, &legacy), "{name}: the plan after the plug-in draws the legacy frame");
        let (uncorrected, _) = run(&bare, &mut Chart);
        assert_ne!(bits(&new), bits(&uncorrected), "{name}: the analysis moves the picture");
    }
}

/// The viewer names its primary clip in every render (the engine's
/// `inspect_request`), with nothing read inside its grade: the clip's
/// effects still follow its grade on the GPU, and the CPU kernels draw
/// none of them (#1274: they took the CPU kernels on every frame the GPU
/// stack declined).
#[test]
fn the_viewers_primary_clip_keeps_its_effects_on_the_grades_plan() {
    let seq = graded(colour_family());
    let id = seq.tracks[0].clips[0].id;
    let on_gpu = gpu();
    let (normal, _) = render(&seq, false);
    let mut r = renderer(false);
    let viewer = Inspect { clip: Some(id), ..Default::default() };
    assert!(!viewer.reads_the_grade());
    let (shown, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), viewer);
    assert_eq!(found.stats.effects_followed_grade, u32::from(on_gpu), "the primary clip's effects follow its grade");
    assert_eq!(found.stats.effects_on_cpu_kernels, u32::from(!on_gpu), "the CPU kernels draw them only without a GPU");
    assert_eq!(found.stats.effects_on_cpu, 0);
    assert!(within_cpu_gpu_bar(&shown, &normal), "the viewer's picture is the rendered one");
}

/// Inspecting a clip's grade (the node editor's thumbnails read its
/// intermediate pictures) keeps its effects out of the grade's run, so the
/// thumbnails are the grade's; the effects run as a plan of their own
/// after it, still on the GPU (#1274), and the picture is the one the
/// normal render draws. What it costs is printed.
#[test]
fn an_inspected_clip_shows_the_picture_the_render_draws() {
    let seq = graded(colour_family());
    let clip = &seq.tracks[0].clips[0];
    let id = clip.id;
    let nodes: std::collections::HashSet<_> = clip.grade().nodes.iter().map(|n| klipp_compose::inspect::grade_node(id, n.id)).collect();
    let on_gpu = gpu();
    let (normal, followed) = render(&seq, false);
    assert_eq!(followed, u32::from(on_gpu), "the normal render takes the plan route when there is a GPU");
    let mut r = renderer(false);
    let inspect = || Inspect { clip: Some(id), thumbs: Some(nodes.clone()), ..Default::default() };
    assert!(inspect().reads_the_grade());
    let (inspected, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), inspect());
    assert_eq!(found.stats.effects_followed_grade, 0, "an inspected grade runs alone");
    assert_eq!(found.stats.effects_inspected_apart, u32::from(on_gpu), "its effects run as a plan of their own");
    assert_eq!(found.stats.effects_on_cpu_kernels, u32::from(!on_gpu), "on the GPU when there is one");
    assert_eq!(found.stats.effects_on_cpu, 0, "inspecting isn't a fallback");
    assert!(!found.thumbs.is_empty(), "the grade's thumbnails come back");
    assert!(within_cpu_gpu_bar(&inspected, &normal), "the inspected picture is the rendered one");
    let mut no_fx = seq.clone();
    no_fx.tracks[0].clips[0].set_effect_list(vec![], vec![]);
    assert!(inspected != render(&no_fx, false).0, "the effects are in the inspected picture");
    // The cost, for the PR: medians of the same frame, inspected and not.
    let median = |inspected: bool, r: &mut Renderer| {
        let mut ms: Vec<f64> = (0..9)
            .map(|_| {
                let t = std::time::Instant::now();
                let how = if inspected { inspect() } else { Inspect::default() };
                std::hint::black_box(r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), how));
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        ms[4]
    };
    let mut plain = renderer(false);
    eprintln!("at {W}x{H}: rendered {:.2} ms, inspected {:.2} ms", median(false, &mut plain), median(true, &mut r));
}

/// A scene-referred clip whose grade the tools read inside (#1274): its
/// display step still runs, and its effects as a plan of their own after
/// the grade, so the picture is the one the plain render draws, for the
/// viewer's primary-only inspection and for the node canvas's thumbnails.
#[test]
fn an_inspected_scene_referred_clip_keeps_its_display_step() {
    let mut fx = colour_family();
    fx.push(E::Glow { radius: 6.0, intensity: 0.5, threshold: 0.0 });
    let seq = graded(fx);
    let clip = &seq.tracks[0].clips[0];
    let nodes: std::collections::HashSet<_> = clip.grade().nodes.iter().map(|n| klipp_compose::inspect::grade_node(clip.id, n.id)).collect();
    let tapped_node = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Exposure).expect("the graded clip's Exposure").id;
    let on_gpu = gpu();
    for (name, transfer) in transfers() {
        let draw = |inspect: Inspect| {
            let mut r = renderer(false);
            r.render_inspected(&seq, tc(1.0), W, H, &mut Scene(transfer), RenderOptions::default(), inspect)
        };
        let (plain, _) = draw(Inspect::default());
        let (viewer, found) = draw(Inspect { clip: Some(clip.id), ..Default::default() });
        assert_eq!(found.stats.effects_followed_grade, u32::from(on_gpu), "{name}: the primary clip's effects follow its grade");
        assert!(within_cpu_gpu_bar(&viewer, &plain), "{name}: the viewer draws the plain picture");
        // What the tools read is the grade's own output, as the CPU
        // renderer reads it: thumbnails and the scope's tap on a grade node,
        // before the display step and the effects.
        let tools = Inspect { clip: Some(clip.id), thumbs: Some(nodes.clone()), scope_tap: Some(klipp_compose::inspect::grade_node(clip.id, tapped_node)), ..Default::default() };
        let (tapped, found) = draw(tools.clone());
        let mut cpu_r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        cpu_r.set_gpu_enabled(false);
        let (want, want_found) = cpu_r.render_inspected(&seq, tc(1.0), W, H, &mut Scene(transfer), RenderOptions::default(), tools);
        assert_eq!(found.stats.effects_inspected_apart, u32::from(on_gpu), "{name}: the effects run apart from the inspected grade");
        assert_eq!(found.stats.effects_on_cpu_kernels, u32::from(!on_gpu), "{name}: on the GPU when there is one");
        assert!(within_cpu_gpu_bar(&tapped, &plain), "{name}: the inspected picture keeps its display step");
        assert!(within_cpu_gpu_bar(&tapped, &want), "{name}: and is the CPU renderer's");
        assert_eq!(found.thumbs.keys().collect::<std::collections::HashSet<_>>(), want_found.thumbs.keys().collect(), "{name}: the same thumbnails");
        for (id, px) in &found.thumbs {
            let worst = px.iter().zip(&want_found.thumbs[id]).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(worst <= 1, "{name}: node {id:?}'s thumbnail is {worst} levels from the CPU renderer's");
        }
        let (tap, want_tap) = (found.scope_tap.expect("the tap"), want_found.scope_tap.expect("the CPU tap"));
        assert!(within_cpu_gpu_bar(&tap, &want_tap), "{name}: the scope reads the grade node, not the displayed picture");
    }
}

/// A scene-referred clip with a node soloed (#1274): the run returns the
/// soloed node's picture before any step spliced after the grade, so the
/// display step must not ride in the run. It runs on what the solo
/// returns, as the CPU renderer runs it, and the effects follow.
#[test]
fn a_soloed_scene_referred_clip_keeps_its_display_step() {
    let mut fx = colour_family();
    fx.push(E::Glow { radius: 6.0, intensity: 0.5, threshold: 0.0 });
    let seq = graded(fx);
    let clip = &seq.tracks[0].clips[0];
    let exposure = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Exposure).expect("the graded clip's Exposure").id;
    let solo = Inspect { clip: Some(clip.id), solo: Some(klipp_compose::inspect::grade_node(clip.id, exposure)), ..Default::default() };
    assert!(solo.reads_the_grade());
    let on_gpu = gpu();
    for (name, transfer) in transfers() {
        let mut gpu_r = renderer(false);
        let (shown, found) = gpu_r.render_inspected(&seq, tc(1.0), W, H, &mut Scene(transfer), RenderOptions::default(), solo.clone());
        let mut cpu_r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        cpu_r.set_gpu_enabled(false);
        let (want, _) = cpu_r.render_inspected(&seq, tc(1.0), W, H, &mut Scene(transfer), RenderOptions::default(), solo.clone());
        assert!(within_cpu_gpu_bar(&shown, &want), "{name}: the soloed picture is displayed as the CPU renderer displays it");
        assert_eq!(found.stats.effects_inspected_apart, u32::from(on_gpu), "{name}: the effects run apart from the soloed grade");
    }
}

/// `c` with its built-in effects taken off, through its picture.
fn strip_effects(c: &mut Clip) {
    let n = klipp_timeline::test_clip::effects(c).len();
    let edits: Vec<klipp_timeline::EffectEdit> = (0..n).rev().map(klipp_timeline::EffectEdit::Remove).collect();
    c.apply_effect_edits(&edits).expect("plain effects").expect("taken off");
}
