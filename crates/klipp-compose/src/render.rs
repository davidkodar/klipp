//! Rendering one timeline frame: every visible track's clip at a time,
//! each graded and effected in its own right, stacked bottom to top.
//!
//! # The rules
//!
//! These are the semantics the viewer and export both follow. Where the
//! two used to disagree (or the viewer disagreed with itself), the choice
//! made here is noted.
//!
//! * **Stacking.** `Sequence::tracks[0]` is the *top* track; the stack is
//!   composited from the last track upwards. (The viewer blended overlays
//!   top-first, inverting the order with two or more of them.)
//! * **Each clip is its own layer**, graded with *its own* node graph and
//!   effects. (The viewer graded the primary track's pixels with the
//!   topmost clip's graph.)
//! * **Per clip, in order:** source pixels → node graph (scene-linear for
//!   PQ / HLG / camera log) → display tone map (scene-referred sources
//!   only) → OFX → built-in effects. Effects therefore always see the
//!   picture, never scene-linear values above 1.0, which OFX (8-bit)
//!   would clip. (The viewer's primary track ran OFX before built-ins and
//!   overlays the reverse; neither tone-mapped overlays.)
//! * **Time.** A clip's graph, effects and keyframes are evaluated at
//!   clip-relative time (timeline time − clip start), the base the UI
//!   writes keyframes in. (The viewer's primary track used source time.)
//! * **Opacity** is a compositing weight — clip opacity (static value,
//!   keyframes, modifiers) × track opacity — applied once, at the blend.
//!   The `Opacity` effect is not also run as a pixel effect. (It
//!   premultiplied into a straight-alpha blend, and overlays applied it a
//!   second time on top.)
//! * **Transitions** belong to a track: both clips are rendered as full
//!   graded layers at the frame's time — each continuing into its handle,
//!   holding its edge frame where there is none — blended, and the result
//!   is that track's layer. (The viewer blended the whole composite with
//!   an ungraded, frozen partner, and only during playback.)
//! * **The global LUT** applies to the whole composite, the timeline's
//!   output. (The viewer applied it to the primary track only.)
//! * **Background** is black: the composite is flattened over it unless
//!   the caller asks to keep alpha.
//! * **HDR deliveries** (`RenderOptions::hdr_peak_nits`) render display
//!   light instead of the SDR picture, 1.0 being SDR white at 203 cd/m²:
//!   scene-referred clips end in `klipp_color::hdr::apply_hdr_display_light`
//!   rather than the SDR tone map, so PQ keeps its own luminance, HLG takes
//!   its OOTF and camera log rolls into the peak; SDR clips are untouched,
//!   their white at 203 nits as BT.2408 places it. Effects then see the
//!   highlights too. The global LUT applies to the SDR range and the light
//!   above SDR white passes through it. The viewer renders SDR.
//! * **Slow motion with frame blending** (a `SpeedRamp` effect with blend
//!   mode 1, the clip running below real time) mixes the two source
//!   frames around the source time, weighted by where it falls between
//!   them, in linear light before the grade. (The viewer mixed the
//!   current picture with the previously *shown* one, in 8-bit, weighted
//!   by a number that was noise at 59.94 fps; export did not blend.)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use klipp_core::{BlendMode, ClipId, Timecode, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_nodes::NodeId;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

use crate::eval::{self, ResolvedTransform};

/// A clip's decoded pixels, before its grade. Shared rather than owned so
/// a source that keeps its frames (the viewer re-renders a paused frame
/// many times) hands them over without a copy; the renderer copies only
/// if it has to write to them.
pub enum SourcePixels {
    /// Display-referred linear RGBA f32, straight alpha, in the working
    /// space: ordinary video.
    Linear(Arc<Vec<f32>>),
    /// Scene-referred packed 10-bit codes (PQ, HLG, camera log), as the
    /// decoders produce them. Graded scene-linear, tone-mapped after.
    SceneReferred {
        codes: Arc<Vec<u32>>,
        transfer: klipp_color::hdr::HdrTransfer,
        /// Source primaries → working space.
        source_to_working: [[f32; 3]; 3],
    },
    /// Display-referred pixels still on the GPU, where a hardware decoder
    /// left them. A grade starts from them there, without the frame
    /// crossing the bus; anything else reads them back.
    Gpu(Arc<dyn GpuPixels>),
}

/// A decoded frame on the GPU (see [`SourcePixels::Gpu`]).
pub trait GpuPixels: Send + Sync {
    /// Seed `exec`'s next grade from the frame. `false` when it cannot
    /// (the size, the device), and the pixels are read back instead.
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool;
    /// The frame on the CPU: display-referred linear RGBA.
    fn read(&self) -> Arc<Vec<f32>>;
}

/// A clip's pixels on their way into its grade: on the CPU, still on the
/// GPU, or a scene-referred source's codes — decoded to linear on the GPU
/// as the grade starts, or here only when something needs them here.
enum GradeInput {
    Cpu(Arc<Vec<f32>>),
    Gpu(Arc<dyn GpuPixels>),
    Scene {
        codes: Arc<Vec<u32>>,
        transfer: klipp_color::hdr::HdrTransfer,
        source_to_working: [[f32; 3]; 3],
    },
}

impl GradeInput {
    fn pixels(&self) -> Arc<Vec<f32>> {
        match self {
            GradeInput::Cpu(p) => Arc::clone(p),
            GradeInput::Gpu(g) => g.read(),
            GradeInput::Scene { codes, transfer, source_to_working } => {
                Arc::new(klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(codes, *transfer, *source_to_working))
            }
        }
    }
}

/// One decoded frame of a clip's media.
pub struct SourceFrame {
    pub pixels: SourcePixels,
    pub width: u32,
    pub height: u32,
    /// The frame's presentation time in the media, when known. Frame
    /// blending weighs two frames by it.
    pub pts: Option<f64>,
}

/// Where decoded frames come from. The viewer answers from its playback
/// decoders, export from its sequential ones; the renderer does not care.
pub trait FrameSource {
    /// `clip`'s media at `source_secs`, ideally at `width` × `height` (a
    /// different size is resampled). `source_secs` may lie before the
    /// media's start or past its end while a transition runs into a clip's
    /// handle; return the nearest available frame. `None` renders nothing
    /// for this clip.
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame>;

    /// The frame after the one [`frame`](Self::frame) returns for
    /// `source_secs`, for frame blending. Asked only after `frame`, for
    /// the same time. `None` (the default) shows the one frame unblended.
    fn frame_after(&mut self, _clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        None
    }

    /// A small sRGB RGBA8 frame of `clip`'s media at `source_secs`, at most
    /// `max_width` wide, for motion estimation (rolling-shutter
    /// correction). Sources that cannot provide one return `None`, and the
    /// correction is skipped.
    fn motion_frame(&mut self, _clip: &Clip, _source_secs: f64, _max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        None
    }

    /// Frames of `clip`'s media for OFX plugins that read other times than
    /// the current one (retime, slit-scan, frame blend): a function from
    /// clip time to an sRGB RGBA8 frame. `None` (the default) runs those
    /// plugins on the current frame alone.
    fn temporal_frames(&mut self, _clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        None
    }
}

/// What the viewer's grade tools want from one clip's grade besides the
/// picture: the node canvas's thumbnails, the scope's tap point, a soloed
/// node, a qualifier's matte, the before/after split's "before" half.
/// Only `clip` is inspected; every other clip renders plainly.
#[derive(Default, Clone)]
pub struct Inspect {
    pub clip: Option<ClipId>,
    /// Nodes whose outputs to capture as thumbnails.
    pub thumbs: Option<HashSet<NodeId>>,
    /// Node whose output the scope reads instead of the graph's.
    pub scope_tap: Option<NodeId>,
    /// Node whose matte the grade shows instead of the picture.
    pub matte_view: Option<NodeId>,
    /// Node whose output the grade shows instead of the graph's.
    pub solo: Option<NodeId>,
    /// Also return the clip's picture before its grade.
    pub ungraded: bool,
    /// A clip whose track-matte mask to return (the inspector's matte
    /// preview). Independent of `clip`.
    pub matte_of: Option<ClipId>,
}

/// What an inspected render found. Fields are empty when not asked for,
/// or when the clip was not on screen.
#[derive(Default)]
pub struct Inspection {
    pub scope_tap: Option<Vec<f32>>,
    pub thumbs: HashMap<NodeId, Vec<u8>>,
    /// The inspected clip before its grade: display-referred linear RGBA
    /// at the output size (scene-referred sources tone-mapped).
    pub ungraded: Option<Vec<f32>>,
    /// `matte_of`'s mask, one value a pixel at the output size.
    pub matte_mask: Option<Vec<f32>>,
    pub stats: RenderStats,
}

/// Where a render's time went, in milliseconds.
#[derive(Default, Clone, Copy, Debug)]
pub struct RenderStats {
    pub grade_ms: f64,
    pub ofx_ms: f64,
    pub effects_ms: f64,
    pub lut_ms: f64,
    pub composite_ms: f64,
    pub total_ms: f64,
}

/// One entry of the stack [`Renderer::render`] composites.
pub struct ActiveLayer<'a> {
    /// Index into `Sequence::tracks`.
    pub track: usize,
    /// The clip shown, or a transition's outgoing and incoming clips.
    pub clips: Vec<&'a Clip>,
    pub transition: bool,
}

/// What a render at `time` stacks, bottom first: every clip whose frame
/// it will ask a [`FrameSource`] for is among these. Lets a source know
/// which media to have ready, and a caller whether a frame is a single
/// plain clip.
pub fn active_layers(seq: &Sequence, time: Timecode) -> Vec<ActiveLayer<'_>> {
    stack(seq, time)
        .into_iter()
        .map(|(track, _, item)| ActiveLayer {
            track,
            transition: matches!(item, TrackItem::Transition { .. }),
            clips: item.clips(),
        })
        .collect()
}

/// A copy of `seq` holding only what a render at `time` reads: the clips
/// in play (both sides of a running transition included) and every clip
/// they refer to — a transform link's target, a tracker's host, a focal
/// point's title. Rendering it gives the same frame as rendering `seq`,
/// so a caller can let go of a large project's lock for the render.
pub fn frame_snapshot(seq: &Sequence, time: Timecode) -> Sequence {
    let mut keep: HashSet<ClipId> = HashSet::new();
    let mut keep_transitions: HashSet<klipp_core::TransitionId> = HashSet::new();
    for track in &seq.tracks {
        for c in &track.clips {
            if time >= c.timeline_in && time < c.timeline_out {
                keep.insert(c.id);
            }
        }
        for tr in &track.transitions {
            if let Some(TrackItem::Transition { a, b, .. }) = transition_at(track, tr, time) {
                keep.insert(a.id);
                keep.insert(b.id);
                keep_transitions.insert(tr.id);
            }
        }
    }
    // Follow references until nothing new turns up (links chain).
    let by_id: HashMap<ClipId, &Clip> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
    let mut frontier: Vec<ClipId> = keep.iter().copied().collect();
    while let Some(id) = frontier.pop() {
        let Some(c) = by_id.get(&id) else { continue };
        for r in clip_references(c) {
            if by_id.contains_key(&r) && keep.insert(r) {
                frontier.push(r);
            }
        }
    }
    let tracks = seq
        .tracks
        .iter()
        .map(|t| Track {
            id: t.id,
            name: t.name.clone(),
            kind: t.kind,
            clips: t.clips.iter().filter(|c| keep.contains(&c.id)).cloned().collect(),
            transitions: t.transitions.iter().filter(|tr| keep_transitions.contains(&tr.id)).cloned().collect(),
            opacity: t.opacity,
            muted: t.muted,
            solo: t.solo,
            visible: t.visible,
            ..Track::new(&t.name, t.kind)
        })
        .collect();
    Sequence {
        id: seq.id,
        name: seq.name.clone(),
        fps: seq.fps,
        resolution: seq.resolution,
        tracks,
        in_point: seq.in_point,
        out_point: seq.out_point,
        markers: Vec::new(),
        last_playhead: None,
    }
}

/// Clips `c` reads when it renders: its transform link's target or
/// tracker host, and its focal point's title or tracker.
fn clip_references(c: &Clip) -> Vec<ClipId> {
    let mut refs = Vec::new();
    if let Some(link) = &c.transform_link {
        match &link.target {
            klipp_timeline::TransformLinkTarget::Clip(id) => refs.push(*id),
            klipp_timeline::TransformLinkTarget::Tracker { clip_id, .. } => refs.push(*clip_id),
        }
    }
    for e in &c.builtin_effects {
        if let BuiltinEffect::Transform { focal_ref: Some(f), .. } = e {
            match f {
                klipp_effects::FocalRef::Glyph { title_clip_id, .. } => refs.push(ClipId(*title_clip_id)),
                klipp_effects::FocalRef::Tracker { clip_id, .. } => refs.push(ClipId(*clip_id)),
            }
        }
    }
    refs
}

/// Visible tracks with something on them at `time`, bottom first.
fn stack(seq: &Sequence, time: Timecode) -> Vec<(usize, &Track, TrackItem<'_>)> {
    seq.tracks
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, t)| is_visual(t))
        .filter_map(|(i, t)| active_item(t, time).map(|item| (i, t, item)))
        .collect()
}

/// The timeline's global LUT.
pub struct GlobalLut {
    pub lut: klipp_color::lut::LutData,
    pub opacity: f32,
}

/// Per-render choices that are not part of the timeline.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderOptions {
    /// Leave the composite's alpha as is instead of flattening over black
    /// (for exports with an alpha channel).
    pub keep_alpha: bool,
    /// Render for an HDR delivery mastered to this peak, cd/m²: display
    /// light, 1.0 = SDR white at 203 nits (see the rules above). `None`
    /// renders the SDR picture.
    pub hdr_peak_nits: Option<f32>,
}

/// The resources rendering needs, owned per thread: the viewer has one,
/// every export job has its own.
pub struct Renderer {
    executor: klipp_nodes::GraphExecutor,
    gpu: Option<klipp_effects::gpu::GpuEffectsPipeline>,
    gpu_tried: bool,
    allow_gpu: bool,
    ofx: klipp_plugin::OfxHost,
    titles: klipp_title::TitleRenderer,
    glyph_cache: HashMap<ClipId, klipp_title::GlyphCacheEntry>,
    working: WorkingColorspace,
    global_lut: Option<GlobalLut>,
    /// Rolling-shutter motion per (clip, source millisecond): estimating
    /// it decodes three frames, and a paused viewer re-renders the same
    /// frame many times.
    rsc_motion: HashMap<(ClipId, i64), (klipp_effects::rolling_shutter::Homography, klipp_effects::rolling_shutter::Homography)>,
    /// The current render's inspection request, and what it has found.
    inspect: Inspect,
    found: Inspection,
    /// The current render's `RenderOptions::hdr_peak_nits`.
    hdr_peak: Option<f32>,
    /// Tetrahedral 3D-LUT interpolation, or trilinear for speed.
    lut_tetrahedral: bool,
}

/// A rendered clip, ready to composite: display-referred linear RGBA,
/// straight alpha, at the output size.
struct Layer {
    pixels: Vec<f32>,
    /// Clip opacity × track opacity.
    opacity: f32,
    blend: BlendMode,
}

/// What a track contributes at the frame's time.
enum TrackItem<'a> {
    Clip(&'a Clip),
    Transition {
        a: &'a Clip,
        b: &'a Clip,
        kind: klipp_core::TransitionKind,
        progress: f32,
        params: klipp_core::TransitionUserParams,
    },
}

impl<'a> TrackItem<'a> {
    fn clips(&self) -> Vec<&'a Clip> {
        match self {
            TrackItem::Clip(c) => vec![*c],
            TrackItem::Transition { a, b, .. } => vec![*a, *b],
        }
    }
}

impl Renderer {
    /// A renderer with the given OFX host (plugin discovery is the
    /// caller's: it is slow, and the viewer and export each have theirs).
    pub fn new(ofx: klipp_plugin::OfxHost, titles: klipp_title::TitleRenderer) -> Self {
        Self {
            executor: klipp_nodes::GraphExecutor::new(),
            gpu: None,
            gpu_tried: false,
            allow_gpu: true,
            ofx,
            titles,
            glyph_cache: HashMap::new(),
            working: WorkingColorspace::default(),
            global_lut: None,
            rsc_motion: HashMap::new(),
            inspect: Inspect::default(),
            found: Inspection::default(),
            hdr_peak: None,
            lut_tetrahedral: true,
        }
    }

    /// The node-graph executor. The viewer's GPU-resident fast paths
    /// drive it directly, so there is one executor and one GPU pipeline
    /// per renderer rather than one per path.
    pub fn executor(&mut self) -> &mut klipp_nodes::GraphExecutor {
        &mut self.executor
    }

    /// The GPU effects pipeline, built on first use; `None` without a
    /// usable GPU or with the GPU disabled.
    pub fn gpu_pipeline(&mut self) -> Option<&mut klipp_effects::gpu::GpuEffectsPipeline> {
        self.gpu()
    }

    pub fn ofx_host(&mut self) -> &mut klipp_plugin::OfxHost {
        &mut self.ofx
    }

    pub fn title_renderer(&mut self) -> &mut klipp_title::TitleRenderer {
        &mut self.titles
    }

    /// Use the GPU for grades, effects and transitions when available
    /// (default). Off, everything runs on the CPU paths — for tests, and
    /// machines without a usable GPU. The grade's executor opens its own
    /// GPU otherwise, so it is replaced along with the effects pipeline.
    pub fn set_gpu_enabled(&mut self, enabled: bool) {
        if enabled != self.allow_gpu {
            self.executor = if enabled {
                klipp_nodes::GraphExecutor::new()
            } else {
                klipp_nodes::GraphExecutor::new_cpu_only()
            };
        }
        self.allow_gpu = enabled;
        if !enabled {
            self.gpu = None;
        }
    }

    /// Tetrahedral 3D-LUT interpolation (the default, and what a
    /// delivery should have) or trilinear, which the viewer offers for
    /// speed. Applies to the timeline's global LUT and to every clip's
    /// LUT node.
    pub fn set_lut_tetrahedral(&mut self, tetrahedral: bool) {
        self.lut_tetrahedral = tetrahedral;
    }

    pub fn set_working_colorspace(&mut self, ws: WorkingColorspace) {
        self.working = ws;
    }

    pub fn set_global_lut(&mut self, lut: Option<GlobalLut>) {
        self.global_lut = lut;
    }

    fn gpu(&mut self) -> Option<&mut klipp_effects::gpu::GpuEffectsPipeline> {
        if self.allow_gpu && self.gpu.is_none() && !self.gpu_tried {
            self.gpu_tried = true;
            self.gpu = klipp_effects::gpu::GpuEffectsPipeline::new();
        }
        self.gpu.as_mut()
    }

    /// Render `seq` at `time` into display-referred linear RGBA f32 at
    /// `width` × `height` — everything up to the display encode.
    pub fn render(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
    ) -> Vec<f32> {
        self.render_inspected(seq, time, width, height, src, opts, Inspect::default()).0
    }

    /// [`render`](Self::render), also answering an [`Inspect`] request.
    /// The picture is the same either way, except that a soloed node or a
    /// matte view replaces the inspected clip's grade — the viewer shows
    /// what the grade tools ask it to.
    #[allow(clippy::too_many_arguments)]
    pub fn render_inspected(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        inspect: Inspect,
    ) -> (Vec<f32>, Inspection) {
        let t0 = Instant::now();
        self.inspect = inspect;
        self.found = Inspection::default();
        self.hdr_peak = opts.hdr_peak_nits;
        let out = self.render_stack(seq, time, width, height, src, opts);
        self.inspect = Inspect::default();
        self.hdr_peak = None;
        // A solo belongs to the inspected clip; leave the executor clean
        // for whoever drives it next.
        self.executor.set_solo_node(None);
        let mut found = std::mem::take(&mut self.found);
        found.stats.total_ms = ms_since(t0);
        (out, found)
    }

    fn render_stack(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
    ) -> Vec<f32> {
        let fps = seq.fps.as_f64();
        // Bottom of the stack first.
        let stack = stack(seq, time);

        // Transforms for every clip in play, links and focal points
        // resolved once against the whole sequence.
        let mut transforms: HashMap<ClipId, ResolvedTransform> = HashMap::new();
        for (_, _, item) in &stack {
            for c in item.clips() {
                eval::resolve_clip_transform(
                    c, seq, time, fps, &mut transforms, &mut self.glyph_cache, &mut self.titles,
                    seq.resolution.width, seq.resolution.height,
                );
            }
        }

        // Clips whose rendered pixels another clip masks with.
        let mut matte_sources: HashSet<ClipId> = HashSet::new();
        for (_, _, item) in &stack {
            for c in item.clips() {
                if let Some(b) = &c.matte_source {
                    matte_sources.insert(b.source_clip_id);
                }
                if c.matte_stencil.is_some() {
                    matte_sources.insert(c.id);
                }
            }
        }

        // Render every layer first: a matte source may sit above or below
        // the clip it masks.
        let mut layers: Vec<Option<Layer>> = Vec::with_capacity(stack.len());
        let mut rendered: HashMap<ClipId, Vec<f32>> = HashMap::new();
        for (_, track, item) in &stack {
            let layer = match item {
                TrackItem::Clip(c) if matches!(c.kind, ClipKind::AdjustmentLayer) => None,
                TrackItem::Clip(c) => self
                    .render_clip(c, time, fps, width, height, src, &transforms)
                    .map(|(pixels, opacity)| Layer { pixels, opacity: opacity * track.opacity, blend: c.blend_mode }),
                TrackItem::Transition { a, b, kind, progress, params } => {
                    self.render_transition(a, b, *kind, *progress, params, time, fps, width, height, src, &transforms)
                        .map(|pixels| Layer { pixels, opacity: track.opacity, blend: BlendMode::Normal })
                }
            };
            if let (TrackItem::Clip(c), Some(l)) = (item, &layer) {
                if matte_sources.contains(&c.id) {
                    rendered.insert(c.id, l.pixels.clone());
                }
            }
            layers.push(layer);
        }

        // Stencils mask every layer below them.
        let stencils: Vec<(usize, &Clip)> = stack
            .iter()
            .enumerate()
            .filter_map(|(i, (_, _, item))| match item {
                TrackItem::Clip(c) if c.matte_stencil.is_some() => Some((i, *c)),
                _ => None,
            })
            .collect();

        // The composite starts empty. The first layer, when it is a Normal
        // blend at full weight, *is* the composite (src-over onto nothing
        // leaves it unchanged), so it is moved in rather than blended onto
        // a zeroed canvas — at 4K that canvas is 133 MB to allocate, zero
        // and blend through, every frame.
        let canvas = || vec![0.0f32; (width as usize) * (height as usize) * 4];
        let mut composite_ms = 0.0;
        let mut acc: Option<Vec<f32>> = None;
        for (i, ((_, track, item), layer)) in stack.iter().zip(layers).enumerate() {
            if let TrackItem::Clip(c) = item {
                if matches!(c.kind, ClipKind::AdjustmentLayer) {
                    let opacity = eval::evaluate_clip_opacity(c, time, fps) * track.opacity;
                    let below = acc.take().unwrap_or_else(canvas);
                    acc = Some(self.apply_adjustment(c, below, opacity, time, fps, width, height, &transforms));
                    continue;
                }
                if c.consumed_as_matte || c.matte_stencil.is_some() {
                    continue;
                }
            }
            let Some(mut layer) = layer else { continue };
            if layer.opacity <= 0.001 {
                continue;
            }
            let t = Instant::now();
            for (si, st) in &stencils {
                if *si <= i {
                    continue;
                }
                let s = st.matte_stencil.as_ref().unwrap();
                let binding = klipp_timeline::MatteBinding {
                    source_clip_id: st.id,
                    channel: s.channel,
                    invert: s.invert,
                    feather_px: s.feather_px,
                    choke_px: s.choke_px,
                };
                apply_matte(&mut layer.pixels, &rendered, &binding, width, height);
            }
            if let TrackItem::Clip(c) = item {
                if let Some(b) = &c.matte_source {
                    let t = eval_clip_time(c, time);
                    let binding = klipp_timeline::MatteBinding {
                        source_clip_id: b.source_clip_id,
                        channel: b.channel,
                        invert: b.invert,
                        feather_px: klipp_core::keyframe::evaluate(&c.matte_feather_keyframes, t, b.feather_px),
                        choke_px: klipp_core::keyframe::evaluate(&c.matte_choke_keyframes, t, b.choke_px),
                    };
                    if self.inspect.matte_of == Some(c.id) {
                        self.found.matte_mask = matte_mask(&rendered, &binding, width, height);
                    }
                    apply_matte(&mut layer.pixels, &rendered, &binding, width, height);
                }
            }
            match &mut acc {
                None if layer.blend == BlendMode::Normal && layer.opacity >= 1.0 => acc = Some(layer.pixels),
                _ => {
                    let a = acc.get_or_insert_with(canvas);
                    klipp_effects::composite::blend_layer_linear(a, &layer.pixels, layer.opacity, layer.blend);
                }
            }
            composite_ms += ms_since(t);
        }
        let mut acc = acc.unwrap_or_else(canvas);

        if let Some(lut) = self.global_lut.take() {
            let t = Instant::now();
            let tetrahedral = self.lut_tetrahedral;
            let above_white = if self.hdr_peak.is_some() { split_above_white(&mut acc) } else { None };
            acc = match self.gpu() {
                Some(gpu) => gpu.apply_3d_lut_gpu(acc, width, height, &lut.lut.data, lut.lut.size as u32, lut.opacity, tetrahedral),
                None => klipp_effects::apply_3d_lut(acc, &lut.lut.data, lut.lut.size as u32, lut.opacity, tetrahedral),
            };
            if let Some(over) = above_white {
                use rayon::prelude::*;
                acc.par_iter_mut().zip(over.par_iter()).for_each(|(v, o)| *v += o);
            }
            self.global_lut = Some(lut);
            self.found.stats.lut_ms += ms_since(t);
        }

        if !opts.keep_alpha {
            use rayon::prelude::*;
            let t = Instant::now();
            acc.par_chunks_exact_mut(4).for_each(|px| {
                let a = px[3].clamp(0.0, 1.0);
                px[0] *= a;
                px[1] *= a;
                px[2] *= a;
                px[3] = 1.0;
            });
            composite_ms += ms_since(t);
        }
        self.found.stats.composite_ms += composite_ms;
        acc
    }

    /// One clip as a layer: source pixels, grade, tone map, OFX, built-in
    /// effects. Returns the pixels and the clip's opacity (not applied).
    #[allow(clippy::too_many_arguments)]
    fn render_clip(
        &mut self,
        clip: &Clip,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        transforms: &HashMap<ClipId, ResolvedTransform>,
    ) -> Option<(Vec<f32>, f32)> {
        let clip_time = eval_clip_time(clip, time);
        let (input, hdr) = match &clip.kind {
            ClipKind::Media { .. } => {
                let at = source_secs(clip, time, fps);
                let f = src.frame(clip, at, width, height)?;
                let next = if frame_blends(clip, time) { src.frame_after(clip, at, width, height) } else { None };
                match f {
                    // Left on the GPU for the grade to start from.
                    SourceFrame { pixels: SourcePixels::Gpu(g), width: w, height: h, .. }
                        if next.is_none() && (w, h) == (width, height) =>
                    {
                        (GradeInput::Gpu(g), None)
                    }
                    // Scene-referred codes at the render size go to the
                    // grade as they are.
                    SourceFrame { pixels: SourcePixels::SceneReferred { codes, transfer, source_to_working }, width: w, height: h, .. }
                        if next.is_none() && (w, h) == (width, height) && codes.len() == (w as usize) * (h as usize) =>
                    {
                        (GradeInput::Scene { codes, transfer, source_to_working }, Some(transfer))
                    }
                    f => {
                        let pts = f.pts;
                        let (pixels, hdr) = to_linear(f, width, height);
                        let pixels = match (next, pts) {
                            (Some(n), Some(p0)) => blend_frames(pixels, hdr, p0, n, at, width, height),
                            _ => pixels,
                        };
                        (GradeInput::Cpu(pixels), hdr)
                    }
                }
            }
            ClipKind::Title(td) => {
                let over = crate::synth::typewriter_override(td, clip_time);
                let td = over.as_ref().unwrap_or(td);
                let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
                let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                let u8s = self.titles.render_with_time(td, width, height, gpu, clip_time as f32);
                (GradeInput::Cpu(Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&u8s))), None)
            }
            ClipKind::Generator(g) => {
                let total = (clip.timeline_out - clip.timeline_in).to_secs_f64();
                let u8s = crate::synth::render_generator_with_slate(&mut self.titles, g, width, height, clip_time, total);
                (GradeInput::Cpu(Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&u8s))), None)
            }
            ClipKind::Gap | ClipKind::Sequence { .. } | ClipKind::AdjustmentLayer => return None,
        };

        if self.inspect.ungraded && self.inspect.clip == Some(clip.id) {
            let mut before = (*input.pixels()).clone();
            if let Some(t) = hdr {
                self.display_render(&mut before, t);
            }
            self.found.ungraded = Some(before);
        }
        let mut pixels = self.grade(clip, input, clip_time, fps, width, height);
        if let Some(t) = hdr {
            self.display_render(&mut pixels, t);
        }
        let (fx, opacity) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied());
        self.prepare_temporal_effects(clip, &fx, time, fps, height, src);
        let provider = if clip.effects.iter().any(|e| e.enabled && is_temporal_plugin(&e.plugin_name)) {
            src.temporal_frames(clip)
        } else {
            None
        };
        let pixels = self.ofx(clip, pixels, clip_time, width, height, provider);
        let pixels = match title_motion_blur(clip) {
            Some(samples) => self.motion_blurred(clip, pixels, &fx, samples, time, fps, width, height),
            None => self.effects(&fx, pixels, clip_time, width, height),
        };
        Some((pixels, opacity))
    }

    /// A scene-referred clip's last step of its own: the SDR display tone
    /// map, or for an HDR delivery its display light.
    fn display_render(&self, pixels: &mut [f32], t: klipp_color::hdr::HdrTransfer) {
        match self.hdr_peak {
            Some(peak) => klipp_color::hdr::apply_hdr_display_light(
                pixels,
                t,
                klipp_color::primaries::luminance_weights(self.working),
                peak,
            ),
            None => klipp_color::hdr::apply_hdr_display_tonemap(pixels, t),
        }
    }

    /// Upload the per-frame data the GPU's stabilisation and rolling-shutter
    /// effects read: the stabilisation correction for this frame, and the
    /// row transforms from motion between the neighbouring source frames.
    /// Both exist on the GPU path only; the CPU effects skip them.
    fn prepare_temporal_effects(
        &mut self,
        clip: &Clip,
        fx: &[BuiltinEffect],
        time: Timecode,
        fps: f64,
        height: u32,
        src: &mut dyn FrameSource,
    ) {
        use klipp_effects::rolling_shutter::{compute_row_transforms, estimate_motion, Homography};
        let stabilise = fx.iter().any(|e| matches!(e, BuiltinEffect::VideoStabilization { .. }));
        let rsc = fx.iter().find_map(|e| match e {
            BuiltinEffect::RollingShutterCorrection { strength, readout_ratio } if *strength > f32::EPSILON => {
                Some((*strength, *readout_ratio))
            }
            _ => None,
        });
        if !stabilise && rsc.is_none() {
            return;
        }
        let clip_time = eval_clip_time(clip, time);
        if stabilise {
            if let Some(tr) = clip.smoothed_trajectory.as_ref().filter(|t| !t.corrections.is_empty()) {
                let source_dur = (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()).max(0.001);
                let idx = ((clip_time * tr.corrections.len() as f64 / source_dur).round() as usize).min(tr.corrections.len() - 1);
                let h = &tr.corrections[idx].h;
                let m = [
                    h[0][0] as f32, h[0][1] as f32, h[0][2] as f32,
                    h[1][0] as f32, h[1][1] as f32, h[1][2] as f32,
                    h[2][0] as f32, h[2][1] as f32, h[2][2] as f32,
                    0.0, 0.0, 0.0,
                ];
                if let Some(gpu) = self.gpu() {
                    gpu.upload_stabilization_homography(&m);
                }
            }
        }
        if let Some((strength, readout)) = rsc {
            if self.gpu().is_none() {
                return;
            }
            let at = source_secs(clip, time, fps);
            let key = (clip.id, (at * 1000.0).round() as i64);
            let motion = match self.rsc_motion.get(&key) {
                Some(m) => Some(*m),
                None => {
                    let dt = 1.0 / fps.max(1.0);
                    let frames = (
                        src.motion_frame(clip, (at - dt).max(0.0), 480),
                        src.motion_frame(clip, at, 480),
                        src.motion_frame(clip, at + dt, 480),
                    );
                    let m = match frames {
                        (Some((p, pw, ph)), Some((c, cw, ch)), Some((n, nw, nh))) => {
                            Some(estimate_motion(&p, pw, ph, &c, cw, ch, &n, nw, nh)
                                .unwrap_or((Homography::identity(), Homography::identity())))
                        }
                        _ => None,
                    };
                    if let Some(m) = m {
                        if self.rsc_motion.len() > 512 {
                            self.rsc_motion.clear();
                        }
                        self.rsc_motion.insert(key, m);
                    }
                    m
                }
            };
            if let Some((prev, next)) = motion {
                let rows = compute_row_transforms(&prev, &next, height, readout, strength);
                if let Some(gpu) = self.gpu() {
                    gpu.upload_rsc_row_transforms(&rows);
                }
            }
        }
    }

    /// The clip's node graph, at clip-relative time. The inspected clip
    /// also runs the grade tools' captures, its solo and its matte view.
    /// Pixels still on the GPU seed the grade there; read back only when
    /// there is no grade to run, or a node needs them on the CPU.
    fn grade(&mut self, clip: &Clip, input: GradeInput, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        let inspecting = self.inspect.clip == Some(clip.id);
        let matte_view = if inspecting { self.inspect.matte_view } else { None };
        if clip.node_graph.is_identity() && matte_view.is_none() {
            return unshare(input.pixels());
        }
        let t = Instant::now();
        self.executor.prepare_clip(&clip.node_graph);
        self.executor.set_working_colorspace(self.working);
        self.executor.set_lut_tetrahedral(self.lut_tetrahedral);
        self.executor.set_current_clip_track_points(&clip.track_points);
        self.executor.set_solo_node(if inspecting { self.inspect.solo } else { None });
        let frame = grade_frame(clip_time, fps);
        let out = match &input {
            GradeInput::Cpu(p) => self.run_graph(clip, p, width, height, clip_time, frame, inspecting, matte_view),
            GradeInput::Gpu(g) if g.seed(&mut self.executor, width, height) => {
                match self.run_graph(clip, &[], width, height, clip_time, frame, inspecting, matte_view) {
                    Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {
                        self.run_graph(clip, &g.read(), width, height, clip_time, frame, inspecting, matte_view)
                    }
                    r => r,
                }
            }
            GradeInput::Gpu(g) => self.run_graph(clip, &g.read(), width, height, clip_time, frame, inspecting, matte_view),
            // Decoded on the GPU as the chain starts; the codes stay there
            // while the same frame is regraded.
            GradeInput::Scene { codes, transfer, source_to_working }
                if self.allow_gpu
                    && self.executor.prime_chain_from_hdr10_codes(
                        codes,
                        width,
                        height,
                        transfer.transfer_id(),
                        transfer.scene_scale(),
                        klipp_effects::color_wgsl::pack_primaries_matrix(*source_to_working),
                    ) =>
            {
                match self.run_graph(clip, &[], width, height, clip_time, frame, inspecting, matte_view) {
                    Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {
                        self.run_graph(clip, &input.pixels(), width, height, clip_time, frame, inspecting, matte_view)
                    }
                    r => r,
                }
            }
            GradeInput::Scene { .. } => {
                self.run_graph(clip, &input.pixels(), width, height, clip_time, frame, inspecting, matte_view)
            }
        };
        self.found.stats.grade_ms += ms_since(t);
        match out {
            Ok(out) => out,
            Err(e) => {
                tracing::error!("node graph failed for clip {:?}: {e:?}", clip.id);
                unshare(input.pixels())
            }
        }
    }

    /// Run the executor on `source` (empty when the chain is seeded on
    /// the GPU), with the grade tools' captures for the inspected clip.
    #[allow(clippy::too_many_arguments)]
    fn run_graph(
        &mut self,
        clip: &Clip,
        source: &[f32],
        width: u32,
        height: u32,
        clip_time: f64,
        frame: u64,
        inspecting: bool,
        matte_view: Option<NodeId>,
    ) -> Result<Vec<f32>, klipp_nodes::executor::ExecutorError> {
        if !inspecting {
            return self
                .executor
                .execute_linear(source, &clip.node_graph, width, height, clip_time, frame, None, None)
                .map(|(out, _)| out);
        }
        let thumbs = self.inspect.thumbs.take();
        let r = self.executor.execute_linear_with_thumbs(
            source, &clip.node_graph, width, height, clip_time, frame, self.inspect.scope_tap, matte_view, thumbs.as_ref(),
        );
        self.inspect.thumbs = thumbs;
        r.map(|(out, tapped, thumbs)| {
            self.found.scope_tap = tapped;
            self.found.thumbs = thumbs;
            out
        })
    }

    /// OFX plugins take 8-bit sRGB; the round trip is scoped to them.
    fn ofx(
        &mut self,
        clip: &Clip,
        pixels: Vec<f32>,
        clip_time: f64,
        width: u32,
        height: u32,
        frames: Option<klipp_plugin::FrameProviderFn>,
    ) -> Vec<f32> {
        if clip.effects.is_empty() {
            return pixels;
        }
        let t = Instant::now();
        let u8s = klipp_effects::linear_f32_rgba_to_srgb_u8(&pixels);
        let out = match frames {
            Some(frames) => self.ofx.apply_effects_chain_temporal(&clip.effects, &u8s, width, height, clip_time, frames),
            None => self.ofx.apply_effects_chain(&clip.effects, &u8s, width, height, clip_time),
        };
        let out = klipp_effects::srgb_u8_to_linear_f32_rgba(&out);
        self.found.stats.ofx_ms += ms_since(t);
        out
    }

    fn effects(&mut self, fx: &[BuiltinEffect], pixels: Vec<f32>, clip_time: f64, width: u32, height: u32) -> Vec<f32> {
        if fx.is_empty() {
            return pixels;
        }
        let t = Instant::now();
        let gpu_ok = klipp_effects::chain_supports_gpu(fx);
        let out = match self.gpu() {
            Some(gpu) if gpu_ok => gpu.apply_effects(fx, pixels, width, height),
            _ => klipp_effects::apply_effects_with_time(fx, pixels, width, height, clip_time),
        };
        self.found.stats.effects_ms += ms_since(t);
        out
    }

    /// A title with motion blur: the transform is sampled across the
    /// shutter and the samples averaged, in linear light. The transform is
    /// applied once — as these samples — and not again by the effects.
    #[allow(clippy::too_many_arguments)]
    fn motion_blurred(
        &mut self,
        clip: &Clip,
        pixels: Vec<f32>,
        fx: &[BuiltinEffect],
        mb: klipp_timeline::MotionBlur,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        let clip_time = eval_clip_time(clip, time);
        let others: Vec<BuiltinEffect> =
            fx.iter().filter(|e| !matches!(e, BuiltinEffect::Transform { .. })).cloned().collect();
        let n = mb.samples.max(2) as usize;
        let half_shutter = (mb.evaluated_shutter_angle(clip_time) as f64 / 360.0) / fps.max(1.0);
        let mut acc = vec![0.0f32; pixels.len()];
        for si in 0..n {
            let offset = (si as f64 / (n - 1) as f64 - 0.5) * 2.0 * half_shutter;
            let t = clip_time + offset;
            let ctx = klipp_core::EvalContext { clip_time_secs: t.max(0.0), ..eval::eval_context(clip, time, fps) };
            let (px, py, sx, sy, ro, ax, ay) = eval::evaluate_clip_own_transform(clip, t.max(0.0), &ctx);
            let xf = transform_effect(fx, (px, py, sx, sy, ro, ax, ay));
            let sample = self.effects(&[xf], pixels.clone(), t.max(0.0), width, height);
            for (a, s) in acc.iter_mut().zip(&sample) {
                *a += *s;
            }
        }
        let inv = 1.0 / n as f32;
        acc.iter_mut().for_each(|v| *v *= inv);
        self.effects(&others, acc, clip_time, width, height)
    }

    /// Both clips of a transition as full layers at `time`, blended.
    #[allow(clippy::too_many_arguments)]
    fn render_transition(
        &mut self,
        a: &Clip,
        b: &Clip,
        kind: klipp_core::TransitionKind,
        progress: f32,
        params: &klipp_core::TransitionUserParams,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        transforms: &HashMap<ClipId, ResolvedTransform>,
    ) -> Option<Vec<f32>> {
        let n = (width as usize) * (height as usize) * 4;
        let side = |this: &mut Self, c: &Clip, src: &mut dyn FrameSource| {
            this.render_clip(c, time, fps, width, height, src, transforms)
                .map(|(mut p, opacity)| {
                    // Opacity is the clip's alpha within the transition.
                    p.chunks_exact_mut(4).for_each(|px| px[3] *= opacity);
                    p
                })
                .unwrap_or_else(|| vec![0.0; n])
        };
        let from = side(self, a, src);
        let to = side(self, b, src);
        let kind_id = klipp_core::spec_for_kind(kind).kind_id as u32;
        let eased = if matches!(kind, klipp_core::TransitionKind::CrossDissolve) {
            klipp_transitions::apply_easing(progress, params.param0)
        } else {
            progress
        };
        Some(match self.gpu() {
            Some(gpu) => gpu.render_transition_linear(
                &from, &to, kind_id, eased, width, height,
                params.param0, params.param1, params.param2, params.param3, params.color,
            ),
            // The same shader, transcribed: the same picture without a GPU.
            None => klipp_effects::render_transition_linear(
                &from, &to, kind_id, eased, width, height,
                params.param0, params.param1, params.param2, params.param3, params.color,
            ),
        })
    }

    /// An adjustment layer: its graph and effects applied to everything
    /// below it, mixed by its opacity.
    #[allow(clippy::too_many_arguments)]
    fn apply_adjustment(
        &mut self,
        clip: &Clip,
        below: Vec<f32>,
        opacity: f32,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        transforms: &HashMap<ClipId, ResolvedTransform>,
    ) -> Vec<f32> {
        if opacity <= 0.001 {
            return below;
        }
        let clip_time = eval_clip_time(clip, time);
        let before = if opacity < 0.999 { Some(below.clone()) } else { None };
        let graded = self.grade(clip, GradeInput::Cpu(Arc::new(below)), clip_time, fps, width, height);
        let graded = self.ofx(clip, graded, clip_time, width, height, None);
        let (fx, _) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied());
        let mut after = self.effects(&fx, graded, clip_time, width, height);
        if let Some(before) = before {
            for (a, b) in after.iter_mut().zip(&before) {
                *a = b + (*a - b) * opacity;
            }
        }
        after
    }
}

/// Video, text and subtitle tracks that are shown.
fn is_visual(t: &Track) -> bool {
    matches!(t.kind, TrackKind::Video | TrackKind::Text | TrackKind::Subtitle) && !t.muted && t.visible
}

/// The clip, or the transition, a track shows at `time`.
fn active_item(track: &Track, time: Timecode) -> Option<TrackItem<'_>> {
    if let Some(item) = track.transitions.iter().find_map(|tr| transition_at(track, tr, time)) {
        return Some(item);
    }
    let clip = track.clip_at_timecode(time).map(|(_, c)| c).filter(|c| c.enabled)?;
    Some(TrackItem::Clip(multicam_angle(track, clip, time)))
}

/// `tr`, when it is running at `time` between two enabled clips.
fn transition_at<'a>(track: &'a Track, tr: &klipp_timeline::Transition, time: Timecode) -> Option<TrackItem<'a>> {
    let (a, b) = (track.find_clip(tr.clip_a_id)?, track.find_clip(tr.clip_b_id)?);
    if !a.enabled || !b.enabled {
        return None;
    }
    let cut = b.timeline_in;
    let (start, end) = match tr.alignment {
        klipp_core::TransitionAlignment::Center => {
            let half = klipp_core::RationalTime { value: tr.duration.value / 2, rate: tr.duration.rate };
            (cut - half, cut + half)
        }
        klipp_core::TransitionAlignment::StartAtCut => (cut, cut + tr.duration),
        klipp_core::TransitionAlignment::EndAtCut => (cut - tr.duration, cut),
    };
    if time < start || time >= end {
        return None;
    }
    let total = tr.duration.to_secs_f64();
    let progress = if total > 0.0 { ((time - start).to_secs_f64() / total).clamp(0.0, 1.0) as f32 } else { 0.0 };
    Some(TrackItem::Transition { a, b, kind: tr.kind, progress, params: tr.user_params })
}

/// The clip a multicam group shows at `time`: the angle chosen by the
/// primary (angle 0) clip's cut list — angle 0 until its first cut. Angle
/// clips span the primary's range, so the chosen clip's own timing maps
/// the frame. Clips outside a multicam group are returned as they are.
fn multicam_angle<'a>(track: &'a Track, clip: &'a Clip, time: Timecode) -> &'a Clip {
    let Some(group) = clip.multicam_group else { return clip };
    let in_group = |c: &&Clip| c.multicam_group == Some(group) && c.enabled;
    let Some(primary) = track
        .clips
        .iter()
        .filter(in_group)
        .find(|c| c.multicam_angle == 0 && time >= c.timeline_in && time < c.timeline_out)
    else {
        return clip;
    };
    let rel = (time - primary.timeline_in).to_secs_f64();
    let angle = primary
        .multicam_cuts
        .iter()
        .filter(|(at, _)| *at <= rel + 1e-9)
        .last()
        .map(|(_, a)| *a)
        .unwrap_or(0);
    track.clips.iter().filter(in_group).find(|c| c.multicam_angle == angle).unwrap_or(primary)
}

/// Seconds since the clip's start on the timeline; negative before it
/// (a transition's incoming side), clamped to zero for evaluation.
fn eval_clip_time(clip: &Clip, time: Timecode) -> f64 {
    (time - clip.timeline_in).to_secs_f64().max(0.0)
}

/// Where a clip's node graph is evaluated at `time`: clip-relative
/// seconds, and the frame number there. The viewer's GPU fast path grades
/// at exactly this too.
pub fn grade_time(clip: &Clip, time: Timecode, fps: f64) -> (f64, u64) {
    let t = eval_clip_time(clip, time);
    (t, grade_frame(t, fps))
}

fn grade_frame(clip_time: f64, fps: f64) -> u64 {
    (clip_time * fps).max(0.0) as u64
}

/// Source time for `time`, through the clip's speed curve. Outside the
/// clip (a transition running into a handle) it extends at the edge speed.
pub fn source_secs(clip: &Clip, time: Timecode, fps: f64) -> f64 {
    let elapsed = (time - clip.timeline_in).to_secs_f64();
    let dur = (clip.timeline_out - clip.timeline_in).to_secs_f64();
    let offset = if elapsed < 0.0 {
        elapsed * clip.speed_at(0.0)
    } else if elapsed > dur {
        clip.speed_integral(dur, fps) + (elapsed - dur) * clip.speed_at(dur)
    } else {
        clip.speed_integral(elapsed, fps)
    };
    clip.source_in.to_secs_f64() + offset
}

/// A clip's built-in effects evaluated at `time`, with its opacity split
/// out. Keyframes and modifier stacks applied, the transform replaced by
/// the resolved one (own keyframes, link, focal point), disabled and
/// identity effects dropped, `Opacity` removed — it is a compositing
/// weight, returned separately — and `SpeedRamp` removed, which acts on
/// time, not pixels.
pub fn clip_effects(
    clip: &Clip,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
) -> (Vec<BuiltinEffect>, f32) {
    let t = eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let enabled = |i: usize| clip.builtin_effects_enabled.get(i).copied().unwrap_or(true);
    let mut fx: Vec<(usize, BuiltinEffect)> =
        clip.builtin_effects.iter().cloned().enumerate().filter(|(i, _)| enabled(*i)).collect();

    let mut opacity = 1.0f32;
    let mut have_opacity = false;
    for (i, effect) in fx.iter_mut() {
        let param = |name: &str, v: f32| {
            let kfs = clip.effect_keyframes.get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
            eval::eval_effect_with_modifiers(kfs, &clip.param_modifiers, *i, name, t, v, &ctx)
        };
        match effect {
            BuiltinEffect::GaussianBlur { radius } => *radius = param("radius", *radius),
            BuiltinEffect::Sharpen { amount } => *amount = param("amount", *amount),
            BuiltinEffect::Crop { left, right, top, bottom, feather } => {
                *left = param("left", *left);
                *right = param("right", *right);
                *top = param("top", *top);
                *bottom = param("bottom", *bottom);
                *feather = param("feather", *feather);
            }
            BuiltinEffect::ChromaKey { tolerance, soft_edge, spill_strength, .. } => {
                *tolerance = param("tolerance", *tolerance);
                // The "spill" key has always addressed the soft edge.
                *soft_edge = param("spill", *soft_edge);
                *spill_strength = param("spill_strength", *spill_strength);
            }
            BuiltinEffect::Opacity { value } => {
                // Clip opacity keyframes / modifiers first, then the
                // effect's own parameter keyframes — the viewer's order.
                let base = eval::eval_with_modifiers(
                    &clip.opacity_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Opacity, t, *value, &ctx,
                );
                opacity = param("value", base);
                have_opacity = true;
            }
            BuiltinEffect::ColorFade { duration_secs, mode, progress, .. } => {
                let dur = (*duration_secs as f64).max(0.0);
                let clip_dur = (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()).max(0.0);
                *progress = if dur <= 0.0 {
                    1.0
                } else {
                    match *mode {
                        klipp_effects::ColorFadeMode::In => (t / dur).clamp(0.0, 1.0) as f32,
                        klipp_effects::ColorFadeMode::Out => ((clip_dur - t).max(0.0) / dur).clamp(0.0, 1.0) as f32,
                    }
                };
            }
            _ => {}
        }
    }
    if !have_opacity {
        opacity = eval::eval_with_modifiers(
            &clip.opacity_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Opacity, t, 1.0, &ctx,
        );
    }
    // Opacity is a compositing weight and SpeedRamp a matter of time and
    // frame blending; neither is a pixel effect. (SpeedRamp through the
    // CPU chain was an 8-bit round trip that changed nothing else.)
    let mut fx: Vec<BuiltinEffect> = fx
        .into_iter()
        .map(|(_, e)| e)
        .filter(|e| !matches!(e, BuiltinEffect::Opacity { .. } | BuiltinEffect::SpeedRamp { .. }))
        .collect();

    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, &ctx));
    let has_transform = fx.iter().any(|e| matches!(e, BuiltinEffect::Transform { .. }));
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5);
    if has_transform {
        for e in fx.iter_mut() {
            if matches!(e, BuiltinEffect::Transform { .. }) {
                *e = transform_effect(std::slice::from_ref(e), xf);
            }
        }
    } else if !identity {
        fx.push(transform_effect(&[], xf));
    }
    fx.retain(|e| !klipp_effects::is_identity_effect(e));
    (fx, opacity.clamp(0.0, 1.0))
}

/// A `Transform` effect with the resolved values, keeping any 3D rotation
/// and perspective from the clip's own `Transform`.
fn transform_effect(fx: &[BuiltinEffect], xf: ResolvedTransform) -> BuiltinEffect {
    let (rx, ry, persp) = fx
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Transform { rotation_x, rotation_y, perspective, .. } => Some((*rotation_x, *rotation_y, *perspective)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 0.0));
    let (px, py, sx, sy, ro, ax, ay) = xf;
    BuiltinEffect::Transform {
        position_x: px, position_y: py, scale_x: sx, scale_y: sy, rotation: ro,
        anchor_x: ax, anchor_y: ay, rotation_x: rx, rotation_y: ry, perspective: persp,
        focal_ref: None,
    }
}

/// A title's motion blur, when on and not overridden by a transform link
/// (a linked target would need resolving per sample).
fn title_motion_blur(clip: &Clip) -> Option<klipp_timeline::MotionBlur> {
    let td = clip.title_data()?;
    if clip.transform_link.is_some() {
        return None;
    }
    td.motion_blur().filter(|mb| mb.samples >= 2)
}

/// Mask `consumer`'s alpha with a rendered matte source. A source that is
/// not on screen this frame makes the consumer invisible — predictable,
/// rather than an unmasked flash while the timeline is rearranged.
pub fn apply_matte(
    consumer: &mut [f32],
    rendered: &HashMap<ClipId, Vec<f32>>,
    binding: &klipp_timeline::MatteBinding,
    width: u32,
    height: u32,
) {
    use klipp_effects::track_matte as tm;
    let Some(source) = rendered.get(&binding.source_clip_id).filter(|s| s.len() == consumer.len()) else {
        consumer.chunks_exact_mut(4).for_each(|px| px[3] = 0.0);
        return;
    };
    let channel = matte_channel(binding.channel);
    if binding.choke_px < 0.5 && binding.feather_px < 0.25 {
        tm::apply_matte_fused_linear(consumer, source, channel, binding.invert, true);
        return;
    }
    let mut mask = tm::extract_mask_linear(source, channel, binding.invert, true);
    tm::apply_choke_mask(&mut mask, width, height, binding.choke_px);
    tm::feather_mask(&mut mask, width, height, binding.feather_px);
    tm::apply_mask_to_alpha_linear(consumer, &mask);
}

pub fn matte_channel(ch: klipp_nodes::params::MatteChannel) -> klipp_effects::track_matte::MatteChannel {
    use klipp_effects::track_matte::MatteChannel as E;
    use klipp_nodes::params::MatteChannel as N;
    match ch {
        N::Alpha => E::Alpha,
        N::Luminance => E::Luminance,
        N::Red => E::Red,
        N::Green => E::Green,
        N::Blue => E::Blue,
    }
}

/// Resample to the output size when a source arrives at another size
/// (straight alpha): an area average when shrinking by more than half, so
/// a 4K overlay over a 1080p frame does not alias, bilinear otherwise.
fn fit(p: Arc<Vec<f32>>, w: u32, h: u32, ow: u32, oh: u32) -> Arc<Vec<f32>> {
    use rayon::prelude::*;
    if (w, h) == (ow, oh) || w == 0 || h == 0 || ow == 0 || oh == 0 {
        return p;
    }
    let (w, h, ow_u) = (w as usize, h as usize, ow as usize);
    let (sx, sy) = (w as f32 / ow as f32, h as f32 / oh as f32);
    let mut out = vec![0.0f32; ow_u * (oh as usize) * 4];
    out.par_chunks_exact_mut(ow_u * 4).enumerate().for_each(|(y, row)| {
        if sx >= 2.0 && sy >= 2.0 {
            let y0 = ((y as f32 * sy) as usize).min(h - 1);
            let y1 = (((y + 1) as f32 * sy) as usize).clamp(y0 + 1, h);
            for x in 0..ow_u {
                let x0 = ((x as f32 * sx) as usize).min(w - 1);
                let x1 = (((x + 1) as f32 * sx) as usize).clamp(x0 + 1, w);
                let mut acc = [0.0f32; 4];
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let i = (yy * w + xx) * 4;
                        for c in 0..4 {
                            acc[c] += p[i + c];
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as f32;
                for c in 0..4 {
                    row[x * 4 + c] = acc[c] / n;
                }
            }
            return;
        }
        let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..ow_u {
            let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..4 {
                let at = |xx: usize, yy: usize| p[(yy * w + xx) * 4 + c];
                let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
                let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
                row[x * 4 + c] = top + (bot - top) * ty;
            }
        }
    });
    Arc::new(out)
}

/// A source frame as linear RGBA at the output size: scene-linear for
/// scene-referred sources, whose transfer comes back alongside.
/// Take the light above SDR white out of an HDR picture: clamp its
/// colour channels to 1 and return what was over, for adding back after a
/// step (the LUT) that only knows the SDR range. `None` when nothing is.
fn split_above_white(px: &mut [f32]) -> Option<Vec<f32>> {
    use rayon::prelude::*;
    let is_colour_over = |(i, v): (usize, &f32)| i % 4 != 3 && *v > 1.0;
    if !px.par_iter().enumerate().any(is_colour_over) {
        return None;
    }
    let mut over = vec![0.0f32; px.len()];
    px.par_chunks_exact_mut(4).zip(over.par_chunks_exact_mut(4)).for_each(|(p, o)| {
        for c in 0..3 {
            if p[c] > 1.0 {
                o[c] = p[c] - 1.0;
                p[c] = 1.0;
            }
        }
    });
    Some(over)
}

fn to_linear(f: SourceFrame, width: u32, height: u32) -> (Arc<Vec<f32>>, Option<klipp_color::hdr::HdrTransfer>) {
    match f.pixels {
        SourcePixels::Linear(p) => (fit(p, f.width, f.height, width, height), None),
        SourcePixels::SceneReferred { codes, transfer, source_to_working } => {
            let p = klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(&codes, transfer, source_to_working);
            (fit(Arc::new(p), f.width, f.height, width, height), Some(transfer))
        }
        SourcePixels::Gpu(g) => (fit(g.read(), f.width, f.height, width, height), None),
    }
}

/// Whether `clip` frame-blends at `time`: a `SpeedRamp` in blend mode 1,
/// not frozen, with the clip running below real time.
fn frame_blends(clip: &Clip, time: Timecode) -> bool {
    let enabled = |i: usize| clip.builtin_effects_enabled.get(i).copied().unwrap_or(true);
    let blend = clip.builtin_effects.iter().enumerate().any(|(i, e)| {
        enabled(i) && matches!(e, BuiltinEffect::SpeedRamp { blend_mode: 1, freeze: false, .. })
    });
    blend && clip.speed_at(eval_clip_time(clip, time)).abs() < 1.0 - 1e-6
}

/// Mix `current` (the frame at `pts`) with `next`, by where `at` falls
/// between their times. Mismatched frames — another transfer, no time —
/// leave `current` as it is.
fn blend_frames(
    current: Arc<Vec<f32>>,
    hdr: Option<klipp_color::hdr::HdrTransfer>,
    pts: f64,
    next: SourceFrame,
    at: f64,
    width: u32,
    height: u32,
) -> Arc<Vec<f32>> {
    let Some(next_pts) = next.pts.filter(|p| *p > pts + 1e-6) else { return current };
    let weight = (((at - pts) / (next_pts - pts)).clamp(0.0, 1.0)) as f32;
    if weight <= 1e-4 {
        return current;
    }
    let (next, next_hdr) = to_linear(next, width, height);
    if next_hdr != hdr || next.len() != current.len() {
        return current;
    }
    let mut out = unshare(current);
    for (o, n) in out.iter_mut().zip(next.iter()) {
        *o += (n - *o) * weight;
    }
    Arc::new(out)
}

/// OFX plugins known to read frames at other times than the current one.
fn is_temporal_plugin(name: &str) -> bool {
    ["Retime", "SlitScan", "timeOffset", "TimeOffset", "FrameBlend", "TimeBlur"]
        .iter()
        .any(|k| name.contains(k))
}

/// A track matte's mask, for the inspector's preview.
fn matte_mask(
    rendered: &HashMap<ClipId, Vec<f32>>,
    binding: &klipp_timeline::MatteBinding,
    width: u32,
    height: u32,
) -> Option<Vec<f32>> {
    use klipp_effects::track_matte as tm;
    let source = rendered.get(&binding.source_clip_id)?;
    let mut mask = tm::extract_mask_linear(source, matte_channel(binding.channel), binding.invert, true);
    tm::apply_choke_mask(&mut mask, width, height, binding.choke_px);
    tm::feather_mask(&mut mask, width, height, binding.feather_px);
    Some(mask)
}

/// Owned pixels, copied only if someone else still holds them.
fn unshare(p: Arc<Vec<f32>>) -> Vec<f32> {
    Arc::try_unwrap(p).unwrap_or_else(|p| (*p).clone())
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
