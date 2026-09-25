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
/// Frames of a clip's media at other clip times, in linear RGBA f32 in
/// the working space.
pub type LinearFrameProviderFn =
    Box<dyn FnMut(f64) -> Option<(Vec<f32>, u32, u32)> + Send>;

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

    /// Frames of `clip`'s media at other clip times, rendered the way
    /// [`Self::frame`] renders the current one: linear RGBA f32 in the
    /// working space.
    ///
    /// Distinct from [`Self::temporal_frames`], which gives an OFX
    /// plugin 8-bit sRGB because that is what the API takes. Anything
    /// that compares a neighbour with the frame beside it needs the
    /// same rendering of both — a scene-referred source goes to scene
    /// linear here and to display sRGB there, and averaging those
    /// compares two pictures rather than two samples.
    fn temporal_frames_linear(
        &mut self,
        _clip: &Clip,
        _working: WorkingColorspace,
    ) -> Option<LinearFrameProviderFn> {
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

/// Where the GPU stack's display frame goes: `texture` an `Rgba8Unorm`
/// texture of the frame's size with a storage binding, and `bytes`
/// whether the CPU gets a copy too.
#[derive(Clone, Copy, Default)]
pub struct DisplayDelivery<'a> {
    pub texture: Option<&'a wgpu::Texture>,
    pub bytes: bool,
}

/// What the single-clip export path made of a frame.
enum Single {
    Planes((Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)),
    /// Neither path can take it.
    Refused,
    /// More than one clip's grade: the stack's frame.
    ForTheStack,
}

impl From<Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)>> for Single {
    fn from(planes: Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)>) -> Self {
        planes.map_or(Single::Refused, Single::Planes)
    }
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
    /// Index into `Sequence::tracks` — of the sequence the clips live in,
    /// which for a precomposition's inner layers is the scene, not the
    /// sequence asked about.
    pub track: usize,
    /// The clip shown, or a transition's outgoing and incoming clips.
    pub clips: Vec<&'a Clip>,
    pub transition: bool,
    /// The time the clips are shown at, in their own sequence: the time
    /// asked for, or, inside a precomposition, that time carried through
    /// the precomposition's source window.
    pub at: Timecode,
    /// Their sequence's frame rate, for reading their source time.
    pub fps: f64,
}

/// What a render at `time` stacks, bottom first: every clip whose frame
/// it will ask a [`FrameSource`] for is among these. Lets a source know
/// which media to have ready, and a caller whether a frame is a single
/// plain clip.
pub fn active_layers(seq: &Sequence, time: Timecode) -> Vec<ActiveLayer<'_>> {
    let fps = seq.fps.as_f64();
    let mut out = Vec::new();
    for (track, _, item) in stack(seq, time) {
        let clips = item.clips();
        // A precomposition in play has its scene in play: the scene's
        // own layers follow, at the scene's time, so a source knows to
        // have their media ready too. They are layers of their own
        // rather than clips of this one, so a caller asking "is this
        // frame a single plain clip" hears no.
        let mut inner = Vec::new();
        for c in &clips {
            if let Some(scene) = &c.precomp {
                inner.extend(active_layers(scene, precomposition_time(c, scene, time, fps)));
            }
        }
        out.push(ActiveLayer { track, transition: matches!(item, TrackItem::Transition { .. }), clips, at: time, fps });
        out.extend(inner);
    }
    out
}

/// The moment of `scene` a precomposition clip shows at `time`: the
/// clip's source time, as any clip's is read, at the scene's rate. Time
/// before the scene's start — a transition running into the clip's
/// handle — shows its first frame, as a media clip shows its nearest.
pub fn precomposition_time(clip: &Clip, scene: &Sequence, time: Timecode, fps: f64) -> Timecode {
    Timecode::from_secs_f64(source_secs(clip, time, fps).max(0.0), scene.fps.rate())
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
        camera: seq.camera.clone(),
        timeline_graph: seq.timeline_graph.clone(),
        drop_frame: seq.drop_frame,
        fps: seq.fps,
        resolution: seq.resolution,
        tracks,
        in_point: seq.in_point,
        out_point: seq.out_point,
        markers: Vec::new(),
        last_playhead: None,
        brief: None,
        edit_plan: None,
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
///
/// Subtitle tracks are over everything wherever they are in the list:
/// they are kept at the bottom of it, under the sound, and painted in
/// that place the picture covered every caption.
fn stack(seq: &Sequence, time: Timecode) -> Vec<(usize, &Track, TrackItem<'_>)> {
    let mut stack: Vec<_> = seq
        .tracks
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, t)| is_visual(t))
        .filter_map(|(i, t)| active_item(t, time).map(|item| (i, t, item)))
        .collect();
    // Stable: subtitle tracks keep their order among themselves.
    stack.sort_by_key(|(_, t, _)| t.kind == TrackKind::Subtitle);
    stack
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
    /// The warm-up thread's ends: titles to render ahead go out, their
    /// resting renders come back for `titles` to keep.
    warm_tx: Option<std::sync::mpsc::Sender<(klipp_timeline::TitleData, u32, u32)>>,
    warm_rx: Option<std::sync::mpsc::Receiver<(klipp_timeline::TitleData, u32, u32, klipp_title::RestingRender)>>,
    glyph_cache: HashMap<ClipId, klipp_title::GlyphCacheEntry>,
    /// Render pixels per sequence pixel for the frame being rendered, for
    /// the effect parameters measured in pixels. Set with the transforms.
    px_scale: f32,
    /// This frame's motion blur: for each clip that moves while the
    /// sequence camera's shutter is open, its transform and clip time at
    /// each sample across the shutter. Filled with the transforms.
    shutter: HashMap<ClipId, Vec<(ResolvedTransform, f64)>>,
    working: WorkingColorspace,
    /// Why the GPU stack last declined a frame, so the log says it once.
    last_gpu_stack_decline: Option<&'static str>,
    /// The clips the GPU stack had to render on the CPU, so the log says
    /// so once per clip and not per frame.
    cpu_layers_said: std::collections::HashSet<ClipId>,
    /// How many precompositions deep the render is: a scene inside a
    /// scene is rendered by the same renderer, and a scene that is its
    /// own ancestor is refused at resolution, so this only bounds a
    /// chain that is merely long.
    nesting: u8,
    /// Whether the GPU stack had begun a frame when it declined it: the
    /// frame is then rendered twice, once here and once by the caller.
    stack_work_started: bool,
    /// Whether the last `render_yuv420_on_gpu` declined only after
    /// rendering; see [`Self::gpu_export_declined_after_work`].
    gpu_export_declined_after_work: bool,
    global_lut: Option<GlobalLut>,
    /// Rolling-shutter motion per (clip, source millisecond): estimating
    /// it decodes three frames, and a paused viewer re-renders the same
    /// frame many times.
    rsc_motion: HashMap<(ClipId, i64), (klipp_effects::rolling_shutter::Homography, klipp_effects::rolling_shutter::Homography)>,
    /// Frame providers for clips with temporal noise reduction on.
    ///
    /// Kept across frames: building one opens a decoder, and a
    /// denoised clip asks for two neighbours on every rendered frame.
    /// The linear one, not the OFX one — a neighbour has to be the
    /// same rendering as the frame it is averaged with.
    tnr_frames: HashMap<ClipId, LinearFrameProviderFn>,
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
            warm_tx: None,
            warm_rx: None,
            glyph_cache: HashMap::new(),
            shutter: HashMap::new(),
            px_scale: 1.0,
            working: WorkingColorspace::default(),
            last_gpu_stack_decline: None,
            cpu_layers_said: std::collections::HashSet::new(),
            nesting: 0,
            stack_work_started: false,
            gpu_export_declined_after_work: false,
            global_lut: None,
            rsc_motion: HashMap::new(),
            tnr_frames: HashMap::new(),
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

    /// A title clip's layer at `time` as straight-alpha sRGB bytes at
    /// the frame size, for laying over a presented picture on the
    /// viewer's fast path. `None` for any other clip kind.
    /// Have the resting renders of these titles, at these frame sizes,
    /// made on the warm-up thread; `adopt_warm_titles` takes them in.
    /// A title's first frame then places a sprite that is already
    /// there, instead of rastering 4K of text on the frame's thread.
    pub fn warm_titles(&mut self, wants: Vec<(klipp_timeline::TitleData, u32, u32)>) {
        if wants.is_empty() {
            return;
        }
        if self.warm_tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<(klipp_timeline::TitleData, u32, u32)>();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("klipp-title-warm".into())
                .spawn(move || {
                    let mut titles = klipp_title::TitleRenderer::new();
                    while let Ok((td, w, h)) = rx.recv() {
                        let t = Instant::now();
                        let render = titles.render_resting(&td, w, h);
                        tracing::debug!("title warmed ahead at {w}x{h} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
                        if done_tx.send((td, w, h, render)).is_err() {
                            break;
                        }
                    }
                })
                .is_ok();
            if spawned {
                self.warm_tx = Some(tx);
                self.warm_rx = Some(done_rx);
            }
        }
        if let Some(tx) = &self.warm_tx {
            for want in wants {
                let _ = tx.send(want);
            }
        }
    }

    /// Take in the resting renders the warm-up thread has finished.
    pub fn adopt_warm_titles(&mut self) -> usize {
        let mut n = 0;
        if let Some(rx) = &self.warm_rx {
            while let Ok((td, w, h, render)) = rx.try_recv() {
                self.titles.adopt(&td, w, h, render);
                n += 1;
            }
        }
        n
    }

    pub fn render_title_layer_u8(&mut self, clip: &Clip, time: Timecode, width: u32, height: u32) -> Option<Vec<u8>> {
        let ClipKind::Title(td) = &clip.kind else { return None };
        let clip_time = eval_clip_time(clip, time);
        let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
        let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
        let total = (clip.timeline_out - clip.timeline_in).to_secs_f64() as f32;
        Some(self.titles.render_with_window(td, width, height, gpu, clip_time as f32, Some(total)))
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

    /// The render as 8-bit 4:2:0 planes — Y, then Cb, then Cr, tightly
    /// packed — produced on the GPU, when the whole render is one clip's
    /// grade and nothing else.
    ///
    /// This exists so a render does not have to bring a linear frame back
    /// across the bus only for the CPU to convert it. At 4K that trade is
    /// 132 MB and a per-pixel pass against 12.4 MB and no pass at all.
    ///
    /// `None` — having rendered nothing, so the caller can render normally
    /// without having wasted the frame — whenever the grade is *not* the
    /// whole render. The list is long and deliberately conservative,
    /// because being wrong here means a wrong picture rather than a slow
    /// one: anything composited, faded, transformed, masked, blended,
    /// keyed, plugin-processed, blended between frames, HDR, inspected, or
    /// synthetic goes the ordinary way. `export_gpu_path_agrees_with_the_renderer`
    /// holds this against `render` over each of those shapes.
    ///
    /// The conditions that are properties of the clip's own effect stack
    /// are asked of `clip_effects` itself rather than re-derived here, so
    /// there is no second opinion about what a transform or an opacity
    /// keyframe amounts to.
    ///
    /// A clip with an **identity grade** takes this path too, without
    /// running a graph: the source frame is the render, so it is seeded
    /// into the chain and converted from there. It has to be a special
    /// case because `grade`'s own identity short-circuit returns
    /// `input.pixels()`, which for a GPU frame means downloading it —
    /// the very thing this path exists to avoid.
    #[allow(clippy::too_many_arguments)]
    pub fn render_yuv420_on_gpu(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        target: klipp_effects::gpu::Yuv420Target,
    ) -> Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)> {
        self.gpu_export_declined_after_work = false;
        match self.render_yuv420_single(seq, time, width, height, src, opts, target) {
            Single::Planes(planes) => Some(planes),
            Single::Refused => None,
            Single::ForTheStack => self.render_yuv420_from_stack(seq, time, width, height, src, opts, target),
        }
    }

    /// Whether the last [`Self::render_yuv420_on_gpu`] declined only
    /// after rendering — a grade that came back on the CPU, a stack that
    /// failed part-way — so the caller rendered the frame twice. A
    /// caller in a loop stops asking after that; a decline before any
    /// work costs nothing and says nothing about the next frame.
    pub fn gpu_export_declined_after_work(&self) -> bool {
        self.gpu_export_declined_after_work
    }

    /// The export's frame from the GPU stack: the composite of
    /// [`Self::composite_on_gpu`] encoded to the delivery's planes where
    /// it lies. An SDR delivery only — the stack composites SDR linear,
    /// and each layer has had its own display pass; the delivery's
    /// transform is the encode's.
    #[allow(clippy::too_many_arguments)]
    fn render_yuv420_from_stack(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        target: klipp_effects::gpu::Yuv420Target,
    ) -> Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)> {
        if opts.hdr_peak_nits.is_some() || target.hdr_op != 0 {
            return None;
        }
        let composited = self.composite_on_gpu(seq, time, width, height, src, opts).is_some();
        if let Some(gpu) = self.executor.gpu_pipeline_mut() {
            gpu.stack_end();
        }
        if !composited {
            return None;
        }
        self.executor.set_output_in_chain_slot_a();
        let target = klipp_effects::gpu::Yuv420Target { scene: None, ..target };
        let planes = self.executor.encode_output_to_yuv420(width, height, target);
        if planes.is_none() {
            self.gpu_export_declined_after_work = true;
        }
        planes
    }

    /// [`Self::render_yuv420_on_gpu`] for the render that is one clip's
    /// grade, which needs no stack at all: `ForTheStack` sends anything
    /// else to the stack, `Refused` is a frame neither can take.
    #[allow(clippy::too_many_arguments)]
    fn render_yuv420_single(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        target: klipp_effects::gpu::Yuv420Target,
    ) -> Single {
        // The project-wide colour pipeline runs after the composite. A
        // single-clip render has no composite, so it can run as one more
        // chain pass — but only where its *position* is unambiguous, and
        // the two cases below are where it is not:
        //
        //  * An HDR delivery splits the light above SDR white out before
        //    the LUT and adds it back after, because a LUT only knows the
        //    0..1 range. That split has no chain pass.
        //  * A scene-referred clip ends in a display pass of its own, and
        //    the LUT goes *after* it. The fused writer does that pass
        //    inside `display_encode`, which is last, so a chain LUT would
        //    land on the wrong side of it.
        //
        // Either way the answer is to decline, not to apply it anyway:
        // before this gate existed at all the GPU path exported the
        // timeline *without* its global LUT, silently.
        let global_lut_here = self.global_lut.is_some();
        if global_lut_here && opts.hdr_peak_nits.is_some() {
            return Single::Refused;
        }
        // A timeline grade runs on the composite, which a single-clip
        // render does not have; the stack takes the frame instead.
        if self.timeline_grade(seq).is_some() {
            return Single::ForTheStack;
        }
        // The render and the encode have to be for the same delivery: the
        // target says whether the encoder wants HDR, `opts` says whether
        // the render is producing display light for one. Both come from
        // the same `ExportSettings`, so disagreement is a wiring mistake
        // rather than a user's choice — and it would show as a wrong
        // picture, not an error, so it is checked rather than assumed.
        let hdr_delivery = opts.hdr_peak_nits.is_some();
        if (target.hdr_op != 0) != hdr_delivery {
            return Single::Refused;
        }
        // Set below when the source is scene-referred, which is the one
        // case with a step of its own after the grade.
        let mut scene_end: Option<klipp_effects::gpu::SceneReferredEnd> = None;
        // The peak the display-light pass rolls a camera log's highlights
        // off against; the target carries it for a PQ delivery already.
        let peak = opts.hdr_peak_nits.unwrap_or(target.peak_nits);
        let fps = seq.fps.as_f64();
        let stack = stack(seq, time);
        let (track, clip) = match stack.as_slice() {
            [(_, track, TrackItem::Clip(c))] => (*track, *c),
            // Nothing, a transition, or more than one layer.
            _ => return Single::ForTheStack,
        };
        if (track.opacity - 1.0).abs() > 1e-6 {
            return Single::ForTheStack;
        }
        if !matches!(clip.kind, ClipKind::Media { .. }) {
            return Single::ForTheStack;
        }
        if !matches!(clip.blend_mode, klipp_core::BlendMode::Normal) {
            return Single::ForTheStack;
        }
        if clip.consumed_as_matte || clip.matte_source.is_some() || clip.matte_stencil.is_some() {
            return Single::ForTheStack;
        }
        // OFX plugins run on the CPU frame, and a temporal one wants
        // neighbouring frames too.
        if !clip.effects.is_empty() {
            return Single::ForTheStack;
        }
        // Two frames blended for a speed ramp is a CPU operation on the
        // source, before the grade even starts.
        if frame_blends(clip, time) {
            return Single::ForTheStack;
        }
        if title_motion_blur(clip).is_some() || may_blur(seq, clip) {
            return Single::ForTheStack;
        }
        // Anything the grade panel asks for is read off the linear frame
        // or a node's output, neither of which this path produces.
        let i = &self.inspect;
        if i.clip.is_some()
            || i.ungraded
            || i.thumbs.is_some()
            || i.scope_tap.is_some()
            || i.matte_view.is_some()
            || i.solo.is_some()
            || i.matte_of.is_some()
        {
            return Single::Refused;
        }
        // And the clip's own stack, answered by the code that builds it:
        // an empty effect list at full opacity means the grade's output is
        // the clip's output. The transform is in there too — a pan, a
        // zoom or a rotation arrives as an effect.
        let mut transforms: HashMap<ClipId, ResolvedTransform> = HashMap::new();
        let resolved = eval::resolve_clip_transform(
            clip, seq, time, fps, &mut transforms, &mut self.glyph_cache, &mut self.titles,
            seq.resolution.width, seq.resolution.height,
        );
        let resolved = to_render_pixels(resolved, render_scale(seq, width, height));
        self.px_scale = render_scale(seq, width, height).1;
        let resolved = match eval::camera_at(seq, time).and_then(|cam| eval::camera_at_depth(cam, clip.depth)) {
            Some(cam) => eval::apply_camera(resolved, cam, width, height),
            None => resolved,
        };
        let (fx, opacity) = clip_effects(clip, time, fps, Some(resolved));
        if !fx.is_empty() || (opacity - 1.0).abs() > 1e-6 {
            return Single::ForTheStack;
        }

        // Everything checked. Run the clip's grade with its result left in
        // the chain, and convert it there.
        let clip_time = eval_clip_time(clip, time);
        let at = source_secs(clip, time, fps);
        let Some(f) = src.frame(clip, at, width, height) else { return Single::Refused };
        if (f.width, f.height) != (width, height) {
            return Single::Refused;
        }
        let input = match f.pixels {
            SourcePixels::Gpu(g) => GradeInput::Gpu(g),
            SourcePixels::SceneReferred { codes, transfer, source_to_working }
                if codes.len() == (width as usize) * (height as usize) =>
            {
                // A scene-referred clip ends in its own display pass —
                // `display_render`: the SDR tone map, or an HDR delivery's
                // display light. The Y'CbCr writer runs that pass itself
                // now, off the same `HdrTransfer` numbers, so the frame
                // never has to come down for it. This is what a *real* HDR
                // export is — PQ, HLG or log footage — and it used to be
                // refused here for want of that pass.
                // `grade` seeds the chain from the codes itself, the
                // same way the viewer's path does.
                // The LUT would have to follow this clip's display pass,
                // and the fused writer runs that last.
                if global_lut_here {
                    return Single::Refused;
                }
                scene_end = Some(klipp_effects::gpu::SceneReferredEnd {
                    display_light: hdr_delivery,
                    kind: transfer.display_light_kind(),
                    scale: transfer.scene_scale(),
                    exposure: transfer.display_exposure(),
                });
                GradeInput::Scene { codes, transfer, source_to_working }
            }
            SourcePixels::Linear(pixels) => GradeInput::Cpu(pixels),
            _ => return Single::ForTheStack,
        };

        // An identity grade has nothing to run: the source frame is the
        // render. Seed the chain with it and convert that, rather than
        // going through `grade`, whose identity short-circuit would pull
        // the frame off the GPU (`input.pixels()`) only to hand it back.
        // The executor needs the project's basis whichever branch runs:
        // the display transform's blob is built from it (an ACES ODT is
        // defined on AP1 and walks a foreign basis there; an HDR
        // delivery's matrix goes to BT.2020 from it). `grade` sets it for
        // a real graph, but the pass-through below never calls `grade`,
        // and a stale value there is a wrong picture rather than an error.
        self.executor.set_working_colorspace(self.working);
        if clip.node_graph.is_identity() {
            let GradeInput::Gpu(g) = &input else { return Single::Refused };
            if !g.seed(&mut self.executor, width, height)
                || !self.executor.take_seeded_input_as_output()
            {
                return Single::Refused;
            }
            let target = klipp_effects::gpu::Yuv420Target {
                scene: scene_end,
                peak_nits: peak,
                ..target
            };
            if !self.global_lut_on_chain(width, height) {
                return Single::Refused;
            }
            return Single::from(self.executor.encode_output_to_yuv420(width, height, target));
        }

        self.executor.request_output_on_gpu();
        let pixels = self.grade(clip, input, clip_time, fps, width, height);
        // The grade kept its result on the GPU exactly when it returns
        // nothing. Coming back on the CPU instead — a node with no GPU
        // dispatch, a device that failed — means this frame gets rendered
        // twice, once here and once by the caller. That is the one case
        // where taking this path costs something, so a caller in a loop
        // should stop asking after the first time it happens rather than
        // pay it on every frame.
        if !pixels.is_empty() {
            self.gpu_export_declined_after_work = true;
            return Single::Refused;
        }
        let target =
            klipp_effects::gpu::Yuv420Target { scene: scene_end, peak_nits: peak, ..target };
        if !self.global_lut_on_chain(width, height) {
            return Single::Refused;
        }
        Single::from(self.executor.encode_output_to_yuv420(width, height, target))
    }

    /// Run the timeline's global LUT over the chain's graded frame, if
    /// there is one. `false` if there is one and it could not run, which
    /// sends the frame down the ordinary path rather than exporting it
    /// ungraded by the look.
    /// The frame composited on the GPU and encoded to display bytes
    /// there. Every clip lands in its own layer buffer — a decoded frame
    /// seeded where it is, a title or generator as an sRGB sprite over
    /// only the rows it covers, anything else rendered on the CPU and
    /// uploaded — with its built-in effects run on it there; a matte or a
    /// stencil above it multiplies into its alpha; a transition is a pass
    /// between its two sides; an adjustment layer runs its chain on the
    /// accumulator and mixes back; each layer blends on with its opacity
    /// and mode; the accumulator is premultiplied and encoded. `None`
    /// means the CPU path must take this frame — HDR delivery, a global
    /// LUT, an OFX or Frei0r plugin, frame blending, title motion blur,
    /// an inspection request — and the log says which.
    /// `tests/gpu_composite.rs` holds it to the CPU render.
    pub fn render_display_u8_on_gpu(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
    ) -> Option<(Vec<u8>, RenderStats)> {
        let out = DisplayDelivery { texture: None, bytes: true };
        self.render_display_on_gpu(seq, time, width, height, src, opts, out)
            .map(|(bytes, stats)| (bytes.unwrap_or_default(), stats))
    }

    /// [`Self::render_display_u8_on_gpu`], delivered as `out` asks: into
    /// a texture the viewer samples, to the CPU, or both. With the
    /// texture alone the frame never leaves the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn render_display_on_gpu(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        out: DisplayDelivery<'_>,
    ) -> Option<(Option<Vec<u8>>, RenderStats)> {
        let result = self.render_display_on_gpu_inner(seq, time, width, height, src, opts, out);
        // A frame's passes record into one encoder from `stack_begin`;
        // a decline part-way must not leave them waiting for the next
        // frame.
        if let Some(gpu) = self.executor.gpu_pipeline_mut() {
            gpu.stack_end();
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn render_display_on_gpu_inner(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
        out: DisplayDelivery<'_>,
    ) -> Option<(Option<Vec<u8>>, RenderStats)> {
        let t0 = Instant::now();
        let mut stats = self.composite_on_gpu(seq, time, width, height, src, opts)?;
        let t = Instant::now();
        let gpu = self.executor.gpu_pipeline_or_init()?;
        let bytes = gpu.encode_chain_to_display(true, width, height, None, false, None, out.texture, out.bytes)?;
        stats.composite_ms += ms_since(t);
        stats.total_ms = ms_since(t0);
        Some((bytes, stats))
    }

    /// The frame composited on the GPU stack and left premultiplied in
    /// chain slot A — the timeline's LUT applied over it as the CPU stack
    /// does — for an encode to take from there: the viewer's display
    /// bytes, or the export's Y'CbCr planes. `None`, with the reason
    /// logged once, when the stack cannot take this frame.
    fn composite_on_gpu(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
    ) -> Option<RenderStats> {
        self.stack_work_started = false;
        if !self.allow_gpu || !gpu_composite_enabled() {
            return self.gpu_stack_declines("GPU off");
        }
        if opts.hdr_peak_nits.is_some() {
            return self.gpu_stack_declines("HDR delivery");
        }
        if self.working != WorkingColorspace::Rec709Linear {
            return self.gpu_stack_declines("working space is not Rec.709");
        }
        let i = &self.inspect;
        if i.ungraded || i.thumbs.is_some() || i.scope_tap.is_some() || i.matte_view.is_some() || i.solo.is_some() || i.matte_of.is_some() {
            return self.gpu_stack_declines("inspection");
        }
        let t0 = Instant::now();
        let fps = seq.fps.as_f64();
        let stack = stack(seq, time);
        if stack.is_empty() {
            return self.gpu_stack_declines("nothing on screen");
        }

        // What a clip needs that the stack does not have.
        let blocker = |c: &Clip| -> Option<&'static str> {
            if !c.effects.is_empty() {
                Some("OFX plugin")
            } else if frame_blends(c, time) {
                Some("frame blending")
            } else if title_motion_blur(c).is_some() {
                Some("title motion blur")
            } else if may_blur(seq, c) {
                Some("motion blur")
            } else {
                None
            }
        };
        // Slots: a clip takes one layer, a transition three (both sides
        // and the result).
        enum Item<'a> {
            Clip { c: &'a Clip, slot: usize },
            Transition { a: &'a Clip, b: &'a Clip, kind: klipp_core::TransitionKind, progress: f32, params: klipp_core::TransitionUserParams, slots: [usize; 3] },
        }
        let mut items: Vec<(&Track, Item)> = Vec::with_capacity(stack.len());
        let mut n_slots = 0usize;
        let mut sources: HashSet<ClipId> = HashSet::new();
        for (_, track, item) in &stack {
            match item {
                TrackItem::Clip(c) => {
                    if let Some(why) = blocker(c) {
                        return self.gpu_stack_declines(why);
                    }
                    if let Some(b) = &c.matte_source {
                        sources.insert(b.source_clip_id);
                    }
                    if c.matte_stencil.is_some() {
                        sources.insert(c.id);
                    }
                    items.push((track, Item::Clip { c, slot: n_slots }));
                    n_slots += 1;
                }
                TrackItem::Transition { a, b, kind, progress, params } => {
                    for side in [a, b] {
                        if let Some(why) = blocker(side) {
                            return self.gpu_stack_declines(why);
                        }
                        if side.consumed_as_matte || side.matte_source.is_some() || side.matte_stencil.is_some() || matches!(side.kind, ClipKind::AdjustmentLayer) {
                            return self.gpu_stack_declines("matte or adjustment inside a transition");
                        }
                    }
                    items.push((track, Item::Transition { a, b, kind: *kind, progress: *progress, params: *params, slots: [n_slots, n_slots + 1, n_slots + 2] }));
                    n_slots += 3;
                }
            }
        }
        // A matte source that is itself matted would be read after it
        // was cut: the CPU stack reads sources untouched.
        for (_, item) in &items {
            if let Item::Clip { c, .. } = item {
                let is_source = c.consumed_as_matte || sources.contains(&c.id);
                if is_source && c.matte_source.is_some() {
                    return self.gpu_stack_declines("a matte source with a matte of its own");
                }
            }
        }

        let mut transforms: HashMap<ClipId, ResolvedTransform> = HashMap::new();
        for (_, item) in &items {
            let clips: Vec<&Clip> = match item {
                Item::Clip { c, .. } => vec![c],
                Item::Transition { a, b, .. } => vec![a, b],
            };
            for c in clips {
                eval::resolve_clip_transform(
                    c, seq, time, fps, &mut transforms, &mut self.glyph_cache, &mut self.titles,
                    seq.resolution.width, seq.resolution.height,
                );
            }
        }
        in_render_pixels(seq, width, height, &mut transforms);
        self.px_scale = render_scale(seq, width, height).1;
        behind_the_camera(seq, time, width, height, &mut transforms);
        self.open_shutter(seq, time, fps, width, height, &transforms);

        self.executor.set_working_colorspace(self.working);
        let luma = klipp_color::linear::luma_coefs(self.working);
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| {
            g.set_luma_coefs(luma);
            g.stack_begin(width, height, n_slots)
        }) {
            return self.gpu_stack_declines("the GPU stack would not start");
        }
        self.stack_work_started = true;

        let mut stats = RenderStats::default();
        // ── 1. Every layer in its buffer, effects run on it there ──
        let mut placed: HashMap<usize, Placed> = HashMap::new();
        let mut slot_of_clip: HashMap<ClipId, usize> = HashMap::new();
        for (_, item) in &items {
            match item {
                Item::Clip { c, slot } => {
                    if matches!(c.kind, ClipKind::AdjustmentLayer) {
                        continue;
                    }
                    match self.place_layer(*slot, c, time, fps, width, height, src, &transforms, &mut stats) {
                        Ok(Some(p)) => {
                            placed.insert(*slot, p);
                            slot_of_clip.insert(c.id, *slot);
                        }
                        Ok(None) => {}
                        Err(why) => return self.gpu_stack_declines(why),
                    }
                }
                Item::Transition { a, b, kind, progress, params, slots } => {
                    for (side, slot) in [(a, slots[0]), (b, slots[1])] {
                        match self.place_layer(slot, side, time, fps, width, height, src, &transforms, &mut stats) {
                            Ok(Some(p)) => {
                                if p.opacity < 0.999 && !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_scale_alpha(slot, p.opacity)) {
                                    return self.gpu_stack_declines("a transition side could not be faded");
                                }
                            }
                            Ok(None) => {
                                if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_layer_clear(slot)) {
                                    return self.gpu_stack_declines("a transition side could not be cleared");
                                }
                            }
                            Err(why) => return self.gpu_stack_declines(why),
                        }
                    }
                    let kind_id = klipp_core::spec_for_kind(*kind).kind_id as u32;
                    let eased = if matches!(kind, klipp_core::TransitionKind::CrossDissolve) {
                        klipp_transitions::apply_easing(*progress, params.param0)
                    } else {
                        *progress
                    };
                    let p = [params.param0, params.param1, params.param2, params.param3];
                    let t = Instant::now();
                    if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_transition(slots[0], slots[1], slots[2], kind_id, eased, p, params.color)) {
                        return self.gpu_stack_declines("the transition pass failed");
                    }
                    stats.composite_ms += ms_since(t);
                    placed.insert(slots[2], Placed { opacity: 1.0, rows: (0, height) });
                }
            }
        }

        // ── 2. Mattes and stencils, multiplied into the consumers' alpha ──
        let t = Instant::now();
        let stencils: Vec<(usize, &Clip)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, (_, item))| match item {
                Item::Clip { c, .. } if c.matte_stencil.is_some() => Some((i, *c)),
                _ => None,
            })
            .collect();
        for (i, (_, item)) in items.iter().enumerate() {
            let Item::Clip { c, slot } = item else { continue };
            if matches!(c.kind, ClipKind::AdjustmentLayer) || c.consumed_as_matte || c.matte_stencil.is_some() || !placed.contains_key(slot) {
                continue;
            }
            for (si, st) in &stencils {
                if *si <= i {
                    continue;
                }
                let s = st.matte_stencil.as_ref().unwrap();
                let source = slot_of_clip.get(&st.id).copied();
                if !self.stack_apply_matte(*slot, source, matte_channel_index(s.channel), s.invert, s.feather_px * self.px_scale, s.choke_px * self.px_scale) {
                    return self.gpu_stack_declines("a stencil pass failed");
                }
            }
            if let Some(b) = &c.matte_source {
                let ct = eval_clip_time(c, time);
                // In the rendered buffer's pixels, like every other size.
                let feather = klipp_core::keyframe::evaluate(&c.matte_feather_keyframes, ct, b.feather_px) * self.px_scale;
                let choke = klipp_core::keyframe::evaluate(&c.matte_choke_keyframes, ct, b.choke_px) * self.px_scale;
                let source = slot_of_clip.get(&b.source_clip_id).copied();
                if !self.stack_apply_matte(*slot, source, matte_channel_index(b.channel), b.invert, feather, choke) {
                    return self.gpu_stack_declines("a matte pass failed");
                }
            }
        }
        stats.composite_ms += ms_since(t);

        // ── 3. Bottom to top onto the accumulator ──
        for (track, item) in &items {
            let t = Instant::now();
            match item {
                Item::Clip { c, .. } if matches!(c.kind, ClipKind::AdjustmentLayer) => {
                    let opacity = eval::evaluate_clip_opacity(c, time, fps) * track.opacity;
                    if opacity <= 0.001 {
                        continue;
                    }
                    if let Err(why) = self.stack_adjust(c, opacity, time, fps, width, height, &transforms, &mut stats) {
                        return self.gpu_stack_declines(why);
                    }
                }
                Item::Clip { c, slot } => {
                    if c.consumed_as_matte || c.matte_stencil.is_some() {
                        continue;
                    }
                    let Some(p) = placed.get(slot) else { continue };
                    let opacity = p.opacity * track.opacity;
                    if opacity <= 0.001 {
                        continue;
                    }
                    let (y0, y1) = p.rows;
                    let mode = c.blend_mode as u32;
                    if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_blend_rows(*slot, opacity, mode, y0, y1)) {
                        return self.gpu_stack_declines("the blend pass failed");
                    }
                }
                Item::Transition { slots, .. } => {
                    if track.opacity <= 0.001 {
                        continue;
                    }
                    if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_blend(slots[2], track.opacity, 0)) {
                        return self.gpu_stack_declines("the blend pass failed");
                    }
                }
            }
            stats.composite_ms += ms_since(t);
        }

        // ── 4. The timeline grade, as `render_stack` runs it: the
        // composite graded as an adjustment layer over everything ──
        if let Some(tg) = self.timeline_grade(seq) {
            if let Err(why) = self.stack_adjust(&tg, 1.0, time, fps, width, height, &transforms, &mut stats) {
                return self.gpu_stack_declines(why);
            }
        }

        // ── 4b. The timeline's LUT, then premultiply ──
        // The LUT goes over the composite while it is still straight
        // alpha, as `render_stack` applies it: the accumulator through
        // the chain and back.
        if self.global_lut.is_some() {
            let t = Instant::now();
            let gpu = self.executor.gpu_pipeline_or_init()?;
            if !gpu.stack_acc_to_chain() {
                return self.gpu_stack_declines("the accumulator could not seed the LUT");
            }
            self.executor.set_output_in_chain_slot_a();
            if !self.global_lut_on_chain(width, height) {
                return self.gpu_stack_declines("global LUT");
            }
            let Some(slot_a) = self.executor.output_on_gpu() else {
                return self.gpu_stack_declines("global LUT");
            };
            let gpu = self.executor.gpu_pipeline_or_init()?;
            if !gpu.stack_mix_from_chain(slot_a, 1.0) {
                return self.gpu_stack_declines("the LUT could not return to the accumulator");
            }
            stats.lut_ms += ms_since(t);
        }
        let t = Instant::now();
        let gpu = self.executor.gpu_pipeline_or_init()?;
        if !gpu.stack_finish_into_chain(opts.keep_alpha) {
            // Nothing was laid on the canvas: an empty frame.
            let acc = gpu.stack_acc_buffer()?;
            let _ = acc;
            if !gpu.stack_finish_into_chain(opts.keep_alpha) {
                return self.gpu_stack_declines("nothing blended");
            }
        }
        stats.composite_ms += ms_since(t);
        stats.total_ms = ms_since(t0);
        self.last_gpu_stack_decline = None;
        self.stack_work_started = false;
        Some(stats)
    }

    /// One clip into layer `slot`: the source seeded, uploaded or
    /// rendered, then its built-in effects run on it there. `Ok(None)`
    /// when the source has no frame (the layer is skipped, as on the
    /// CPU); `Err` names what the stack cannot do.
    #[allow(clippy::too_many_arguments)]
    fn place_layer(
        &mut self,
        slot: usize,
        c: &Clip,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        transforms: &HashMap<ClipId, ResolvedTransform>,
        stats: &mut RenderStats,
    ) -> Result<Option<Placed>, &'static str> {
        let (mut fx, opacity) = clip_effects(c, time, fps, transforms.get(&c.id).copied());
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        let temporal = fx.iter().any(|e| matches!(e, BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. }));
        let fx_on_gpu = fx.is_empty() || (klipp_effects::chain_supports_gpu(&fx) && !temporal);
        let clip_time = eval_clip_time(c, time);
        let mut rows = (0, height);
        let mut effects_done = false;
        let placed = match &c.kind {
            ClipKind::Media { .. } if fx_on_gpu => {
                let at = source_secs(c, time, fps);
                let Some(f) = src.frame(c, at, width, height) else { return Ok(None) };
                // A source of another size is fitted into the frame, as on
                // the CPU: on the GPU when the source can seed at another
                // size, else fitted here and sent up. It used to be
                // declined, which sent every frame with a 1440p overlay
                // on a 1080p timeline down the CPU path.
                let other_size = (f.width, f.height) != (width, height);
                let pixels = match f.pixels {
                    SourcePixels::Gpu(g) if other_size => {
                        SourcePixels::Gpu(Arc::new(FittedGpu { inner: g, w: f.width, h: f.height, ow: width, oh: height, fitted: std::sync::OnceLock::new() }))
                    }
                    SourcePixels::Linear(p) if other_size => SourcePixels::Linear(fit(p, f.width, f.height, width, height)),
                    SourcePixels::SceneReferred { .. } if other_size => return Err("scene-referred source of another size"),
                    other => other,
                };
                match pixels {
                    SourcePixels::Gpu(g) if c.node_graph.is_identity() => {
                        if g.seed(&mut self.executor, width, height) {
                            self.executor.take_seeded_input_as_output()
                                && self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_chain(slot, true))
                        } else if other_size {
                            let fitted = g.read();
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &fitted))
                        } else {
                            false
                        }
                    }
                    SourcePixels::Gpu(g) => {
                        let t = Instant::now();
                        self.executor.request_output_on_gpu();
                        let pixels = self.grade(c, GradeInput::Gpu(g), clip_time, fps, width, height);
                        stats.grade_ms += ms_since(t);
                        if pixels.is_empty() {
                            let slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_chain(slot, slot_a))
                        } else {
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
                        }
                    }
                    SourcePixels::Linear(p) => {
                        let t = Instant::now();
                        let pixels = if c.node_graph.is_identity() {
                            unshare(p)
                        } else {
                            self.grade(c, GradeInput::Cpu(p), clip_time, fps, width, height)
                        };
                        stats.grade_ms += ms_since(t);
                        self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
                    }
                    SourcePixels::SceneReferred { .. } => return Err("scene-referred source"),
                }
            }
            ClipKind::Title(td) if fx_on_gpu && c.node_graph.is_identity() => {
                let total = (c.timeline_out - c.timeline_in).to_secs_f64() as f32;
                let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
                let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                // A title that is its resting render moved and faded
                // keeps that render on the GPU: no raster, no upload, one
                // pass. Anything else is rendered here and goes up as rows.
                match self.titles.frame_with_window(td, width, height, gpu, clip_time as f32, Some(total)) {
                    klipp_title::TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, blur, rows: r } => {
                        rows = r;
                        if blur > 0.5 {
                            // The blur entrance: the placed sprite, blurred
                            // on the layer with the clip's other effects.
                            fx.push(BuiltinEffect::GaussianBlur { radius: blur });
                        }
                        self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                            p.stack_layer_place_sprite_posed(slot, sprite.id, &sprite.pixels, dx, dy, opacity, scale, anchor, under, r.0, r.1)
                        })
                    }
                    klipp_title::TitleFrame::Glyphs { sprite, poses, rows: r } => {
                        rows = r;
                        let packed: Vec<[f32; 16]> = poses.iter().map(|p| p.packed()).collect();
                        match &sprite.units {
                            Some(u) => self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                                p.stack_layer_place_glyphs(slot, sprite.id, &sprite.pixels, (u.x0, u.y0, u.w, u.h, &u.index), &packed, r.0, r.1)
                            }),
                            None => false,
                        }
                    }
                    klipp_title::TitleFrame::Rendered { pixels, rows: r } => {
                        rows = r;
                        self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_srgb_u8_rows(slot, &pixels, r.0, r.1))
                    }
                }
            }
            ClipKind::Generator(g) if fx_on_gpu && c.node_graph.is_identity() => {
                let total = (c.timeline_out - c.timeline_in).to_secs_f64();
                // Half floats, sRGB-encoded, decoded on the way into
                // the layer: the precision eight bits could not hold,
                // without moving sRGB's power curve onto the CPU.
                let px = crate::synth::render_generator_with_slate_f16(&mut self.titles, g, width, height, clip_time, total);
                self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_srgb_f16(slot, &px))
            }
            _ => {
                // Anything the stack has no seed for, or a CPU-only
                // effect: rendered on the CPU, effects included, up once.
                // Said once per clip, with why: this is the slow way in —
                // a full frame rendered here and uploaded as floats — and
                // a title or a plain clip landing on it is worth knowing.
                if self.cpu_layers_said.len() < 256 && self.cpu_layers_said.insert(c.id) {
                    let kind = match &c.kind {
                        ClipKind::Media { .. } => "media",
                        ClipKind::Title(_) => "title",
                        ClipKind::Generator(_) => "generator",
                        _ => "other",
                    };
                    let names: Vec<String> = fx.iter().map(|e| format!("{e:?}").split([' ', '{', '(']).next().unwrap_or("").to_string()).collect();
                    tracing::debug!(
                        "GPU stack: a {kind} layer rendered on the CPU and uploaded: effects on gpu {fx_on_gpu} {names:?}, temporal {temporal}, graph identity {}",
                        c.node_graph.is_identity(),
                    );
                }
                let Some((pixels, _)) = self.render_clip(c, time, fps, width, height, src, transforms) else { return Ok(None) };
                effects_done = true;
                self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
            }
        };
        if !placed {
            return Err("a layer could not be placed on the GPU");
        }
        if !effects_done && !fx.is_empty() {
            let t = Instant::now();
            if !self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_apply_effects(slot, &fx)) {
                return Err("an effect pass failed");
            }
            stats.effects_ms += ms_since(t);
            // A blur or a transform can reach outside the sprite's rows.
            rows = (0, height);
        }
        Ok(Some(Placed { opacity, rows }))
    }

    /// A matte from layer `source` into layer `consumer`'s alpha; a
    /// source that is not on screen leaves the consumer invisible, as
    /// on the CPU.
    fn stack_apply_matte(&mut self, consumer: usize, source: Option<usize>, channel: u32, invert: bool, feather_px: f32, choke_px: f32) -> bool {
        let Some(gpu) = self.executor.gpu_pipeline_or_init() else { return false };
        match source {
            Some(s) => gpu.stack_matte(consumer, s, channel, invert, feather_px, choke_px),
            None => gpu.stack_scale_alpha(consumer, 0.0),
        }
    }

    /// An adjustment layer on the GPU: the accumulator seeds the clip's
    /// grade, its effects run on the result, and the result mixes back
    /// with the layer's opacity.
    #[allow(clippy::too_many_arguments)]
    fn stack_adjust(
        &mut self,
        c: &Clip,
        opacity: f32,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        transforms: &HashMap<ClipId, ResolvedTransform>,
        stats: &mut RenderStats,
    ) -> Result<(), &'static str> {
        let clip_time = eval_clip_time(c, time);
        let (mut fx, _) = clip_effects(c, time, fps, transforms.get(&c.id).copied());
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        let temporal = fx.iter().any(|e| matches!(e, BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. }));
        if !fx.is_empty() && !(klipp_effects::chain_supports_gpu(&fx) && !temporal) {
            return Err("adjustment layer with a CPU-only effect");
        }
        let mut slot_a = true;
        if c.node_graph.is_identity() {
            if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_acc_to_chain()) {
                return Err("the accumulator could not seed the chain");
            }
        } else {
            let t = Instant::now();
            let seed: Option<Arc<AccSeed>> = self.executor.gpu_pipeline_or_init().and_then(|g| {
                let buffer = g.stack_acc_buffer()?;
                Some(Arc::new(AccSeed { buffer, device: g.device(), queue: g.queue(), width, height }))
            });
            let Some(seed) = seed else { return Err("the accumulator could not seed the chain") };
            self.executor.request_output_on_gpu();
            let pixels = self.grade(c, GradeInput::Gpu(seed), clip_time, fps, width, height);
            stats.grade_ms += ms_since(t);
            if pixels.is_empty() {
                slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
            } else if let Some(g) = self.executor.gpu_pipeline_or_init() {
                g.upload_chain_input_linear(&pixels);
            }
        }
        if !fx.is_empty() {
            let t = Instant::now();
            let Some(g) = self.executor.gpu_pipeline_or_init() else { return Err("GPU gone") };
            g.stack_chain_to_slot_a(slot_a);
            slot_a = g.run_effects_in_chain(&fx, width, height);
            stats.effects_ms += ms_since(t);
        }
        let t = Instant::now();
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_mix_from_chain(slot_a, opacity)) {
            return Err("the adjustment mix failed");
        }
        stats.composite_ms += ms_since(t);
        Ok(())
    }

    /// The timeline grade to run over this composite, as a clip. Not
    /// inside a precomposition's scene, which is one layer of the
    /// sequence that holds it: the grade runs once, over the whole, as
    /// the timeline's LUT does.
    fn timeline_grade(&self, seq: &Sequence) -> Option<Clip> {
        if self.nesting > 0 {
            return None;
        }
        Clip::timeline_grade_of(seq)
    }

    /// The stack cannot take this frame: `None`, and the reason in the
    /// log once, until it changes.
    fn gpu_stack_declines<T>(&mut self, why: &'static str) -> Option<T> {
        self.gpu_export_declined_after_work = self.stack_work_started;
        if self.last_gpu_stack_decline != Some(why) {
            tracing::debug!("GPU stack declines the frame: {why}");
            self.last_gpu_stack_decline = Some(why);
        }
        None
    }

    fn global_lut_on_chain(&mut self, width: u32, height: u32) -> bool {
        let Some(lut) = self.global_lut.take() else { return true };
        let ok = self.executor.apply_global_lut_to_output(
            width,
            height,
            &lut.lut.data,
            lut.lut.size as u32,
            lut.opacity,
            self.lut_tetrahedral,
        );
        self.global_lut = Some(lut);
        ok
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
        in_render_pixels(seq, width, height, &mut transforms);
        self.px_scale = render_scale(seq, width, height).1;
        behind_the_camera(seq, time, width, height, &mut transforms);
        self.open_shutter(seq, time, fps, width, height, &transforms);

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
                    feather_px: s.feather_px * self.px_scale,
                    choke_px: s.choke_px * self.px_scale,
                };
                apply_matte(&mut layer.pixels, &rendered, &binding, width, height,
                    klipp_color::linear::luma_coefs(self.working));
            }
            if let TrackItem::Clip(c) = item {
                if let Some(b) = &c.matte_source {
                    let t = eval_clip_time(c, time);
                    let binding = klipp_timeline::MatteBinding {
                        source_clip_id: b.source_clip_id,
                        channel: b.channel,
                        invert: b.invert,
                        feather_px: klipp_core::keyframe::evaluate(&c.matte_feather_keyframes, t, b.feather_px) * self.px_scale,
                        choke_px: klipp_core::keyframe::evaluate(&c.matte_choke_keyframes, t, b.choke_px) * self.px_scale,
                    };
                    if self.inspect.matte_of == Some(c.id) {
                        self.found.matte_mask = matte_mask(&rendered, &binding, width, height,
                            klipp_color::linear::luma_coefs(self.working));
                    }
                    apply_matte(&mut layer.pixels, &rendered, &binding, width, height,
                    klipp_color::linear::luma_coefs(self.working));
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

        // The timeline grade: over the composite, before the timeline's
        // LUT — the overall look, then the output transform.
        if let Some(tg) = self.timeline_grade(seq) {
            let t = Instant::now();
            acc = self.apply_adjustment(&tg, acc, 1.0, time, fps, width, height, &transforms);
            self.found.stats.grade_ms += ms_since(t);
        }

        if let Some(lut) = self.global_lut.take() {
            let t = Instant::now();
            let tetrahedral = self.lut_tetrahedral;
            let above_white = if self.hdr_peak.is_some() { split_above_white(&mut acc) } else { None };
            acc = match self.gpu() {
                Some(gpu) => gpu.apply_3d_lut_gpu(&acc, width, height, &lut.lut.data, lut.lut.size as u32, lut.opacity, tetrahedral),
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
                // Temporal noise reduction happens here, on the source,
                // before the grade — so a clip using it gives up the
                // GPU-resident fast path below and comes down to the
                // CPU. That cost is the feature, not an oversight: the
                // neighbours it averages against are CPU-decoded source
                // frames.
                let tnr = temporal_denoise_of(clip);
                match f {
                    // Left on the GPU for the grade to start from.
                    SourceFrame { pixels: SourcePixels::Gpu(g), width: w, height: h, .. }
                        if tnr.is_none() && next.is_none() && (w, h) == (width, height) =>
                    {
                        (GradeInput::Gpu(g), None)
                    }
                    // Scene-referred codes at the render size go to the
                    // grade as they are.
                    SourceFrame { pixels: SourcePixels::SceneReferred { codes, transfer, source_to_working }, width: w, height: h, .. }
                        if tnr.is_none() && next.is_none() && (w, h) == (width, height) && codes.len() == (w as usize) * (h as usize) =>
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
                        // Scene-referred sources included: the linear
                        // provider renders them to scene linear, the
                        // same as `to_linear` just did for this frame,
                        // so the neighbours are samples of one picture
                        // rather than renderings of two.
                        let pixels = match tnr {
                            Some((strength, threshold)) => self.temporal_denoise(
                                clip, at, pixels, width, height, fps, strength, threshold, src,
                            ),
                            None => pixels,
                        };
                        (GradeInput::Cpu(pixels), hdr)
                    }
                }
            }
            ClipKind::Title(td) => (GradeInput::Cpu(Arc::new(self.title_linear_at(clip, td, clip_time, width, height))), None),
            ClipKind::Generator(g) => {
                let total = (clip.timeline_out - clip.timeline_in).to_secs_f64();
                let packed = crate::synth::render_generator_with_slate_f16(&mut self.titles, g, width, height, clip_time, total);
                let mut linear = klipp_core::generator::srgb_f16_to_linear_f32(&packed);
                self.authored_to_working(&mut linear);
                (GradeInput::Cpu(Arc::new(linear)), None)
            }
            ClipKind::Gap | ClipKind::Sequence { .. } | ClipKind::AdjustmentLayer => return None,
            // The scene, composed on a surface of its own, is this clip's
            // source: from here on it is graded, transformed, faded and
            // blurred once, as one picture. Unresolved — the scene is
            // missing, or contains itself — it draws nothing.
            ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. } => {
                let scene = clip.precomp.clone()?;
                let at = precomposition_time(clip, &scene, time, fps);
                let surface = self.render_scene(&scene, at, width, height, src)?;
                (GradeInput::Cpu(Arc::new(surface)), None)
            }
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
        let (mut fx, opacity) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied());
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        self.prepare_temporal_effects(clip, &fx, time, fps, height, src);
        let provider = if clip.effects.iter().any(|e| e.enabled && is_temporal_plugin(&e.plugin_name)) {
            src.temporal_frames(clip)
        } else {
            None
        };
        let pixels = self.ofx(clip, pixels, clip_time, width, height, provider);
        // A title whose own drawing moves is drawn again at every shutter
        // sample, by `title_pixels_at`, which carries a sample through
        // what the frame above was just carried through.
        let redrawn = match &clip.kind {
            ClipKind::Title(td) => title_raster_moves(td),
            _ => None,
        };
        let pixels = match (title_motion_blur(clip), self.shutter.get(&clip.id).cloned()) {
            (Some(samples), _) => self.motion_blurred(clip, pixels, redrawn, &fx, samples, time, fps, width, height),
            (None, Some(samples)) => self.shuttered(clip, pixels, &fx, &samples, clip_time, width, height),
            (None, None) => self.effects(&fx, pixels, clip_time, width, height),
        };
        Some((pixels, opacity))
    }

    /// A title as working-space linear pixels at one instant: rasterised
    /// at that time, decoded, its deferred blur entrance applied, and
    /// brought into the working space. The frame's Title branch in
    /// `render_clip` and each shutter sample in `title_pixels_at` both
    /// start here, so the title-specific steps exist once.
    fn title_linear_at(&mut self, clip: &Clip, td: &klipp_timeline::TitleData, t: f64, width: u32, height: u32) -> Vec<f32> {
        let total = (clip.timeline_out - clip.timeline_in).to_secs_f64() as f32;
        let u8s = {
            let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
            let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
            self.titles.render_with_window(td, width, height, gpu, t as f32, Some(total))
        };
        if tracing::enabled!(tracing::Level::DEBUG) {
            let opaque = u8s.chunks_exact(4).step_by(64).filter(|p| p[3] > 0).count() * 64;
            tracing::debug!(
                "TITLE_LAYER clip={} t={t:.2}/{total:.2} {width}x{height} objects={} text={:?} font={} size={} opaque_px~{opaque}",
                clip.id, td.objects.len(), td.text(), td.font_name(), td.font_size(),
            );
        }
        let mut linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&u8s);
        let blur = self.titles.last_blur();
        if blur > 0.5 {
            // The blur entrance, as the GPU stack blurs the placed sprite.
            let luma = klipp_color::linear::luma_coefs(self.working);
            linear = klipp_effects::apply_effects_with_luma(&[BuiltinEffect::GaussianBlur { radius: blur }], linear, width, height, t, luma);
        }
        self.authored_to_working(&mut linear);
        linear
    }

    /// One shutter sample of a title: `title_linear_at`, then the node
    /// grade and the plugins at that instant — the two calls `render_clip`
    /// makes for the frame after its own `title_linear_at`. The one
    /// difference is that a temporal plugin is handed no neighbouring
    /// frames for a sample; it runs on the sample alone, as on a still.
    fn title_pixels_at(&mut self, clip: &Clip, td: &klipp_timeline::TitleData, t: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        let linear = self.title_linear_at(clip, td, t, width, height);
        let graded = self.grade(clip, GradeInput::Cpu(Arc::new(linear)), t, fps, width, height);
        self.ofx(clip, graded, t, width, height, None)
    }

    /// A precomposition's scene at `at`, on a transparent surface: its
    /// tracks, camera, masks and shutter resolve here, in the working
    /// space, with nothing of the outer render's applied to them. The
    /// per-render state the outer stack has in hand — its shutter map,
    /// its global LUT, what it is inspecting — is set aside for the
    /// scene's render and restored after, so a scene neither inherits
    /// nor disturbs it. `None` past the nesting bound.
    fn render_scene(&mut self, scene: &Sequence, at: Timecode, width: u32, height: u32, src: &mut dyn FrameSource) -> Option<Vec<f32>> {
        if self.nesting >= klipp_timeline::flatten::MAX_COMPOUND_DEPTH {
            return None;
        }
        self.nesting += 1;
        let shutter = std::mem::take(&mut self.shutter);
        let lut = self.global_lut.take();
        let inspect = std::mem::take(&mut self.inspect);
        let surface = self.render_stack(scene, at, width, height, src, RenderOptions { keep_alpha: true, hdr_peak_nits: self.hdr_peak });
        self.inspect = inspect;
        self.global_lut = lut;
        self.shutter = shutter;
        self.nesting -= 1;
        Some(surface)
    }

    /// sRGB-authored pixels — a title, a generator, an OFX plugin's
    /// output — into the working space. They are written in BT.709
    /// primaries; in an ACEScg or BT.2020 project the same bytes mean a
    /// different colour, so a "pure red" title was the working space's
    /// red, more saturated than the one the user picked.
    fn authored_to_working(&self, linear: &mut [f32]) {
        let Some(m) = klipp_color::primaries::working_space_conversion_matrix(
            WorkingColorspace::Rec709Linear,
            self.working,
        ) else {
            return;
        };
        use rayon::prelude::*;
        linear.par_chunks_exact_mut(4).for_each(|px| {
            let c = klipp_color::primaries::matrix3_mul_vec(m, [px[0], px[1], px[2]]);
            px[..3].copy_from_slice(&c);
        });
    }

    /// The pixel-sized parameters of `fx` in the pixels of the buffer being
    /// rendered; see [`BuiltinEffect::at_render_scale`].
    fn at_render_scale(&self, fx: &mut [BuiltinEffect]) {
        for e in fx.iter_mut() {
            *e = std::mem::replace(e, BuiltinEffect::Sharpen { amount: 0.0 }).at_render_scale(self.px_scale);
        }
    }

    /// The colours an effect carries as parameters — a chroma key's key
    /// colour, a fade's colour — are picked in BT.709, like the titles.
    /// An effect compares them against the picture it is given, so in
    /// another working space they have to be the same colour expressed
    /// there, or the key misses the green it was pointed at.
    fn authored_colours(&self, fx: &mut [BuiltinEffect]) {
        let Some(m) = klipp_color::primaries::working_space_conversion_matrix(
            WorkingColorspace::Rec709Linear,
            self.working,
        ) else {
            return;
        };
        let convert = |c: &mut [u8]| {
            let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&[c[0], c[1], c[2], 255]);
            let working = klipp_color::primaries::matrix3_mul_vec(m, [linear[0], linear[1], linear[2]]);
            let bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&[working[0], working[1], working[2], 1.0]);
            c[..3].copy_from_slice(&bytes[..3]);
        };
        for effect in fx {
            match effect {
                BuiltinEffect::ChromaKey { key_color, .. } => convert(key_color),
                BuiltinEffect::ColorFade { color, .. } => convert(color),
                _ => {}
            }
        }
    }

    /// The working space's pixels as BT.709, for something that only
    /// speaks BT.709 — an 8-bit OFX plugin.
    fn working_to_authored(&self, linear: &mut [f32]) {
        let Some(m) = klipp_color::primaries::working_space_conversion_matrix(
            self.working,
            WorkingColorspace::Rec709Linear,
        ) else {
            return;
        };
        use rayon::prelude::*;
        linear.par_chunks_exact_mut(4).for_each(|px| {
            let c = klipp_color::primaries::matrix3_mul_vec(m, [px[0], px[1], px[2]]);
            px[..3].copy_from_slice(&c);
        });
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

    /// Average the source frame with its neighbours, before the grade.
    ///
    /// The neighbours come from the clip's own media through the frame
    /// provider, which reads clip time — `source_secs` returns a
    /// position in the media, so the clip's `source_in` comes back off
    /// before asking.
    ///
    /// A neighbour of a different size is dropped rather than scaled:
    /// it means the render size and the media size disagree, and a
    /// resampled neighbour would blur the very detail the guard exists
    /// to protect.
    #[allow(clippy::too_many_arguments)]
    fn temporal_denoise(
        &mut self,
        clip: &Clip,
        source_at: f64,
        pixels: Arc<Vec<f32>>,
        width: u32,
        height: u32,
        fps: f64,
        strength: f32,
        threshold: f32,
        src: &mut dyn FrameSource,
    ) -> Arc<Vec<f32>> {
        if !self.tnr_frames.contains_key(&clip.id) {
            // One decoder per clip, kept. Bounded so a long timeline
            // does not hold one open for every clip it has passed.
            if self.tnr_frames.len() > 8 {
                self.tnr_frames.clear();
            }
            let Some(provider) = src.temporal_frames_linear(clip, self.working) else {
                return pixels;
            };
            self.tnr_frames.insert(clip.id, provider);
        }
        let Some(provider) = self.tnr_frames.get_mut(&clip.id) else { return pixels };

        let dt = 1.0 / fps.max(1.0);
        let clip_at = source_at - clip.source_in.to_secs_f64();
        let want = (width as usize) * (height as usize) * 4;
        let mut neighbours: Vec<Vec<f32>> = Vec::with_capacity(2);
        for t in [clip_at - dt, clip_at + dt] {
            if t < 0.0 {
                continue;
            }
            let Some((linear, w, h)) = provider(t) else { continue };
            if (w, h) != (width, height) || linear.len() != want {
                continue;
            }
            neighbours.push(linear);
        }
        if neighbours.is_empty() {
            return pixels;
        }
        let refs: Vec<&[f32]> = neighbours.iter().map(|n| n.as_slice()).collect();
        let t = Instant::now();
        let out = klipp_effects::temporal_denoise::denoise_frame(&pixels, &refs, threshold, strength);
        self.found.stats.effects_ms += ms_since(t);
        Arc::new(out)
    }

    /// The clip's node graph, at clip-relative time. The inspected clip
    /// also runs the grade tools' captures, its solo and its matte view.
    /// Pixels still on the GPU seed the grade there; read back only when
    /// there is no grade to run, or a node needs them on the CPU.
    fn grade(&mut self, clip: &Clip, input: GradeInput, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        let inspecting = self.inspect.clip == Some(clip.id);
        let matte_view = if inspecting { self.inspect.matte_view } else { None };
        if clip.node_graph.is_identity() && matte_view.is_none() {
            // Take the buffer rather than clone the handle: a layer this
            // renderer just made is held once, and moves out for free.
            return match input {
                GradeInput::Cpu(p) => unshare(p),
                other => unshare(other.pixels()),
            };
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
        // An OFX plugin takes 8-bit BT.709, so the frame goes out of the
        // working space and comes back into it.
        let mut as_709 = pixels;
        self.working_to_authored(&mut as_709);
        let u8s = klipp_effects::linear_f32_rgba_to_srgb_u8(&as_709);
        let out = match frames {
            Some(frames) => self.ofx.apply_effects_chain_temporal(&clip.effects, &u8s, width, height, clip_time, frames),
            None => self.ofx.apply_effects_chain(&clip.effects, &u8s, width, height, clip_time),
        };
        let mut out = klipp_effects::srgb_u8_to_linear_f32_rgba(&out);
        self.authored_to_working(&mut out);
        self.found.stats.ofx_ms += ms_since(t);
        out
    }

    fn effects(&mut self, fx: &[BuiltinEffect], pixels: Vec<f32>, clip_time: f64, width: u32, height: u32) -> Vec<f32> {
        if fx.is_empty() {
            return pixels;
        }
        let t = Instant::now();
        let gpu_ok = klipp_effects::chain_supports_gpu(fx);
        // The working space's luminance weights, for the effects that gate
        // on brightness — a bright pass, a denoise threshold, an edge
        // measure. Both arms get the same numbers, which is what
        // `cpu_gpu_effect_parity` checks; before this they both assumed
        // BT.709 and mis-estimated brightness in any other basis. This
        // renderer owns its own effects pipeline, separate from the
        // executor's, so the executor's `set_luma_coefs` does not cover
        // it.
        let luma = klipp_color::linear::luma_coefs(self.working);
        let out = match self.gpu() {
            Some(gpu) if gpu_ok => {
                gpu.set_luma_coefs(luma);
                gpu.apply_effects(fx, &pixels, width, height)
            }
            _ => klipp_effects::apply_effects_with_luma(
                fx, pixels, width, height, clip_time, luma,
            ),
        };
        self.found.stats.effects_ms += ms_since(t);
        out
    }

    /// A title with motion blur: the transform is sampled across the
    /// shutter and the samples averaged, in linear light. The transform is
    /// applied once — as these samples — and not again by the effects.
    /// The sequence camera's shutter over one frame: for every clip that
    /// moves while it is open, its full transform — own keyframes, the
    /// camera from its depth — at each sample across the shutter. A clip
    /// that does not move gets none and renders once.
    fn open_shutter(&mut self, seq: &Sequence, time: Timecode, fps: f64, width: u32, height: u32, transforms: &HashMap<ClipId, ResolvedTransform>) {
        self.shutter.clear();
        let cam = &seq.camera;
        if cam.shutter_deg <= 0.0 {
            return;
        }
        let offsets = shutter_offsets(cam.shutter_deg, fps, cam.shutter_samples.clamp(2, 32));
        let n = offsets.len();
        let clips: HashMap<ClipId, &Clip> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        for id in transforms.keys() {
            let Some(c) = clips.get(id).copied() else { continue };
            // A title carrying its own blur keeps it; sampling the
            // sequence shutter over it as well would blur it twice.
            if title_motion_blur(c).is_some() {
                continue;
            }
            // A clip that follows another has no transform of its own to
            // sample — its position at an instant is whatever its target
            // was at that instant. So it is resolved the same way the
            // frame resolves it, once per sample, each with a cache of
            // its own: the resolver keys by clip id for one moment, and
            // reusing one across the shutter would hand every sample the
            // first sample's answer.
            let linked = c.transform_link.is_some();
            let mut samples: Vec<(ResolvedTransform, f64)> = Vec::with_capacity(n);
            for &o in &offsets {
                let t = (eval_clip_time(c, time) + o).max(0.0);
                let own = if linked {
                    // Carried at a far finer rate than the sequence's.
                    // A Timecode rounds to whole frames and a shutter's
                    // offsets are fractions of one, so at the sequence
                    // rate every sample lands on one of two frames and
                    // the clip smears about half as far as it should.
                    let fine = time.rate.saturating_mul(1000);
                    let at_sample = Timecode::from_secs_f64(time.to_secs_f64() + o, fine);
                    // In the sequence's pixels, as the frame resolves it,
                    // then scaled with everything else.
                    let mut cache = HashMap::new();
                    eval::resolve_clip_transform(
                        c, seq, at_sample, fps, &mut cache,
                        &mut self.glyph_cache, &mut self.titles, seq.resolution.width, seq.resolution.height,
                    )
                } else {
                    let ctx = klipp_core::EvalContext { clip_time_secs: t, ..eval::eval_context(c, time, fps) };
                    eval::evaluate_clip_own_transform(c, t, &ctx)
                };
                let own = to_render_pixels(own, render_scale(seq, width, height));
                let xf = match eval::camera_at_secs(seq, time.to_secs_f64() + o).and_then(|k| eval::camera_at_depth(k, c.depth)) {
                    Some(k) => eval::apply_camera(own, k, width, height),
                    None => own,
                };
                samples.push((xf, t));
            }
            let tilt_at = |t: f64| (klipp_core::keyframe::evaluate(&c.transform_tilt_x_keyframes, t, 0.0), klipp_core::keyframe::evaluate(&c.transform_tilt_y_keyframes, t, 0.0));
            let (first, last) = (samples[0], samples[n - 1]);
            let moved = (first.0 .0 - last.0 .0).abs() > 0.05
                || (first.0 .1 - last.0 .1).abs() > 0.05
                || (first.0 .2 - last.0 .2).abs() > 1e-4
                || (first.0 .4 - last.0 .4).abs() > 0.01
                || tilt_at(first.1) != tilt_at(last.1);
            if moved {
                self.shutter.insert(*id, samples);
            }
        }
    }

    /// A clip smeared along its path while the shutter is open. What sits
    /// before the transform in its chain — a crop, rounded corners — is
    /// drawn once, the picture is placed at every sample and the samples
    /// averaged, and what draws outside the picture — a shadow, a glow —
    /// is cast once from the blurred result, so it blurs with it.
    #[allow(clippy::too_many_arguments)]
    fn shuttered(&mut self, clip: &Clip, pixels: Vec<f32>, fx: &[BuiltinEffect], samples: &[(ResolvedTransform, f64)], clip_time: f64, width: u32, height: u32) -> Vec<f32> {
        let cut = fx
            .iter()
            .position(|e| matches!(e, BuiltinEffect::Transform { .. }))
            .unwrap_or_else(|| fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len()));
        let before = &fx[..cut];
        let after: Vec<BuiltinEffect> = fx[cut..].iter().filter(|e| !matches!(e, BuiltinEffect::Transform { .. })).cloned().collect();
        let base = self.effects(before, pixels, clip_time, width, height);
        let mut acc = vec![0.0f32; base.len()];
        for &(xf, t) in samples {
            let placed = self.effects(&[transform_effect(xf, tilt(clip, fx, t))], base.clone(), t, width, height);
            for (a, s) in acc.iter_mut().zip(&placed) {
                *a += *s;
            }
        }
        let inv = 1.0 / samples.len() as f32;
        acc.iter_mut().for_each(|v| *v *= inv);
        self.effects(&after, acc, clip_time, width, height)
    }

    #[allow(clippy::too_many_arguments)]
    fn motion_blurred(
        &mut self,
        clip: &Clip,
        pixels: Vec<f32>,
        redrawn: Option<&klipp_timeline::TitleData>,
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
        let offsets = shutter_offsets(mb.evaluated_shutter_angle(clip_time), fps, mb.samples);
        let n = offsets.len();
        // A title whose glyphs move has a different picture at every
        // instant the shutter is open, so placing one rasterisation
        // eight times blurs only where the clip went, not where the
        // letters did — a word rising into place came out as sharp
        // letters sliding, which is the one thing motion blur exists to
        // avoid. When `redrawn` names the title, each sample is drawn
        // afresh at its own instant — one sample in memory at a time,
        // consumed before the next is made, so a UHD title at sixteen
        // samples holds one 127 MiB sample and not sixteen. For anything
        // else the single buffer is reused exactly as before and this
        // costs nothing.
        //
        // What it costs when it does fire: one title rasterisation a
        // sample instead of one a frame, and a title is not cheap to
        // draw — the renderer's own note puts a lower third at 110 ms.
        // `samples` on the title's own MotionBlur is the control, and
        // it is already the quality dial: two samples is a smear with
        // a seam in it, eight is smooth, and the cost is linear in
        // between. The renderer's cache does not soften this, because
        // a title that moves is exactly the one it declines to cache.
        let mut acc = vec![0.0f32; pixels.len()];
        for offset in offsets {
            let t = clip_time + offset;
            let ctx = klipp_core::EvalContext { clip_time_secs: t.max(0.0), ..eval::eval_context(clip, time, fps) };
            let (px, py, sx, sy, ro, ax, ay) = eval::evaluate_clip_own_transform(clip, t.max(0.0), &ctx);
            let xf = transform_effect((px, py, sx, sy, ro, ax, ay), tilt(clip, fx, t.max(0.0)));
            let at_sample = match redrawn {
                Some(td) => self.title_pixels_at(clip, td, t.max(0.0), fps, width, height),
                None => pixels.clone(),
            };
            let sample = self.effects(&[xf], at_sample, t.max(0.0), width, height);
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
        let (mut fx, _) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied());
        self.at_render_scale(&mut fx);
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
        .iter().rfind(|(at, _)| *at <= rel + 1e-9)
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
    frame_index(clip_time, fps).max(0) as u64
}

/// The frame that is showing at `secs`: `floor(secs * fps)`, with a
/// thousandth of a frame of slack for the arithmetic. Frame times come
/// from timecodes as `value / rate`, and at 29.97 that product lands a
/// hair under the integer for one frame in ten — frame 15 is
/// `0.5005 * 29.97 = 14.9999…` — so a bare truncation evaluated
/// keyframes, trackers and speed ramps one frame late there.
pub fn frame_index(secs: f64, fps: f64) -> i64 {
    (secs * fps + 1e-3).floor() as i64
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

/// How many render pixels one sequence pixel is, across and down: 1 when
/// rendering at the sequence's size, ½ for a half-size preview, and
/// whatever an export at another resolution makes it.
pub fn render_scale(seq: &Sequence, width: u32, height: u32) -> (f32, f32) {
    let (sw, sh) = (seq.resolution.width.max(1) as f32, seq.resolution.height.max(1) as f32);
    (width as f32 / sw, height as f32 / sh)
}

/// A resolved transform moved from the sequence's pixels, where every
/// position is written (the inspector, the viewer's handles, the pan and
/// compose ops all work at the sequence's size), into the pixels of the
/// buffer being rendered. Scale, rotation and anchor are already
/// relative — the source is fitted to the buffer first — so only the
/// offset changes. Without this a clip moved 400 px right in a 4K
/// sequence landed 400 px right of centre in the 1080p preview the
/// decoder hands 4K media down to: twice as far across the picture.
pub fn to_render_pixels(t: ResolvedTransform, (kx, ky): (f32, f32)) -> ResolvedTransform {
    let (px, py, sx, sy, ro, ax, ay) = t;
    (px * kx, py * ky, sx, sy, ro, ax, ay)
}

/// [`to_render_pixels`] over the finished map. Like the camera it runs
/// once the map is complete: the map is the resolver's cache, and a
/// linked clip reads its target's entry during the walk, in the
/// sequence's pixels, so the two scale together here exactly once.
fn in_render_pixels(seq: &Sequence, width: u32, height: u32, transforms: &mut HashMap<ClipId, ResolvedTransform>) {
    let k = render_scale(seq, width, height);
    if k == (1.0, 1.0) {
        return;
    }
    for t in transforms.values_mut() {
        *t = to_render_pixels(*t, k);
    }
}

/// Put every resolved layer behind the sequence's camera, once the map
/// is complete.
///
/// `width` and `height` are what is being rendered, not the sequence's
/// own size: a camera is written in fractions of the frame, and a
/// transform's position is in the pixels of the buffer it lands in, so
/// the two meet at the size actually in hand. A preview at a quarter
/// size then makes the same move as the export.
///
/// It runs over the finished map rather than inside the resolver because
/// the map is also the resolver's cache: a clip whose transform is
/// linked to another reads that clip's entry, and a camera applied
/// during the walk would be applied again through the link.
fn behind_the_camera(seq: &Sequence, time: Timecode, width: u32, height: u32, transforms: &mut HashMap<ClipId, ResolvedTransform>) {
    let Some(cam) = eval::camera_at(seq, time) else { return };
    // Each layer sees the camera from its own depth: parallax.
    let depth: HashMap<ClipId, f32> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c.depth)).collect();
    for (id, t) in transforms.iter_mut() {
        if let Some(seen) = eval::camera_at_depth(cam, depth.get(id).copied().unwrap_or(1.0)) {
            *t = eval::apply_camera(*t, seen, width, height);
        }
    }
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
            // The animated film effects take a fresh pattern per frame:
            // the seed is the timeline frame number, so a still export
            // and a scrub land on the same grain.
            BuiltinEffect::FilmGrain { amount, size, shadows, seed, .. } => {
                *amount = param("amount", *amount);
                *size = param("size", *size);
                *shadows = param("shadows", *shadows);
                *seed = (t * fps).round().max(0.0) as u32;
            }
            BuiltinEffect::Deflicker { strength, window_secs, gain } => {
                *strength = param("strength", *strength);
                *window_secs = param("window_secs", *window_secs);
                // The measured gain for this frame, at full weight —
                // the effect applies `strength` itself, and doing it
                // here as well would square it.
                *gain = clip
                    .deflicker_gains
                    .as_ref()
                    .map(|g| klipp_effects::deflicker::gain_at(&g.gains, t, g.fps, 1.0))
                    .unwrap_or(1.0);
            }
            BuiltinEffect::OldFilm { flicker, scratches, dust, weave, sepia, seed } => {
                *flicker = param("flicker", *flicker);
                *scratches = param("scratches", *scratches);
                *dust = param("dust", *dust);
                *weave = param("weave", *weave);
                *sepia = param("sepia", *sepia);
                *seed = (t * fps).round().max(0.0) as u32;
            }
            BuiltinEffect::Vhs { tracking, bleed, noise, scanlines, seed } => {
                *tracking = param("tracking", *tracking);
                *bleed = param("bleed", *bleed);
                *noise = param("noise", *noise);
                *scanlines = param("scanlines", *scanlines);
                *seed = (t * fps).round().max(0.0) as u32;
            }
            BuiltinEffect::TiltShift { center, width, softness, radius } => {
                *center = param("center", *center);
                *width = param("width", *width);
                *softness = param("softness", *softness);
                *radius = param("radius", *radius);
            }
            BuiltinEffect::CornerPin { tl, tr, br, bl } => {
                for (corner, names) in [tl, tr, br, bl].into_iter().zip(klipp_effects::corner_pin::PARAMS) {
                    corner[0] = param(names[0], corner[0]);
                    corner[1] = param(names[1], corner[1]);
                }
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
                // `t` is timeline seconds into the clip, so the fade out
                // is measured from the clip's timeline end. Measured from
                // its *source* length, a clip sped up 2× faded out an
                // hour early in the source and never on the timeline.
                let clip_dur = (clip.timeline_out - clip.timeline_in).to_secs_f64().max(0.0);
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
        // TemporalDenoise joins them: it ran at the head of the
        // pipeline, before the grade, and running again here would
        // average the graded picture against source neighbours.
        .filter(|e| !matches!(e, BuiltinEffect::Opacity { .. } | BuiltinEffect::SpeedRamp { .. } | BuiltinEffect::TemporalDenoise { .. }))
        .collect();

    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, &ctx));
    let has_transform = fx.iter().any(|e| matches!(e, BuiltinEffect::Transform { .. }));
    let three_d = tilt(clip, &fx, t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    if has_transform {
        for e in fx.iter_mut() {
            if matches!(e, BuiltinEffect::Transform { .. }) {
                *e = transform_effect(xf, three_d);
            }
        }
    } else if !identity {
        // A shadow or a glow draws in the transparent margin the
        // transform leaves around a placed layer, so it has to run after
        // the transform that made the margin. Everything else — rounded
        // corners included — runs before it, with the picture, so it
        // scales with the layer.
        let at = fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len());
        fx.insert(at, transform_effect(xf, three_d));
    }
    fx.retain(|e| !klipp_effects::is_identity_effect(e));
    (fx, opacity.clamp(0.0, 1.0))
}

/// A clip's 3D tilt and perspective at clip time `t`: the tilt tracks,
/// over whatever its own `Transform` effect says. A tilt with no
/// perspective given is seen in moderate perspective (1), since flat it
/// would only squash the picture rather than lean it.
fn tilt(clip: &Clip, fx: &[BuiltinEffect], t: f64) -> (f32, f32, f32) {
    let (rx, ry, persp) = fx
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Transform { rotation_x, rotation_y, perspective, .. } => Some((*rotation_x, *rotation_y, *perspective)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 0.0));
    let rx = klipp_core::keyframe::evaluate(&clip.transform_tilt_x_keyframes, t, rx);
    let ry = klipp_core::keyframe::evaluate(&clip.transform_tilt_y_keyframes, t, ry);
    // Keyframed, or set still in the inspector: either way a tilt.
    let tilted = rx != 0.0 || ry != 0.0 || !clip.transform_tilt_x_keyframes.is_empty() || !clip.transform_tilt_y_keyframes.is_empty();
    (rx, ry, if tilted && persp <= 0.0 { 1.0 } else { persp })
}

/// A `Transform` effect with the resolved values and the clip's tilt.
fn transform_effect(xf: ResolvedTransform, (rx, ry, persp): (f32, f32, f32)) -> BuiltinEffect {
    let (px, py, sx, sy, ro, ax, ay) = xf;
    BuiltinEffect::Transform {
        position_x: px, position_y: py, scale_x: sx, scale_y: sy, rotation: ro,
        anchor_x: ax, anchor_y: ay, rotation_x: rx, rotation_y: ry, perspective: persp,
        focal_ref: None,
    }
}

/// When a shutter of `angle` degrees samples a frame, in seconds from
/// the frame: `samples` instants spread evenly across the time it is
/// open — `angle / 360` of a frame at `fps` — centred on the frame. A
/// title's blur and every other layer's use the same, so the two smear
/// alike at the same setting.
fn shutter_offsets(angle: f32, fps: f64, samples: u8) -> Vec<f64> {
    let n = samples.max(2) as usize;
    let open = (angle.max(0.0) as f64 / 360.0) / fps.max(1.0);
    (0..n).map(|i| (i as f64 / (n - 1) as f64 - 0.5) * open).collect()
}

/// Whether a clip could be blurred by the sequence camera's shutter this
/// frame: the shutter is open and something that places it is animated.
/// The fast paths step aside for such a clip; whether it really moved is
/// settled with the samples.
fn may_blur(seq: &Sequence, c: &Clip) -> bool {
    seq.camera.shutter_deg > 0.0 && (camera_moves_it(seq, c) || moves_itself(c) || follows_something_moving(seq, c, 0))
}

fn animated_keys(k: &[klipp_core::Keyframe]) -> bool {
    k.windows(2).any(|w| (w[0].value - w[1].value).abs() > 1e-6)
}

fn camera_moves_it(seq: &Sequence, c: &Clip) -> bool {
    let cam = &seq.camera;
    c.depth > 0.0
        && [&cam.x_keyframes, &cam.y_keyframes, &cam.zoom_keyframes, &cam.roll_keyframes]
            .iter()
            .any(|k| animated_keys(k))
}

fn moves_itself(c: &Clip) -> bool {
    [
        &c.transform_pos_x_keyframes,
        &c.transform_pos_y_keyframes,
        &c.transform_scale_keyframes,
        &c.transform_rotation_keyframes,
        &c.transform_tilt_x_keyframes,
        &c.transform_tilt_y_keyframes,
    ]
    .iter()
    .any(|k| animated_keys(k))
}

/// Whether a clip is carried by something that moves.
///
/// A linked clip usually has no keyframes of its own — being carried is
/// the whole point of linking one — so asking only what it animates
/// answers "nothing" and it never reaches the blur path at all. What
/// moves it is its target, so that is what gets asked, up the chain.
/// A tracker target counts as moving: a tracker that held still would
/// not be one.
fn follows_something_moving(seq: &Sequence, c: &Clip, depth: u32) -> bool {
    // The same cap the transform resolver uses, against a corrupt file.
    if depth >= 64 {
        return false;
    }
    let Some(link) = &c.transform_link else { return false };
    let target_id = match &link.target {
        klipp_timeline::TransformLinkTarget::Clip(id) => *id,
        klipp_timeline::TransformLinkTarget::Tracker { .. } => return true,
    };
    let Some(target) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|t| t.id == target_id) else {
        return false;
    };
    camera_moves_it(seq, target) || moves_itself(target) || follows_something_moving(seq, target, depth + 1)
}

/// A title's motion blur, when on and not overridden by a transform
/// link.
///
/// A linked title still blurs — the sequence shutter samples it now,
/// with its target resolved at each one — but not by its own setting,
/// which samples the title's own animation and would need the link
/// resolved inside that path too. See #196, which is about what the
/// title's own blur samples.
/// The title data behind a clip whose own drawing changes with time.
///
/// Entrance and exit motion, per-character animation and the continuous
/// loops (pulse, wave, neon) all redraw the title as the clip plays. A
/// title with none of them draws the same picture at every instant, and
/// re-rasterising it across the shutter would cost frames to arrive at
/// the buffer already in hand.
///
/// Read off the model rather than asked of the renderer: the question
/// here is "can this move", which the data answers, and not "is it
/// moving now", which is the rasteriser's own cache-bypass decision and
/// belongs to it.
fn title_raster_moves(td: &klipp_timeline::TitleData) -> Option<&klipp_timeline::TitleData> {
    // Any layer's own motion: the stack is the object's, whatever the
    // object is, and a character animation is a text's alone.
    let moves = td.objects.iter().any(|o| {
        let fx = o.fx();
        fx.motion.enter.is_some()
            || fx.motion.exit.is_some()
            || !fx.loops.is_empty()
            || matches!(&o.content, klipp_timeline::TitleObjectContent::TextBox(tb) if tb.char_animation.as_ref().is_some_and(|c| c.enabled))
    });
    moves.then_some(td)
}

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
    // The project's luminance row. A Luminance-channel matte weighs the
    // source clip's pixels in the project's own primaries, so Rec.709's
    // row would under-weight green by a fifth on an ACEScg timeline.
    luma: [f32; 3],
) {
    use klipp_effects::track_matte as tm;
    let Some(source) = rendered.get(&binding.source_clip_id).filter(|s| s.len() == consumer.len()) else {
        consumer.chunks_exact_mut(4).for_each(|px| px[3] = 0.0);
        return;
    };
    let channel = matte_channel(binding.channel);
    if binding.choke_px < 0.5 && binding.feather_px < 0.25 {
        tm::apply_matte_fused_linear(consumer, source, channel, binding.invert, true, luma);
        return;
    }
    let mut mask = tm::extract_mask_linear(source, channel, binding.invert, true, luma);
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

/// A GPU frame of another size than the render, standing in for one of
/// the render's size: seeded, the source fits it on the GPU; read, it
/// is fitted here, once.
struct FittedGpu {
    inner: Arc<dyn GpuPixels>,
    w: u32,
    h: u32,
    ow: u32,
    oh: u32,
    fitted: std::sync::OnceLock<Arc<Vec<f32>>>,
}

impl GpuPixels for FittedGpu {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        (width, height) == (self.ow, self.oh) && self.inner.seed(exec, width, height)
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(self.fitted.get_or_init(|| fit(self.inner.read(), self.w, self.h, self.ow, self.oh)))
    }
}

/// Place a source of another size in the output frame the way the
/// suites do: scaled by one factor to fit, centred, the rest left empty
/// (a portrait clip in a landscape sequence gets bars, not a stretch).
/// Straight alpha; an area average when shrinking by more than half so
/// a 4K overlay over a 1080p frame does not alias, bilinear otherwise.
fn fit(p: Arc<Vec<f32>>, w: u32, h: u32, ow: u32, oh: u32) -> Arc<Vec<f32>> {
    use rayon::prelude::*;
    if (w, h) == (ow, oh) || w == 0 || h == 0 || ow == 0 || oh == 0 {
        return p;
    }
    let (w, h, ow_u) = (w as usize, h as usize, ow as usize);
    let scale = (ow as f32 / w as f32).min(oh as f32 / h as f32);
    // The picture's place in the frame, in whole output pixels.
    let dw = ((w as f32 * scale).round() as usize).clamp(1, ow_u);
    let dh = ((h as f32 * scale).round() as usize).clamp(1, oh as usize);
    let (x_off, y_off) = ((ow_u - dw) / 2, (oh as usize - dh) / 2);
    let (sx, sy) = (w as f32 / dw as f32, h as f32 / dh as f32);
    let mut out = vec![0.0f32; ow_u * (oh as usize) * 4];
    out.par_chunks_exact_mut(ow_u * 4).enumerate().for_each(|(oy, row)| {
        if oy < y_off || oy >= y_off + dh {
            return;
        }
        let y = oy - y_off;
        let dest = &mut row[x_off * 4..(x_off + dw) * 4];
        if sx >= 2.0 && sy >= 2.0 {
            let y0 = ((y as f32 * sy) as usize).min(h - 1);
            let y1 = (((y + 1) as f32 * sy) as usize).clamp(y0 + 1, h);
            for x in 0..dw {
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
                    dest[x * 4 + c] = acc[c] / n;
                }
            }
            return;
        }
        let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..4 {
                let at = |xx: usize, yy: usize| p[(yy * w + xx) * 4 + c];
                let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
                let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
                dest[x * 4 + c] = top + (bot - top) * ty;
            }
        }
    });
    Arc::new(out)
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn frame(w: usize, h: usize, rgb: [f32; 3]) -> Arc<Vec<f32>> {
        Arc::new((0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 1.0]).collect())
    }

    fn px(out: &[f32], ow: usize, x: usize, y: usize) -> [f32; 4] {
        let i = (y * ow + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    }

    #[test]
    fn a_portrait_clip_in_a_landscape_frame_is_centred_with_bars_not_stretched() {
        // 9:16 into 16:9: the picture fills the height and sits in the
        // middle at 9/16 of the height wide.
        let src = frame(90, 160, [0.5, 0.25, 0.75]);
        let out = fit(src, 90, 160, 320, 180);
        let (ow, oh) = (320usize, 180usize);
        let dw = (180.0 * 90.0 / 160.0_f32).round() as usize; // 101
        let x_off = (ow - dw) / 2;
        assert_eq!(px(&out, ow, x_off - 1, oh / 2), [0.0; 4], "bar on the left is empty");
        assert_eq!(px(&out, ow, x_off + dw, oh / 2), [0.0; 4], "bar on the right is empty");
        assert_eq!(px(&out, ow, ow / 2, 0), [0.5, 0.25, 0.75, 1.0], "the picture reaches the top");
        assert_eq!(px(&out, ow, ow / 2, oh - 1), [0.5, 0.25, 0.75, 1.0], "and the bottom");
        assert_eq!(px(&out, ow, x_off, oh / 2), [0.5, 0.25, 0.75, 1.0]);
        assert_eq!(px(&out, ow, x_off + dw - 1, oh / 2), [0.5, 0.25, 0.75, 1.0]);
    }

    #[test]
    fn a_landscape_clip_in_a_portrait_frame_gets_bars_above_and_below_and_a_same_size_frame_passes_through() {
        let src = frame(160, 90, [1.0, 0.0, 0.0]);
        let out = fit(src.clone(), 160, 90, 180, 320);
        let (ow, oh) = (180usize, 320usize);
        let dh = (180.0 * 90.0 / 160.0_f32).round() as usize; // 101
        let y_off = (oh - dh) / 2;
        assert_eq!(px(&out, ow, ow / 2, y_off - 1), [0.0; 4]);
        assert_eq!(px(&out, ow, ow / 2, y_off + dh), [0.0; 4]);
        assert_eq!(px(&out, ow, 0, oh / 2), [1.0, 0.0, 0.0, 1.0], "the picture fills the width");
        assert_eq!(px(&out, ow, ow - 1, oh / 2), [1.0, 0.0, 0.0, 1.0]);
        // A source that is already the output size is the output.
        let same = fit(src.clone(), 160, 90, 160, 90);
        assert!(Arc::ptr_eq(&same, &src));
        // A shrink by more than half takes the area-average path and
        // still keeps its aspect: 4K portrait into 640x360.
        let big = frame(2160, 3840, [0.2, 0.4, 0.6]);
        let out = fit(big, 2160, 3840, 640, 360);
        assert_eq!(px(&out, 640, 10, 180), [0.0; 4], "bars at the sides");
        let c = px(&out, 640, 320, 180);
        assert!(c.iter().zip([0.2, 0.4, 0.6, 1.0]).all(|(a, b)| (a - b).abs() < 1e-4), "{c:?}");
    }
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

/// The temporal noise reduction settings on `clip`, if it has any
/// enabled and they would do something.
pub fn temporal_denoise_of(clip: &Clip) -> Option<(f32, f32)> {
    let enabled = |i: usize| clip.builtin_effects_enabled.get(i).copied().unwrap_or(true);
    clip.builtin_effects.iter().enumerate().find_map(|(i, e)| match e {
        BuiltinEffect::TemporalDenoise { strength, threshold } if enabled(i) && *strength > 0.0 => {
            Some((*strength, *threshold))
        }
        _ => None,
    })
}

/// Whether `clip` frame-blends at `time`: a `SpeedRamp` in blend mode 1,
/// not frozen, with the clip running below real time.
pub fn frame_blends(clip: &Clip, time: Timecode) -> bool {
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
    luma: [f32; 3],
) -> Option<Vec<f32>> {
    use klipp_effects::track_matte as tm;
    let source = rendered.get(&binding.source_clip_id)?;
    let mut mask = tm::extract_mask_linear(source, matte_channel(binding.channel), binding.invert, true, luma);
    tm::apply_choke_mask(&mut mask, width, height, binding.choke_px);
    tm::feather_mask(&mut mask, width, height, binding.feather_px);
    Some(mask)
}

/// Owned pixels, copied only if someone else still holds them.
/// Straight-alpha "over" of `src` onto an opaque `dst`, both RGBA8 at
/// `width` pixels a row, across threads; rows without coverage are
/// skipped. The viewer's fast path lays title sprites onto a presented
/// picture with this, in display space: exact where the text is solid,
/// a hair off the linear composite only across an anti-aliased edge.
pub fn over_u8_straight(dst: &mut [u8], src: &[u8], width: u32) {
    use rayon::prelude::*;
    let row = (width as usize) * 4;
    if row == 0 || dst.len() != src.len() {
        return;
    }
    dst.par_chunks_mut(row).zip(src.par_chunks(row)).for_each(|(d, s)| {
        if s.chunks_exact(4).all(|p| p[3] == 0) {
            return;
        }
        for (dp, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
            let sa = sp[3] as u32;
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dp[0] = sp[0];
                dp[1] = sp[1];
                dp[2] = sp[2];
                continue;
            }
            let inv = 255 - sa;
            for i in 0..3 {
                dp[i] = ((sp[i] as u32 * sa + dp[i] as u32 * inv + 127) / 255) as u8;
            }
        }
    });
}

/// A layer placed on the GPU stack: its own opacity (the track's is
/// applied at the blend) and the rows it covers.
struct Placed {
    opacity: f32,
    rows: (u32, u32),
}

/// A matte channel as the GPU matte pass numbers it.
fn matte_channel_index(ch: klipp_nodes::params::MatteChannel) -> u32 {
    use klipp_nodes::params::MatteChannel as N;
    match ch {
        N::Alpha => 0,
        N::Luminance => 1,
        N::Red => 2,
        N::Green => 3,
        N::Blue => 4,
    }
}

/// The stack's accumulator as a grade chain's source: seeded by an
/// on-GPU copy, read back only if a node insists on CPU pixels.
struct AccSeed {
    buffer: Arc<wgpu::Buffer>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    width: u32,
    height: u32,
}

impl GpuPixels for AccSeed {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_gpu_linear_buffer(&self.buffer, width, height)
    }
    fn read(&self) -> Arc<Vec<f32>> {
        let bytes = (self.width as u64) * (self.height as u64) * 16;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("acc_seed_readback"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("acc_seed_readback") });
        enc.copy_buffer_to_buffer(&self.buffer, 0, &staging, 0, bytes);
        self.queue.submit(std::iter::once(enc.finish()));
        let slice = staging.slice(..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let out = match klipp_effects::gpu::poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let v: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
                drop(data);
                staging.unmap();
                v
            }
            _ => vec![0.0; (self.width as usize) * (self.height as usize) * 4],
        };
        Arc::new(out)
    }
}

/// `KLIPP_GPU_COMPOSITE=0` keeps every frame on the CPU stack.
pub fn gpu_composite_enabled() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| std::env::var("KLIPP_GPU_COMPOSITE").ok().as_deref() != Some("0"))
}

fn unshare(p: Arc<Vec<f32>>) -> Vec<f32> {
    Arc::try_unwrap(p).unwrap_or_else(|p| (*p).clone())
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod frame_index_tests {
    use super::frame_index;

    /// Every NTSC frame time, as a timecode makes it, indexes back to
    /// itself. A bare `(secs * fps) as u64` got one frame in ten wrong.
    #[test]
    fn every_ntsc_frame_time_indexes_to_its_own_frame() {
        let fps = 30000.0 / 1001.0;
        for n in 0..200_000i64 {
            let secs = klipp_core::Timecode { value: n * 1001, rate: 30000 }.to_secs_f64();
            assert_eq!(frame_index(secs, fps), n, "frame {n} at {secs}s");
        }
        for n in 0..200_000i64 {
            let secs = klipp_core::Timecode { value: n, rate: 24 }.to_secs_f64();
            assert_eq!(frame_index(secs, 24.0), n);
        }
    }

    #[test]
    fn a_time_inside_a_frame_is_that_frame() {
        assert_eq!(frame_index(0.5 / 24.0, 24.0), 0);
        assert_eq!(frame_index(1.9 / 24.0, 24.0), 1);
        assert_eq!(frame_index(-0.5, 24.0), -12);
    }
}

#[cfg(test)]
mod stack_order_tests {
    use super::*;
    use klipp_timeline::{Clip, ClipKind, TitleData};

    /// Captions are made on a subtitle track at the bottom of the list,
    /// under the sound. They are still the last thing painted.
    #[test]
    fn a_subtitle_track_is_over_the_picture_wherever_it_is_in_the_list() {
        let tc = |s: f64| Timecode::from_secs_f64(s, 24);
        let mut seq = Sequence::new_default();
        let title = |text: &str| Clip { kind: ClipKind::Title(TitleData::single_text(klipp_timeline::TextBox { text: text.into(), ..Default::default() })), ..Clip::gap(tc(0.0), tc(2.0)) };
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips = vec![title("a name")];
        let mut picture = Track::new("V1", TrackKind::Video);
        picture.clips = vec![Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(2.0)) }];
        let mut subs = Track::new("S1", TrackKind::Subtitle);
        subs.clips = vec![title("a caption")];
        seq.tracks = vec![text, picture, Track::new("A1", TrackKind::Audio), subs];
        let order: Vec<usize> = active_layers(&seq, tc(1.0)).iter().map(|l| l.track).collect();
        assert_eq!(order, vec![1, 0, 3], "bottom first: the picture, the title, and the captions last");
    }
}

#[cfg(test)]
mod temporal_denoise_placement_tests {
    use super::*;
    use klipp_core::ClipId;
    use klipp_timeline::{Clip, ClipKind};

    fn tc(f: i64) -> klipp_core::Timecode {
        klipp_core::Timecode::from_frames(f, 24)
    }

    fn clip_with(effects: Vec<BuiltinEffect>) -> Clip {
        Clip {
            id: ClipId::new(),
            kind: ClipKind::Gap,
            builtin_effects: effects,
            ..Clip::gap(tc(0), tc(48))
        }
    }

    /// The chain must never see it. It ran at the head of the pipeline,
    /// on the source; running again after the grade would average a
    /// graded picture against source neighbours — the exact mistake the
    /// placement exists to avoid.
    #[test]
    fn the_effects_chain_never_receives_it() {
        let clip = clip_with(vec![
            BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.02 },
            BuiltinEffect::Sharpen { amount: 0.5 },
        ]);
        let (fx, _) = clip_effects(&clip, tc(0), 24.0, None);
        assert!(
            !fx.iter().any(|e| matches!(e, BuiltinEffect::TemporalDenoise { .. })),
            "temporal denoise reached the chain: {fx:?}",
        );
        assert!(
            fx.iter().any(|e| matches!(e, BuiltinEffect::Sharpen { .. })),
            "the other effects were lost with it",
        );
    }

    /// And the head of the pipeline has to find it wherever it sits,
    /// because the inspector lets it be dragged anywhere.
    #[test]
    fn the_head_of_the_pipeline_finds_it_at_any_position() {
        let tnr = BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.02 };
        let other = BuiltinEffect::Sharpen { amount: 0.5 };
        for effects in [
            vec![tnr.clone(), other.clone()],
            vec![other.clone(), tnr.clone()],
        ] {
            let clip = clip_with(effects);
            assert_eq!(
                temporal_denoise_of(&clip),
                Some((0.8, 0.02)),
                "not found at this position",
            );
        }
    }

    #[test]
    fn a_disabled_or_zero_strength_one_is_not_applied() {
        let mut clip = clip_with(vec![BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.02 }]);
        clip.builtin_effects_enabled = vec![false];
        assert_eq!(temporal_denoise_of(&clip), None, "a disabled effect ran");

        let off = clip_with(vec![BuiltinEffect::TemporalDenoise { strength: 0.0, threshold: 0.02 }]);
        assert_eq!(temporal_denoise_of(&off), None, "strength 0 still took the CPU route");

        let none = clip_with(vec![BuiltinEffect::Sharpen { amount: 0.5 }]);
        assert_eq!(temporal_denoise_of(&none), None);
    }
}

#[cfg(test)]
mod shutter_tests {
    use super::shutter_offsets;

    #[test]
    fn a_shutter_samples_the_time_it_is_open_and_no_more() {
        // 180 degrees at 24 fps is open for half a frame, 1/48 s, centred.
        let o = shutter_offsets(180.0, 24.0, 5);
        assert_eq!(o.len(), 5);
        assert!((o[4] - o[0] - 1.0 / 48.0).abs() < 1e-12, "spans {} s", o[4] - o[0]);
        assert!((o[0] + o[4]).abs() < 1e-12 && o[2].abs() < 1e-12, "centred on the frame");
        // A full 360 degrees is a whole frame; closed is no spread at all.
        let full = shutter_offsets(360.0, 24.0, 8);
        assert!((full[7] - full[0] - 1.0 / 24.0).abs() < 1e-12);
        assert!(shutter_offsets(0.0, 24.0, 8).iter().all(|t| *t == 0.0));
    }
}
