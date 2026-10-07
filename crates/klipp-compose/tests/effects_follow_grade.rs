//! A clip's effects take its grade's result where it sits on the GPU,
//! with no download and upload between them, and draw exactly what the
//! legacy route draws (#1089, phase 2b M2 step 10).
//!
//! Each case renders one graded clip with effects twice: on the new route
//! and on the legacy one (`set_legacy_effects`), comparing the linear
//! floats bit for bit. The same shaders run on the same f32 frame either
//! way, so anything but equal is a fault. The render counter says which
//! route a frame took, so a case can't pass by both falling back.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::RationalTime;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
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
    let n = effects.len();
    clip.set_effect_list(effects, vec![true; n]);
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
    r.set_legacy_effects(legacy);
    // Both pipelines: the grade's (whose chain the effects follow on) and
    // the renderer's own (the legacy route's).
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
    (out, found.stats.effects_followed_grade)
}

thread_local! {
    /// The last render's declines (`cpu_layers`), effect breaks and
    /// submits, for the cases that check them.
    static LAST: std::cell::Cell<(u32, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
}

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
    let (legacy, legacy_followed) = render(&seq, true);
    assert_eq!(declines, last().0, "{name}: no new declines");
    if followed == 1 {
        let recorded = seq.tracks[0].clips[0].builtin_effects().iter().all(|e| klipp_effects::gpu::slots_needed(e).is_some());
        if recorded {
            assert_eq!((breaks, submits), (0, 1), "{name}: grade and effects in one submit");
        }
    }
    assert_eq!(legacy_followed, 0, "{name}: the legacy route never follows");
    if new.iter().map(|v| v.to_bits()).ne(legacy.iter().map(|v| v.to_bits())) {
        let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        panic!("{name}: the new route differs from the legacy one (worst {worst})");
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
    let gpu = both("colour family", colour_family()).unwrap();
    if !gpu {
        assert!(!required(), "required, but the effects did not follow the grade on the GPU");
        eprintln!("UNVERIFIED: no GPU route");
    }
}

#[test]
fn an_effect_not_yet_recorded_still_follows_and_draws_the_same() {
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
    let mut seq = graded(colour_family());
    seq.tracks[0].clips[0].set_grade(NodeGraph::default());
    assert!(seq.tracks[0].clips[0].grade_is_identity());
    let gpu = both_seq("no grade", seq).unwrap();
    assert!(gpu || !required(), "required, but the effects did not run as a plan");
}

#[test]
fn an_effect_that_could_not_be_recorded_sends_the_frame_the_legacy_way() {
    // The grade's picture is gone with the GPU run, so the frame is graded
    // again and takes the legacy route: never drawn without the effect.
    let seq = graded(colour_family());
    let (new, followed) = render_with(&seq, false, true);
    let (legacy, _) = render_with(&seq, true, true);
    assert_eq!(followed, 0, "nothing followed the grade");
    assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "the legacy route's picture");
    let (all, _) = render(&seq, true);
    assert!(new.iter().zip(&all).all(|(a, b)| (a - b).abs() < 1e-4), "with every effect in it");
}

#[test]
fn a_grade_run_that_fails_never_drops_the_effects() {
    // A grade whose run fails (here a cycle, the executor's error) gives
    // back its input with nothing applied. The spliced effects were in
    // that run, so the frame takes the legacy route and gets them.
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
    let (legacy, _) = render(&seq, true);
    assert_eq!(followed, 0, "the failed run is not taken as done");
    assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "the legacy route's picture, effects in it");
    let mut no_fx = seq.clone();
    no_fx.tracks[0].clips[0].set_effect_list(vec![], vec![]);
    let (bare, _) = render(&no_fx, true);
    assert!(new != bare, "the effects are in the picture");
}

#[test]
fn animated_effects_and_a_fixed_placement_from_the_picture_draw_the_legacy_frames() {
    // Step 10c-2: the plan route's effects come from the clip's lifted
    // picture. A keyed parameter, a modifier stack, and a Transform effect
    // placed after a shadow while the clip moves, at times around the keys.
    let shadow = E::DropShadow { offset_x: 0.03, offset_y: 0.02, blur: 0.01, color: [10, 20, 40], opacity: 0.8 };
    let tf = E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.7, scale_y: 0.7, rotation: 4.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 8.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let mut seq = graded(vec![E::Gamma { gamma: 1.0 }, E::Vibrance { amount: 0.2 }, shadow, tf]);
    let clip = &mut seq.tracks[0].clips[0];
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
    clip.edit_effect_keyframes(|k| {
        k.insert("0:gamma".into(), vec![key(0.2, 0.7), key(1.0, 1.5), key(1.8, 0.9)]);
    });
    clip.edit_param_modifiers(|m| {
        let osc = klipp_core::Modifier::Oscillate { amplitude: 0.3, frequency: 1.3, phase: 0.1, waveform: Default::default(), blend: Default::default(), enabled: true };
        m.insert(klipp_core::ParamId::Effect { effect_index: 1, param_name: "amount".into() }, klipp_core::ModifierStack { modifiers: vec![osc] });
    });
    clip.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, -10.0), key(2.0, 10.0)]);
    let mut seen = Vec::new();
    let counted = klipp_compose::PLAN_EFFECT_CLIPS.load(std::sync::atomic::Ordering::Relaxed);
    for t in [0.1, 0.6, 1.0, 1.4, 1.9] {
        let (new, followed) = render_at(&seq, false, false, t);
        let (legacy, _) = render_at(&seq, true, false, t);
        assert!(followed == 1 || !required(), "at {t}: the plan route was taken");
        assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "at {t}: the legacy route's frame");
        seen.push(new);
    }
    assert!(seen.windows(2).all(|w| w[0] != w[1]), "the frames animate");
    // The process-wide count the control socket's `perf` reports (other
    // tests run alongside, so it has grown by at least these five).
    let now = klipp_compose::PLAN_EFFECT_CLIPS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(now >= counted + 5 || !required(), "{counted} -> {now}");
}

/// Stabilisation reads motion data the renderer makes per frame and hands
/// the plan's executor too (#1209, step 12): a graded clip with it takes
/// the plan route and draws its correction as the legacy route does.
#[test]
fn a_graded_clip_with_stabilisation_keeps_its_correction() {
    use klipp_effects::homography::Homography;
    use klipp_effects::stabilization::SmoothedTrajectory;
    let mut seq = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let shift = Homography { h: [[1.0, 0.0, 3.5], [0.0, 1.0, -1.5], [0.0, 0.0, 1.0]] };
    seq.tracks[0].clips[0].smoothed_trajectory = Some(SmoothedTrajectory {
        smoothed: Vec::new(),
        corrections: vec![shift; 96],
        smoothing_window_secs: 1.0,
        raw_jitter_px: 1.0,
        smoothed_jitter_px: 0.1,
    });
    let (new, followed) = render(&seq, false);
    let (legacy, _) = render(&seq, true);
    assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "the correction is drawn on both routes");
    if gpu() {
        assert_eq!(followed, 1, "a stabilised clip takes the plan route (step 12)");
    }
    let mut still = seq.clone();
    still.tracks[0].clips[0].smoothed_trajectory = None;
    let (uncorrected, _) = render(&still, true);
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
    use klipp_effects::homography::Homography;
    use klipp_effects::stabilization::SmoothedTrajectory;
    let mut seq = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let shift = Homography { h: [[1.0, 0.0, 3.5], [0.0, 1.0, -1.5], [0.0, 0.0, 1.0]] };
    seq.tracks[0].clips[0].smoothed_trajectory = Some(SmoothedTrajectory {
        smoothed: Vec::new(),
        corrections: vec![shift; 96],
        smoothing_window_secs: 1.0,
        raw_jitter_px: 1.0,
        smoothed_jitter_px: 0.1,
    });
    let plain = graded(vec![E::Gamma { gamma: 1.2 }]);
    let draw = |r: &mut Renderer, s: &Sequence| r.render(s, tc(1.0), W, H, &mut Chart, RenderOptions::default());
    let new = || {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        r
    };
    let mut r = new();
    draw(&mut r, &seq);
    let after = draw(&mut r, &plain);
    let alone = draw(&mut new(), &plain);
    assert!(after.iter().map(|v| v.to_bits()).eq(alone.iter().map(|v| v.to_bits())), "the plain clip isn't warped by the one before it");
}

/// The chart, with motion frames for rolling-shutter correction to
/// measure: a blocky texture six pixels further right each frame.
struct Moving;

impl FrameSource for Moving {
    fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Chart.frame(c, secs, w, h)
    }

    fn motion_frame(&mut self, _c: &Clip, secs: f64, max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let (w, h) = (256u32, 144u32);
        if w > max_width {
            return None;
        }
        let n = (secs * 24.0).floor() as i64;
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let (bx, by) = ((x - 6 * n).div_euclid(8), y.div_euclid(8));
                let g = ((bx * 7919 + by * 104_729).rem_euclid(251)) as u8;
                v.extend_from_slice(&[g, g, g, 255]);
            }
        }
        Some((v, w, h))
    }
}

/// The three analysis-driven effects on a graded clip, each with its
/// analysis (a drifting trajectory, the moving source's motion, gains
/// that change every frame), and the same clip without it.
fn analysed() -> Vec<(&'static str, Sequence, Sequence)> {
    use klipp_effects::homography::Homography;
    use klipp_effects::stabilization::SmoothedTrajectory;
    let mut stab = graded(vec![E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }, E::Gamma { gamma: 1.2 }]);
    let bare_stab = stab.clone();
    stab.tracks[0].clips[0].smoothed_trajectory = Some(SmoothedTrajectory {
        smoothed: Vec::new(),
        corrections: (0..48).map(|i| Homography { h: [[1.0, 0.0, 0.2 * i as f64 - 4.0], [0.0, 1.0, 2.5 - 0.1 * i as f64], [0.0, 0.0, 1.0]] }).collect(),
        smoothing_window_secs: 1.0,
        raw_jitter_px: 1.0,
        smoothed_jitter_px: 0.1,
    });
    let rsc = graded(vec![E::RollingShutterCorrection { strength: 1.0, readout_ratio: 0.9 }, E::Gamma { gamma: 1.2 }]);
    let mut flicker = graded(vec![E::Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.0 }, E::Gamma { gamma: 1.2 }]);
    let bare_flicker = flicker.clone();
    flicker.tracks[0].clips[0].deflicker_gains = Some(klipp_effects::deflicker::DeflickerGains {
        gains: (0..48).map(|i| if i % 2 == 0 { 0.7 } else { 1.3 } + 0.004 * i as f32).collect(),
        window_secs: 1.0,
        fps: 24.0,
    });
    // Rolling-shutter correction's analysis is the source's motion: on
    // the chart alone there is none.
    vec![("stabilise", stab, bare_stab), ("rolling shutter", rsc.clone(), rsc), ("deflicker", flicker, bare_flicker)]
}

fn renderer(legacy: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_legacy_effects(legacy);
    r
}

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
    let on_gpu = gpu();
    for (name, seq, bare) in analysed() {
        let (mut plan, mut legacy) = (renderer(false), renderer(true));
        let mut frames = Vec::new();
        // Forward, then a seek back to a frame the analysis treats differently.
        for secs in [0.5, 0.5 + 1.0 / 24.0, 1.0, 1.5, 0.25] {
            let (a, followed) = draw(&mut plan, &seq, secs, &mut Moving);
            let (b, _) = draw(&mut legacy, &seq, secs, &mut Moving);
            assert_eq!(followed, on_gpu, "{name} at {secs}: the plan route when there is a GPU");
            assert_eq!(bits(&a), bits(&b), "{name} at {secs}: the plan route draws the legacy frame");
            let (fresh, _) = draw(&mut renderer(false), &seq, secs, &mut Moving);
            assert_eq!(bits(&a), bits(&fresh), "{name} at {secs}: nothing left from the frames before");
            let (uncorrected, _) = if name == "rolling shutter" { draw(&mut renderer(false), &bare, secs, &mut Chart) } else { draw(&mut renderer(false), &bare, secs, &mut Moving) };
            assert_ne!(bits(&a), bits(&uncorrected), "{name} at {secs}: the analysis moves the picture");
            frames.push(a);
        }
        if name != "rolling shutter" {
            // The source moves evenly, so its correction is the same at every frame.
            assert_ne!(bits(&frames[4]), bits(&frames[3]), "{name}: the seek back draws its own frame");
        }
    }
}

/// Analysis that finishes mid-play: the frames before it draw the clip
/// uncorrected, and the first frame after it is corrected as a fresh
/// renderer corrects it, on the plan route, with no frame drawn with old
/// or missing data.
#[test]
fn analysis_arriving_mid_play_reaches_the_next_plan_frame() {
    let on_gpu = gpu();
    for (name, seq, bare) in analysed() {
        let mut r = renderer(false);
        // Rolling shutter's analysis arriving is the source's motion
        // becoming readable.
        let before = if name == "rolling shutter" { draw(&mut r, &bare, 0.5, &mut Chart) } else { draw(&mut r, &bare, 0.5, &mut Moving) };
        let (alone, _) = if name == "rolling shutter" { draw(&mut renderer(false), &bare, 0.5, &mut Chart) } else { draw(&mut renderer(false), &bare, 0.5, &mut Moving) };
        assert_eq!(bits(&before.0), bits(&alone), "{name}: before the analysis, the clip uncorrected");
        let secs = 0.5 + 1.0 / 24.0;
        let (after, followed) = draw(&mut r, &seq, secs, &mut Moving);
        assert_eq!(followed, on_gpu, "{name}: the plan route");
        let (want, _) = draw(&mut renderer(true), &seq, secs, &mut Moving);
        assert_eq!(bits(&after), bits(&want), "{name}: the first frame after the analysis is corrected");
        // And the analysis going away (cleared, re-analysing) leaves none behind.
        let (gone, _) = if name == "rolling shutter" { draw(&mut r, &bare, 1.0, &mut Chart) } else { draw(&mut r, &bare, 1.0, &mut Moving) };
        let (plain, _) = if name == "rolling shutter" { draw(&mut renderer(false), &bare, 1.0, &mut Chart) } else { draw(&mut renderer(false), &bare, 1.0, &mut Moving) };
        assert_eq!(bits(&gone), bits(&plain), "{name}: no correction once the analysis is gone");
    }
}

/// A scene-referred source: 10-bit codes ramping down the frame with a
/// hue turning across it, so the codes span black to peak and no two
/// channels agree (the HLG branch weighs them by luma).
struct Scene(klipp_color::hdr::HdrTransfer);

impl FrameSource for Scene {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let codes = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let (v, t) = (y * 1023 / h, x * 1023 / w);
                    klipp_color::hdr::pack_rgb10(v, (v * 3 / 4 + t / 4).min(1023), (v / 2 + t / 2).min(1023))
                })
            })
            .collect();
        let pixels = SourcePixels::SceneReferred {
            codes: Arc::new(codes),
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
    r.set_legacy_effects(legacy);
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
    const BAR: f32 = 1.0 / 25_500.0;
    let mut fx = colour_family();
    fx.push(E::Glow { radius: 6.0, intensity: 0.5 });
    let seq = graded(fx);
    for (name, transfer) in transfers() {
        for peak in [None, Some(1000.0)] {
            let case = format!("{name}, peak {peak:?}");
            let (new, followed, (breaks, submits)) = render_scene(&seq, false, transfer, peak);
            let (legacy, legacy_followed, _) = render_scene(&seq, true, transfer, peak);
            assert_eq!(legacy_followed, 0, "{case}: the legacy route never follows");
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
            assert!(worst <= BAR * scale, "{case}: the plan differs from the legacy route by {worst}");
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
    r.set_legacy_effects(legacy);
    r
}

/// `seq`'s first clip with the built-in invert plug-in on.
fn with_invert(mut seq: Sequence) -> Sequence {
    seq.tracks[0].clips[0].set_ofx_effects(vec![klipp_plugin::AppliedEffect {
        plugin_name: "com.klipp.test.Invert".into(),
        enabled: true,
        ..Default::default()
    }]);
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
        let (legacy, legacy_after, _) = run(true, &seq);
        assert_eq!(legacy_after, 0, "{name}: the legacy route runs no plan after the plug-in");
        assert_eq!(followed, 0, "{name}: a plug-in's clip never splices its effects into the grade");
        if after == 0 {
            assert!(!required(), "{name}: required, but the effects after the plug-in did not run as a plan");
            eprintln!("UNVERIFIED: no GPU route");
            return;
        }
        assert_eq!(after, 1, "{name}");
        let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("{name}: plan after the plug-in vs legacy: worst {worst}");
        assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "{name}: differs from the legacy route by {worst}");
        let (plain, _, _) = run(false, &graded(fx.clone()));
        assert!(plain != new, "{name}: the plug-in changes the picture");
    }
}

/// The legacy route takes a plug-in's clip whose effects can't be applied
/// after it (a lost device's refusal): never drawn without them.
#[test]
fn effects_after_a_plugin_that_cannot_be_applied_take_the_legacy_route() {
    let _turn = PLUGIN.lock().unwrap_or_else(|e| e.into_inner());
    let seq = with_invert(graded(colour_family()));
    let mut r = renderer_with_invert(false);
    if let Some(g) = r.executor().gpu_pipeline_or_init() {
        g.refuse_recording(true);
    }
    let (refused, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert_eq!(found.stats.effects_after_plugins, 0, "nothing ran as a plan");
    let mut l = renderer_with_invert(true);
    let (legacy, _) = l.render_inspected(&seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
    assert!(refused.iter().zip(&legacy).all(|(a, b)| (a - b).abs() < 1e-4), "the legacy route's picture, effects in it");
}

/// Step 11c-3: a moving clip under an open shutter runs its effects as
/// plans of their own around the sampling (the colour effects before its
/// transform once, the transform at each of eight samples, a vignette and
/// a blur after it once on the average) and draws the legacy frame bit
/// for bit, since the same shaders run on the same floats and the samples
/// are averaged on the CPU either way. The shutter really smears: the
/// frame differs from the same clip with it shut.
#[test]
fn a_moving_clip_under_an_open_shutter_runs_its_effects_as_plans() {
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
        clip.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 0.0, -20.0));
        clip.edit_transform_pos_x_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, 2.0, 20.0));
        seq.camera.shutter_deg = deg;
        seq.camera.shutter_samples = 8;
        seq
    };
    let run = |seq: &Sequence, legacy: bool| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        r.set_legacy_effects(legacy);
        let (out, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default());
        assert_eq!(found.stats.effects_fallback_lists, 0, "every plan list came from the picture");
        (out, found.stats.effects_shuttered, found.stats.effects_followed_grade)
    };
    let open = shot(180.0);
    let (new, shuttered, followed) = run(&open, false);
    let (legacy, legacy_shuttered, _) = run(&open, true);
    assert_eq!((legacy_shuttered, followed), (0, 0), "the legacy route runs no plan; a shuttered clip splices nothing into its grade");
    if shuttered == 0 {
        assert!(!required(), "required, but the shuttered effects did not run as plans");
        eprintln!("UNVERIFIED: no GPU route");
        return;
    }
    assert_eq!(shuttered, 1);
    let worst = new.iter().zip(&legacy).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("shutter: plans vs legacy: worst {worst}");
    assert!(new.iter().map(|v| v.to_bits()).eq(legacy.iter().map(|v| v.to_bits())), "differs from the legacy route by {worst}");
    let (shut, _, shut_followed) = run(&shot(0.0), false);
    assert_eq!(shut_followed, 1, "with the shutter shut the effects follow the grade as before");
    assert!(shut != new, "the shutter smears the moving clip");
}
