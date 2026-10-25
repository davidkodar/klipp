//! A fixed corpus of small projects, each drawn by every render path and
//! compared (#1001).
//!
//! The same project renders through four paths:
//!
//! * **cpu**: `Renderer::render` with the GPU off;
//! * **cpu+gpu**: `Renderer::render` with the GPU on, where grades and
//!   effects run on the GPU and the composite on the CPU;
//! * **stack**: the GPU stack, `Renderer::render_display_u8_on_gpu`;
//! * **export**: `Renderer::render_yuv420_on_gpu`, the export's GPU path,
//!   fed frames that arrive on the GPU as the export's decoders hand them
//!   over.
//!
//! Every path that takes the frame has to agree with **cpu** within one
//! level of 8-bit display code, the unit of the existing parity tests
//! (`gpu_composite.rs`); the export is compared on its luma plane. A path
//! may decline a frame (the stack declines what it cannot do yet), and
//! which ones did is printed for each case, so a new decline shows up in
//! the test's output. **cpu** must also give the same frame twice.
//!
//! Projects are flattened the way the engine flattens them before
//! rendering (`flatten_sequence`), so track effects, grade groups and
//! compounds are baked as they are for real.
//!
//! The corpus is the yardstick for #997: every later change to how a frame
//! is rendered has to leave these frames where they are. Reference frames
//! are recorded once phase 0 has landed; until then the paths are held to
//! each other. A difference the corpus knows about is listed in [`KNOWN`]
//! with its issue, and fails the moment it goes away, so the list cannot
//! outlive the bug.

use klipp_timeline::GradeAccess;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use klipp_color::hdr::HdrTransfer;
use klipp_compose::{FrameSource, GpuPixels, LinearFrameProviderFn, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, Keyframe, RationalTime, Timecode, TransitionAlignment, TransitionId, TransitionKind, WorkingColorspace};
use klipp_effects::{BuiltinEffect, ColorFadeMode, SpillMethod};
use klipp_nodes::params::MatteChannel;

// An analysis effect's analysis, as its media's asset (M4 step 9).
#[path = "support/analysis_assets.rs"]
mod analysis;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, GradeGroup, MatteBinding, Sequence, StencilSettings, TextBox, TitleData, Track, TrackKind, Transition};
use klipp_timeline::test_clip::{self, TestClip};

/// A multiple of 8 wide and even high, so the export's encode tiles.
const W: u32 = 64;
const H: u32 = 36;
/// The worst difference two paths may show, in 8-bit display levels.
const TOLERANCE: i32 = 1;

// ── Frames ──────────────────────────────────────────────────────────────

/// A clip's picture as sRGB bytes: a ramp across, a ramp down and a
/// checker, shifted by `seed`, so no two clips and no two channels agree.
fn pattern(seed: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let r = ((x * 255 / (W - 1)) as u8).wrapping_add(seed);
            let g = (y * 255 / (H - 1)) as u8;
            let b = if ((x / 8) + (y / 6)) % 2 == 0 { 200u8.wrapping_sub(seed) } else { 40 };
            v.extend_from_slice(&[r, g, b, 255]);
        }
    }
    v
}

/// The picture of a moving, grainy clip at frame `n`: [`pattern`] moved
/// two pixels to the right a frame, with grain that changes every frame.
/// What frame blending, temporal noise reduction and motion analysis
/// need: neighbouring frames that differ.
fn pattern_at(seed: u8, n: i64) -> Vec<u8> {
    let still = pattern(seed);
    let mut v = Vec::with_capacity(still.len());
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let sx = (x - 2 * n).rem_euclid(W as i64);
            let i = ((y * W as i64 + sx) * 4) as usize;
            let grain = ((x * 7919 + y * 104_729 + n * 15_485_863).rem_euclid(9) - 4) as i16;
            for c in 0..3 {
                v.push((still[i + c] as i16 + grain).clamp(0, 255) as u8);
            }
            v.push(255);
        }
    }
    v
}

/// The frame of `source_secs` at the corpus's rate.
fn frame_no(source_secs: f64) -> i64 {
    (source_secs * 24.0).floor() as i64
}

/// Scene-referred codes, as a log, PQ or HLG decoder hands them over: a
/// ramp down the frame with a hue across it, so every curve's segments
/// are sampled.
fn scene_codes() -> Vec<u32> {
    (0..H)
        .flat_map(|y| {
            (0..W).map(move |x| {
                let v = (y + 1) * 1000 / H;
                let t = x * 1000 / W;
                klipp_color::hdr::pack_rgb10(v.min(1023), (v * 3 / 4 + t / 4).min(1023), (v / 2 + t / 2).min(1023))
            })
        })
        .collect()
}

fn linear_of(srgb: &[u8]) -> Vec<f32> {
    srgb.chunks_exact(4)
        .flat_map(|p| {
            let c = |v: u8| klipp_color::transfer::srgb_to_linear(v as f32 / 255.0);
            [c(p[0]), c(p[1]), c(p[2]), p[3] as f32 / 255.0]
        })
        .collect()
}

/// A frame the decoder left on the GPU, seeded from the same sRGB bytes
/// the CPU paths are given, so both start from one picture.
struct Seeded {
    srgb: Arc<Vec<u8>>,
    linear: Arc<Vec<f32>>,
}

impl GpuPixels for Seeded {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_srgb8(&self.srgb, width, height)
    }
    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.linear)
    }
}

/// Each media clip's picture, handed over as linear pixels or, for the
/// export path, as a frame on the GPU.
#[derive(Clone, Default)]
struct Frames {
    by_clip: HashMap<ClipId, Arc<Vec<u8>>>,
    seeds: HashMap<ClipId, u8>,
    /// Clips whose picture moves and changes grain every frame.
    moving: HashSet<ClipId>,
    /// Clips whose source is scene-referred codes in this transfer.
    scene: HashMap<ClipId, HdrTransfer>,
    on_gpu: bool,
}

impl Frames {
    fn with(mut self, c: &Clip, seed: u8) -> Self {
        self.by_clip.insert(c.id, Arc::new(pattern(seed)));
        self.seeds.insert(c.id, seed);
        self
    }

    /// `c` as a moving, grainy picture ([`pattern_at`]).
    fn moving(mut self, c: &Clip, seed: u8) -> Self {
        self.seeds.insert(c.id, seed);
        self.moving.insert(c.id);
        self
    }

    /// `c` as scene-referred codes in `transfer`.
    fn scene(mut self, c: &Clip, transfer: HdrTransfer) -> Self {
        self.scene.insert(c.id, transfer);
        self
    }

    /// `clip`'s picture at `source_secs` as sRGB bytes.
    fn srgb_at(&self, clip: &Clip, source_secs: f64) -> Option<Arc<Vec<u8>>> {
        if self.moving.contains(&clip.id) {
            return Some(Arc::new(pattern_at(*self.seeds.get(&clip.id)?, frame_no(source_secs))));
        }
        self.by_clip.get(&clip.id).cloned()
    }
}

impl FrameSource for Frames {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        if let Some(&transfer) = self.scene.get(&clip.id) {
            let pixels = SourcePixels::SceneReferred {
                codes: Arc::new(klipp_color::hdr::SceneCodes::Packed10(scene_codes())),
                transfer,
                source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            };
            return Some(SourceFrame { pixels, width, height, pts: None });
        }
        let srgb = self.srgb_at(clip, source_secs)?;
        let linear = Arc::new(linear_of(&srgb));
        let pixels = if self.on_gpu {
            SourcePixels::Gpu(Arc::new(Seeded { srgb, linear }))
        } else {
            SourcePixels::Linear(linear)
        };
        // A moving clip's frames carry their time, which frame blending
        // weighs the two frames by.
        let pts = self.moving.contains(&clip.id).then(|| frame_no(source_secs) as f64 / 24.0);
        Some(SourceFrame { pixels, width, height, pts })
    }

    fn frame_after(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        if !self.moving.contains(&clip.id) {
            return None;
        }
        let next = (frame_no(source_secs) + 1) as f64 / 24.0;
        self.frame(clip, next, width, height)
    }

    fn temporal_frames_linear(&mut self, clip: &Clip, _working: WorkingColorspace) -> Option<LinearFrameProviderFn> {
        if !self.moving.contains(&clip.id) {
            return None;
        }
        let seed = *self.seeds.get(&clip.id)?;
        let source_in = clip.source_in.to_secs_f64();
        Some(Box::new(move |clip_secs| Some((std::sync::Arc::new(linear_of(&pattern_at(seed, frame_no(source_in + clip_secs)))), W, H))))
    }

    /// Every media clip's analyses, under the one stand-in key the
    /// corpus' renderers hold them by ([`renderer`]).
    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        analysis::key(clip, kind)
    }
}

// ── Projects ────────────────────────────────────────────────────────────

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
}

fn media(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) }
}

fn adjustment(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(from), tc(to)) }
}

fn title(text: &str, from: f64, to: f64) -> Clip {
    let td = TitleData::single_text(TextBox { text: text.into(), font_size: 40.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

/// A sequence of the corpus's size with `tracks`, top first.
fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

/// Source → one node → Output.
fn graph_of(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let node = |kind, params| Node {
        id: NodeId::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Source → a Corrector desaturating fully, gated on its port 1 by a
/// `kind` matte producer → Output.
fn gated_by(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let mut g = graph_of(NodeKind::Corrector, NodeParams { saturation: Some(0.0), ..Default::default() });
    let matte = Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    if kind != NodeKind::ExternalMatte {
        g.edges.push(Edge { from_node: g.nodes[0].id, from_port: 0, to_node: matte.id, to_port: 0 });
    }
    g.edges.push(Edge { from_node: matte.id, from_port: 0, to_node: g.nodes[1].id, to_port: 1 });
    g.node_positions.insert(matte.id, NodePos { x: 100.0, y: 100.0 });
    g.nodes.insert(2, matte);
    g
}

fn exposure(stops: f32) -> NodeGraph {
    graph_of(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() })
}

fn transform(scale: f32, x: f32, y: f32) -> BuiltinEffect {
    BuiltinEffect::Transform {
        position_x: x,
        position_y: y,
        scale_x: scale,
        scale_y: scale,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    }
}

/// One project of the corpus: its sequences (the first is rendered), its
/// pictures, the times to draw it at, and its working space.
struct Case {
    name: String,
    project: Vec<Sequence>,
    frames: Frames,
    times: Vec<f64>,
    working: WorkingColorspace,
    /// Paths expected to decline this case. `None` takes the default
    /// for its working space (see [`expected_declines`]).
    declines: Option<Vec<Path>>,
    /// The delivery: an HDR peak makes the export a PQ encode.
    opts: RenderOptions,
}

impl Case {
    fn new(name: impl Into<String>, seq: Sequence, frames: Frames) -> Self {
        Case { name: name.into(), project: vec![seq], frames, times: vec![0.5, 1.25], working: WorkingColorspace::Rec709Linear, declines: None, opts: RenderOptions::default() }
    }
}

/// Which paths a case is expected to leave to the others. The GPU stack
/// takes every working space (#1279); the export path is checked here
/// for Rec.709 and BT.2020 only ([`export_target`] has no ACES target),
/// so an ACEScg case's export counts as declined. A case that differs
/// says so in [`Case::declines`]. A path that starts declining what it
/// took, or taking what it declined, fails: losing GPU coverage
/// wholesale must not pass quietly.
fn expected_declines(case: &Case) -> Vec<Path> {
    match &case.declines {
        Some(d) => d.clone(),
        None if export_target(case.working).is_some() => vec![],
        None => vec![Path::Export],
    }
}

/// One media clip over the frame carrying `fx`.
fn single(name: impl Into<String>, fx: Vec<BuiltinEffect>) -> Case {
    let mut c = media(0.0, 4.0);
    c = TestClip::of(c).effects(fx).build();
    let frames = Frames::default().with(&c, 0);
    Case::new(name, sequence(vec![track("V1", TrackKind::Video, vec![c])]), frames)
}

/// A clip placed small over a full-frame base clip, carrying `fx` after
/// its transform: what shadows, glows and rounded corners need.
fn placed(name: impl Into<String>, fx: Vec<BuiltinEffect>) -> Case {
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top).effects(std::iter::once(transform(0.6, 4.0, -3.0)).chain(fx)).build();
    let frames = Frames::default().with(&base, 90).with(&top, 0);
    Case::new(name, sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), frames)
}

/// The built-in effects, each with values that do something. The
/// full-frame list follows the CPU/GPU parity suite
/// (`klipp-effects/tests/cpu_gpu_effect_parity.rs`); here each also goes
/// through the compositor, the stack and the export.
fn effect_cases() -> Vec<Case> {
    use BuiltinEffect::*;
    let full = vec![
        ("Opacity", Opacity { value: 0.5 }),
        ("ColorFade", ColorFade { color: [20, 40, 200, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.4 }),
        ("Crop", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.0 }),
        ("Crop feathered", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.05 }),
        ("GaussianBlur", GaussianBlur { radius: 4.0 }),
        ("FocusBlur", FocusBlur { radius: 5.0, vertical: true, band: 0.3 }),
        ("Sharpen", Sharpen { amount: 1.0 }),
        ("CornerPin", CornerPin { tl: [0.1, 0.15], tr: [0.85, 0.05], br: [0.8, 0.9], bl: [0.2, 0.8] }),
        ("Transform", transform(0.8, 10.0, -5.0)),
        ("ChromaKey", ChromaKey { key_color: [13, 230, 26], tolerance: 0.3, soft_edge: 0.1, spill_method: SpillMethod::None, spill_strength: 0.0 }),
        ("Bloom", Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }),
        ("Glow", Glow { radius: 6.0, intensity: 0.6 }),
        ("LightRays", LightRays { center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 32, decay: 0.95, intensity: 0.5 }),
        ("Vignette", Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }),
        ("Diffusion", Diffusion { amount: 0.5, radius: 5.0, threshold: 0.4, desaturation: 0.2 }),
        ("LensFlare", LensFlare { position_x: 0.7, position_y: 0.3, brightness: 0.8, flare_size: 0.3, ghost_count: 4, ghost_intensity: 0.4, chromatic_aberration: 0.3 }),
        ("LightWrap", LightWrap { width: 6.0, intensity: 0.5, softness: 0.5 }),
        ("AnamorphicStreak", AnamorphicStreak { threshold: 0.7, length: 60.0, intensity: 0.6, tint: 0.5 }),
        ("FilmHalation", FilmHalation { threshold: 0.6, radius: 6.0, intensity: 0.5, hue: 0.05 }),
        ("Invert", Invert { mix: 0.7 }),
        // Six levels, not five: a band edge then falls at 255/6, which no
        // byte reaches. With five, the pattern's row of exactly 51/255
        // sits on an edge, and one bit of float difference between paths
        // moves the whole row a band (64 levels); that is the effect's
        // step, not a disagreement.
        ("Posterize", Posterize { levels: 6.0 }),
        ("Mosaic", Mosaic { block_size: 8.0 }),
        ("EdgeDetect", EdgeDetect { strength: 1.0, mix: 0.8 }),
        ("ChannelMixer", ChannelMixer { red_red: 0.8, red_green: 0.2, red_blue: 0.0, green_red: 0.1, green_green: 0.8, green_blue: 0.1, blue_red: 0.0, blue_green: 0.3, blue_blue: 0.7 }),
        ("DirectionalBlur", DirectionalBlur { angle: 30.0, distance: 8.0, samples: 12 }),
        ("Emboss", Emboss { angle: 45.0, strength: 1.0, mix: 0.7 }),
        ("Mirror", Mirror { horizontal: 1, vertical: 0 }),
        ("LensDistortion", LensDistortion { distortion: 0.3, scale: 1.0 }),
        ("Dilate", Dilate { radius: 2 }),
        ("Erode", Erode { radius: 2 }),
        ("Denoise", Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 1 }),
        ("Gamma", Gamma { gamma: 1.6 }),
        ("HSVAdjust", HSVAdjust { hue_shift: 40.0, saturation: 1.3, value: 0.9 }),
        ("Vibrance", Vibrance { amount: 0.5 }),
        ("NoiseGenerator", NoiseGenerator { amount: 0.2, monochrome: 0, seed: 7 }),
        ("FilmGrain", FilmGrain { amount: 0.5, size: 2.0, color: 0, shadows: 0.6, seed: 11 }),
        ("TiltShift", TiltShift { center: 0.5, width: 0.3, softness: 0.2, radius: 6.0 }),
        ("OldFilm", OldFilm { flicker: 0.5, scratches: 1.0, dust: 1.0, weave: 0.5, sepia: 0.6, seed: 13 }),
        ("Vhs", Vhs { tracking: 0.6, bleed: 0.6, noise: 0.5, scanlines: 0.6, seed: 14 }),
        ("MedianFilter", MedianFilter { radius: 2 }),
        ("RadialBlur", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 15.0, samples: 16, mode: 0 }),
        ("Levels", Levels { in_black: 0.1, in_white: 0.9, gamma: 1.3, out_black: 0.05, out_white: 0.95 }),
        ("Equalize", Equalize { strength: 1.0 }),
        ("Deflicker", Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.18 }),
        ("Letterbox", Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 }),
        ("ChromaticAberration", ChromaticAberration { red_shift: 3.0, blue_shift: -3.0, center_x: 0.5, center_y: 0.5 }),
    ];
    let around = vec![
        ("RoundedCorners", RoundedCorners { radius: 6.0, feather: 1.0 }),
        ("DropShadow", DropShadow { offset_x: 0.03, offset_y: 0.04, blur: 3.0, color: [0, 0, 0], opacity: 0.7 }),
        ("OuterGlow", OuterGlow { radius: 5.0, intensity: 0.8, color: [255, 200, 80] }),
        ("FillBars", FillBars { blur: 0.05, zoom: 1.0, darken: 0.25 }),
    ];
    full.into_iter()
        .map(|(n, e)| single(format!("effect/{n}"), vec![e]))
        .chain(around.into_iter().map(|(n, e)| placed(format!("effect/{n}"), vec![e])))
        .collect()
}

/// Variants covered elsewhere for now, with the reason. The temporal
/// effects are in [`temporal_cases`], and a frei0r plug-in in
/// [`frei0r_case`] when one is installed.
const EFFECTS_IN_A_LATER_BATCH: &[&str] = &[
    // A LUT file and the transform's tables, prepared by the renderer
    // (#1746): drawn on every path against their kernels in
    // `colour_effects_render.rs`, and viewer against export in klipp-engine's
    // `viewer_matches_export.rs`. Their stored frames come with the next
    // recording on main.
    "Lut3D",
    "ColourSpaceTransform",
    // Drawn on every path against the Transform effect's kernels in
    // `picture_transform_render.rs`; its stored frames come with the next
    // recording on main (#1746).
    "PictureTransform",
    // Flicker and the binocular vignette (#989): drawn alike on the CPU
    // and the GPU in klipp-effects' `cpu_gpu_effect_parity.rs`, and the
    // flicker's time from the clip's in `effect_keyframes.rs`. Their
    // stored frames come with the next recording on main.
    "Flicker",
    "BinocularVignette",
    // Particles (#1141): drawn alike on the CPU and the GPU in
    // klipp-effects' `cpu_gpu_effect_parity.rs`, their time from the
    // clip's in `effect_keyframes.rs`.
    "Particles",
    // The luma key (#1638): drawn alike on the CPU and the GPU, alpha
    // included, in klipp-effects' `luma_key_effect.rs`, and by op as by
    // the app in klipp-render's `keyer_op_matches_the_app.rs`. Its stored
    // frames come with the next recording on main.
    "LumaKey",
    // Drawn with a generator beside the chain against the blend's formula
    // in `merge_render.rs`, and the generator's kernels in klipp-effects'
    // CPU/GPU parity (#1746).
    "Merge",
    "Generator",
    // Another media item's frame, read by the frame source, which the
    // corpus's has none of: drawn against its item in
    // `media_source_render.rs`.
    "MediaSource",
    // What the tracks below composite to, which a one-clip case has
    // none of: drawn against its stack in `clip_below_render.rs`.
    "ClipBelow",
    // A grade in the chain, a marker the plan turns into the executor's
    // compound node: drawn against the clip's own grade in
    // `grade_compound_render.rs`.
    "GradeCompound",
];

/// One moving, grainy clip carrying `fx` (#1024).
fn moving(name: impl Into<String>, fx: Vec<BuiltinEffect>) -> (Case, Clip) {
    let mut c = media(0.0, 4.0);
    c = TestClip::of(c).effects(fx).build();
    let frames = Frames::default().moving(&c, 0);
    (Case::new(name, sequence(vec![track("V1", TrackKind::Video, vec![c.clone()])]), frames), c)
}

/// The effects that read other frames or an analysis of the clip, and
/// sources that are not display video (#1024).
fn temporal_cases() -> Vec<Case> {
    let mut out = Vec::new();

    // Temporal noise reduction: the frame averaged with its neighbours
    // on the source, ahead of the grade (#996 sends it to the CPU on
    // every path).
    let (mut case, _) = moving("temporal/noise reduction", vec![BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.05 }]);
    // Both GPU paths hand it to the CPU, which decodes the neighbours.
    case.declines = Some(vec![Path::Stack, Path::Export]);
    out.push(case);

    // Frame blending: a speed ramp at half speed in blend mode, so each
    // frame mixes two source frames by where it falls between them.
    let (mut case, _) = moving(
        "temporal/frame blending",
        vec![BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1 }],
    );
    for c in case.project[0].tracks[0].clips.iter_mut() {
        c.speed = 0.5;
        c.source_out = tc(2.0);
    }
    // Odd timeline frames: at half speed each falls halfway between two
    // source frames, so both are weighed in.
    case.times = vec![13.0 / 24.0, 31.0 / 24.0];
    // Blending two decoded frames is a CPU step on the source.
    case.declines = Some(vec![Path::Stack, Path::Export]);
    out.push(case);

    // Stabilisation with its analysis (`renderer`'s): every frame
    // corrected by a shift.
    out.push(moving("temporal/stabilisation", vec![BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]).0);

    // Rolling-shutter correction, with the file's motion as its analysis.
    out.push(moving("temporal/rolling shutter", vec![BuiltinEffect::RollingShutterCorrection { strength: 1.0, readout_ratio: 0.9 }]).0);

    // Deflicker, its analysis light that flickers frame to frame.
    let mut c = media(0.0, 4.0);
    c = TestClip::of(c).effects(vec![BuiltinEffect::Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.0 }]).build();
    let f = Frames::default().with(&c, 0);
    out.push(Case::new("temporal/deflicker gains", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f));

    // Scene-referred sources, graded and with an effect.
    for (name, transfer) in [
        ("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)),
        ("PQ", HdrTransfer::Pq),
        ("HLG", HdrTransfer::Hlg),
    ] {
        let mut c = media(0.0, 4.0);
        c.set_grade(exposure(0.3));
        c = TestClip::of(c).effects(vec![BuiltinEffect::Glow { radius: 4.0, intensity: 0.5 }]).build();
        let f = Frames::default().scene(&c, transfer);
        // A scene-referred clip with an effect: the export's single-clip
        // path hands it to the stack, which places it, its display step in
        // its plan before the effect (#1276).
        out.push(Case::new(format!("scene/{name} graded with glow"), sequence(vec![track("V1", TrackKind::Video, vec![c])]), f));
    }

    // An HDR delivery: an SDR clip and a PQ source, exported as PQ.
    let mut c = media(0.0, 4.0);
    c.set_grade(exposure(0.5));
    let f = Frames::default().with(&c, 0);
    let mut case = Case::new("hdr/PQ delivery", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f);
    case.opts = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    // The stack draws SDR display bytes; an HDR delivery is the export's.
    case.declines = Some(vec![Path::Stack]);
    out.push(case);
    let mut c = media(0.0, 4.0);
    c.set_grade(exposure(0.2));
    let f = Frames::default().scene(&c, HdrTransfer::Pq);
    let mut case = Case::new("hdr/PQ source in a PQ delivery", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f);
    case.opts = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    case.declines = Some(vec![Path::Stack]);
    out.push(case);

    out
}

/// The rules the compositor applies around effects, each where it is
/// subtle today.
fn rule_cases() -> Vec<Case> {
    let mut out = Vec::new();

    // Keyframed effect parameters reach the picture (#992).
    let mut c = media(0.0, 4.0);
    c = TestClip::of(c)
        .effects([BuiltinEffect::GaussianBlur { radius: 0.0 }, BuiltinEffect::Bloom { threshold: 0.6, radius: 4.0, intensity: 0.2 }])
        .key(0, "radius", 0.0, 0.0)
        .key(0, "radius", 2.0, 6.0)
        .key(1, "intensity", 0.0, 0.1)
        .key(1, "intensity", 2.0, 1.2)
        .build();
    let f = Frames::default().with(&c, 0);
    out.push(Case::new("rule/effect keyframes", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f));

    // Opacity: the last enabled Opacity effect wins, over a base.
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top).effect(BuiltinEffect::Opacity { value: 0.9 }).effect(BuiltinEffect::Opacity { value: 0.2 }).effect(BuiltinEffect::Opacity { value: 0.6 }).off().build();
    let f = Frames::default().with(&base, 90).with(&top, 0);
    // Every enabled Opacity multiplies (#1386): 0.9 × 0.2, the disabled
    // 0.6 not at all. The historical case name identifies its baseline.
    out.push(Case::new("rule/opacity last enabled wins", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // Opacity: the effect's keys over the clip's opacity keys.
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top).effect(BuiltinEffect::Opacity { value: 0.5 }).lane("opacity", 0.0, 0.9).lane("opacity", 2.0, 0.9).build();
    top.edit_effect_keyframes(|v| v.insert("0:value".into(), vec![key(0.0, 0.2), key(2.0, 0.8)]));
    let f = Frames::default().with(&base, 90).with(&top, 0);
    out.push(Case::new("rule/opacity effect keys over clip keys", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // Opacity: an adjustment layer by the clip's rule (#1386): a disabled
    // Opacity never counts, so this one draws its Invert whole. The
    // historical case name identifies its baseline.
    let base = media(0.0, 4.0);
    let mut adj = adjustment(0.0, 4.0);
    adj = TestClip::of(adj).effect(BuiltinEffect::Opacity { value: 0.4 }).off().effect(BuiltinEffect::Invert { mix: 1.0 }).build();
    let f = Frames::default().with(&base, 0);
    out.push(Case::new("rule/adjustment first opacity", sequence(vec![track("Adj", TrackKind::Video, vec![adj]), track("V1", TrackKind::Video, vec![base])]), f));

    // A disabled Transform carries stored values but must not place the
    // clip (#1207). Keep the historical case name to identify its baseline.
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top).effect(transform(0.5, 12.0, 6.0)).off().build();
    let f = Frames::default().with(&base, 90).with(&top, 0);
    out.push(Case::new("rule/disabled transform carries values", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // Transform: two enabled Transforms (#995 makes it apply once).
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top).effects(vec![transform(0.7, 6.0, 0.0), BuiltinEffect::Invert { mix: 0.5 }, transform(0.7, 6.0, 0.0)]).build();
    let f = Frames::default().with(&base, 90).with(&top, 0);
    out.push(Case::new("rule/two transforms", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // Transform keyframes move a placed clip with a shadow behind it.
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top)
        .effects([transform(0.5, 0.0, 0.0), BuiltinEffect::DropShadow { offset_x: 0.03, offset_y: 0.04, blur: 2.0, color: [0, 0, 0], opacity: 0.8 }])
        .lane("pos_x", 0.0, -10.0)
        .lane("pos_x", 2.0, 10.0)
        .build();
    let f = Frames::default().with(&base, 90).with(&top, 0);
    out.push(Case::new("rule/keyed transform with shadow", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // A title's soft edges over footage, blurred, with a shadow.
    let base = media(0.0, 4.0);
    let mut t = title("Corpus", 0.0, 4.0);
    t = TestClip::of(t).effects(vec![BuiltinEffect::GaussianBlur { radius: 1.5 }, BuiltinEffect::DropShadow { offset_x: 0.02, offset_y: 0.03, blur: 2.0, color: [0, 0, 0], opacity: 0.7 }]).build();
    let f = Frames::default().with(&base, 60);
    out.push(Case::new("rule/title over footage", sequence(vec![track("T1", TrackKind::Text, vec![t]), track("V1", TrackKind::Video, vec![base])]), f));

    // A blend mode and opacity between two clips.
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top.blend_mode = klipp_core::BlendMode::Screen;
    top = TestClip::of(top).effects(vec![BuiltinEffect::Opacity { value: 0.7 }]).build();
    let f = Frames::default().with(&base, 30).with(&top, 120);
    out.push(Case::new("rule/screen at 70 percent", sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])]), f));

    // An adjustment layer with a blur and an opacity weight.
    let base = media(0.0, 4.0);
    let mut adj = adjustment(0.0, 4.0);
    adj = TestClip::of(adj).effects(vec![BuiltinEffect::GaussianBlur { radius: 3.0 }, BuiltinEffect::Opacity { value: 0.6 }]).build();
    let f = Frames::default().with(&base, 0);
    out.push(Case::new("rule/adjustment blur at 60 percent", sequence(vec![track("Adj", TrackKind::Video, vec![adj]), track("V1", TrackKind::Video, vec![base])]), f));

    out
}

/// Grades, and where track and group grades go.
fn grade_cases() -> Vec<Case> {
    let mut out = Vec::new();

    // A clip's own grade, then an effect.
    let mut c = media(0.0, 4.0);
    c.set_grade(exposure(0.7));
    c = TestClip::of(c).effects(vec![BuiltinEffect::GaussianBlur { radius: 2.0 }]).build();
    let f = Frames::default().with(&c, 0);
    out.push(Case::new("grade/clip grade then blur", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f));

    // Track effects and a track grade, on a clip with and without an
    // enabled Transform, and a track shadow drawn around a placed clip.
    for (name, own) in [("no transform", vec![]), ("own transform", vec![transform(0.6, 0.0, 0.0)])] {
        let base = media(0.0, 4.0);
        let mut top = media(0.0, 4.0);
        top = TestClip::of(top).effects(own).build();
        let mut t = track("V2", TrackKind::Video, vec![top.clone()]);
        t.effects.set_video_effect_list(vec![BuiltinEffect::Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }, BuiltinEffect::DropShadow { offset_x: 0.03, offset_y: 0.03, blur: 2.0, color: [0, 0, 0], opacity: 0.7 }], vec![]);
        t.effects.grade_slot().set(Some(exposure(-0.5)));
        let f = Frames::default().with(&base, 90).with(&top, 0);
        out.push(Case::new(format!("grade/track effects and grade, {name}"), sequence(vec![t, track("V1", TrackKind::Video, vec![base])]), f));
    }

    // A track's Opacity weighs each clip's own, still or keyed (#1078).
    for (name, keyed) in [("still", false), ("keyed", true)] {
        let base = media(0.0, 4.0);
        let mut top = media(0.0, 4.0);
        top = TestClip::of(top).effect(BuiltinEffect::Opacity { value: 0.8 }).build();
        if keyed {
            top = TestClip::of(top).lane("opacity", 0.0, 0.3).lane("opacity", 2.0, 0.9).build();
        }
        let mut t = track("V2", TrackKind::Video, vec![top.clone()]);
        t.effects.set_video_effect_list(vec![BuiltinEffect::Opacity { value: 0.5 }], vec![]);
        let f = Frames::default().with(&base, 90).with(&top, 0);
        out.push(Case::new(format!("grade/track opacity over a clip's, {name}"), sequence(vec![t, track("V1", TrackKind::Video, vec![base])]), f));
    }

    // A grade group's pre and post stages around a member's grade, and a
    // timeline grade over the composite.
    let mut c = media(0.0, 4.0);
    c.set_grade(exposure(0.4));
    let mut group = GradeGroup::new("Scene");
    group.stage_slot(klipp_timeline::GradeStage::Pre).set(Some(exposure(-0.3)));
    group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(graph_of(NodeKind::Saturation, NodeParams { saturation: Some(0.5), ..Default::default() })));
    c.grade_group = Some(group.id);
    let f = Frames::default().with(&c, 0);
    let mut seq = sequence(vec![track("V1", TrackKind::Video, vec![c])]);
    seq.grade_groups = vec![group];
    seq.timeline_grade_slot().set(Some(exposure(0.2)));
    out.push(Case::new("grade/group stages and timeline grade", seq, f));

    // A matte producer with nothing chosen selects the whole frame
    // (#1039): the correction it gates grades everything, opaque, as with
    // no key at all.
    let missing = std::env::temp_dir().join("klipp-corpus-1039-no-such-image.png");
    for (name, kind, params) in [
        ("difference key, no plate", NodeKind::DifferenceKey, NodeParams { difference_key: Some(klipp_core::color_grade::DifferenceKeySettings { enabled: true, ..Default::default() }), ..Default::default() }),
        ("difference key, off", NodeKind::DifferenceKey, NodeParams { difference_key: Some(klipp_core::color_grade::DifferenceKeySettings { enabled: false, plate_path: Some(missing.clone()), ..Default::default() }), ..Default::default() }),
        ("external matte, no image", NodeKind::ExternalMatte, NodeParams::default()),
        ("external matte, a missing image", NodeKind::ExternalMatte, NodeParams { matte_image_path: Some(missing.clone()), ..Default::default() }),
    ] {
        let mut c = media(0.0, 4.0);
        c.set_grade(gated_by(kind, params));
        let f = Frames::default().with(&c, 0);
        let mut case = Case::new(format!("grade/nothing chosen, {name}"), sequence(vec![track("V1", TrackKind::Video, vec![c])]), f);
        // A gated correction whose matte producer runs per node: the
        // export's GPU path declines it, and the CPU export renders it.
        case.declines = Some(vec![Path::Export]);
        out.push(case);
    }

    // Key nodes whose matte used to differ between the paths (#1037): a
    // qualifier that is switched off selects the whole frame, and a luma
    // key's matte is opaque over a part-transparent title.
    let mut c = media(0.0, 4.0);
    let off = klipp_core::QualifierSettings { enabled: false, hue_center: 200.0, hue_width: 20.0, lum_low: 0.1, lum_high: 0.5, ..Default::default() };
    c.set_grade(graph_of(NodeKind::Qualifier, NodeParams { qualifier: Some(off), ..Default::default() }));
    let f = Frames::default().with(&c, 0);
    let mut case = Case::new("grade/qualifier switched off", sequence(vec![track("V1", TrackKind::Video, vec![c])]), f);
    // A lone qualifier runs per node, off the GPU chain, so the export's
    // GPU path declines the grade and the CPU export renders it.
    case.declines = Some(vec![Path::Export]);
    out.push(case);
    let base = media(0.0, 4.0);
    let mut t = title("Key", 0.0, 4.0);
    let lk = klipp_core::LumaKeySettings { enabled: true, low: 0.2, high: 0.7, soft: 0.1, ..Default::default() };
    t.set_grade(graph_of(NodeKind::LumaKey, NodeParams { luma_key: Some(lk), ..Default::default() }));
    let f = Frames::default().with(&base, 60);
    out.push(Case::new("grade/luma key on a title", sequence(vec![track("T1", TrackKind::Text, vec![t]), track("V1", TrackKind::Video, vec![base])]), f));

    // The same grade in each working space.
    for ws in [WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear] {
        let mut c = media(0.0, 4.0);
        c.set_grade(exposure(0.5));
        c = TestClip::of(c).effects(vec![BuiltinEffect::Glow { radius: 4.0, intensity: 0.5 }, BuiltinEffect::ColorFade { color: [200, 40, 20, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.5 }]).build();
        let f = Frames::default().with(&c, 0);
        let mut case = Case::new(format!("grade/working space {ws:?}"), sequence(vec![track("V1", TrackKind::Video, vec![c])]), f);
        case.working = ws;
        out.push(case);
    }

    out
}

/// Compounds: an expanded one carrying its inner track's effects, and one
/// whose inner sequence has a timeline grade, drawn as one picture.
fn compound_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for graded in [false, true] {
        let inner_clip = media(0.0, 4.0);
        let mut inner_track = track("Inner", TrackKind::Video, vec![inner_clip.clone()]);
        inner_track.effects.set_video_effect_list(vec![BuiltinEffect::Invert { mix: 0.6 }], vec![]);
        let mut inner = sequence(vec![inner_track]);
        if graded {
            inner.timeline_grade_slot().set(Some(exposure(0.6)));
        }
        let mut compound = Clip { kind: ClipKind::Sequence { sequence_id: inner.id }, ..Clip::gap(tc(0.0), tc(4.0)) };
        compound = TestClip::of(compound).effects(vec![BuiltinEffect::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }]).build();
        let outer = sequence(vec![track("V1", TrackKind::Video, vec![compound])]);
        let f = Frames::default().with(&inner_clip, 0);
        let name = if graded { "compound/graded inner sequence" } else { "compound/expanded" };
        let mut case = Case::new(name, outer, f);
        case.project.push(inner);
        out.push(case);
    }
    out
}

/// Two clips on two tracks, top first.
fn two_tracks(top: Clip, base: Clip) -> Sequence {
    sequence(vec![track("V2", TrackKind::Video, vec![top]), track("V1", TrackKind::Video, vec![base])])
}

/// `consumer` matted by `source`, which is consumed and not shown.
fn matted(consumer: &mut Clip, source: &mut Clip, channel: MatteChannel, invert: bool, feather_px: f32, choke_px: f32) {
    source.consumed_as_matte = true;
    let mut binding = MatteBinding::new(source.id);
    binding.channel = channel;
    binding.invert = invert;
    binding.feather_px = feather_px;
    binding.choke_px = choke_px;
    consumer.matte_source = Some(binding);
}

fn transition(kind: TransitionKind, a: &Clip, b: &Clip) -> Transition {
    Transition {
        id: TransitionId::new(),
        kind,
        duration: tc(1.0),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    }
}

/// Track mattes and stencils (#1025).
fn matte_cases() -> Vec<Case> {
    let mut out = Vec::new();
    // Luma and alpha mattes, plain and inverted, with feather and choke.
    // The alpha matte's source is cropped with a feather, so its alpha
    // has an edge to read.
    for (name, channel, invert, feather, choke) in [
        ("luma", MatteChannel::Luminance, false, 0.0, 0.0),
        ("luma inverted", MatteChannel::Luminance, true, 0.0, 0.0),
        ("luma feathered and choked", MatteChannel::Luminance, false, 3.0, 1.0),
        ("alpha", MatteChannel::Alpha, false, 0.0, 0.0),
        ("alpha inverted, feathered", MatteChannel::Alpha, true, 2.0, 0.0),
    ] {
        let base = media(0.0, 4.0);
        let mut source = media(0.0, 4.0);
        if channel == MatteChannel::Alpha {
            source = TestClip::of(source).effects(vec![BuiltinEffect::Crop { left: 0.2, right: 0.3, top: 0.1, bottom: 0.25, feather: 0.08 }]).build();
        }
        let mut consumer = media(0.0, 4.0);
        consumer = TestClip::of(consumer).effects(vec![BuiltinEffect::HSVAdjust { hue_shift: 90.0, saturation: 1.2, value: 1.0 }]).build();
        matted(&mut consumer, &mut source, channel, invert, feather, choke);
        let f = Frames::default().with(&base, 90).with(&source, 40).with(&consumer, 0);
        let seq = sequence(vec![
            track("Matte", TrackKind::Video, vec![source]),
            track("V2", TrackKind::Video, vec![consumer]),
            track("V1", TrackKind::Video, vec![base]),
        ]);
        out.push(Case::new(format!("matte/{name}"), seq, f));
    }

    // Feather and choke keyframed, and a matte source carrying an effect.
    let base = media(0.0, 4.0);
    let mut source = media(0.0, 4.0);
    source = TestClip::of(source).effects(vec![BuiltinEffect::GaussianBlur { radius: 3.0 }, BuiltinEffect::Levels { in_black: 0.2, in_white: 0.7, gamma: 1.0, out_black: 0.0, out_white: 1.0 }]).build();
    let mut consumer = media(0.0, 4.0);
    matted(&mut consumer, &mut source, MatteChannel::Luminance, false, 0.0, 0.0);
    consumer.matte_feather_keyframes = vec![key(0.0, 0.0), key(2.0, 6.0)];
    consumer.matte_choke_keyframes = vec![key(0.0, 2.0), key(2.0, 0.0)];
    let f = Frames::default().with(&base, 90).with(&source, 40).with(&consumer, 0);
    let seq = sequence(vec![
        track("Matte", TrackKind::Video, vec![source]),
        track("V2", TrackKind::Video, vec![consumer]),
        track("V1", TrackKind::Video, vec![base]),
    ]);
    out.push(Case::new("matte/keyed feather and choke, source with effects", seq, f));

    // A stencil over two tracks: it cuts both, and a track above it shows.
    let mut stencil = media(0.0, 4.0);
    stencil.matte_stencil = Some(StencilSettings { channel: MatteChannel::Luminance, invert: false, feather_px: 2.0, choke_px: 0.0 });
    let mut above = media(0.0, 4.0);
    above = TestClip::of(above).effects(vec![transform(0.4, -12.0, 0.0)]).build();
    let (mid, low) = (media(0.0, 4.0), media(0.0, 4.0));
    let mut mid = mid;
    mid = TestClip::of(mid).effects(vec![transform(0.7, 8.0, 4.0)]).build();
    let f = Frames::default().with(&stencil, 50).with(&above, 200).with(&mid, 0).with(&low, 120);
    let seq = sequence(vec![
        track("V4", TrackKind::Video, vec![above]),
        track("Stencil", TrackKind::Video, vec![stencil]),
        track("V2", TrackKind::Video, vec![mid]),
        track("V1", TrackKind::Video, vec![low]),
    ]);
    out.push(Case::new("matte/stencil over two tracks", seq, f));
    out
}

/// Transitions between clips that carry effects and grades (#1025). The
/// cut is at 2 s and each transition a second long, so the corpus's
/// times are moved into it.
fn transition_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for kind in [TransitionKind::CrossDissolve, TransitionKind::WipeLeft] {
        let mut a = media(0.0, 2.0);
        a = TestClip::of(a).effects(vec![BuiltinEffect::Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }]).build();
        a.set_grade(exposure(0.5));
        let mut b = media(2.0, 4.0);
        b = TestClip::of(b).effects(vec![transform(0.8, 6.0, 0.0), BuiltinEffect::Invert { mix: 0.4 }]).build();
        b.set_grade(graph_of(NodeKind::Saturation, NodeParams { saturation: Some(0.4), ..Default::default() }));
        let mut t = track("V1", TrackKind::Video, vec![a.clone(), b.clone()]);
        t.transitions.push(transition(kind, &a, &b));
        let f = Frames::default().with(&a, 0).with(&b, 120);
        let mut case = Case::new(format!("transition/{kind:?} with effects and grades"), sequence(vec![t]), f);
        case.times = vec![1.75, 2.2];
        out.push(case);
    }

    // #718's case: a wipe out of a matted clip keeps the matte.
    let mut source = media(0.0, 4.0);
    let mut a = media(0.0, 2.0);
    matted(&mut a, &mut source, MatteChannel::Luminance, false, 2.0, 0.0);
    let b = media(2.0, 4.0);
    let mut t = track("V1", TrackKind::Video, vec![a.clone(), b.clone()]);
    t.transitions.push(transition(TransitionKind::WipeLeft, &a, &b));
    let f = Frames::default().with(&source, 40).with(&a, 0).with(&b, 160);
    let mut case = Case::new("transition/wipe out of a matted clip", sequence(vec![track("Matte", TrackKind::Video, vec![source]), t]), f);
    case.times = vec![1.75, 2.2];
    // The GPU stack mattes a transition's side before the sides mix (#2668).
    out.push(case);
    out
}

/// A multicam angle whose own track carries effects, shown where the
/// primary is (#1025): angle 0 until 1 s, then angle 1.
fn multicam_cases() -> Vec<Case> {
    let group = klipp_core::GroupId::new();
    let mut primary = media(0.0, 4.0);
    primary.multicam_group = Some(group);
    primary.multicam_cuts = vec![(1.0, 1)];
    let mut angle = media(0.0, 4.0);
    angle.multicam_group = Some(group);
    angle.multicam_angle = 1;
    angle = TestClip::of(angle).effects(vec![BuiltinEffect::GaussianBlur { radius: 2.0 }]).build();
    let mut angle_track = track("Angle 1", TrackKind::Video, vec![angle.clone()]);
    angle_track.effects.set_video_effect_list(vec![BuiltinEffect::Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }], vec![]);
    let f = Frames::default().with(&primary, 0).with(&angle, 120);
    vec![Case::new("multicam/angle track with effects", sequence(vec![angle_track, track("V1", TrackKind::Video, vec![primary])]), f)]
}

/// Compounds and precompositions beyond the first batch's two (#1025).
fn nesting_cases() -> Vec<Case> {
    let mut out = Vec::new();

    // A graded compound nested one level inside an expanded compound.
    let inner_clip = media(0.0, 4.0);
    let mut graded = sequence(vec![track("Inner", TrackKind::Video, vec![inner_clip.clone()])]);
    graded.timeline_grade_slot().set(Some(exposure(0.6)));
    let mut middle_clip = Clip::sequence(graded.id, tc(0.0), tc(4.0));
    middle_clip = TestClip::of(middle_clip).effects(vec![BuiltinEffect::Invert { mix: 0.3 }]).build();
    let middle = sequence(vec![track("Middle", TrackKind::Video, vec![middle_clip])]);
    let mut outer_clip = Clip::sequence(middle.id, tc(0.0), tc(4.0));
    outer_clip = TestClip::of(outer_clip).effects(vec![BuiltinEffect::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }]).build();
    let mut case = Case::new("compound/graded inside expanded", sequence(vec![track("V1", TrackKind::Video, vec![outer_clip])]), Frames::default().with(&inner_clip, 0));
    case.project.extend([middle, graded]);
    out.push(case);

    // A graded compound on a Text track, over footage.
    let inner_clip = media(0.0, 4.0);
    let mut inner = sequence(vec![track("Inner", TrackKind::Video, vec![inner_clip.clone()])]);
    inner.timeline_grade_slot().set(Some(exposure(-0.4)));
    let mut compound = Clip::sequence(inner.id, tc(0.0), tc(4.0));
    compound = TestClip::of(compound).effects(vec![transform(0.5, 10.0, 5.0)]).build();
    let base = media(0.0, 4.0);
    let f = Frames::default().with(&inner_clip, 0).with(&base, 90);
    let mut case = Case::new("compound/graded on a text track", sequence(vec![track("T1", TrackKind::Text, vec![compound]), track("V1", TrackKind::Video, vec![base])]), f);
    case.project.push(inner);
    out.push(case);

    // A precomposition whose scene is another size than its parent.
    let scene_clip = media(0.0, 4.0);
    let mut scene_top = title("Pre", 0.0, 4.0);
    scene_top = TestClip::of(scene_top).effects(vec![BuiltinEffect::DropShadow { offset_x: 0.03, offset_y: 0.04, blur: 2.0, color: [0, 0, 0], opacity: 0.7 }]).build();
    let mut scene = sequence(vec![track("T1", TrackKind::Text, vec![scene_top]), track("V1", TrackKind::Video, vec![scene_clip.clone()])]);
    scene.resolution = klipp_core::Resolution { width: W / 2, height: H / 2 };
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    pre = TestClip::of(pre).effects(vec![BuiltinEffect::GaussianBlur { radius: 1.5 }]).build();
    let base = media(0.0, 4.0);
    let f = Frames::default().with(&scene_clip, 0).with(&base, 90);
    let mut case = Case::new("precomposition/another size than its parent", two_tracks(pre, base), f);
    case.project.push(scene);
    out.push(case);
    out
}

/// The camera's shutter over a moving camera, on a clip with effects on
/// both sides of its transform (#1025).
fn camera_cases() -> Vec<Case> {
    let base = media(0.0, 4.0);
    let mut top = media(0.0, 4.0);
    top = TestClip::of(top)
        .effects([
            BuiltinEffect::HSVAdjust { hue_shift: 30.0, saturation: 1.2, value: 1.0 },
            transform(0.5, 0.0, 0.0),
            BuiltinEffect::DropShadow { offset_x: 0.03, offset_y: 0.04, blur: 2.0, color: [0, 0, 0], opacity: 0.7 },
        ])
        .lane("pos_x", 0.0, -12.0)
        .lane("pos_x", 2.0, 12.0)
        .build();
    let f = Frames::default().with(&base, 90).with(&top, 0);
    let mut seq = two_tracks(top, base);
    seq.camera.x_keyframes = vec![key(0.0, 0.45), key(2.0, 0.55)];
    seq.camera.shutter_deg = 180.0;
    seq.camera.shutter_samples = 8;
    // The stack smears it (#1923) and casts the shadow after the move from
    // the average (#2090).
    vec![Case::new("camera/shutter over a moving camera, effects around the transform", seq, f)]
}

/// A moving card that is a shape title with its own motion blur (#1025,
/// after #1021): plain, with a crop before its transform, under a moving
/// camera, and drawn after a precomposition in the same frame.
fn title_blur_cases() -> Vec<Case> {
    let card = |crop: bool| {
        let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: klipp_timeline::TitleBackground::None, style: None, published: Vec::new(), template: None };
        let mut xf = klipp_timeline::default_text_transform();
        xf.size = klipp_timeline::TitlePosition { x: 0.25, y: 0.4 };
        let id = td.add_object(
            klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([240, 200, 60, 255]), gradient: None, stroke: None, fit: None }),
            xf,
        );
        let blur = klipp_timeline::MotionBlur { enabled: true, shutter_angle: 180.0, shutter_angle_keyframes: vec![], samples: 8 };
        td.object_mut(id).unwrap().fx = Some(klipp_timeline::LayerFx { motion_blur: Some(blur), ..Default::default() });
        let mut c = Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) };
        c = TestClip::of(c)
            .effects(if crop {
                vec![BuiltinEffect::Crop { left: 0.0, right: 0.3, top: 0.0, bottom: 0.0, feather: 0.0 }, transform(1.0, 0.0, 0.0)]
            } else {
                vec![transform(1.0, 0.0, 0.0)]
            })
            .lane("pos_x", 0.0, -20.0)
            .lane("pos_x", 2.0, 20.0)
            .build();
        c
    };
    let mut out = Vec::new();
    for (name, crop, camera) in [("plain", false, false), ("a crop before the transform", true, false), ("under a moving camera", false, true)] {
        let base = media(0.0, 4.0);
        let f = Frames::default().with(&base, 90);
        let mut seq = sequence(vec![track("T1", TrackKind::Text, vec![card(crop)]), track("V1", TrackKind::Video, vec![base])]);
        if camera {
            seq.camera.x_keyframes = vec![key(0.0, 0.45), key(2.0, 0.55)];
        }
        // The GPU stack takes a title's own blur, as the CPU does.
        out.push(Case::new(format!("title blur/{name}"), seq, f));
    }
    // Drawn after a precomposition: the scene's render must leave the
    // card's shutter alone.
    let scene_clip = media(0.0, 4.0);
    let scene = sequence(vec![track("V1", TrackKind::Video, vec![scene_clip.clone()])]);
    let pre = Clip::precomposition(scene.id, tc(0.0), tc(4.0));
    let f = Frames::default().with(&scene_clip, 90);
    let mut case = Case::new("title blur/after a precomposition", sequence(vec![track("T1", TrackKind::Text, vec![card(false)]), track("V1", TrackKind::Video, vec![pre])]), f);
    case.project.push(scene);
    out.push(case);
    out
}

/// Grade groups whose stages work in another space than the project, and
/// a track grade on a clip in a group (#1025).
fn group_cases() -> Vec<Case> {
    let mut out = Vec::new();
    let mut c = media(0.0, 4.0);
    let mut own = exposure(0.3);
    own.working_space_override = Some(WorkingColorspace::AcesCG);
    c.set_grade(own);
    let mut group = GradeGroup::new("Mismatch");
    let mut pre = graph_of(NodeKind::Saturation, NodeParams { saturation: Some(1.4), ..Default::default() });
    pre.working_space_override = Some(WorkingColorspace::Bt2020Linear);
    group.stage_slot(klipp_timeline::GradeStage::Pre).set(Some(pre));
    group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(exposure(-0.2)));
    c.grade_group = Some(group.id);
    let mut t = track("V1", TrackKind::Video, vec![c.clone()]);
    t.effects.grade_slot().set(Some(exposure(0.25)));
    let f = Frames::default().with(&c, 0);
    let mut seq = sequence(vec![t]);
    seq.grade_groups = vec![group];
    out.push(Case::new("grade/group in other spaces, with a track grade", seq, f));
    out
}

fn corpus() -> Vec<Case> {
    let mut all = effect_cases();
    all.extend(rule_cases());
    all.extend(grade_cases());
    all.extend(compound_cases());
    all.extend(matte_cases());
    all.extend(transition_cases());
    all.extend(multicam_cases());
    all.extend(nesting_cases());
    all.extend(camera_cases());
    all.extend(group_cases());
    all.extend(title_blur_cases());
    all.extend(temporal_cases());
    all
}

// ── Paths ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Path {
    CpuWithGpu,
    Stack,
    Export,
}

/// A difference the corpus knows about: the case, the path that differs
/// from cpu, the time (`None` for every time), and the issue tracking it.
const KNOWN: &[(&str, Path, Option<f64>, &str)] = &[];

fn renderer(gpu: bool, ws: WorkingColorspace) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(ws);
    // The temporal cases' analyses: every frame shifted (3.5, -1.5),
    // rows sheared by the picture moving between frames, light that
    // flickers frame to frame.
    r.set_asset_store(analysis::store(vec![analysis::stabilised_by(3.5, -1.5, 96), analysis::sheared(96), analysis::flickering(96)]));
    r
}

fn display(linear: &[f32], ws: WorkingColorspace) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb)
}

const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The export's encode target for `ws`, where the corpus checks the
/// export path: Rec.709, and BT.2020 with its gamut step.
fn export_target(ws: WorkingColorspace) -> Option<klipp_effects::gpu::Yuv420Target> {
    let plain = klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: LUMA_709,
        semi_planar: false,
        bits: 8,
        sample_shift: 0,
    };
    match ws {
        WorkingColorspace::Rec709Linear => Some(plain),
        WorkingColorspace::Bt2020Linear => Some(klipp_effects::gpu::Yuv420Target { gamut_op: 1, ..plain }),
        _ => None,
    }
}

/// A PQ delivery mastered to `peak`: ten bits, P010, BT.2020 luma.
fn pq_target(peak: f32) -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 1,
        peak_nits: peak,
        luma_coefs: [0.2627, 0.6780, 0.0593],
        semi_planar: true,
        bits: 10,
        sample_shift: 6,
    }
}

/// The CPU render as a PQ export encodes it: display light, PQ, BT.2020
/// luma at 16/219 in ten bits.
fn pq_luma_of(linear: &[f32], ws: WorkingColorspace, peak: f32) -> Vec<u16> {
    let display = klipp_color::display::ExportDisplay::hdr(ws, klipp_core::HdrMode::Pq, peak);
    let k = [0.2627f32, 0.6780, 0.0593];
    linear
        .chunks_exact(4)
        .map(|p| {
            let (e, _) = display.encode_light([p[0], p[1], p[2]]);
            ((16.0 + (k[0] * e[0] + k[1] * e[1] + k[2] * e[2]) * 219.0) * 4.0 + 0.5).clamp(0.0, 1023.0) as u16
        })
        .collect()
}

/// The CPU render as the export encodes it: its display transform, luma
/// at 16/219, and each 2x2 block's chroma from the mean of its
/// display-encoded RGB, at 128 ± 224/2. That's the encoder's own rule
/// (`SHADER_LINEAR_TO_YUV420`): the mean of the encoded colour, then the
/// difference, not the mean of four chroma values. Planar: Y, then Cb,
/// then Cr.
fn planes_of(linear: &[f32], ws: WorkingColorspace) -> Vec<u8> {
    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, ws, klipp_core::OdtChoice::MatchViewer);
    let e: Vec<[f32; 3]> = linear.chunks_exact(4).map(|p| display.encode([p[0], p[1], p[2]])).collect();
    let [kr, kg, kb] = LUMA_709;
    let luma = |c: [f32; 3]| kr * c[0] + kg * c[1] + kb * c[2];
    let quant = |v: f32, offset: f32, scale: f32| (offset + v * scale).round().clamp(0.0, 255.0) as u8;
    let mut y: Vec<u8> = e.iter().map(|c| quant(luma(*c), 16.0, 219.0)).collect();
    let (cw, ch) = (W / 2, H / 2);
    let mut cb = Vec::with_capacity((cw * ch) as usize);
    let mut cr = Vec::with_capacity((cw * ch) as usize);
    for cy in 0..ch {
        for cx in 0..cw {
            let at = |dx: u32, dy: u32| e[((cy * 2 + dy) * W + cx * 2 + dx) as usize];
            let q = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
            let m = [0, 1, 2].map(|i| q.iter().map(|c| c[i]).sum::<f32>() * 0.25);
            let l = luma(m);
            cb.push(quant((m[2] - l) / (2.0 * (1.0 - kb)), 128.0, 224.0));
            cr.push(quant((m[0] - l) / (2.0 * (1.0 - kr)), 128.0, 224.0));
        }
    }
    y.extend(cb);
    y.extend(cr);
    y
}

/// The worst difference between two byte pictures, and how many pixels
/// differ by more than [`TOLERANCE`], ignoring alpha when `stride` is 4.
fn compare(a: &[u8], b: &[u8], stride: usize) -> (i32, usize) {
    assert_eq!(a.len(), b.len(), "pictures of different sizes");
    let mut worst = 0;
    let mut over = BTreeSet::new();
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if stride == 4 && i % 4 == 3 {
            continue;
        }
        let d = (*x as i32 - *y as i32).abs();
        worst = worst.max(d);
        if d > TOLERANCE {
            over.insert(i / stride);
        }
    }
    (worst, over.len())
}

/// What one path made of one frame.
enum Took {
    Declined,
    Within(i32),
    /// The worst difference, and how many pixels exceed the tolerance.
    Differs(i32, usize),
}

fn judge((worst, over): (i32, usize)) -> Took {
    if worst <= TOLERANCE {
        Took::Within(worst)
    } else {
        Took::Differs(worst, over)
    }
}

/// One CPU and one GPU renderer per working space, reused across cases as
/// the viewer reuses its renderer: building the GPU pipelines afresh for
/// every case cost minutes.
#[derive(Default)]
struct Renderers(HashMap<String, (Renderer, Renderer)>);

impl Renderers {
    fn get(&mut self, ws: WorkingColorspace) -> &mut (Renderer, Renderer) {
        self.0.entry(format!("{ws:?}")).or_insert_with(|| (renderer(false, ws), renderer(true, ws)))
    }
}

/// What one path drew of one frame, in the form it is compared in.
#[derive(Clone)]
enum Drawn {
    /// Display bytes, RGBA; alpha isn't compared.
    Rgba(Vec<u8>),
    /// An export's 8-bit planes (Y, then Cb and Cr), compared byte by byte.
    Planes(Vec<u8>),
    /// An HDR export's ten-bit luma.
    Luma10(Vec<u16>),
}

impl Drawn {
    fn kind(&self) -> &'static str {
        match self {
            Drawn::Rgba(_) => "rgba",
            Drawn::Planes(_) => "planes",
            Drawn::Luma10(_) => "luma10",
        }
    }

    /// The bytes stored for it: a ten-bit sample as two bytes, little-endian.
    fn bytes(&self) -> Vec<u8> {
        match self {
            Drawn::Rgba(b) | Drawn::Planes(b) => b.clone(),
            Drawn::Luma10(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }

    fn from_bytes(kind: &str, b: &[u8]) -> Option<Drawn> {
        match kind {
            "rgba" => Some(Drawn::Rgba(b.to_vec())),
            "planes" => Some(Drawn::Planes(b.to_vec())),
            "luma10" => Some(Drawn::Luma10(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())),
            _ => None,
        }
    }

    /// Its worst difference from `other`, and how many samples differ by
    /// more than [`TOLERANCE`]; `None` when they aren't the same kind or size.
    fn against(&self, other: &Drawn) -> Option<(i32, usize)> {
        match (self, other) {
            (Drawn::Rgba(a), Drawn::Rgba(b)) if a.len() == b.len() => Some(compare(a, b, 4)),
            (Drawn::Planes(a), Drawn::Planes(b)) if a.len() == b.len() => Some(compare(a, b, 1)),
            (Drawn::Luma10(a), Drawn::Luma10(b)) if a.len() == b.len() => {
                let d: Vec<i32> = a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).collect();
                Some((d.iter().copied().max().unwrap_or(0), d.iter().filter(|x| **x > TOLERANCE).count()))
            }
            _ => None,
        }
    }
}

/// Every frame of `case` on every path: cpu, then the other three (`None`
/// where a path declined it), at each of its times.
fn frames_of(case: &Case, renderers: &mut Renderers) -> (Vec<String>, Vec<(f64, Option<Path>, Option<Drawn>)>) {
    let seq = klipp_timeline::flatten::flatten_sequence(&case.project[0], &case.project);
    let ws = case.working;
    let mut problems = Vec::new();
    let mut out = Vec::new();
    for &t in &case.times {
        let mut frames = case.frames.clone();
        let (cpu_r, gpu_r) = renderers.get(ws);
        let opts = case.opts;
        let cpu_linear = cpu_r.render(&seq, tc(t), W, H, &mut frames, opts);
        let again = cpu_r.render(&seq, tc(t), W, H, &mut frames, opts);
        if cpu_linear != again {
            problems.push(format!("{} @{t}: cpu gave two different frames", case.name));
        }
        out.push((t, None, Some(Drawn::Rgba(display(&cpu_linear, ws)))));

        let with_gpu = display(&gpu_r.render(&seq, tc(t), W, H, &mut frames, opts), ws);
        out.push((t, Some(Path::CpuWithGpu), Some(Drawn::Rgba(with_gpu))));

        let stack = gpu_r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut frames, opts);
        out.push((t, Some(Path::Stack), stack.map(|(px, _)| Drawn::Rgba(px))));

        let peak = opts.hdr_peak_nits;
        let target = match peak {
            Some(p) => (ws == WorkingColorspace::Rec709Linear).then(|| pq_target(p)),
            None => export_target(ws),
        };
        let export = target.and_then(|target| {
            let mut on_gpu = Frames { on_gpu: true, ..case.frames.clone() };
            gpu_r.render_yuv420_on_gpu(&seq, tc(t), W, H, &mut on_gpu, opts, target)
        });
        let drawn = match (export, peak) {
            (None, _) => None,
            (Some((planes, _)), None) => {
                let n = planes_of(&cpu_linear, ws).len();
                Some(Drawn::Planes(planes[..n].to_vec()))
            }
            // Ten-bit luma, P010: each sample's ten bits at the top of a
            // little-endian u16.
            (Some((planes, _)), Some(p)) => {
                let n = pq_luma_of(&cpu_linear, ws, p).len();
                Some(Drawn::Luma10(planes[..n * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6).collect()))
            }
        };
        out.push((t, Some(Path::Export), drawn));
    }
    (problems, out)
}

/// What cpu's frame of `case` at `t` is held to on each path: its own
/// display bytes, or the export's planes or ten-bit luma made from it.
fn cpu_reference(case: &Case, cpu: &Drawn, path: Path, renderers: &mut Renderers, t: f64) -> Drawn {
    if path != Path::Export {
        return cpu.clone();
    }
    let seq = klipp_timeline::flatten::flatten_sequence(&case.project[0], &case.project);
    let mut frames = case.frames.clone();
    let (cpu_r, _) = renderers.get(case.working);
    let linear = cpu_r.render(&seq, tc(t), W, H, &mut frames, case.opts);
    match case.opts.hdr_peak_nits {
        Some(p) => Drawn::Luma10(pq_luma_of(&linear, case.working, p)),
        None => Drawn::Planes(planes_of(&linear, case.working)),
    }
}

/// Every path at every time of `case`, against cpu.
fn draw(case: &Case, renderers: &mut Renderers) -> (Vec<String>, Vec<(f64, Path, Took)>) {
    let (problems, frames) = frames_of(case, renderers);
    let mut results = Vec::new();
    let mut cpu = None;
    for (t, path, drawn) in frames {
        let Some(path) = path else {
            cpu = drawn;
            continue;
        };
        let took = match (drawn, &cpu) {
            (None, _) => Took::Declined,
            (Some(d), Some(c)) => {
                let want = cpu_reference(case, c, path, renderers, t);
                judge(d.against(&want).expect("a frame of the same kind and size as cpu's"))
            }
            (Some(_), None) => unreachable!("cpu comes first"),
        };
        results.push((t, path, took));
    }
    (problems, results)
}

#[test]
fn every_path_draws_the_corpus_alike() {
    let mut failures = Vec::new();
    let mut routes = Vec::new();
    let known_at = |case: &str, path: Path, t: f64| {
        KNOWN.iter().find(|(c, p, at, _)| *c == case && *p == path && at.is_none_or(|a| (a - t).abs() < 1e-9)).map(|k| k.3)
    };
    let mut known_seen = BTreeSet::new();
    let mut renderers = Renderers::default();
    for case in corpus() {
        let (problems, results) = draw(&case, &mut renderers);
        failures.extend(problems);
        let mut route = Vec::new();
        let declines = expected_declines(&case);
        for (t, path, took) in results {
            let listed = known_at(&case.name, path, t);
            match (&took, declines.contains(&path)) {
                (Took::Declined, false) => failures.push(format!("{} @{t}: {path:?} declined a case it should take", case.name)),
                (Took::Within(_) | Took::Differs(..), true) => {
                    failures.push(format!("{} @{t}: {path:?} took a case it is expected to decline (update the case's declines)", case.name))
                }
                _ => {}
            }
            match (&took, listed) {
                (Took::Differs(d, n), None) => failures.push(format!("{} @{t}: {path:?} differs from cpu by up to {d} levels in {n} pixels", case.name)),
                (Took::Differs(..), Some(_)) => {
                    known_seen.insert((case.name.clone(), path, t.to_bits()));
                }
                (Took::Within(_), Some(issue)) => {
                    failures.push(format!("{} @{t}: {path:?} now agrees with cpu: take it off KNOWN ({issue})", case.name))
                }
                _ => {}
            }
            let mark = match took {
                Took::Declined => "-".to_string(),
                Took::Within(d) => format!("{d}"),
                Took::Differs(d, n) => format!("!{d}x{n}px"),
            };
            route.push(format!("{path:?}@{t}={mark}"));
        }
        routes.push(format!("{:<46} {}", case.name, route.join(" ")));
    }
    eprintln!("corpus routes (worst difference from cpu in levels; - = path declined):\n{}", routes.join("\n"));
    for (c, p, at, i) in KNOWN {
        let seen = known_seen.iter().any(|(sc, sp, st)| sc == c && sp == p && at.is_none_or(|a| a.to_bits() == *st));
        if !seen && !failures.iter().any(|f| f.starts_with(c)) {
            failures.push(format!("KNOWN lists {c} on {p:?} at {at:?} ({i}), but that case never differed there"));
        }
    }
    assert!(failures.is_empty(), "{} problems:\n{}", failures.len(), failures.join("\n"));
}

// ── Stored frames ───────────────────────────────────────────────────────
//
// Every path's frame of every case and time, recorded on main and stored in
// `tests/goldens/`: `corpus.png` holds the frames' bytes end to end as one
// grey image (PNG compresses them), and `corpus.json` says where each one
// is, with the commit and the reason they were recorded. The paths are held
// to them within the corpus's one-level tolerance, as they are to cpu: the
// node-graph switch (#1089) must draw what main drew.
//
// To record them again, on main, with a reason for the commit message:
//
//     KLIPP_RECORD_GOLDENS="<why>" cargo test -p klipp-compose --test all golden_corpus::every_path_draws_its_recorded_frames
//
// A rule change records only the cases it moves, every other frame held
// to its stored one: add `KLIPP_RECORD_GOLDEN_CASE="<case>|<case>"`.
//
// A re-recording is reviewed like code: its commit says why each frame may
// change.

const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
/// One row of the stored image is one row of a frame (64 RGBA pixels), so
/// PNG's row filters see the pictures' own structure and compress them.
const GOLDEN_WIDTH: u32 = W * 4;

fn path_name(path: Option<Path>) -> String {
    path.map_or("cpu".into(), |p| format!("{p:?}"))
}

/// Every frame the corpus draws, keyed by case, time and path, in order.
fn all_frames() -> (Vec<String>, Vec<((String, String, String), Option<Drawn>)>) {
    let mut renderers = Renderers::default();
    let (mut problems, mut out) = (Vec::new(), Vec::new());
    for case in corpus() {
        let (p, frames) = frames_of(&case, &mut renderers);
        problems.extend(p);
        for (t, path, drawn) in frames {
            out.push(((case.name.clone(), format!("{t}"), path_name(path)), drawn));
        }
    }
    (problems, out)
}

fn record(frames: &[((String, String, String), Option<Drawn>)], reason: &str) {
    let mut data = Vec::new();
    let mut index = Vec::new();
    for ((case, t, path), drawn) in frames {
        let Some(d) = drawn else {
            index.push(serde_json::json!({ "case": case, "t": t, "path": path, "declined": true }));
            continue;
        };
        let b = d.bytes();
        index.push(serde_json::json!({ "case": case, "t": t, "path": path, "kind": d.kind(), "offset": data.len(), "len": b.len() }));
        data.extend(b);
    }
    let rows = (data.len() as u32).div_ceil(GOLDEN_WIDTH).max(1);
    data.resize((GOLDEN_WIDTH * rows) as usize, 0);
    std::fs::create_dir_all(GOLDENS).unwrap();
    let file = std::fs::File::create(format!("{GOLDENS}/corpus.png")).unwrap();
    let png = image::codecs::png::PngEncoder::new_with_quality(std::io::BufWriter::new(file), image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive);
    image::ImageEncoder::write_image(png, &data, GOLDEN_WIDTH, rows, image::ExtendedColorType::L8).unwrap();
    let commit = std::process::Command::new("git").args(["rev-parse", "--short=8", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
    // One frame a line, so a re-recording's diff reads frame by frame.
    let lines: Vec<String> = index.iter().map(|f| format!("  {f}")).collect();
    let head = serde_json::json!({ "recorded_on": commit, "reason": reason });
    let text = format!("{{\n  \"recorded_on\": {},\n  \"reason\": {},\n  \"frames\": [\n{}\n  ]\n}}\n", head["recorded_on"], head["reason"], lines.join(",\n"));
    std::fs::write(format!("{GOLDENS}/corpus.json"), text).unwrap();
}

/// The stored frames: each key's kind and bytes, `None` for a declined one.
fn stored() -> std::collections::BTreeMap<(String, String, String), Option<Drawn>> {
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{GOLDENS}/corpus.json")).expect("tests/goldens/corpus.json")).unwrap();
    let data = image::open(format!("{GOLDENS}/corpus.png")).expect("tests/goldens/corpus.png").to_luma8().into_raw();
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
    doc["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let key = (s(f, "case"), s(f, "t"), s(f, "path"));
            if f["declined"] == true {
                return (key, None);
            }
            let (o, n) = (f["offset"].as_u64().unwrap() as usize, f["len"].as_u64().unwrap() as usize);
            (key, Drawn::from_bytes(&s(f, "kind"), &data[o..o + n]))
        })
        .collect()
}

/// The two opacity rule cases draw what their rule computes (#1386), on
/// every path: three Opacities [0.9 on, 0.2 on, 0.6 off] draw as one at
/// 0.9 × 0.2 = 0.18, and an adjustment layer whose only Opacity (0.4) is
/// off draws as one with no Opacity, its Invert whole. So their stored
/// frames, recorded under the rule, hold the computed result.
#[test]
fn the_opacity_rule_cases_draw_what_the_rule_computes() {
    let mut renderers = Renderers::default();
    let named = |name: &str| corpus().into_iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no case {name:?}"));
    let base = media(0.0, 4.0);
    let mut one = media(0.0, 4.0);
    one = TestClip::of(one).effect(BuiltinEffect::Opacity { value: 0.9 * 0.2 }).build();
    let f = Frames::default().with(&base, 90).with(&one, 0);
    let multiplied = Case::new("check/opacity 0.18", sequence(vec![track("V2", TrackKind::Video, vec![one]), track("V1", TrackKind::Video, vec![base])]), f);
    let base = media(0.0, 4.0);
    let mut adj = adjustment(0.0, 4.0);
    adj = TestClip::of(adj).effect(BuiltinEffect::Invert { mix: 1.0 }).build();
    let f = Frames::default().with(&base, 0);
    let unweighted = Case::new("check/adjustment no opacity", sequence(vec![track("Adj", TrackKind::Video, vec![adj]), track("V1", TrackKind::Video, vec![base])]), f);
    for (case, want) in [(named("rule/opacity last enabled wins"), multiplied), (named("rule/adjustment first opacity"), unweighted)] {
        let (_, got) = frames_of(&case, &mut renderers);
        let (_, expected) = frames_of(&want, &mut renderers);
        assert_eq!(got.len(), expected.len(), "{}", case.name);
        for ((t, path, a), (_, _, b)) in got.iter().zip(&expected) {
            let (Some(a), Some(b)) = (a, b) else { continue };
            let (worst, _) = a.against(b).expect("frames of one kind and size");
            assert!(worst <= TOLERANCE, "{} @{t} {}: {worst} levels from what its rule computes", case.name, path_name(*path));
        }
    }
}

/// Every path draws each case as main drew it when the frames were stored,
/// within the corpus's tolerance; a path declines where it declined then.
#[test]
fn every_path_draws_its_recorded_frames() {
    let (problems, frames) = all_frames();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    if let Ok(reason) = std::env::var("KLIPP_RECORD_GOLDENS") {
        assert!(!reason.trim().is_empty(), "say why the frames are recorded again");
        if let Ok(case) = std::env::var("KLIPP_RECORD_GOLDEN_CASE") {
            // One rule change may move several cases: their names apart by
            // `|`.
            let cases: Vec<&str> = case.split('|').map(str::trim).collect();
            // A reviewed rule change may replace those cases only. All
            // other stored bytes stay exact, and must still agree with
            // today's render before any reference is written.
            let mut old = stored();
            let mut selected = frames.clone();
            let mut count = 0;
            for (key, drawn) in &mut selected {
                let previous = old.remove(key).expect("a case-only recording cannot add frames");
                if cases.contains(&key.0.as_str()) {
                    count += 1;
                    match (&previous, drawn.as_ref()) {
                        (Some(w), Some(d)) => println!("re-record {} @{} {}: {:?}", key.0, key.1, key.2, d.against(w)),
                        _ => println!("re-record {} @{} {}: route availability changed", key.0, key.1, key.2),
                    }
                } else {
                    assert!(match (&previous, drawn.as_ref()) {
                        (None, None) => true,
                        (Some(w), Some(d)) => d.against(w).is_some_and(|(worst, _)| worst <= TOLERANCE),
                        _ => false,
                    }, "an unselected golden changed: {key:?}");
                    *drawn = previous;
                }
            }
            assert!(count > 0, "no frames of {case:?}");
            for c in &cases {
                assert!(selected.iter().any(|(k, _)| k.0 == *c), "no frames of {c:?}");
            }
            assert!(old.is_empty(), "a case-only recording cannot remove frames");
            record(&selected, &reason);
        } else {
            record(&frames, &reason);
        }
        return;
    }
    let mut want = stored();
    let mut failures = Vec::new();
    for (key, drawn) in &frames {
        let (case, t, path) = key;
        match (want.remove(key), drawn) {
            (None, _) => failures.push(format!("{case} @{t} {path}: no stored frame (a new case: record them again)")),
            (Some(None), None) => {}
            (Some(None), Some(_)) => failures.push(format!("{case} @{t} {path}: declined when recorded, draws now")),
            (Some(Some(_)), None) => failures.push(format!("{case} @{t} {path}: drew when recorded, declines now")),
            (Some(Some(w)), Some(d)) => match d.against(&w) {
                None => failures.push(format!("{case} @{t} {path}: not the kind or size recorded")),
                Some((worst, n)) if worst > TOLERANCE => failures.push(format!("{case} @{t} {path}: differs from its recorded frame by up to {worst} levels in {n} samples")),
                Some(_) => {}
            },
        }
    }
    for (case, t, path) in want.keys() {
        failures.push(format!("{case} @{t} {path}: stored, but the corpus no longer draws it"));
    }
    assert!(failures.is_empty(), "{} frames differ from the stored ones:\n{}", failures.len(), failures.join("\n"));
}

/// Every effect variant is in the corpus, or listed with a reason.
#[test]
fn every_effect_variant_is_in_the_corpus() {
    let src = include_str!("../../klipp-effects/src/lib.rs");
    let start = src.find("pub enum BuiltinEffect {").expect("the enum");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("the enum's end");
    let declared: BTreeSet<String> = body[..end]
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("    ")?;
            rest.chars().next()?.is_ascii_uppercase().then(|| rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect())
        })
        .collect();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    // Frei0r has its own test below, at a size it accepts.
    covered.insert("Frei0r".into());
    for case in effect_cases().into_iter().chain(temporal_cases()) {
        for clip in case.project[0].tracks.iter().flat_map(|t| &t.clips) {
            for e in test_clip::effects(clip) {
                covered.insert(format!("{e:?}").chars().take_while(|c| c.is_ascii_alphanumeric()).collect());
            }
        }
    }
    covered.extend(EFFECTS_IN_A_LATER_BATCH.iter().map(|s| s.to_string()));
    let missing: Vec<_> = declared.difference(&covered).collect();
    assert!(declared.len() > 40, "the variant scan found only {declared:?}");
    assert!(missing.is_empty(), "effects missing from the corpus: {missing:?}");
}

/// The deflicker and rolling-shutter cases draw what their analysis
/// computes: the frame without the effect, through the effect's own kernel
/// with the gain or rows worked out from the asset itself
/// (`analysis::flicker_gain`, `analysis::shear_rows`). Their recorded
/// frames are held to this, as #1386's were, never to whatever rendered.
#[test]
fn the_analysed_cases_draw_what_their_analysis_computes() {
    let mut seen = 0;
    for case in temporal_cases().into_iter().filter(|c| c.name == "temporal/deflicker gains" || c.name == "temporal/rolling shutter") {
        seen += 1;
        let clip = case.project[0].tracks[0].clips[0].clone();
        let mut plain = case.project[0].clone();
        strip_effects(&mut plain.tracks[0].clips[0]);
        let mut r = renderer(false, case.working);
        for &t in &case.times {
            let with = r.render(&case.project[0], tc(t), W, H, &mut case.frames.clone(), case.opts);
            let without = r.render(&plain, tc(t), W, H, &mut case.frames.clone(), case.opts);
            let clip_time = t - clip.timeline_in.to_secs_f64();
            let source_secs = clip.source_secs_at(clip_time, 24.0);
            let (mut effects, mut motion) = (clip.builtin_effects().to_vec(), klipp_effects::MotionData::default());
            for e in &mut effects {
                match e {
                    BuiltinEffect::Deflicker { gain, window_secs, .. } => *gain = analysis::flicker_gain(96, *window_secs as f64, source_secs),
                    BuiltinEffect::RollingShutterCorrection { strength, readout_ratio } => motion.rsc_rows = Some(analysis::shear_rows(H, *readout_ratio, *strength)),
                    _ => {}
                }
            }
            let luma = klipp_color::linear::luma_coefs(case.working);
            let want = klipp_effects::apply_effects_with_motion(&effects, without, W, H, luma, &motion);
            let worst = with.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(with.len() == want.len() && worst < 1e-4, "{} @{t}: {worst} from what its analysis computes", case.name);
        }
    }
    assert_eq!(seen, 2, "both analysed cases are in the corpus");
}

/// Each temporal case's effect does something on the CPU, so the paths
/// agreeing is not agreement on doing nothing: without the effect (and
/// its analysis), the frame comes out different (#1024).
#[test]
fn the_temporal_effects_change_the_picture() {
    for case in temporal_cases().into_iter().filter(|c| c.name.starts_with("temporal/")) {
        let mut plain = case.project[0].clone();
        for c in plain.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            c.set_effect_list(Vec::new(), c.builtin_effects_enabled().to_vec());
        }
        let mut r = renderer(false, case.working);
        let t = case.times[0];
        let with = r.render(&case.project[0], tc(t), W, H, &mut case.frames.clone(), case.opts);
        let without = r.render(&plain, tc(t), W, H, &mut case.frames.clone(), case.opts);
        let moved = with.iter().zip(&without).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(moved > 0.01, "{}: the effect changed nothing (largest change {moved})", case.name);
    }
}

/// A frei0r plug-in draws alike on every path that takes it (#1024).
/// Apart from the corpus because frei0r refuses a frame whose sides are
/// not multiples of 8, and the corpus's are 64x36: there it would pass
/// the picture through on every path and agree by doing nothing. Run
/// with `invert0r`, which every frei0r install carries; without it the
/// test says it is skipped.
#[test]
fn a_frei0r_plugin_draws_alike() {
    const FW: u32 = 64;
    const FH: u32 = 40;
    struct Ramp;
    impl FrameSource for Ramp {
        fn frame(&mut self, _c: &Clip, _s: f64, w: u32, h: u32) -> Option<SourceFrame> {
            let px: Vec<f32> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [x as f32 / w as f32, y as f32 / h as f32, 0.3, 1.0])).collect();
            Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
        }
    }
    // The GPU first. Discovery loads every frei0r plug-in, and one that
    // links NVIDIA's OpenGL runs its loader with the dynamic linker's lock
    // held; a Vulkan instance created at the same moment on another
    // thread (the corpus test's renderers) waits for that lock inside the
    // same driver, and the two deadlock (#1032).
    let _ = klipp_effects::gpu_context::GpuContext::shared();
    let plugins = klipp_frei0r::discover_frei0r_plugins();
    let Some(info) = plugins.iter().find(|p| p.name.eq_ignore_ascii_case("invert0r")).cloned() else {
        eprintln!("skipped: no frei0r invert0r plug-in is installed");
        return;
    };
    klipp_effects::frei0r_effect::set_plugin_registry(plugins);
    let type_id = info.type_id();
    let params = klipp_effects::frei0r_effect::default_params(&type_id).unwrap_or_default();
    let mut c = media(0.0, 4.0);
    let plain_seq = {
        let mut s = sequence(vec![track("V1", TrackKind::Video, vec![c.clone()])]);
        s.resolution = klipp_core::Resolution { width: FW, height: FH };
        s
    };
    c = TestClip::of(c).effects(vec![BuiltinEffect::Frei0r { type_id, params }]).build();
    let mut seq = sequence(vec![track("V1", TrackKind::Video, vec![c])]);
    seq.resolution = klipp_core::Resolution { width: FW, height: FH };
    let ws = WorkingColorspace::Rec709Linear;
    let (mut cpu_r, mut gpu_r) = (renderer(false, ws), renderer(true, ws));
    let opts = RenderOptions::default();
    let cpu_linear = cpu_r.render(&seq, tc(0.5), FW, FH, &mut Ramp, opts);
    let plain = cpu_r.render(&plain_seq, tc(0.5), FW, FH, &mut Ramp, opts);
    let moved = cpu_linear.iter().zip(&plain).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(moved > 0.1, "invert0r changed nothing (largest change {moved})");
    let cpu = display(&cpu_linear, ws);
    let with_gpu = display(&gpu_r.render(&seq, tc(0.5), FW, FH, &mut Ramp, opts), ws);
    let (worst, _) = compare(&cpu, &with_gpu, 4);
    assert!(worst <= TOLERANCE, "cpu+gpu differs from cpu by {worst} levels");
    match gpu_r.render_display_u8_on_gpu(&seq, tc(0.5), FW, FH, &mut Ramp, opts) {
        Some((stack, _)) => {
            let (worst, _) = compare(&cpu, &stack, 4);
            assert!(worst <= TOLERANCE, "the stack differs from cpu by {worst} levels");
        }
        None => eprintln!("frei0r: the stack declined"),
    }
}

/// `c` with its built-in effects taken off, through its picture.
fn strip_effects(c: &mut Clip) {
    let n = klipp_timeline::test_clip::effects(c).len();
    let edits: Vec<klipp_timeline::EffectEdit> = (0..n).rev().map(klipp_timeline::EffectEdit::Remove).collect();
    c.apply_effect_edits(&edits).expect("plain effects").expect("taken off");
}
