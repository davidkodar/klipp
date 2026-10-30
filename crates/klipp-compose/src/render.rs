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

use klipp_effects::residency::{self, Work};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use klipp_core::{BlendMode, ClipId, Timecode, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_nodes::NodeId;
pub use crate::inspect::InspectAt;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

use crate::eval::{self, ResolvedTransform};

/// A clip's decoded pixels, before its grade. Shared rather than owned so
/// a source that keeps its frames (the viewer re-renders a paused frame
/// many times) hands them over without a copy; the renderer copies only
/// if it has to write to them.
pub enum SourcePixels {
    /// Display-referred linear RGBA f32, straight alpha, in the working
    /// space: ordinary video.
    Linear(Arc<Vec<f32>>),
    /// Scene-referred codes (PQ, HLG, camera log), as the decoders
    /// produce them: 10-bit, or 16-bit for a deeper source (#1226).
    /// Graded scene-linear, tone-mapped after.
    SceneReferred {
        codes: Arc<klipp_color::hdr::SceneCodes>,
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
#[derive(Clone)]
enum GradeInput {
    Cpu(Arc<Vec<f32>>),
    Gpu(Arc<dyn GpuPixels>),
    Scene {
        codes: Arc<klipp_color::hdr::SceneCodes>,
        transfer: klipp_color::hdr::HdrTransfer,
        source_to_working: [[f32; 3]; 3],
    },
}

impl GradeInput {
    fn pixels(&self) -> Arc<Vec<f32>> {
        match self {
            GradeInput::Cpu(p) => Arc::clone(p),
            GradeInput::Gpu(g) => g.read(),
            GradeInput::Scene { codes, transfer, source_to_working } => Arc::new(codes.to_scene_linear_rgba(*transfer, *source_to_working)),
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

/// A clip's frames on the GPU by clip time (#1988), as
/// [`FrameSource::temporal_frames_on_gpu`] gives them.
pub type GpuFrameProviderFn = Box<dyn FnMut(f64) -> Option<Arc<dyn klipp_nodes::frame_context::GpuFramePixels>> + Send>;

/// Where decoded frames come from. The viewer answers from its playback
/// decoders, export from its sequential ones; the renderer does not care.
/// Frames of a clip's media at other clip times, in linear RGBA f32 in
/// the working space.
/// Shared, not owned: a frame a viewer holds while parked (#1863) is
/// handed out without a copy.
pub type LinearFrameProviderFn =
    Box<dyn FnMut(f64) -> Option<(Arc<Vec<f32>>, u32, u32)> + Send>;

/// Fit a native file reader's linear neighbours as the renderer fits its
/// current frame: one scale factor, centred, with transparent bars. Missing or
/// malformed samples remain missing, never a replacement of the current frame.
pub fn fit_linear_reader(mut reader: LinearFrameProviderFn, width: u32, height: u32) -> LinearFrameProviderFn {
    Box::new(move |time| {
        let (pixels, w, h) = reader(time)?;
        Some((fit_linear_shared(pixels, w, h, width, height)?, width, height))
    })
}

/// A linear frame `w × h` fitted to `width × height` as
/// [`fit_linear_reader`] fits a neighbour: one scale factor, centred, with
/// transparent bars. `None` for a malformed frame.
pub fn fit_linear(pixels: Vec<f32>, w: u32, h: u32, width: u32, height: u32) -> Option<Vec<f32>> {
    fit_linear_shared(Arc::new(pixels), w, h, width, height).map(unshare)
}

/// [`fit_linear`] on a shared frame: one already at the size comes back
/// as the same buffer, not a copy (#1863).
pub fn fit_linear_shared(pixels: Arc<Vec<f32>>, w: u32, h: u32, width: u32, height: u32) -> Option<Arc<Vec<f32>>> {
    let len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if w == 0 || h == 0 || width == 0 || height == 0 || pixels.len() != len {
        return None;
    }
    Some(fit(pixels, w, h, width, height))
}

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
    /// [`Self::frame_after`]'s frame where the source holds it on the GPU
    /// at `width` × `height`, in `working` (#2854): its presentation time
    /// and its pixels as a frame blend on the chain binds them. `None`
    /// (the default) where it hasn't it there: the blend reads
    /// [`Self::frame_after`] then.
    fn frame_after_on_gpu(&mut self, _clip: &Clip, _source_secs: f64, _working: WorkingColorspace, _width: u32, _height: u32) -> Option<(f64, Arc<dyn klipp_nodes::frame_context::GpuFramePixels>)> {
        None
    }

    fn frame_after(&mut self, _clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        None
    }

    /// The file a still image's frames are read from, stamped now, when
    /// `clip` shows one (#2092): its picture is the same every frame while
    /// this is, so the GPU stack keeps it and asks for no frame. `None`
    /// (the default) reads every frame.
    fn still_file(&mut self, _clip: &Clip) -> Option<HeldFile> {
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

    /// Linear neighbours for a render at this size. Synthetic providers retain
    /// their declared sizes so a malformed neighbour still fails the node's
    /// bounds check. File readers override this to fit their native samples.
    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, working: WorkingColorspace, _width: u32, _height: u32) -> Option<LinearFrameProviderFn> {
        self.temporal_frames_linear(clip, working)
    }

    /// [`Self::temporal_frames_linear_at_size`]'s frames where the source
    /// holds them on the GPU already, linear in `working` at `width` ×
    /// `height` on the render's device (#1988): a temporal denoise on the
    /// chain binds them as they are. `None` (the default), or a time it
    /// gives `None` for, and the neighbour is read the CPU way.
    fn temporal_frames_on_gpu(&mut self, _clip: &Clip, _working: WorkingColorspace, _width: u32, _height: u32) -> Option<GpuFrameProviderFn> {
        None
    }

    /// The source times `clip`'s media can be read at, `[start, end)` in
    /// source seconds: the whole media, the clip's handles included.
    /// `None` (the default) when the source doesn't know; then only the
    /// readers can say a time has no frame.
    fn media_range(&mut self, _clip: &Clip) -> Option<(f64, f64)> {
        None
    }

    /// The frame rate of `clip`'s media, when the source knows it: a
    /// temporal node's neighbours are its captures either side. `None`
    /// (the default): the sequence's rate stands in.
    fn media_fps(&mut self, _clip: &Clip) -> Option<f64> {
        None
    }

    /// The key of `kind`'s analysis of `clip`'s media (M4 steps 8 and 9):
    /// the original file's, whatever the source decodes. `None` (the
    /// default) when it can't say: the clip's analysis nodes then have
    /// none.
    fn analysis_key(&mut self, _clip: &Clip, _kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        None
    }

    /// The file `clip`'s frames are decoded from, for a viewer parked on
    /// a frame to hold its heavy nodes' outputs by (#1863). `None` (the
    /// default) holds nothing: export, playback, and any source but a
    /// parked viewer's.
    fn held_file(&mut self, _clip: &Clip) -> Option<HeldFile> {
        None
    }

    /// The folder of the subject matte `model` made of `clip`'s media
    /// (#985 phase 3; 0 the subject, 1 the foreground), by the media as it
    /// is linked now. `None` (the default) when the source can't say: a
    /// transition cutting round the subject then declines.
    fn subject_matte_dir(&mut self, _clip: &Clip, _model: u32) -> Option<std::path::PathBuf> {
        None
    }

    /// Another media item's frame at `secs` in it (a media source, #1746),
    /// by the pool item's id: linear RGBA f32 in `working`, fitted to
    /// `width × height` ([`fit_linear`]). The default reads no other
    /// media.
    fn media_frame(&mut self, _media: &str, _secs: f64, _working: WorkingColorspace, _width: u32, _height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        Err(klipp_nodes::side_picture::SideMissing::Unavailable)
    }

    /// The transfer another media item's frames decode scene-referred
    /// through (HDR or camera log): a media source gives its frame that
    /// display step (#1857). `None` (the default) for an SDR item.
    fn media_transfer(&mut self, _media: &str) -> Option<klipp_color::hdr::HdrTransfer> {
        None
    }
}

/// The file a parked viewer decodes a clip's frames from (#1863): the
/// part of a held output's key the frame source knows.
#[derive(Clone, Debug, PartialEq)]
pub struct HeldFile {
    pub media: klipp_core::MediaId,
    pub path: std::path::PathBuf,
    pub len: u64,
    pub modified: std::time::SystemTime,
    pub log_format: Option<klipp_core::LogFormat>,
    /// The proxy the frame is shown from, stamped alike, when it is: the
    /// neighbours stay the original's.
    pub proxy: Option<(std::path::PathBuf, u64, std::time::SystemTime)>,
}

impl HeldFile {
    /// `path` as it is on disk now, and `proxy` when the frame is shown
    /// from one; `None` when either can't be read.
    pub fn stamp(media: klipp_core::MediaId, path: std::path::PathBuf, log_format: Option<klipp_core::LogFormat>, proxy: Option<std::path::PathBuf>) -> Option<Self> {
        let stamp = |p: &std::path::Path| {
            let meta = std::fs::metadata(p).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        };
        let (len, modified) = stamp(&path)?;
        let proxy = match proxy {
            Some(p) => {
                let (len, modified) = stamp(&p)?;
                Some((p, len, modified))
            }
            None => None,
        };
        Some(Self { media, len, modified, path, log_format, proxy })
    }
}

/// The analysis asset `effect` reads, if it reads one.
pub fn asset_kind(effect: &BuiltinEffect) -> Option<klipp_analysis::assets::AssetKind> {
    use klipp_analysis::assets::AssetKind;
    match effect {
        BuiltinEffect::Deflicker { .. } => Some(AssetKind::Deflicker),
        BuiltinEffect::VideoStabilization { .. } => Some(AssetKind::Stabilisation),
        BuiltinEffect::RollingShutterCorrection { .. } => Some(AssetKind::RollingShutter),
        _ => None,
    }
}

/// What an analysis effect's derivation depends on: its kind and the
/// settings it takes.
fn derivation_of(effect: &BuiltinEffect) -> Option<(klipp_analysis::assets::AssetKind, u64)> {
    Some((asset_kind(effect)?, klipp_effects::node_analysis::derivation_params(effect)?))
}

/// What a read of an analysis asset found, kept by the asset's stamp.
#[derive(Clone)]
enum Kept {
    Absent,
    /// By another algorithm, or one whose data won't derive: owed again.
    Stale,
    Ready(Arc<klipp_effects::node_analysis::NodeAnalysis>),
}

/// One clip's analyses for a frame (M4 step 9), by the effect that reads
/// each: ready, with its settings applied, or stale (never read). An
/// effect with neither has no analysis.
#[derive(Default)]
pub struct ClipAnalysis {
    found: Vec<((klipp_analysis::assets::AssetKind, u64), Option<klipp_nodes::frame_context::AnalysisBlob>)>,
}

impl klipp_nodes::frame_context::AnalysisInputs for ClipAnalysis {
    fn lookup(&self, _node: NodeId, _schema: u32) -> klipp_nodes::frame_context::Analysis<'_> {
        klipp_nodes::frame_context::Analysis::Absent
    }

    fn lookup_effect(&self, _node: NodeId, effect: &BuiltinEffect, schema: u32) -> klipp_nodes::frame_context::Analysis<'_> {
        use klipp_nodes::frame_context::Analysis;
        let Some(wanted) = derivation_of(effect) else { return Analysis::Absent };
        match self.found.iter().find(|(k, _)| *k == wanted) {
            None => Analysis::Absent,
            Some((_, None)) => Analysis::Stale { found: 0 },
            Some((_, Some(blob))) if blob.schema != schema => Analysis::Stale { found: blob.schema },
            Some((_, Some(blob))) => Analysis::Ready(blob),
        }
    }
}

/// A clip's source as a non-pure node reads it (M4 step 1): the frame
/// source's neighbour readers, asked in source seconds over the media's
/// range ([`FrameSource::media_range`]), the clip's handles included. A
/// neighbour past the clip's in or out point is a real frame of the
/// media, and a temporal node reads it: bounded to the trim, the first and
/// last frame of every trimmed clip would lose a neighbour and step at the
/// cut. Out of range is only past the media itself. The encoded frames are [`FrameSource::temporal_frames`]'
/// (sRGB, for a plug-in) and the linear ones
/// [`FrameSource::temporal_frames_linear`]'s (the working space, for the
/// denoiser), never one for the other. Each reader is opened on its first
/// request. Held to the render's size and a node's budget by
/// [`klipp_nodes::frame_context::BoundedSources`].
pub struct ClipSources<'s> {
    src: &'s mut dyn FrameSource,
    clip: &'s Clip,
    working: WorkingColorspace,
    range: (f64, f64),
    /// One of the media's frames, in seconds, when known.
    frame_secs: Option<f64>,
    /// The rendered frame: its source time, transfer and the render size,
    /// for its next frame.
    current: Option<(f64, Option<klipp_color::hdr::HdrTransfer>, u32, u32)>,
    encoded: Option<Option<klipp_plugin::FrameProviderFn>>,
    linear: Option<Option<LinearFrameProviderFn>>,
    on_gpu: Option<Option<GpuFrameProviderFn>>,
}

impl<'s> ClipSources<'s> {
    pub fn new(src: &'s mut dyn FrameSource, clip: &'s Clip, working: WorkingColorspace) -> Self {
        // A clip with no media (a title, a generator) has no source: an
        // empty range, so a node asking for another frame runs as a
        // fallback (as `NoSources`), never as a frame that failed to read.
        // Media whose range the source can't say is read anywhere.
        let range = match clip.media_id() {
            None => (0.0, 0.0),
            Some(_) => src.media_range(clip).unwrap_or((0.0, f64::INFINITY)),
        };
        let frame_secs = src.media_fps(clip).filter(|f| f.is_finite() && *f > 0.0).map(|f| 1.0 / f);
        Self { src, clip, working, range, frame_secs, current: None, encoded: None, linear: None, on_gpu: None }
    }

    /// The frame being rendered, shown at `source_secs` with `hdr`'s
    /// transfer, at `width × height`: its next frame comes from the same
    /// reader ([`FrameSource::frame_after`]), with no second seek.
    pub fn with_current(mut self, source_secs: f64, hdr: Option<klipp_color::hdr::HdrTransfer>, width: u32, height: u32) -> Self {
        self.current = Some((source_secs, hdr, width, height));
        self
    }

    /// The readers' time for `source_secs`: from the clip's source in-point.
    fn reader_time(&self, source_secs: f64) -> f64 {
        source_secs - self.clip.source_in.to_secs_f64()
    }
}

impl klipp_nodes::frame_context::SourceProvider for ClipSources<'_> {
    fn bounds(&self) -> (f64, f64) {
        self.range
    }

    fn frame_secs(&self) -> Option<f64> {
        self.frame_secs
    }

    fn encoded(&mut self, source_secs: f64) -> klipp_nodes::frame_context::Neighbour<klipp_nodes::frame_context::EncodedFrame> {
        use klipp_nodes::frame_context::{EncodedFrame, Missing, Neighbour};
        if self.encoded.is_none() {
            self.encoded = Some(self.src.temporal_frames(self.clip));
        }
        let t = self.reader_time(source_secs);
        match self.encoded.as_ref().and_then(|r| r.as_ref()).and_then(|read| read(t)) {
            // Fitted to the render size, as the frame beside it was: a
            // viewer drawing below the media's size (or from a proxy)
            // otherwise gave a plug-in no neighbours at all (#2013).
            Some((rgba8, width, height)) => match self.current {
                Some((_, _, w, h)) if (w, h) != (width, height) => {
                    let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&rgba8);
                    match fit_linear(linear, width, height, w, h) {
                        Some(fitted) => Neighbour::Frame(EncodedFrame { rgba8: klipp_color::ofx::encode(&fitted, WorkingColorspace::Rec709Linear), width: w, height: h, source_secs }),
                        None => Neighbour::Missing(Missing::Unavailable),
                    }
                }
                _ => Neighbour::Frame(EncodedFrame { rgba8, width, height, source_secs }),
            },
            None => Neighbour::Missing(Missing::Unavailable),
        }
    }

    fn linear(&mut self, source_secs: f64) -> klipp_nodes::frame_context::Neighbour<klipp_nodes::frame_context::LinearFrame> {
        use klipp_nodes::frame_context::{LinearFrame, Missing, Neighbour};
        if self.linear.is_none() {
            self.linear = Some(match self.current {
                Some((_, _, width, height)) => self.src.temporal_frames_linear_at_size(self.clip, self.working, width, height),
                None => self.src.temporal_frames_linear(self.clip, self.working),
            });
        }
        let t = self.reader_time(source_secs);
        match self.linear.as_mut().and_then(|r| r.as_mut()).and_then(|read| read(t)) {
            Some((rgba, width, height)) => Neighbour::Frame(LinearFrame { rgba, width, height, source_secs, pts: None }),
            None => Neighbour::Missing(Missing::Unavailable),
        }
    }

    /// The frame on the GPU, at the render size, where the source holds it
    /// there (#1988); only for a render, which knows its size.
    fn linear_on_gpu(&mut self, source_secs: f64) -> klipp_nodes::frame_context::Neighbour<klipp_nodes::frame_context::GpuLinearFrame> {
        use klipp_nodes::frame_context::{GpuLinearFrame, Missing, Neighbour};
        let Some((_, _, width, height)) = self.current else { return Neighbour::Missing(Missing::Unavailable) };
        if self.on_gpu.is_none() {
            self.on_gpu = Some(self.src.temporal_frames_on_gpu(self.clip, self.working, width, height));
        }
        let t = self.reader_time(source_secs);
        match self.on_gpu.as_mut().and_then(|r| r.as_mut()).and_then(|read| read(t)) {
            Some(pixels) => Neighbour::Frame(GpuLinearFrame { pixels, width, height, source_secs }),
            None => Neighbour::Missing(Missing::Unavailable),
        }
    }

    /// The reader's next frame, fitted to the render size as the frame
    /// shown was (ruling 10-09). Another transfer than the frame shown's,
    /// or no time, isn't a frame to blend toward.
    /// The frame after the one at `source_secs`: the node asks after the
    /// frame it shows, which a blend reads half a media frame early.
    fn next(&mut self, source_secs: f64) -> klipp_nodes::frame_context::Neighbour<klipp_nodes::frame_context::LinearFrame> {
        use klipp_nodes::frame_context::{LinearFrame, Missing, Neighbour};
        let Some((_, hdr, width, height)) = self.current else { return Neighbour::Missing(Missing::Unavailable) };
        let Some(next) = self.src.frame_after(self.clip, source_secs, width, height) else { return Neighbour::Missing(Missing::Unavailable) };
        let Some(pts) = next.pts else { return Neighbour::Missing(Missing::Unavailable) };
        let (rgba, next_hdr) = to_linear(next, width, height);
        if next_hdr != hdr {
            return Neighbour::Missing(Missing::Unavailable);
        }
        Neighbour::Frame(LinearFrame { rgba, width, height, source_secs: pts, pts: Some(pts) })
    }

    /// [`Self::next`] where the source holds it on the GPU (#2854): only
    /// a display-referred frame's, as the frame shown was.
    fn next_on_gpu(&mut self, source_secs: f64) -> klipp_nodes::frame_context::Neighbour<klipp_nodes::frame_context::GpuLinearFrame> {
        use klipp_nodes::frame_context::{GpuLinearFrame, Missing, Neighbour};
        let Some((_, None, width, height)) = self.current else { return Neighbour::Missing(Missing::Unavailable) };
        match self.src.frame_after_on_gpu(self.clip, source_secs, self.working, width, height) {
            Some((pts, pixels)) => Neighbour::Frame(GpuLinearFrame { pixels, width, height, source_secs: pts }),
            None => Neighbour::Missing(Missing::Unavailable),
        }
    }
}

/// What the viewer's grade tools want from one clip's grade besides the
/// picture: the node canvas's thumbnails, the scope's tap point, a soloed
/// node, a qualifier's matte, the before/after split's "before" half.
/// Only `clip` is inspected; every other clip renders plainly.
#[derive(Default, Clone)]
pub struct Inspect {
    pub clip: Option<ClipId>,
    /// Node outputs to capture as thumbnails, by path (M3 step 16a).
    pub thumbs: Option<HashSet<InspectAt>>,
    /// The node output the scope reads instead of the graph's.
    pub scope_tap: Option<InspectAt>,
    /// The node whose matte the grade shows instead of the picture.
    pub matte_view: Option<InspectAt>,
    /// The node output the grade shows instead of the graph's.
    pub solo: Option<InspectAt>,
    /// Also return the clip's picture before its grade.
    pub ungraded: bool,
    /// A clip whose track-matte mask to return (the inspector's matte
    /// preview). Independent of `clip`.
    pub matte_of: Option<ClipId>,
    /// Render this clip alone: its picture graded, with its effects and
    /// placement, at full opacity, without its track matte or anything
    /// above or below it. What a colour measurement of the clip reads, so
    /// a title, a matte or a colour card over it is not taken for the
    /// footage (#719).
    pub only: Option<ClipId>,
}

impl Inspect {
    /// Whether the grade tools read inside the inspected clip's grade:
    /// its node thumbnails, the scope's tap, a matte view or a soloed
    /// node. Naming a clip alone reads nothing inside it.
    pub fn reads_the_grade(&self) -> bool {
        self.thumbs.is_some() || self.scope_tap.is_some() || self.matte_view.is_some() || self.solo.is_some()
    }
}

/// What an inspected render found. Fields are empty when not asked for,
/// or when the clip was not on screen.
#[derive(Default)]
pub struct Inspection {
    pub scope_tap: Option<Vec<f32>>,
    pub thumbs: HashMap<InspectAt, Vec<u8>>,
    /// Each request that names nothing the render ran, and why: never
    /// answered with another node's output or the picture's (M3 step 16a).
    pub unavailable: Vec<(InspectAt, String)>,
    /// The inspected clip before its grade: display-referred linear RGBA
    /// at the output size (scene-referred sources tone-mapped).
    pub ungraded: Option<Vec<f32>>,
    /// `matte_of`'s mask, one value a pixel at the output size.
    pub matte_mask: Option<Vec<f32>>,
    /// The nodes this frame's graphs didn't draw as asked, and why (M4
    /// step 10): every clip's, in the order they ran. An export refuses a
    /// frame with any that [`fails`](klipp_nodes::Decline::fails).
    pub declines: Vec<klipp_nodes::Decline>,
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
    /// Neither path can take it, and why.
    Refused(&'static str),
    /// More than one clip's grade: the stack's frame.
    ForTheStack,
}

impl From<Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)>> for Single {
    fn from(planes: Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)>) -> Self {
        planes.map_or(Single::Refused("the Y'CbCr encode failed"), Single::Planes)
    }
}

/// Clip renders whose effects ran as nodes of the grade's plan, read from
/// the clip's lifted picture, and those whose picture could not give them
/// (the list stood in), in this process: `RenderStats`' two counts summed
/// over every renderer, for a reader with no renderer of its own (the
/// control socket's `perf`). Cumulative; a reader takes deltas.
pub static PLAN_EFFECT_CLIPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PLAN_EFFECT_FALLBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Effect runs a plan couldn't apply (a lost device, a failed run) that
/// the CPU kernels drew instead (`RenderStats::effects_on_cpu`), in this
/// process. Cumulative; zero unless something failed.
pub static EFFECTS_ON_CPU: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Clip effect runs drawn on the CPU, for any reason: the CPU kernels (no
/// GPU, an effect with no GPU kernel, a plan that couldn't apply them) or
/// the CPU executor running a failed plan again
/// (`RenderStats::effects_on_cpu_kernels`), in this process (#1274).
/// Cumulative.
pub static EFFECTS_ON_CPU_KERNELS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Masked effects blended on the CPU in a run apart from their grade
/// (`RenderStats::effects_blended_on_cpu`), in this process.
pub static EFFECTS_BLENDED_ON_CPU: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Masked effects in a run apart from their grade whose window ran in
/// their own plan, blended there (`RenderStats::effects_blended_apart`,
/// #1265), in this process.
pub static EFFECTS_BLENDED_APART: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Frames the GPU stack declined to the CPU composite, in this process,
/// for whatever reason (`Renderer::frame_decline` says which for the last).
/// Cumulative; a reader takes deltas.
pub static GPU_STACK_DECLINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Masked effects whose matte their run couldn't give
/// (`RenderStats::effects_mask_unavailable`), in this process.
pub static EFFECT_MASKS_UNAVAILABLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Native living-gradient draws completed on the GPU, process-wide.
pub static GPU_GENERATOR_DRAWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Time spent rasterizing CPU generator pixels, process-wide microseconds.
pub static CPU_GENERATOR_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Generator rasterizations (cache hits excluded), process-wide.
pub static CPU_GENERATOR_DRAWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where a render's time went, in milliseconds.
/// Where the GPU stack's grade time went, summed over the frame's layers
/// (#2107): the effects spliced into each clip's plan, its analyses
/// looked up, the executor set up for the clip, its picture seeded on the
/// GPU, and its graph run. In ms.
#[derive(Default, Clone, Copy, Debug)]
pub struct GradeSplit {
    pub splice_ms: f64,
    pub analyses_ms: f64,
    pub prepare_ms: f64,
    pub seed_ms: f64,
    pub graph_ms: f64,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct RenderStats {
    /// Native generators drawn without a CPU pixel buffer this frame.
    pub gpu_generators: u32,
    /// CPU generator rasterization (including cache lookup), before upload.
    pub cpu_generator_ms: f64,
    pub grade_ms: f64,
    pub ofx_ms: f64,
    pub effects_ms: f64,
    pub lut_ms: f64,
    pub composite_ms: f64,
    pub total_ms: f64,
    /// Layers the GPU stack had no seed for, rendered on the CPU and
    /// uploaded: the slow way in.
    pub cpu_layers: u32,
    /// Media layers sent up to the GPU stack from CPU pixels this frame.
    pub layers_uploaded: u32,
    /// Still images whose layer was put back from the GPU instead (#2092).
    pub stills_kept: u32,
    /// Still images read, fitted and sent up this frame: a miss of the
    /// kept layers, which after a still's first frame is a fault (#2102).
    pub stills_uploaded: u32,
    /// The GPU stack's grade time, by part (#2107).
    pub grade_split: GradeSplit,
    /// Clips whose effects ran as nodes of the grade's plan, on the GPU
    /// with the grade (#1089, M2 step 10).
    pub effects_followed_grade: u32,
    /// Clips with plug-ins whose effects ran as a plan of their own after
    /// the plug-ins, which run on the CPU between the grade and the
    /// effects (M2 step 11c-2).
    pub effects_after_plugins: u32,
    /// Clips whose grade the tools read inside (thumbnails, the scope's
    /// tap, a matte view, a solo) whose effects ran as a plan of their own
    /// after the grade, on the GPU (#1274).
    pub effects_inspected_apart: u32,
    /// Clips under a shutter (the camera's, or a title's own motion blur)
    /// whose effects ran as plans of their own around the sampling: the
    /// ones before the transform, the transform at each sample, the rest
    /// after (M2 step 11c-3).
    pub effects_shuttered: u32,
    /// Layers whose effects ran as a plan of their own over the layer: an
    /// adjustment layer on the CPU composite, a title or a generator
    /// placed on the GPU stack (M2 step 11d).
    pub effects_on_layers: u32,
    /// Clips on the plan route whose lifted picture could not give their
    /// effects, so `clip_effects`' list stood in (M2 step 10c). Zero
    /// unless a lift is broken.
    pub effects_fallback_lists: u32,
    /// Effect runs a plan couldn't apply (a lost device, a failed run),
    /// drawn by the CPU kernels instead, so the frame never shows without
    /// its effects (M2 step 11d). Zero unless something failed.
    pub effects_on_cpu: u32,
    /// Clip effect runs drawn on the CPU, for any reason: by the CPU
    /// kernels, or by the CPU executor running a failed plan again;
    /// failures (`effects_on_cpu`) included (#1274).
    pub effects_on_cpu_kernels: u32,
    /// Masked effects whose matte their run couldn't give (an effect run
    /// apart from the grade it reads, after a plug-in or across a shutter;
    /// wiring a file holds that the renderer can't give): drawn bypassed,
    /// the picture as it came in, and said (step 9c-2). Zero unless one is.
    pub effects_mask_unavailable: u32,
    /// Masked effects in a run apart from the grade they read (after a
    /// plug-in, across a shutter, an adjustment layer's): the matte made
    /// in a side run of its producer, the effect run alone, the two
    /// blended on the CPU (step 9c-2 B2). What a window can't give: a
    /// key's or a matte's from the picture the grade reads.
    pub effects_blended_on_cpu: u32,
    /// Masked effects in a run apart from their grade whose matte is a
    /// window, which reads no picture: the window drawn in the effect's
    /// own plan and the effect blended there, on the GPU where there is
    /// one, with nothing downloaded between them (#1265).
    pub effects_blended_apart: u32,
}

impl RenderStats {
    /// Clips whose effects ran as plans, by any route: spliced into the
    /// grade's run, after a plug-in, or around a shutter's samples.
    pub fn plan_clips(&self) -> u32 {
        self.effects_followed_grade + self.effects_after_plugins + self.effects_inspected_apart + self.effects_shuttered + self.effects_on_layers
    }
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
            if let Some(TrackItem::Transition { a, b, .. }) = transition_at(seq, track, tr, time) {
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
        for r in c.picture_references() {
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
            // Its clips carry them already when `seq` is a render copy (the
            // renderer reads them there), but inspection by path finds a
            // node of the track's grade by them (#1655).
            effects: t.effects.clone(),
            ..Track::new(&t.name, t.kind)
        })
        .collect();
    Sequence {
        id: seq.id,
        name: seq.name.clone(),
        camera: seq.camera.clone(),
        timeline_graph: seq.timeline_grade().cloned().map(Into::into),
        grade_groups: seq.grade_groups.clone(),
        title_nits: seq.title_nits,
        // The render reads no labels.
        node_labels: Vec::new(),
        drop_frame: seq.drop_frame,
        fps: seq.fps,
        resolution: seq.resolution,
        first_clip_autoset: seq.first_clip_autoset,
        tracks,
        in_point: seq.in_point,
        out_point: seq.out_point,
        markers: Vec::new(),
        last_playhead: None,
        brief: None,
        edit_plan: None,
        reviews: Vec::new(),
        awaiting_refit: false,
        version_of: None,
    }
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
        .filter_map(|(i, t)| active_item(seq, t, time).map(|item| (i, t, item)))
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
    /// What the viewer shows of the finished frame (#2758): the picture,
    /// or one channel of it in grey. Only the viewer sets it; a
    /// precomposition's scene is always the picture.
    pub view_channel: klipp_core::ViewChannel,
}

/// `channel` as the channel view's CPU twin and shader take it.
fn channel_code(channel: klipp_core::ViewChannel) -> u32 {
    use klipp_core::ViewChannel as V;
    match channel {
        V::Picture => 0,
        V::Red => 1,
        V::Green => 2,
        V::Blue => 3,
        V::Alpha => 4,
        V::Luma => 5,
    }
}

/// Kept scenes' buffers held spare (#2757): one done with is taken by the
/// next scene composed, so a couple is all a nest ever waits on.
const SPARE_SCENES: usize = 2;

/// The resources rendering needs, owned per thread: the viewer has one,
/// every export job has its own.
pub struct Renderer {
    executor: klipp_nodes::GraphExecutor,
    gpu: Option<klipp_effects::gpu::GpuEffectsPipeline>,
    gpu_tried: bool,
    allow_gpu: bool,
    /// The clip's resolved effects for the grade being run, spliced in as
    /// nodes before its output (step 10b); empty runs the grade alone.
    run_effects: Vec<BuiltinEffect>,
    /// `run_effects`' mixes and masks, by place (a picture-truth clip's,
    /// step 9c-2); `None`, or shorter, for effects with neither.
    run_blends: Vec<Option<crate::resolve::Blend>>,
    /// The mixes and masks of the effects the last `plan_effects` gave,
    /// by place, for its caller to take.
    planned_blends: Vec<Option<crate::resolve::Blend>>,
    /// The picture node each planned effect of a clip came from (M3 step
    /// 16a), for its spliced node's id.
    planned_nodes: HashMap<ClipId, Vec<Option<klipp_nodes::NodeId>>>,
    /// Each clip's effect nodes dropped from its run as identities, with
    /// what feeds them there: what an inspection of one shows (#1314).
    identity_feeds: HashMap<ClipId, Vec<(klipp_nodes::NodeId, crate::resolve::IdentityFeed)>>,
    /// The node of `spliced` that gives the grade's output, before the
    /// effects and the display step spliced after it.
    spliced_graded: Option<klipp_nodes::NodeId>,
    /// The inspection being rendered, resolved to the nodes that run.
    resolved: crate::inspect::Resolved,
    /// An executor on the CPU alone: a masked or mixed clip's plan runs on
    /// it again when its GPU run can't apply an effect, since the CPU
    /// kernels can't mask or mix (step 9c-2).
    cpu_executor: Option<klipp_nodes::GraphExecutor>,
    /// The picture the clip being rendered gives its grade (an adjustment
    /// layer's: what is below it), kept while its effects run apart from
    /// its grade, for a masked effect's matte to be made from (9c-2 B2).
    matte_input: Option<Arc<Vec<f32>>>,
    /// A scene-referred clip's display step for the grade being run,
    /// spliced in after the grade and before `run_effects` (step 11c).
    run_display: Option<klipp_color::hdr::SceneToDisplay>,
    /// What the tracks below composite to, while a clip that reads it is
    /// drawn in the stacking loop (#1746): a Clip Below's picture.
    below: Option<Arc<Vec<f32>>>,
    /// What is below the clip the GPU stack is laying now (#2758): the
    /// composite so far, kept on the GPU for its run to read.
    stack_below: Option<klipp_effects::gpu::KeptPicture>,
    /// Where analysis assets are kept (M4 step 8): the analysis cache.
    assets: Box<dyn klipp_analysis::assets::AssetStore + Send>,
    /// Analyses derived from their assets with an effect's settings, each
    /// with the stamp of what was kept when it was read: kept while the
    /// stamp and the settings hold. Bounded.
    derived: HashMap<(klipp_analysis::assets::AssetKey, u64), (u64, Kept)>,
    /// The clip being rendered and its analyses, for each of its runs.
    clip_analysis: Option<(ClipId, ClipAnalysis)>,
    /// The source-boundary nodes for the grade being run (M4 step 6):
    /// spliced in straight after the Source, before the grade, in order.
    /// One node; one per effect, under its picture node's id, when the
    /// grade tools read the clip (#1305).
    run_source_nodes: Vec<klipp_nodes::Node>,
    /// The size the clip being drawn was decoded at, before fitting:
    /// part of its held outputs' key (#1863).
    source_decoded: Option<(u32, u32)>,
    /// The grade being run reads a scene-referred clip's picture (HDR or
    /// log media, decoded to scene light on the CPU): its Source is
    /// scene-referred for the executor's colour domains (#1867).
    grade_source_scene: bool,
    /// Each still generator's layer on the GPU stack, by clip, as it was
    /// last rasterised: its data, the size, and the half-float pixels. A
    /// solid colour, a gradient, bars, a slate or a placeholder is the same
    /// every frame at one size; rasterising and packing a 720p one cost
    /// 2.6 ms a frame (#1850).
    still_generators: HashMap<ClipId, StillGenerator>,
    /// Each still image's picture on the GPU (#2092), by clip: seeded from
    /// there on later frames while its file (`FrameSource::still_file`),
    /// the frame size and the working space are the same. A changed file
    /// is read again.
    still_layers: HashMap<ClipId, StillLayer>,
    /// Each precomposition's scene composed on the stack before its
    /// parent's, kept on the GPU to seed its layer (#2757), by clip; the
    /// buffers reused from frame to frame. `precomp_ready` holds those
    /// composed this frame.
    precomp_scenes: HashMap<ClipId, Arc<wgpu::Buffer>>,
    /// Each media clip's source size as last read, and the render's size
    /// then (#2757): what a crop to a shape is worked out from, a kept
    /// still's too.
    source_sizes: HashMap<ClipId, ((u32, u32), (u32, u32))>,
    precomp_ready: std::collections::HashSet<ClipId>,
    /// Kept scenes' buffers done with this frame, at most [`SPARE_SCENES`],
    /// for the next scene
    /// composed to take: a scene inside another is needed only while that
    /// one is composed, and a nest of templates kept one of each, a full
    /// frame of the stack per level per instance.
    precomp_spare: Vec<Arc<wgpu::Buffer>>,
    /// The first of each clip's stack layers its title sources are drawn
    /// in this frame (#2758), after the layers of the stack.
    side_title_slots: HashMap<ClipId, usize>,
    /// Clean frames read for difference keys (#2758), by their key and
    /// the size they were read at; a few kept.
    clean_frames: HashMap<(klipp_nodes::side_picture::SideKey, u32, u32), Arc<Vec<f32>>>,
    /// This stack frame's grade time by part, as its layers run (#2107).
    grade_split: GradeSplit,
    /// The subject matte sequences piece transitions read (#985 phase 3):
    /// each folder's manifest and frames, as last looked.
    subject_mattes: HashMap<std::path::PathBuf, (klipp_nodes::matte_sequence::Manifest, Vec<i64>)>,
    /// Each clip's tracked subject mattes' folders by model, as its frame
    /// source found them from its media as linked now (#1881), for its
    /// grade's External Mattes bound to a subject.
    clip_subject_dirs: HashMap<ClipId, [Option<std::path::PathBuf>; 2]>,
    /// The last frame of each clip's media whose grade reads a matte
    /// sequence, seconds: read there past the media's end.
    clip_media_last: HashMap<ClipId, f64>,
    /// Effect runs that couldn't be applied as a plan of their own and
    /// took the CPU kernels instead (`effects_by`), for the stats' counts.
    plan_misses: u64,
    /// The grade with `run_effects` spliced in, kept so each frame reuses
    /// its allocations.
    spliced: klipp_nodes::NodeGraph,
    /// The source straight to the output: the base an identity grade's
    /// effects are spliced into (step 11b). Its ids are fixed, so the
    /// executor's plan cache keys it by its effects alone.
    passthrough: klipp_nodes::NodeGraph,
    /// Whether the last `grade` ran its graph to the end. A run that
    /// failed returns the input with nothing applied, its effects
    /// included, so a spliced run that failed is never taken as done.
    grade_ran: bool,
    /// The OFX host, shared with the plug-in instances made from it: each
    /// keeps it alive, so it never goes before them (M4 step 3).
    ofx: std::sync::Arc<std::sync::Mutex<klipp_plugin::OfxHost>>,
    titles: klipp_title::TitleRenderer,
    gpu_text3d: Option<crate::title3d_gpu::GpuText3d>,
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
    /// Each title with a motion blur of its own: its transform at every
    /// sample of its shutter, resolved as the camera shutter's are (#995).
    title_shutter: HashMap<ClipId, Vec<(ResolvedTransform, f64)>>,
    /// Pinned clips' corners on their surfaces this frame (#684), resolved
    /// with the transforms.
    pins: HashMap<ClipId, klipp_timeline::surface::Quad>,
    /// How much of each pinned clip shows where its surface's track is
    /// lost (#2751), with `pins`: absent is all of it.
    pin_presence: HashMap<ClipId, f32>,
    /// What each pinned clip taking its surface's light is multiplied by
    /// this frame (#2751), with `pins`: absent is 1.
    pin_light: HashMap<ClipId, f32>,
    working: WorkingColorspace,
    grade_encoding: klipp_core::GradeEncoding,
    /// The frame's motion from its analyses, for the CPU kernels'
    /// stabilisation and rolling-shutter correction while
    /// `effects_analysed` runs them; otherwise none.
    motion: klipp_effects::MotionData,
    /// Why the GPU stack last declined a frame, so the log says it once.
    last_gpu_stack_decline: Option<&'static str>,
    /// The LUT files read and the colour-space transforms worked out for
    /// the effects that need them (#1746).
    colour_effects: std::sync::Mutex<crate::colour_effects::ColourEffects>,
    /// Why the GPU declined the last frame asked of it, or `None` when it
    /// took it; see [`Self::last_decline`].
    frame_decline: Option<&'static str>,
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
    /// The plug-in instances this renderer's non-pure nodes own (M4):
    /// one owner per renderer, so the viewer's and an export's never meet.
    instances: klipp_nodes::frame_context::PluginInstances,
    /// The current render's inspection request, and what it has found.
    inspect: Inspect,
    found: Inspection,
    /// The current render's `RenderOptions::hdr_peak_nits`.
    hdr_peak: Option<f32>,
    /// The rendered sequence's `Sequence::title_nits`, for this frame.
    title_nits: f32,
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
    /// A reflection's row and mode (#2707): the rows from it down blend
    /// by that mode instead.
    split: Option<(u32, BlendMode)>,
}

/// The contrast protection a clip asks for, as its code (#2734): a
/// title's own, 0 for anything else. A title with a backing has none:
/// the backing is what it stands out on, and the pass, measuring the
/// layer, read the dark box round white words as dark text and lightened
/// the picture into bright streaks at the halo's edge.
fn contrast_protect(c: &Clip) -> u32 {
    match &c.kind {
        ClipKind::Title(td) if matches!(td.background, klipp_timeline::TitleBackground::None) => td.protect.code(),
        _ => 0,
    }
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
        /// The open shutter as a share of the transition's progress, and
        /// the instants across it: the sequence camera's motion blur, for
        /// the posed piece transitions (#985).
        shutter: (f32, u32),
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
    pub fn new(ofx: klipp_plugin::OfxHost, mut titles: klipp_title::TitleRenderer) -> Self {
        // A posed layer of several blurred by its entrance as a one-layer
        // title's frame is, here and on the GPU stack (#2370).
        titles.set_layer_softener(Box::new(soften_title_layer));
        Self {
            executor: klipp_nodes::GraphExecutor::new(),
            gpu: None,
            gpu_tried: false,
            allow_gpu: true,
            run_effects: Vec::new(),
            run_blends: Vec::new(),
            planned_blends: Vec::new(),
            planned_nodes: HashMap::new(),
            identity_feeds: HashMap::new(),
            spliced_graded: None,
            resolved: Default::default(),
            cpu_executor: None,
            matte_input: None,
            run_display: None,
            below: None,
            stack_below: None,
            plan_misses: 0,
            spliced: klipp_nodes::NodeGraph::default(),
            passthrough: passthrough_graph(),
            grade_ran: false,
            ofx: std::sync::Arc::new(std::sync::Mutex::new(ofx)),
            titles,
            gpu_text3d: None,
            warm_tx: None,
            warm_rx: None,
            glyph_cache: HashMap::new(),
            shutter: HashMap::new(),
            title_shutter: HashMap::new(),
            pins: HashMap::new(),
            pin_presence: HashMap::new(),
            pin_light: HashMap::new(),
            px_scale: 1.0,
            working: WorkingColorspace::default(),
            grade_encoding: klipp_core::grade_encoding(),
            motion: klipp_effects::MotionData::default(),
            last_gpu_stack_decline: None,
            colour_effects: Default::default(),
            frame_decline: None,
            cpu_layers_said: std::collections::HashSet::new(),
            nesting: 0,
            stack_work_started: false,
            gpu_export_declined_after_work: false,
            global_lut: None,
            instances: Default::default(),
            run_source_nodes: Vec::new(),
            source_decoded: None,
            grade_source_scene: false,
            still_generators: HashMap::new(),
            still_layers: HashMap::new(),
            precomp_scenes: HashMap::new(),
            precomp_spare: Vec::new(),
            source_sizes: HashMap::new(),
            side_title_slots: HashMap::new(),
            clean_frames: HashMap::new(),
            precomp_ready: std::collections::HashSet::new(),
            grade_split: GradeSplit::default(),
            subject_mattes: HashMap::new(),
            clip_subject_dirs: HashMap::new(),
            clip_media_last: HashMap::new(),
            assets: Box::new(klipp_analysis::assets::CacheStore::default()),
            derived: HashMap::new(),
            clip_analysis: None,
            inspect: Inspect::default(),
            found: Inspection::default(),
            hdr_peak: None,
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
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

    pub fn ofx_host(&mut self) -> std::sync::MutexGuard<'_, klipp_plugin::OfxHost> {
        self.ofx.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Geometry uploads since this renderer created its current GPU solid cache.
    pub fn title_3d_geometry_uploads(&self) -> u64 {
        self.gpu_text3d.as_ref().map_or(0, |g| g.uploads())
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
        // A title the GPU lays out on its glyph route has no resting
        // raster to wait on (#2335); the CPU one is the fallback's.
        let wants: Vec<_> = if self.allow_gpu {
            wants.into_iter().filter(|(td, _, _)| !klipp_title::takes_glyph_route(td, None)).collect()
        } else {
            wants
        };
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
        let clip_time = drawn_time(clip, time);
        let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
        let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
        let total = (clip.timeline_out - clip.timeline_in).to_secs_f64() as f32;
        Some(self.titles.render_with_window(td, width, height, gpu, clip_time as f32, Some(total)))
    }

    /// Source-boundary nodes this renderer has run (M4 step 6), each once
    /// a frame of a clip that has one.
    pub fn source_node_runs(&self) -> u64 {
        self.executor.source_runs() + self.cpu_executor.as_ref().map_or(0, |e| e.source_runs())
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

    /// The project's grade encoding (#467): what the grade's controls
    /// act in under ACEScg.
    pub fn set_grade_encoding(&mut self, enc: klipp_core::GradeEncoding) {
        self.grade_encoding = enc;
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
        // Captions off the lines a title is on, while both are shown (#2567).
        let moved = crate::captions::captions_clear_of_titles(seq, time, width, height);
        let seq = moved.as_ref().unwrap_or(seq);
        let _residency = residency::frame("composition", time.to_secs_f64());
        residency::record("composition.cpu", "CPU composition/reference/inspection route requested", Work::Fallback);
        let t0 = Instant::now();
        self.found = Inspection::default();
        self.resolved = crate::inspect::resolve(seq, &inspect, &mut self.found.unavailable);
        self.inspect = inspect;
        self.hdr_peak = opts.hdr_peak_nits;
        self.title_nits = seq.title_nits;
        let out = self.render_stack(seq, time, width, height, src, opts);
        // On the CPU a render is the whole frame (#1913): anything else is
        // a fault somewhere above, said here, and a black frame of the
        // right size goes on rather than a buffer that crashes its reader.
        let whole = width as usize * height as usize * 4;
        let out = if !self.allow_gpu && out.len() != whole {
            tracing::error!("render gave {} values for a {width}x{height} frame, which takes {whole}; a black frame instead", out.len());
            vec![0.0; whole]
        } else {
            out
        };
        // The picture's Placement and Output show the finished frame.
        if !self.resolved.finished.is_empty() {
            let thumb = klipp_nodes::executor::downsample_to_thumb_u8(&out, width, height, klipp_nodes::executor::THUMB_W, klipp_nodes::executor::THUMB_H);
            for at in std::mem::take(&mut self.resolved.finished) {
                self.found.thumbs.insert(at, thumb.clone());
            }
        }
        // The frame is done: a plug-in instance not rendered near it since
        // goes (`PluginInstances::end_frame`).
        self.instances.end_frame((time.to_secs_f64() * seq.fps.as_f64()).round() as i64);
        self.inspect = Inspect::default();
        self.resolved = Default::default();
        self.hdr_peak = None;
        // A solo belongs to the inspected clip; leave the executor clean
        // for whoever drives it next.
        self.executor.set_solo_node(None);
        let mut found = std::mem::take(&mut self.found);
        found.stats.total_ms = ms_since(t0);
        (out, found)
    }

    /// The executor's last run's declines, each named by `clip`, kept for
    /// this frame's [`Inspection`] (M4 step 10). Called once a graph has
    /// run for good: never after a run whose result is thrown away and
    /// run again, whose declines would be counted twice.
    /// Each switched-off effect and plug-in of `clip` (its picture's), noted
    /// as bypassed (M4 step 10): it isn't run, so no node of it declines
    /// itself, and the frame still says it was passed over as asked. Never
    /// failing, and not shown in the viewer's chip. Noted once a frame, if
    /// a layer's clip is drawn again on another route. And each LUT effect
    /// whose file can't be read, declined as failing (#1746).
    fn note_switched_off(&mut self, clip: &Clip) {
        if let Ok(plugins) = clip.picture_ofx() {
            for p in plugins.into_iter().filter(|p| !p.entry.enabled) {
                if self.found.declines.iter().any(|d| d.clip == Some(clip.id) && d.node == p.node && d.outcome == klipp_nodes::DeclineOutcome::Bypass) {
                    continue;
                }
                self.found.declines.push(klipp_nodes::Decline {
                    clip: Some(clip.id),
                    node: p.node,
                    kind: klipp_nodes::NodeKind::Ofx,
                    outcome: klipp_nodes::DeclineOutcome::Bypass,
                    name: p.entry.plugin_name.clone(),
                    reason: "switched off".into(),
                    why: String::new(),
                });
            }
        }
        let Ok(effects) = clip.picture_effects() else { return };
        // A LUT whose file can't be read passes the picture: declined, so
        // the viewer says why and an export refuses (#1746).
        for e in effects.iter().filter(|e| e.enabled) {
            let BuiltinEffect::Lut3D { path, opacity, .. } = &e.effect else { continue };
            if *opacity <= 0.0 || self.found.declines.iter().any(|d| d.clip == Some(clip.id) && d.node == e.node && d.outcome == klipp_nodes::DeclineOutcome::FileMissing) {
                continue;
            }
            let read = self.colour_effects.lock().unwrap_or_else(std::sync::PoisonError::into_inner).lut(path, self.working);
            if let Err(why) = read {
                self.found.declines.push(klipp_nodes::Decline {
                    clip: Some(clip.id),
                    node: e.node,
                    kind: klipp_nodes::NodeKind::Effect,
                    outcome: klipp_nodes::DeclineOutcome::FileMissing,
                    name: klipp_nodes::decline::effect_name(&e.effect),
                    reason: why.clone(),
                    why,
                });
            }
        }
        for e in effects.into_iter().filter(|e| !e.enabled) {
            if self.found.declines.iter().any(|d| d.clip == Some(clip.id) && d.node == e.node && d.outcome == klipp_nodes::DeclineOutcome::Bypass) {
                continue;
            }
            self.found.declines.push(klipp_nodes::Decline {
                clip: Some(clip.id),
                node: e.node,
                kind: klipp_nodes::NodeKind::Effect,
                outcome: klipp_nodes::DeclineOutcome::Bypass,
                name: klipp_nodes::decline::effect_name(&e.effect),
                reason: "switched off".into(),
                why: String::new(),
            });
        }
    }

    /// A run the executor refused for its colour domains (#1867): its
    /// failing decline, named against `clip`, so the viewer shows why the
    /// run's nodes did nothing and an export refuses the frame.
    fn note_domain_refusal(&mut self, clip: &Clip, e: &klipp_nodes::executor::ExecutorError) {
        if matches!(e, klipp_nodes::executor::ExecutorError::ColourDomain(_)) {
            self.note_declines(clip.id);
        }
    }

    fn note_declines(&mut self, clip: ClipId) {
        let declines = self.executor.last_run_declines().iter().map(|d| klipp_nodes::Decline { clip: Some(clip), ..d.clone() });
        self.found.declines.extend(declines);
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
        // Captions off the lines a title is on, while both are shown (#2567).
        let moved = crate::captions::captions_clear_of_titles(seq, time, width, height);
        let seq = moved.as_ref().unwrap_or(seq);
        let _residency = residency::frame("export", time.to_secs_f64());
        self.gpu_export_declined_after_work = false;
        self.frame_decline = None;
        let planes = match self.render_yuv420_single(seq, time, width, height, src, opts, target) {
            Single::Planes(planes) => Some(planes),
            Single::Refused(why) => {
                residency::record("composition.export", why, Work::Fallback);
                self.frame_decline = Some(why);
                None
            }
            Single::ForTheStack => self.render_yuv420_from_stack(seq, time, width, height, src, opts, target),
        };
        if planes.is_none() && self.frame_decline.is_none() {
            self.frame_decline = Some("the GPU pipeline was unavailable");
        }
        planes
    }

    /// Why the GPU declined the last frame [`Self::render_display_on_gpu`]
    /// or [`Self::render_yuv420_on_gpu`] was asked for, or `None` when it
    /// took it: the decline as data, for a test or a counter, where the log
    /// says it once.
    pub fn last_decline(&self) -> Option<&'static str> {
        self.frame_decline
    }

    /// Whether the last [`Self::render_yuv420_on_gpu`] declined only
    /// after rendering — a grade that came back on the CPU, a stack that
    /// failed part-way — so the caller rendered the frame twice. A
    /// caller in a loop stops asking while the same clips are on screen
    /// (#1276); a decline before any work costs nothing and says nothing
    /// about the next frame.
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
        // An HDR delivery's target encodes PQ or HLG; one asked for with
        // the other's target is not a frame this can make.
        if opts.hdr_peak_nits.is_some() != (target.hdr_op != 0) {
            self.frame_decline = Some("a delivery its target does not encode");
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
        // Every layer has had its own display step, so the encode runs
        // none of its own; an HDR delivery is encoded at the frame's peak.
        let target = klipp_effects::gpu::Yuv420Target { scene: None, peak_nits: opts.hdr_peak_nits.unwrap_or(target.peak_nits), ..target };
        let planes = self.executor.encode_output_to_yuv420(width, height, target);
        if planes.is_none() {
            self.gpu_export_declined_after_work = true;
            self.frame_decline = Some("the Y'CbCr encode failed");
        }
        planes
    }

    /// The frame composited on the GPU stack and brought back once, as
    /// the linear frame [`Self::render`] gives: for an export whose encoder
    /// takes no Y'CbCr planes made on the GPU (ProRes, DNxHD) or whose
    /// picture is letterboxed, padded after (#2488). The CPU composition
    /// those used ran each layer's effects with a download of its own and
    /// rastered generators on the CPU. `None`, the reason in
    /// [`Self::last_decline`], when the stack cannot take the frame.
    #[allow(clippy::too_many_arguments)]
    pub fn render_linear_on_gpu(
        &mut self,
        seq: &Sequence,
        time: Timecode,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        opts: RenderOptions,
    ) -> Option<Vec<f32>> {
        // Captions off the lines a title is on, while both are shown (#2567).
        let moved = crate::captions::captions_clear_of_titles(seq, time, width, height);
        let seq = moved.as_ref().unwrap_or(seq);
        let _residency = residency::frame("export", time.to_secs_f64());
        self.gpu_export_declined_after_work = false;
        self.frame_decline = None;
        self.found.declines.clear();
        let composited = self.composite_on_gpu(seq, time, width, height, src, opts).is_some();
        if let Some(gpu) = self.executor.gpu_pipeline_mut() {
            gpu.stack_end();
        }
        if !composited {
            // The stack recorded why it declined.
            self.frame_decline.get_or_insert("the GPU pipeline was unavailable");
            return None;
        }
        let gpu = self.executor.gpu_pipeline_mut()?;
        let pixels = gpu.download_chain_output_linear(true, width as usize * height as usize);
        if pixels.len() != width as usize * height as usize * 4 {
            self.gpu_export_declined_after_work = true;
            self.frame_decline = Some("the frame's download failed");
            return None;
        }
        Some(pixels)
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
            return Single::Refused("global LUT with an HDR delivery");
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
            return Single::Refused("the render and the encode disagree on HDR");
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
        // Its tracked subjects' mattes, for its grade (#1881).
        self.note_subject_mattes(clip, src);
        // OFX plugins run on the CPU frame, and a temporal one wants
        // neighbouring frames too.
        if clip.has_enabled_plugins() {
            return Single::ForTheStack;
        }
        // Two frames blended for a speed ramp is a CPU operation on the
        // source, before the grade even starts.
        let media_fps = src.media_fps(clip);
        if frame_blends(clip, time, media_fps, fps) {
            return Single::ForTheStack;
        }
        // Only while it really moves under the open shutter: a still
        // stretch of an animated clip, its hold, is drawn unblurred by the
        // CPU too, and stays here (#1922).
        if title_motion_blur(clip).is_some() || self.moves_in_shutter(seq, clip, time, width, height) {
            return Single::ForTheStack;
        }
        // Temporal noise reduction too: it averages CPU-decoded
        // neighbours into the source, and the effect list checked below
        // has had it lifted out, so this path would render the clip
        // noisy (#996).
        if temporal_denoise_of(clip, time, fps).is_some() {
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
            || i.only.is_some()
        {
            return Single::Refused("inspection");
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
        let (mut fx, opacity) = clip_effects(clip, time, fps, Some(resolved), eval::surface_pin_corners(clip, seq, time, fps, &|_| None));
        let opacity = opacity * eval::surface_pin_presence(clip, seq, time, fps);
        let light = eval::surface_pin_light(clip, seq, time, fps);
        if (light - 1.0).abs() > 1e-4 {
            fx.insert(0, eval::light_gain_effect(light));
        }
        if has_active_effects(&fx) || (opacity - 1.0).abs() > 1e-6 {
            return Single::ForTheStack;
        }

        // Everything checked. Run the clip's grade with its result left in
        // the chain, and convert it there.
        let clip_time = eval_clip_time(clip, time);
        let at = source_secs(clip, time, fps);
        let Some(f) = src.frame(clip, at, width, height) else { return Single::Refused("the source has no frame") };
        // A source of another size is the stack's to fit, as it fits a
        // layer of another size (`place_layer`): this path converts at the
        // frame's own size. Refusing it sent every frame of, say, a vertical
        // clip on a 16:9 sequence down the CPU composite (#1277). A
        // scene-referred one too, under an SDR delivery (#1276); the stack
        // declines an HDR one before any work.
        if (f.width, f.height) != (width, height) {
            return Single::ForTheStack;
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
                    return Single::Refused("global LUT after a scene-referred display pass");
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
        self.executor.set_grade_encoding(self.grade_encoding);
        self.executor.set_working_colorspace(self.working);
        self.executor.set_render_scale(self.px_scale);
        if clip.grade_is_identity() {
            // Seeded where it is: a decoded frame's own seed, or a
            // scene-referred frame's codes decoded on the GPU as `grade`
            // seeds them for a real graph. Refusing the second sent every
            // frame of ungraded HDR, HLG or log footage down the CPU
            // composite (#1275).
            let seeded = match &input {
                GradeInput::Gpu(g) => g.seed(&mut self.executor, width, height),
                GradeInput::Scene { codes, transfer, source_to_working } => {
                    self.allow_gpu
                        && self.executor.prime_chain_from_hdr10_codes(
                            codes,
                            width,
                            height,
                            transfer.transfer_id(),
                            transfer.scene_scale(),
                            klipp_effects::color_wgsl::pack_primaries_matrix(*source_to_working),
                        )
                }
                GradeInput::Cpu(_) => return Single::Refused("an identity grade of a frame not on the GPU"),
            };
            if !seeded || !self.executor.take_seeded_input_as_output() {
                return Single::Refused("the source could not seed the chain");
            }
            let target = klipp_effects::gpu::Yuv420Target {
                scene: scene_end,
                peak_nits: peak,
                ..target
            };
            if !self.global_lut_on_chain(width, height) {
                return Single::Refused("global LUT");
            }
            return Single::from(self.executor.encode_output_to_yuv420(width, height, target));
        }

        self.executor.request_output_on_gpu();
        // No side pictures: a clip that reads one has an effect (its Merge,
        // or the source on its chain) and went to the stack above (#1746);
        // the map holds the last clip's, never this one's.
        self.executor.clear_side_pictures();
        let pixels = self.grade(clip, input, clip_time, fps, width, height, None);
        // The grade kept its result on the GPU exactly when it returns
        // nothing. Coming back on the CPU instead — a node with no GPU
        // dispatch, a device that failed — means this frame gets rendered
        // twice, once here and once by the caller. That is the one case
        // where taking this path costs something, so a caller in a loop
        // should stop asking for the clip's stretch after the first time
        // it happens rather than pay it on every frame.
        if !pixels.is_empty() {
            self.gpu_export_declined_after_work = true;
            return Single::Refused("the grade came back on the CPU");
        }
        let target =
            klipp_effects::gpu::Yuv420Target { scene: scene_end, peak_nits: peak, ..target };
        if !self.global_lut_on_chain(width, height) {
            return Single::Refused("global LUT");
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
    /// The nodes the last frame rendered didn't draw as asked, and why (M4
    /// step 10), whichever route drew it: the GPU stack's, or the
    /// timeline's (which `render_inspected` also returns).
    pub fn frame_declines(&self) -> &[klipp_nodes::Decline] {
        &self.found.declines
    }

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
        // Captions off the lines a title is on, while both are shown (#2567).
        let moved = crate::captions::captions_clear_of_titles(seq, time, width, height);
        let seq = moved.as_ref().unwrap_or(seq);
        let _residency = residency::frame("display", time.to_secs_f64());
        self.frame_decline = None;
        // This frame's declines, the stack's own as the timeline's (M4
        // step 10): read after it with `frame_declines`.
        self.found.declines.clear();
        let result = self.render_display_on_gpu_inner(seq, time, width, height, src, opts, out);
        if result.is_none() && self.frame_decline.is_none() {
            self.frame_decline = Some("the GPU pipeline was unavailable");
            residency::record("composition.gpu_stack", "GPU pipeline unavailable", Work::Fallback);
            GPU_STACK_DECLINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
        // HDR monitoring's preview of a stack (#1279 takes the export's
        // HDR delivery, not the viewer's).
        if opts.hdr_peak_nits.is_some() {
            return self.gpu_stack_declines("the viewer's HDR preview of a stack");
        }
        // An ACES or BT.2020 project's display encode writes bytes, not
        // the viewer's texture (#1279).
        if self.working != WorkingColorspace::Rec709Linear && out.texture.is_some() {
            return self.gpu_stack_declines("a wide working space's display encode to the viewer's texture");
        }
        let mut stats = self.composite_on_gpu(seq, time, width, height, src, opts)?;
        let t = Instant::now();
        let gpu = self.executor.gpu_pipeline_or_init()?;
        // The project's own display transform, as `display_transform_ws`
        // ends it: BT.709 sRGB, the ACES Rec.709 output transform, or
        // BT.2020's basis change and gamut compression.
        let encoded = match self.working {
            WorkingColorspace::Rec709Linear => gpu.encode_chain_to_display(true, width, height, None, false, None, out.texture, out.bytes),
            WorkingColorspace::AcesCG => gpu.encode_chain_to_aces_rec709_u8(true, width, height, &klipp_color::aces::aces_rec709_gpu_data(1024)).map(Some),
            WorkingColorspace::Bt2020Linear => gpu.encode_chain_to_bt2020_display_u8(true, width, height, &klipp_color::gamut::bt2020_to_bt709_gpu_data()).map(Some),
        };
        let Some(bytes) = encoded else {
            return self.gpu_stack_declines("the display encode failed");
        };
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
        // Under an HDR delivery a scene-referred layer's own display step
        // is the delivery's display light, as `render_clip` gives it on the
        // CPU (#1279): the frame's peak, for this frame only, cleared
        // however it ends.
        self.hdr_peak = opts.hdr_peak_nits;
        self.title_nits = seq.title_nits;
        let stats = self.composite_on_gpu_frame(seq, time, width, height, src, opts);
        self.hdr_peak = None;
        stats
    }

    fn composite_on_gpu_frame(
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
        // The CPU applies a global LUT under an HDR delivery to the light
        // up to SDR white only, the rest kept apart (`split_above_white`),
        // which the stack has no twin of; the single-clip path declines it
        // alike.
        if opts.hdr_peak_nits.is_some() && self.global_lut.is_some() {
            return self.gpu_stack_declines("global LUT with an HDR delivery");
        }
        let i = &self.inspect;
        if i.ungraded || i.thumbs.is_some() || i.scope_tap.is_some() || i.matte_view.is_some() || i.solo.is_some() || i.matte_of.is_some() || i.only.is_some() {
            return self.gpu_stack_declines("inspection");
        }
        let t0 = Instant::now();
        let fps = seq.fps.as_f64();
        // Kept still layers of clips this sequence no longer has (another
        // project, a deleted clip) let go of their GPU memory (#2092).
        // A frame with no clips at all is the engine's warm-up of the stack
        // (#2386), drawn with the sequence's clips left out: no sign they
        // are gone. Pruned on it, every still kept went up again on the
        // next frame.
        if !self.still_layers.is_empty() && seq.tracks.iter().any(|t| !t.clips.is_empty()) {
            self.still_layers.retain(|id, _| seq.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == *id)));
        }
        // An empty frame is the clear canvas, graded and finished on the
        // GPU as any composite is (#2377).
        let stack = stack(seq, time);

        // What a clip needs that the stack does not have.
        let blocker = |c: &Clip| -> Option<&'static str> {
            if c.has_enabled_plugins() {
                Some("OFX plugin")
            } else if has_frei0r_node(c) {
                // A frei0r plug-in runs as a node of the timeline render,
                // with its instances (M4 step 5): the stack has no frei0r,
                // and running the layer's effects here would leave it out.
                Some("frei0r plug-in")
            } else if crate::resolve::reads_below(c) && (c.matte_source.is_some() || c.matte_stencil.is_some() || c.consumed_as_matte) {
                // What is below is read by the clip as the stack lays it
                // (#2758); one in a matte's play is the CPU stacking
                // loop's for now (#1746).
                Some("clip below")
            } else if crate::resolve::has_grade_compound(c) {
                // A grade in the chain runs in the timeline render's plan
                // for now (#1746).
                Some("grade compound")
            } else if title_motion_blur(c).is_some() && !title_blur_on_stack(c, time, fps) {
                Some("title motion blur")
            } else if temporal_denoise_of(c, time, fps).is_some() {
                // Run on the CPU-decoded source and its neighbours, and
                // lifted out of the effects the stack runs (#996).
                Some("temporal noise reduction")
            } else {
                None
            }
        };
        // Slots: a clip takes one layer, a transition three (both sides
        // and the result).
        enum Item<'a> {
            Clip { c: &'a Clip, slot: usize },
            Transition { a: &'a Clip, b: &'a Clip, kind: klipp_core::TransitionKind, progress: f32, params: klipp_core::TransitionUserParams, shutter: (f32, u32), slots: [usize; 3] },
        }
        let mut items: Vec<(&Track, Item)> = Vec::with_capacity(stack.len());
        let mut n_slots = 0usize;
        let mut sources: HashSet<ClipId> = HashSet::new();
        self.side_title_slots.clear();
        for (_, track, item) in &stack {
            match item {
                TrackItem::Clip(c) => {
                    let _clip_residency = residency::clip(c.id.0);
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
                    // A layer for each title it draws beside its chain
                    // (#2758), drawn as a title clip's is.
                    let titles = crate::resolve::title_sources(c).len();
                    if titles > 0 {
                        self.side_title_slots.insert(c.id, n_slots);
                        n_slots += titles;
                    }
                }
                TrackItem::Transition { a, b, kind, progress, params, shutter } => {
                    // A smooth cut morphs two held frames by their motion
                    // (#2219): the composite route's, which reads both.
                    if *kind == klipp_core::TransitionKind::SmoothCut {
                        return self.gpu_stack_declines("smooth cut");
                    }
                    for side in [a, b] {
                        if let Some(why) = blocker(side) {
                            return self.gpu_stack_declines(why);
                        }
                        // What is below a transition's side is the CPU
                        // stacking loop's for now (#1746).
                        if crate::resolve::reads_below(side) {
                            return self.gpu_stack_declines("clip below");
                        }
                        if !crate::resolve::title_sources(side).is_empty() {
                            return self.gpu_stack_declines("a title source inside a transition");
                        }
                        // A side seen through a matte is matted before the
                        // sides mix (step 2); one that is a matte, a stencil
                        // or an adjustment is not a picture to mix.
                        if side.consumed_as_matte || side.matte_stencil.is_some() || matches!(side.kind, ClipKind::AdjustmentLayer) {
                            return self.gpu_stack_declines("a matte, stencil or adjustment inside a transition");
                        }
                        if let Some(m) = &side.matte_source {
                            sources.insert(m.source_clip_id);
                        }
                    }
                    items.push((track, Item::Transition { a, b, kind: *kind, progress: *progress, params: *params, shutter: *shutter, slots: [n_slots, n_slots + 1, n_slots + 2] }));
                    n_slots += 3;
                }
            }
        }
        // A matte source that is itself matted would be read after it
        // was cut: the CPU stack reads sources untouched.
        for (_, item) in &items {
            if let Item::Clip { c, .. } = item {
                let is_source = c.consumed_as_matte || sources.contains(&c.id);
                // A matte's source is laid before anything is stacked: what
                // is below it isn't there yet.
                if is_source && crate::resolve::reads_below(c) {
                    return self.gpu_stack_declines("clip below");
                }
                if is_source && c.matte_source.is_some() {
                    return self.gpu_stack_declines("a matte source with a matte of its own");
                }
            }
        }

        // ── 0. Each precomposition's scene composed on the stack first,
        // before this frame's own state is worked out, and kept to seed
        // its layer (#2757) ──
        if self.nesting == 0 {
            // Kept for a clip the last frame did not compose, one removed
            // or undone: let go, not held until eight others crowd it out.
            let ready = std::mem::take(&mut self.precomp_ready);
            for (_, buffer) in self.precomp_scenes.extract_if(|id, _| !ready.contains(id)) {
                if self.precomp_spare.len() < SPARE_SCENES {
                    self.precomp_spare.push(buffer);
                }
            }
        }
        let scenes: Vec<(ClipId, Arc<Sequence>, Timecode)> = items
            .iter()
            .filter_map(|(_, item)| match item {
                Item::Clip { c, .. } if matches!(c.kind, ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. }) => {
                    c.precomp.clone().map(|scene| {
                        let at = precomposition_time(c, &scene, time, fps);
                        (c.id, scene, at)
                    })
                }
                _ => None,
            })
            .collect();
        for (id, scene, at) in scenes {
            self.compose_precomposition(id, &scene, at, width, height, src, opts);
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
        self.resolve_pins(seq, items.iter().flat_map(|(_, item)| match item {
            Item::Clip { c, .. } => vec![*c],
            Item::Transition { a, b, .. } => vec![*a, *b],
        }), time, fps, &transforms);
        in_render_pixels(seq, width, height, &mut transforms);
        self.px_scale = render_scale(seq, width, height).1;
        behind_the_camera(seq, time, width, height, &mut transforms);
        self.open_shutter(seq, time, fps, width, height, &transforms);

        self.executor.set_grade_encoding(self.grade_encoding);
        self.executor.set_working_colorspace(self.working);
        self.executor.set_render_scale(self.px_scale);
        let luma = klipp_color::linear::luma_coefs(self.working);
        // Titles and generators are authored in BT.709: placed into the
        // working space as `authored_to_working` puts them there (#1279).
        let to_working = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, self.working)
            .unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| {
            g.set_luma_coefs(luma);
            g.set_authored_to_working(to_working);
            g.stack_begin(width, height, n_slots)
        }) {
            return self.gpu_stack_declines("the GPU stack would not start");
        }
        self.stack_work_started = true;

        let mut stats = RenderStats::default();
        self.grade_split = GradeSplit::default();
        // ── 1. Every layer in its buffer, effects run on it there ──
        let mut placed: HashMap<usize, Placed> = HashMap::new();
        let mut slot_of_clip: HashMap<ClipId, usize> = HashMap::new();
        for (_, item) in &items {
            match item {
                Item::Clip { c, slot } => {
                    // One reading what is below it is laid as the stack
                    // reaches it, over what is there (#2758).
                    if matches!(c.kind, ClipKind::AdjustmentLayer) || crate::resolve::reads_below(c) {
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
                Item::Transition { a, b, slots, .. } => {
                    for (side, slot) in [(a, slots[0]), (b, slots[1])] {
                        match self.place_layer(slot, side, time, fps, width, height, src, &transforms, &mut stats) {
                            Ok(Some(p)) => {
                                // A transition's side is mixed before
                                // anything is laid: it is finished now.
                                if let Err(why) = self.finish_placed(side, slot, p.held.as_deref(), time, fps, width, height, &transforms, &mut stats) {
                                    return self.gpu_stack_declines(why);
                                }
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
                    // Mixed once the mattes are in (step 2): a side's matte
                    // source may be on a track above, placed after it.
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
            // A transition's side is seen through its own matte before the
            // sides mix, as the CPU composite does (#718, #2668).
            if let Item::Transition { a, b, slots, .. } = item {
                for (side, slot) in [(a, slots[0]), (b, slots[1])] {
                    let Some(m) = self.matte_binding(side, time) else { continue };
                    let source = slot_of_clip.get(&m.source_clip_id).copied();
                    if !self.stack_apply_matte(slot, source, matte_channel_index(m.channel), m.invert, m.feather_px, m.choke_px) {
                        return self.gpu_stack_declines("a matte pass failed");
                    }
                }
                continue;
            }
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

        // ── 2b. Each transition's sides mixed into its own buffer ──
        for (_, item) in &items {
            let Item::Transition { a, b, kind, progress, params, shutter, slots } = item else { continue };
            let kind_id = klipp_core::spec_for_kind(*kind).kind_id as u32;
            let eased = if matches!(kind, klipp_core::TransitionKind::CrossDissolve) {
                klipp_transitions::apply_easing(*progress, params.param0)
            } else {
                *progress
            };
            let p = [params.param0, params.param1, params.param2, params.param3];
            let colour = self.authored_colour(params.color);
            let t = Instant::now();
            let done = if klipp_effects::pieces::is_posed_with(kind_id, p[3], subject_code(*kind, params)) {
                // Posed pieces (#985), blurred across the shutter,
                // cut round the subject when asked.
                let plan = self.piece_plan(a, b, *kind, eased, params, *shutter, time, fps, width, height, src, &transforms);
                self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_pieces(slots[0], slots[1], slots[2], &plan))
            } else {
                self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_transition(slots[0], slots[1], slots[2], kind_id, eased, p, colour))
            };
            if !done {
                return self.gpu_stack_declines("the transition pass failed");
            }
            stats.composite_ms += ms_since(t);
            placed.insert(slots[2], Placed { opacity: 1.0, rows: (0, height), held: None, split: None });
        }

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
                    // Over what is below it: the composite so far, kept for
                    // its run to read, then the clip laid as any is.
                    if crate::resolve::reads_below(c) {
                        self.stack_below = self.executor.gpu_pipeline_or_init().and_then(|g| g.stack_acc_as_picture());
                        let laid = self.place_layer(*slot, c, time, fps, width, height, src, &transforms, &mut stats);
                        // Not read (no frame here, or declined): given back.
                        if let (Some(picture), Some(g)) = (self.stack_below.take(), self.executor.gpu_pipeline_or_init()) {
                            g.release_picture(picture);
                        }
                        self.executor.clear_side_pictures_on_gpu();
                        match laid {
                            Ok(Some(p)) => {
                                placed.insert(*slot, p);
                            }
                            Ok(None) => {}
                            Err(why) => return self.gpu_stack_declines(why),
                        }
                    }
                    let Some(p) = placed.get(slot) else { continue };
                    let opacity = p.opacity * track.opacity;
                    if opacity <= 0.001 {
                        continue;
                    }
                    if let Err(why) = self.finish_placed(c, *slot, p.held.as_deref(), time, fps, width, height, &transforms, &mut stats) {
                        return self.gpu_stack_declines(why);
                    }
                    let protect = contrast_protect(c);
                    let luma = klipp_color::linear::luma_coefs(self.working);
                    if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_contrast_protect(*slot, opacity, protect, luma)) {
                        return self.gpu_stack_declines("a title's contrast protection failed");
                    }
                    // A pinned layer taking its surface's shade and texture
                    // from what is under it (#2751).
                    if let Some(pin) = c.surface_pin.filter(|p| p.shade > 0.0 || p.texture > 0.0) {
                        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_take_surface(*slot, pin.shade, pin.texture, luma)) {
                            return self.gpu_stack_declines("a pinned layer's surface light failed");
                        }
                    }
                    let (y0, y1) = p.rows;
                    let mode = c.blend_mode as u32;
                    // A reflection's rows blend by its own mode (#2707).
                    let bands = match p.split {
                        Some((row, below)) => [(y0, row.clamp(y0, y1), mode), (row.clamp(y0, y1), y1, below as u32)],
                        None => [(y0, y1, mode), (y1, y1, mode)],
                    };
                    for (b0, b1, mode) in bands.into_iter().filter(|(b0, b1, _)| b1 > b0) {
                        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_blend_rows(*slot, opacity, mode, b0, b1)) {
                            return self.gpu_stack_declines("the blend pass failed");
                        }
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

        // Nothing laid (nothing on screen, or every layer skipped): the
        // clear canvas is the composite.
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_clear_canvas()) {
            return self.gpu_stack_declines("the canvas could not be cleared");
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
        let finished = match channel_code(opts.view_channel) {
            0 => gpu.stack_finish_into_chain(opts.keep_alpha),
            c => gpu.stack_finish_viewing(c, klipp_color::linear::luma_coefs(self.working)),
        };
        if !finished {
            return self.gpu_stack_declines("the composite could not be finished");
        }
        stats.composite_ms += ms_since(t);
        stats.total_ms = ms_since(t0);
        stats.grade_split = std::mem::take(&mut self.grade_split);
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
        let _clip_residency = residency::clip(c.id.0);
        self.note_switched_off(c);
        self.note_subject_mattes(c, src);
        let (mut fx, opacity) = clip_effects(c, time, fps, transforms.get(&c.id).copied(), self.pins.get(&c.id).copied());
        self.take_light(c.id, &mut fx);
        let opacity = opacity * self.pin_presence.get(&c.id).copied().unwrap_or(1.0);
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        // Read at the render's size: a gap is in its pixels.
        let split = fx.iter().rev().find_map(|e| e.split_blend(height));
        self.title_light(c, &mut fx);
        if crate::resolve::reads_below(c) {
            // Where the stack places it, as it places any layer.
            let place = transforms.get(&c.id).map(|xf| transform_effect(*xf, tilt(c, &fx, eval_clip_time(c, time))));
            self.put_below_on_gpu(c, &fx, place)?;
        }
        // A clip the camera's shutter blurs this frame (one that really
        // moves while it is open, #1922): its effects before the move run
        // on its picture as any layer's do, the picture is placed at each
        // sample and averaged on the stack (#1923), and the effects after
        // the move run once on the average (#2090), as `shuttered` does on
        // the CPU.
        let mut after_move: Vec<BuiltinEffect> = Vec::new();
        // A title's own motion blur takes its own samples, as on the CPU.
        let samples = if title_motion_blur(c).is_some() { self.title_shutter.get(&c.id) } else { self.shutter.get(&c.id) };
        let shutter: Option<Vec<BuiltinEffect>> = match samples {
            Some(samples) => {
                let cut = shutter_cut(&fx);
                after_move = fx[cut..].iter().filter(|e| !matches!(e, BuiltinEffect::Transform { .. })).cloned().collect();
                let placements = samples.iter().map(|&(xf, t)| transform_effect(xf, tilt(c, &fx, t))).collect();
                fx.truncate(cut);
                Some(placements)
            }
            None => None,
        };
        // Its picture's plan, read below, stops there too.
        let at_move = if shutter.is_some() { PlanPart::BeforeMove } else { PlanPart::Whole };
        // Every effect a layer has runs in its plan on the GPU: a warp by
        // the clip's motion (stabilisation, rolling shutter) reads the
        // clip's analysis, which `grade_on_stack` gives the run (#2241).
        // No frei0r test here: the stack declines a frame with a frei0r
        // plug-in before a layer is placed (M4 step 5), so none reaches it.
        let clip_time = eval_clip_time(c, time);
        // Its media sources' frames, for its plan (#1746), and its titles'
        // pictures, drawn on the stack (#2758).
        self.read_media_frames(c, clip_time, width, height, src);
        self.put_titles(c, clip_time, width, height, true)?;
        let mut rows = (0, height);
        let mut redrawn = false;
        let mut effects_done = false;
        // A title's entrance and focus blur, put before its effects: they
        // are the title's own, not in its picture's list.
        let mut lead: Vec<BuiltinEffect> = Vec::new();
        let placed = match &c.kind {
            // A precomposition whose scene the stack has composed (#2757)
            // is placed as footage is, seeded from that scene.
            kind @ (ClipKind::Media { .. } | ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. })
                if matches!(kind, ClipKind::Media { .. }) || self.precomp_ready.contains(&c.id) =>
            {
                let precomp = !matches!(kind, ClipKind::Media { .. });
                let at = source_secs(c, time, fps);
                // A slowed clip's frames blended at the head of its grade's
                // run, on the chain (#2854): read half a media frame early,
                // as the composite reads them, so the frame read is the one
                // at or before the time and the blend has the next to go to.
                let blending = !precomp && frame_blends(c, time, src.media_fps(c), fps);
                let read_at = match src.media_fps(c).filter(|r| *r > 0.0) {
                    Some(rate) if blending => at - 0.5 / rate,
                    _ => at,
                };
                let mut source_pts = None;
                // A still image's picture, fitted to the frame, kept on the
                // GPU while its file, the size and the working space stand
                // (#2092): not read from the source again, nor uploaded, but
                // seeded from there into its grade or its layer.
                let still = if precomp { None } else { src.still_file(c) };
                let scene = precomp.then(|| self.precomp_scenes.get(&c.id).cloned()).flatten().and_then(|buffer| {
                    let g = self.executor.gpu_pipeline_or_init()?;
                    Some(Arc::new(AccSeed { buffer, device: g.device(), queue: g.queue(), width, height }) as Arc<dyn GpuPixels>)
                });
                let kept = scene.or_else(|| still.as_ref().and_then(|file| {
                    let k = self.still_layers.get(&c.id).filter(|k| &k.file == file && k.size == (width, height) && k.working == self.working)?;
                    let g = self.executor.gpu_pipeline_or_init()?;
                    Some(Arc::new(AccSeed { buffer: Arc::clone(&k.buffer), device: g.device(), queue: g.queue(), width, height }) as Arc<dyn GpuPixels>)
                }));
                let (pixels, other_size, (fw, fh)) = match kept {
                    Some(seed) => {
                        if !precomp {
                            stats.stills_kept += 1;
                        }
                        (SourcePixels::Gpu(seed), false, (width, height))
                    }
                    None => {
                        let Some(f) = src.frame(c, read_at, width, height) else { return Ok(None) };
                        source_pts = f.pts;
                        if !precomp {
                            self.source_sizes.insert(c.id, ((f.width, f.height), (width, height)));
                        }
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
                            other => other,
                        };
                        match (still, pixels) {
                            // A still read now: fitted once, sent up once,
                            // and kept for the frames after.
                            (Some(file), SourcePixels::Linear(p)) if !p.is_empty() => {
                                let Some(g) = self.executor.gpu_pipeline_or_init() else { return Err("no GPU for a still") };
                                let Some(buffer) = g.kept_linear(&p) else { return Err("a still could not be sent up") };
                                let seed = Arc::new(AccSeed { buffer: Arc::clone(&buffer), device: g.device(), queue: g.queue(), width, height });
                                stats.layers_uploaded += 1;
                                stats.stills_uploaded += 1;
                                residency::record("media.still", "Still image read, fitted and uploaded once, then kept on the GPU", Work::Preparation);
                                if self.still_layers.len() >= STILL_LAYERS_KEPT && !self.still_layers.contains_key(&c.id) {
                                    self.still_layers.clear();
                                }
                                self.still_layers.insert(c.id, StillLayer { file, working: self.working, size: (width, height), buffer });
                                (SourcePixels::Gpu(seed), false, (width, height))
                            }
                            (_, pixels) => (pixels, other_size, (f.width, f.height)),
                        }
                    }
                };
                // A crop to a shape, worked out from the source's size now
                // it is known (#2757): a crop and a placement, as on the CPU.
                if let Some(&(size, _)) = self.source_sizes.get(&c.id).filter(|_| !precomp) {
                    klipp_effects::resolve_cover_shapes(&mut fx, size, (width, height));
                }
                match pixels {
                    // An ungraded layer with effects takes the branch below:
                    // its effects run as a plan (step 11d).
                    SourcePixels::Gpu(g) if c.grade_is_identity() && !has_active_effects(&fx) && !blending => {
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
                        // The effects as nodes of the grade's plan (step 10b),
                        // on the GPU with it, instead of a pass over the layer.
                        effects_done = self.splice_effects(c, &fx, time, fps, transforms, at_move, stats);
                        self.grade_split.splice_ms += ms_since(t);
                        self.executor.request_output_on_gpu();
                        let pixels = self.grade_on_stack(c, GradeInput::Gpu(g), time, clip_time, fps, width, height, src, blending.then_some(source_pts));
                        stats.grade_ms += ms_since(t);
                        if effects_done && !self.finish_spliced() {
                            return Err("an effect could not be recorded");
                        }
                        stats.effects_followed_grade += u32::from(effects_done);
                        PLAN_EFFECT_CLIPS.fetch_add(u64::from(effects_done), std::sync::atomic::Ordering::Relaxed);
                        if pixels.is_empty() {
                            let slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_chain(slot, slot_a))
                        } else {
                            residency::record("composition.grade_output", "GPU grade/effect result returned CPU pixels", Work::Fallback);
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
                        }
                    }
                    SourcePixels::Linear(p) => {
                        // An unavailable CPU source can be empty during a seek.
                        // It is not the executor's resident-output sentinel: an
                        // early failed run leaves the previous chain slot intact.
                        if p.is_empty() {
                            return Err("CPU source has no pixels");
                        }
                        let t = Instant::now();
                        let run_on_gpu = !c.grade_is_identity() || has_active_effects(&fx) || blending;
                        let pixels = if !run_on_gpu {
                            unshare(p)
                        } else {
                            effects_done = self.splice_effects(c, &fx, time, fps, transforms, at_move, stats);
                            // A CPU-decoded source still runs its grade/effects on
                            // GPU: retain that result rather than downloading it
                            // only to upload it into the stack again (#2077).
                            self.executor.request_output_on_gpu();
                            let pixels = self.grade_on_stack(c, GradeInput::Cpu(p), time, clip_time, fps, width, height, src, blending.then_some(source_pts));
                            if effects_done && !self.finish_spliced() {
                                return Err("an effect could not be recorded");
                            }
                            stats.effects_followed_grade += u32::from(effects_done);
                            PLAN_EFFECT_CLIPS.fetch_add(u64::from(effects_done), std::sync::atomic::Ordering::Relaxed);
                            pixels
                        };
                        stats.grade_ms += ms_since(t);
                        if pixels.is_empty() && run_on_gpu {
                            let Some(slot_a) = self.executor.last_chain_final_slot() else {
                                return Err("GPU grade/effects returned no resident output");
                            };
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_chain(slot, slot_a))
                        } else {
                            if run_on_gpu {
                                residency::record("composition.grade_output", "GPU grade/effect result returned CPU pixels", Work::Fallback);
                            }
                            stats.layers_uploaded += 1;
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
                        }
                    }
                    // A log, HLG or PQ source: its codes decoded to the
                    // working space as the grade's chain starts, on the GPU
                    // (`grade`), and the clip's display step (the SDR tone
                    // map) in its plan after the grade and before its
                    // effects, the CPU composite's order. Refused, every
                    // frame with one on a crop, a transform, a fade, a title
                    // or a transition went to the CPU composite (#1276).
                    // Only an SDR delivery in a Rec.709 project gets here:
                    // the stack declines the others before any layer.
                    SourcePixels::SceneReferred { codes, transfer, source_to_working } => {
                        // Its next frame's codes aren't the chain's to
                        // blend toward: the composite blends it.
                        if blending {
                            return Err("frame blending of a scene-referred source");
                        }
                        let t = Instant::now();
                        let input = if other_size || codes.len() != (width as usize) * (height as usize) {
                            // Decoded and fitted on the CPU, as the CPU
                            // composite fits it (`to_linear`); graded and
                            // displayed on the GPU from there.
                            let decoded = codes.to_scene_linear_rgba(transfer, source_to_working);
                            GradeInput::Cpu(fit(Arc::new(decoded), fw, fh, width, height))
                        } else {
                            GradeInput::Scene { codes, transfer, source_to_working }
                        };
                        effects_done = self.splice_effects(c, &fx, time, fps, transforms, at_move, stats);
                        self.run_display = Some(self.scene_to_display(transfer));
                        self.executor.request_output_on_gpu();
                        let pixels = self.grade_on_stack(c, input, time, clip_time, fps, width, height, src, None);
                        stats.grade_ms += ms_since(t);
                        // The grade's run took the display step only if it
                        // ran: a failed run gives back its input, scene light
                        // the stack must not blend.
                        let displayed = self.run_display.take().is_some() && self.grade_ran;
                        let recorded = !effects_done || self.finish_spliced();
                        if !displayed || !recorded {
                            // Nothing this layer queued runs in the next
                            // layer's grade or the next frame's.
                            self.clear_queued_run();
                            return Err(if displayed { "an effect could not be recorded" } else { "a scene-referred layer's display step didn't run" });
                        }
                        stats.effects_followed_grade += u32::from(effects_done);
                        PLAN_EFFECT_CLIPS.fetch_add(u64::from(effects_done), std::sync::atomic::Ordering::Relaxed);
                        if pixels.is_empty() {
                            let slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_chain(slot, slot_a))
                        } else {
                            residency::record("composition.grade_output", "GPU grade/effect result returned CPU pixels", Work::Fallback);
                            self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
                        }
                    }
                }
            }
            ClipKind::Title(td) if title_motion_blur(c).is_some() && title_raster_moves(td).is_some() && shutter.is_some() => {
                // Its drawing moves while the shutter is open: drawn at
                // each sample's instant and placed as that sample places
                // it, the sum averaged, as `motion_blurred` does.
                let total = (c.timeline_out - c.timeline_in).to_secs_f64() as f32;
                let times: Vec<f64> = self.title_shutter.get(&c.id).map(|s| s.iter().map(|&(_, t)| t).collect()).unwrap_or_default();
                let placements = shutter.as_deref().unwrap_or_default();
                if times.len() != placements.len() || has_active_effects(&fx) || !c.grade_is_identity() {
                    return Err("title motion blur");
                }
                for (k, (&t, placement)) in times.iter().zip(placements).enumerate() {
                    // Some routes write only the rows they cover: each sample
                    // starts from an empty layer, not the one before it.
                    if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_layer_clear(slot)) {
                        return Err("title motion blur");
                    }
                    let Some((_, soft)) = self.place_title_source(td, slot, width, height, t, total) else { return Err("title motion blur") };
                    let gpu = self.executor.gpu_pipeline_or_init().ok_or("title motion blur")?;
                    if !gpu.stack_title_source_effects(slot, &soft) || !gpu.stack_shutter_add(slot, placement, k == 0) {
                        return Err("title motion blur");
                    }
                }
                redrawn = true;
                rows = (0, height);
                self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_shutter_finish(slot, times.len() as u32))
            }
            ClipKind::Title(td) => {
                let total = (c.timeline_out - c.timeline_in).to_secs_f64() as f32;
                if let Some((r, soft)) = self.place_title_source(td, slot, width, height, drawn_time(c,time), total) {
                    rows = r;
                    lead = soft.clone();
                    fx.splice(0..0,soft);
                    true
                } else { false }
            }
            ClipKind::Generator(g) if c.grade_is_identity() => {
                let total = (c.timeline_out - c.timeline_in).to_secs_f64();
                let clip_time = drawn_time(c, time);
                // Half floats, sRGB-encoded, decoded on the way into
                // the layer: the precision eight bits could not hold,
                // without moving sRGB's power curve onto the CPU.
                // The animated generators drawn on the GPU from their
                // settings alone (#2058, #2067); the CPU's drawing of
                // each is their reference.
                let on_gpu: Option<Box<dyn FnOnce(&mut klipp_effects::gpu::GpuEffectsPipeline) -> bool>> = match g {
                    klipp_core::GeneratorData::LivingGradient { a, b, angle_deg, drift_deg_per_sec, glow, grain } => {
                        let (a, b, angle, drift, glow, grain) = (*a, *b, *angle_deg, *drift_deg_per_sec, *glow, *grain);
                        Some(Box::new(move |p| p.stack_layer_living_gradient(slot, a, b, angle, drift, glow, grain, clip_time)))
                    }
                    klipp_core::GeneratorData::Grain { color, amount, size_px, speed } => {
                        let (color, amount, size, speed) = (*color, *amount, *size_px, *speed);
                        Some(Box::new(move |p| p.stack_layer_grain(slot, color, amount, size, speed, clip_time)))
                    }
                    klipp_core::GeneratorData::Countdown => {
                        let seg = klipp_core::generator::countdown_segments(width, height, clip_time, total);
                        Some(Box::new(move |p| p.stack_layer_segments(slot, seg.background, seg.color, &seg.rects)))
                    }
                    klipp_core::GeneratorData::Timer { mode, initial_seconds, format, text_color, background_color } => {
                        let seg = klipp_core::generator::timer_segments(width, height, clip_time, *mode, *initial_seconds, *format, *text_color, *background_color);
                        Some(Box::new(move |p| p.stack_layer_segments(slot, seg.background, seg.color, &seg.rects)))
                    }
                    _ => None,
                };
                if let Some(draw) = on_gpu {
                    stats.gpu_generators += 1;
                    let drawn = self.executor.gpu_pipeline_or_init().is_some_and(draw);
                    if drawn { GPU_GENERATOR_DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    drawn
                } else {
                    let t = Instant::now();
                    let px = self.generator_layer(c.id, g, width, height, clip_time, total);
                    stats.cpu_generator_ms += ms_since(t);
                    self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_srgb_f16(slot, &px))
                }
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
                        "GPU stack: a {kind} layer rendered on the CPU and uploaded: effects {names:?}, graph identity {}",
                        c.grade_is_identity(),
                    );
                }
                residency::record(match &c.kind { ClipKind::Title(_) => "title.layer", ClipKind::Generator(_) => "generator.layer", _ => "composition.layer" }, "Source/grade/effect combination has no GPU stack seed", Work::Fallback);
                let Some((pixels, _)) = self.render_clip(c, time, fps, width, height, src, transforms) else { return Ok(None) };
                effects_done = true;
                stats.cpu_layers += 1;
                self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels))
            }
        };
        if !placed {
            return Err("a layer could not be placed on the GPU");
        }
        if matches!(c.kind, ClipKind::Title(_)) && !c.grade_is_identity() && !effects_done {
            // Authored title pixels already live in the working space. Seed
            // the same grade/effect plan used for media without a CPU copy.
            if !lead.is_empty() {
                if !self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_title_source_effects(slot,&lead)) {
                    return Err("title source blur before a grade");
                }
                fx.drain(..lead.len());
                lead.clear();
            }
            let seed = self.executor.gpu_pipeline_or_init().and_then(|g| {
                Some(Arc::new(AccSeed { buffer: g.stack_layer_buffer(slot)?, device: g.device(), queue: g.queue(), width, height }))
            });
            let Some(seed) = seed else { return Err("title layer could not seed its grade") };
            effects_done = self.splice_effects(c, &fx, time, fps, transforms, at_move, stats);
            self.executor.request_output_on_gpu();
            let started = Instant::now();
            let pixels = self.grade_on_stack(c, GradeInput::Gpu(seed), time, drawn_time(c, time), fps, width, height, src, None);
            stats.grade_ms += ms_since(started);
            if !self.grade_ran || !pixels.is_empty() || (effects_done && !self.finish_spliced()) {
                // A grade the chain cannot run (a lone key node runs per
                // node): the layer as the CPU renders it, graded and with
                // its effects, up once, as before the title was seeded.
                self.clear_queued_run();
                residency::record("title.layer", "Title grade has no GPU chain; layer rendered on CPU", Work::Fallback);
                let Some((pixels, _)) = self.render_clip(c, time, fps, width, height, src, transforms) else { return Ok(None) };
                stats.cpu_layers += 1;
                if !self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_linear(slot, &pixels)) {
                    return Err("a layer could not be placed on the GPU");
                }
                effects_done = true;
            } else {
                let slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
                if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_layer_from_chain(slot, slot_a)) {
                    return Err("title grade output could not be placed");
                }
                stats.effects_followed_grade += u32::from(effects_done);
                PLAN_EFFECT_CLIPS.fetch_add(u64::from(effects_done), std::sync::atomic::Ordering::Relaxed);
            }
            rows = (0, height);
        }
        // A metal title that reflects the shot behind it (#2706) is
        // finished when the layers below are laid: its effects wait for
        // that, so a glow or a reflection takes the shot it shows.
        let reflects_shot = matches!(c.kind, ClipKind::Title(_)) && shutter.is_none()
            && self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_has_title_metal_shot(slot));
        let mut held = None;
        if !effects_done && has_active_effects(&fx) && reflects_shot {
            held = Some(Box::new(HeldEffects { fx: fx.clone(), lead: lead.clone() }));
            rows = (0, height);
        } else if !effects_done && has_active_effects(&fx) {
            // A layer placed without a grade run (a title, a generator):
            // its effects as a plan of their own over its slot (step 11d).
            let t = Instant::now();
            if !self.effects_on_layer(c, &fx, &lead, slot, time, fps, width, height, transforms, at_move, stats) {
                return Err("a layer's effects could not run as a plan");
            }
            stats.effects_ms += ms_since(t);
            // A blur or a transform can reach outside the sprite's rows.
            rows = (0, height);
        }
        if let Some(placements) = shutter {
            let t = Instant::now();
            if !redrawn && !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_shutter(slot, &placements)) {
                return Err("the shutter pass failed");
            }
            stats.composite_ms += ms_since(t);
            // Smeared along its path: anywhere in the frame.
            rows = (0, height);
            // What comes after the move, cast once from the average: a
            // rounded corner, a glow, a shadow (#2090).
            if has_active_effects(&after_move) {
                let t = Instant::now();
                if !self.effects_on_layer(c, &after_move, &[], slot, time, fps, width, height, transforms, PlanPart::AfterMove, stats) {
                    return Err("a moving layer's effects after the move could not run as a plan");
                }
                stats.effects_ms += ms_since(t);
            }
        }
        Ok(Some(Placed { opacity, rows, held, split }))
    }

    /// A placed layer's last steps before it is laid: a metal title's
    /// reflection of what is below it, then the effects that waited for
    /// it (#2706).
    #[allow(clippy::too_many_arguments)]
    fn finish_placed(&mut self, c: &Clip, slot: usize, held: Option<&HeldEffects>, time: Timecode, fps: f64, width: u32, height: u32, transforms: &HashMap<ClipId, ResolvedTransform>, stats: &mut RenderStats) -> Result<(), &'static str> {
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_title_metal_shot(slot)) {
            return Err("a metal title's reflection failed");
        }
        if let Some(h) = held {
            let t = Instant::now();
            if !self.effects_on_layer(c, &h.fx, &h.lead, slot, time, fps, width, height, transforms, PlanPart::Whole, stats) {
                return Err("a layer's effects could not run as a plan");
            }
            stats.effects_ms += ms_since(t);
        }
        Ok(())
    }

    fn place_title_source(&mut self, td: &klipp_timeline::TitleData, slot: usize, width: u32, height: u32, clip_time: f64, total: f32) -> Option<((u32,u32),Vec<BuiltinEffect>)> {
        self.place_title_source_in_scene(td, slot, width, height, clip_time, total, td, None)
            .or_else(|| self.place_title_frame(td, slot, width, height, clip_time, total))
    }

    fn place_title_source_in_scene(&mut self, td: &klipp_timeline::TitleData, slot: usize, width: u32, height: u32, clip_time: f64, total: f32, scene: &klipp_timeline::TitleData, shape_straight: Option<bool>) -> Option<((u32,u32),Vec<BuiltinEffect>)> {
        if let [object] = td.objects.as_slice() {
            let shape=match &object.content {klipp_timeline::TitleObjectContent::Shape(s)=>Some(s),_=>None};
            let image=match &object.content {klipp_timeline::TitleObjectContent::Image(i)=>Some(i),_=>None};
            if shape.is_some() || image.is_some() {
                // Shape and image sources enter through scene composition,
                // including a single one, to preserve premultiplication and
                // half precision; an image is its fitted picture placed as a
                // sprite, posed and faded as a shape's layer is (#2077).
                if let Some(shape_straight)=shape_straight {
                    let transform=if let Some(fit)=shape.and_then(|s|s.fit) {
                        let Some(transform)=self.titles.resolved_shape_fit(scene,fit,width,height,Some(clip_time as f32)) else {
                            return self.executor.gpu_pipeline_or_init()?.stack_layer_clear(slot).then_some(((0,0),Vec::new()));
                        };
                        transform
                    } else if image.is_some() {object.transform.evaluated_at(clip_time as f32 as f64)
                    } else {object.transform.evaluated_at(clip_time)};
                    // A still path moved or faded whole is its resting render
                    // posed, which the CPU's byte layer rounds exactly; only
                    // changing geometry is scanned here at each pose (#2201).
                    if shape.is_some_and(|s|matches!(s.kind,klipp_timeline::ShapeKind::Path(_)))
                        && object.fx().motion.is_active() && !klipp_title::keys_path_geometry(td) {return None;}
                    // Entrance/exit fades act on the painted byte layer before
                    // edge fades, object opacity and scene masking.
                    let mut source_fx=object.fx().clone();
                    let edges=std::mem::take(&mut source_fx.edge_fade);
                    let motion=std::mem::take(&mut source_fx.motion);
                    if !source_fx.is_default()
                        || motion.enter.iter().chain(motion.exit.iter())
                        .any(|m|!matches!(m.kind,klipp_timeline::MotionKind::Fade|klipp_timeline::MotionKind::Rise|klipp_timeline::MotionKind::Drop|klipp_timeline::MotionKind::SlideLeft|klipp_timeline::MotionKind::SlideRight|klipp_timeline::MotionKind::ScaleUp|klipp_timeline::MotionKind::ScaleDown|klipp_timeline::MotionKind::Blur|klipp_timeline::MotionKind::MaskRise|klipp_timeline::MotionKind::MaskDrop|klipp_timeline::MotionKind::Pop|klipp_timeline::MotionKind::Slam|klipp_timeline::MotionKind::Larger|klipp_timeline::MotionKind::Smaller|klipp_timeline::MotionKind::FlyLeft|klipp_timeline::MotionKind::FlyRight|klipp_timeline::MotionKind::FlyTop|klipp_timeline::MotionKind::FlyBottom|klipp_timeline::MotionKind::Bounce|klipp_timeline::MotionKind::Flicker|klipp_timeline::MotionKind::Typewriter|klipp_timeline::MotionKind::FocusIn|klipp_timeline::MotionKind::SoftRise)) {
                        if image.is_some() {
                            residency::record("title.image","Image layer with layer styles, or a motion the image route does not pose, rasters with its title",Work::Fallback);
                        }
                        return None;
                    }
                    let blurs=motion.enter.iter().chain(motion.exit.iter()).any(|m|matches!(m.kind,klipp_timeline::MotionKind::Blur|klipp_timeline::MotionKind::FocusIn|klipp_timeline::MotionKind::SoftRise));
                    // Shapes pose as a whole layer: character/word stagger has
                    // no effect, and both fades use unit zero as on the CPU.
                    let (enter,exit)=motion.progress(clip_time as f32,Some(total),0);
                    // Mask reveals clip the source without fading their own alpha.
                    let fade=motion.enter.map_or(1.0,|m|m.pose(enter,1.0).0)*motion.exit.map_or(1.0,|m|m.pose(exit,1.0).0);
                    let travel=|m:Option<klipp_timeline::Motion>,progress:f32| {
                        use klipp_timeline::MotionKind;
                        m.map_or([0.0;4],|m| {
                            let direction=match m.kind {MotionKind::MaskRise=>[0.0,1.15],MotionKind::MaskDrop=>[0.0,-1.15],MotionKind::Rise|MotionKind::SoftRise=>[0.0,1.0],MotionKind::Drop=>[0.0,-1.0],MotionKind::SlideLeft=>[1.5,0.0],MotionKind::SlideRight=>[-1.5,0.0],MotionKind::FlyLeft=>[-8.0,0.0],MotionKind::FlyRight=>[8.0,0.0],MotionKind::FlyTop=>[0.0,-6.0],MotionKind::FlyBottom=>[0.0,6.0],MotionKind::Bounce=>[0.0,-2.5],_=>[0.0,0.0]};
                            [1.0-progress,if matches!(m.kind,MotionKind::MaskRise|MotionKind::MaskDrop) {1.0}else{m.distance},direction[0],direction[1]]
                        })
                    };
                    let clip_rows=motion.enter.iter().chain(motion.exit.iter()).any(|m|matches!(m.kind,klipp_timeline::MotionKind::MaskRise|klipp_timeline::MotionKind::MaskDrop));
                    let enter_travel=travel(motion.enter,enter);
                    let exit_travel=travel(motion.exit,exit);
                    let moves=enter_travel[2..].iter().chain(exit_travel[2..].iter()).any(|v|*v!=0.0);
                    let scales=motion.enter.iter().chain(motion.exit.iter()).any(|m|matches!(m.kind,klipp_timeline::MotionKind::ScaleUp|klipp_timeline::MotionKind::ScaleDown|klipp_timeline::MotionKind::Pop|klipp_timeline::MotionKind::Slam|klipp_timeline::MotionKind::Larger|klipp_timeline::MotionKind::Smaller|klipp_timeline::MotionKind::FocusIn));
                    let motion_scale=motion.enter.map_or(1.0,|m|m.pose(enter,1.0).3)*motion.exit.map_or(1.0,|m|m.pose(exit,1.0).3);
                    let selected_anchor=motion.anchor_at(enter,exit);
                    let solid=matches!(selected_anchor,Some(klipp_core::title_motion::MotionAnchor::Solid));
                    let anchor=match selected_anchor {
                        Some(klipp_core::title_motion::MotionAnchor::Point{x,y})=>[x,y],
                        _=>[0.5,0.5],
                    };
                    let gradient=shape.and_then(|s|s.gradient.as_ref()).filter(|g|g.enabled).map(title_gradient_fill);
                    let scale=klipp_title::frame_scale(width,height);
                    // The decoded picture stays on the GPU; its box is fitted
                    // there each frame, keyed or not (#2342).
                    let picture=image.map(|img| self.titles.decoded_image(img).and_then(|d| {
                        let placed=klipp_title::image_placement(&d,img.fit,&transform,width,height)?;
                        Some((d,placed))
                    }));
                    let gpu=self.executor.gpu_pipeline_or_init()?;
                    if let Some(Some((d,_)))=&picture {
                        if !gpu.stack_title_image_fits(d.texels.len()) {
                            residency::record("title.image","Image larger than one GPU buffer rasters with its title",Work::Fallback);
                            return None;
                        }
                        if !gpu.stack_title_image_resident(d.id) {
                            residency::record("title.image","Image layer's picture uploaded once, then kept on the GPU",Work::Preparation);
                        }
                    }
                    let (cx,cy)=(transform.center.x*width as f32,transform.center.y*height as f32);
                    let (hw,hh)=((transform.size.x*width as f32*0.5).max(0.0),(transform.size.y*height as f32*0.5).max(0.0));
                    let painted=if let Some(picture)=picture {
                        match picture {
                            Some((d,(rect,cols,rows)))=>gpu.stack_title_image(slot,d.id,&d.texels,[d.w as u32,d.h as u32],rect,[cols.0,rows.0,cols.1,rows.1]),
                            None=>gpu.stack_layer_clear(slot),
                        }
                    } else if let Some(shape)=shape {match &shape.kind {
                        klipp_timeline::ShapeKind::Rectangle{corner_radius}=>gpu.stack_title_rectangle(slot,[cx,cy,hw,hh],[cx+(transform.anchor_norm.x-0.5)*hw*2.0,cy+(transform.anchor_norm.y-0.5)*hh*2.0],transform.rotation.to_radians(),corner_radius.max(0.0)*scale,
                            shape.fill.unwrap_or([0;4]),shape.stroke.map(|s|(s.color,s.width.max(0.0)*scale)),shape_straight,gradient.as_ref()),
                        klipp_timeline::ShapeKind::Path(path)=>{
                            // Scanned from the CPU's own points, its keys and
                            // transform taken at the CPU's single-precision time
                            // (#2201); a path that paints nothing leaves no layer.
                            let transform=if shape.fit.is_some() {transform.clone()} else {object.transform.evaluated_at(clip_time as f32 as f64)};
                            // A gradient fill is evaluated per pixel at its box
                            // fractions (#2208).
                            match klipp_title::path_coverage(path,&transform,width,height,clip_time as f32,gradient.is_some()) {
                                Some(c)=>gpu.stack_title_path(slot,&klipp_effects::gpu::TitlePath {bounds:c.bounds,fill_edges:c.fill_edges,stroke_segments:c.stroke_segments,half:c.half,fill:c.fill,stroke:c.stroke,glow:c.glow,gradient:gradient.clone().map(|g|(g,c.unmap))}),
                                None=>gpu.stack_layer_clear(slot),
                            }
                        }
                    }} else {false};
                    if !painted {return None;}
                    if motion.is_active() {
                        if !gpu.stack_title_precision(slot,true) {return None;}
                        if scales && motion_scale == 0.0 {
                            // Smaller starts collapsed: there is no sprite to sample.
                            if !gpu.stack_layer_clear(slot) {return None;}
                        } else if blurs {
                            let blur_part=|m:Option<klipp_timeline::Motion>,progress:f32| {
                                use klipp_timeline::MotionKind;
                                let coefficient=m.map_or(0.0,|m|match m.kind {MotionKind::Blur=>0.12,MotionKind::FocusIn=>0.14,MotionKind::SoftRise=>0.08,_=>0.0});
                                // Non-blur easing may overshoot; its progress must not
                                // enter blur validation or affect the blur radius.
                                if coefficient==0.0 {[0.0;2]}else{[1.0-progress,coefficient]}
                            };
                            let a=blur_part(motion.enter,enter);let b=blur_part(motion.exit,exit);
                            let scaled=scales.then_some([motion_scale,fade,anchor[0],anchor[1]]);
                            if !gpu.stack_title_motion_blur_pose(slot,[a[0],a[1],b[0],b[1]],enter_travel,exit_travel,scaled,solid,clip_rows)
                                || (!scales && !gpu.stack_title_motion_alpha(slot,fade)) {return None;}
                        } else if scales {
                            if !gpu.stack_title_motion_scale(slot,enter_travel,exit_travel,motion_scale,fade,anchor,solid,clip_rows) {return None;}
                        } else if (moves && !gpu.stack_title_motion_translate(slot,enter_travel,exit_travel,clip_rows))
                            || !gpu.stack_title_motion_alpha(slot,fade) {return None;}
                    }
                    if !edges.is_none() && (!gpu.stack_title_precision(slot,true)
                        || !gpu.stack_title_alpha(slot,[edges.top,edges.bottom,edges.left,edges.right],1.0)) {return None;}
                    // Scene objects arrive with opacity stripped; matte sources retain it.
                    let opacity=object.evaluated_opacity(clip_time).clamp(0.0,1.0);
                    if (opacity-1.0).abs()>=0.001 && (!gpu.stack_title_precision(slot,true) || !gpu.stack_title_alpha(slot,[0.0;4],opacity)) {return None;}
                    return Some(((0,height),Vec::new()));
                }
            }
        }
        // Whole-object loops are folded into the authored object before layout,
        // just as in the CPU scene. Keep content/wave loops for the glyph source.
        if let [object] = td.objects.as_slice() {
            if let klipp_timeline::TitleObjectContent::TextBox(tb) = &object.content {
                let loops: Vec<_> = object.fx().loops.iter().filter(|l| !title_source_loops(std::slice::from_ref(*l))).cloned().collect();
                if !loops.is_empty() && !matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..}) {
                    let text=tb.at(clip_time);
                    let font_px=text.font_size*klipp_title::frame_scale(width,height);
                    let (text_w,text_h)=text.loop_extent(font_px);
                    let pose=klipp_core::title_motion::eval_loops(&loops,clip_time as f32,Some(total),klipp_timeline::LoopGeometry {frame_w:width as f32,frame_h:height as f32,text_w,text_h,font_px},text.color);
                    // A lone layer the CPU places from its resting render is
                    // softened there by the placed frame's linear `FocusBlur`,
                    // which `place_title_frame` matches; only a layer rastered
                    // afresh is faded and blurred as the authored layer is.
                    let whole=klipp_core::title_motion::eval_loops(&object.fx().loops,clip_time as f32,Some(total),klipp_timeline::LoopGeometry {frame_w:width as f32,frame_h:height as f32,text_w,text_h,font_px},text.color);
                    let placed_on_cpu=scene.objects.iter().filter(|o|o.visible).count()==1 && !scene.mixes_layers() && !title_rasters_each_frame(scene,&text,&whole);
                    if let Some((axis,focus))=pose.focus.filter(|_|!placed_on_cpu) {
                        let mut source=td.clone();let target=&mut source.objects[0];
                        target.fx_mut().loops.retain(|l|title_source_loops(std::slice::from_ref(l)));
                        let edges=target.fx().edge_fade;target.fx_mut().edge_fade=Default::default();
                        target.opacity*=pose.opacity;
                        if target.opacity<=0.001 {
                            return self.executor.gpu_pipeline_or_init()?.stack_layer_clear(slot).then_some(((0,0),Vec::new()));
                        }
                        let opacity=target.evaluated_opacity(clip_time);
                        target.opacity=1.0;target.opacity_keyframes.clear();
                        target.transform.center.x+=pose.dx/width as f32;
                        target.transform.center.y+=pose.dy/height as f32;
                        let (_,soft)=self.place_title_source_in_scene(&source,slot,width,height,clip_time,total,scene,shape_straight)?;
                        let gpu=self.executor.gpu_pipeline_or_init()?;
                        let vertical=axis==klipp_core::title_motion::RampAxis::Vertical;
                        if !gpu.stack_title_source_effects(slot,&soft)
                            || !gpu.stack_title_fade(slot,[edges.top,edges.bottom,edges.left,edges.right],1.0,Some((vertical,focus.band,focus.edge_opacity)))
                            || !gpu.stack_title_focus_blur(slot,focus.blur*klipp_title::frame_scale(width,height),vertical,focus.band)
                            || !gpu.stack_title_alpha(slot,[0.0;4],opacity) {return None;}
                        return Some(((0,height),Vec::new()));
                    }
                    // A lone layer the CPU places from its resting render is
                    // that render posed here too: folding the loop into its
                    // position made each frame a new title, rastered afresh
                    // on the CPU every frame (#2335).
                    if pose.focus.is_none() && placed_on_cpu {
                        return self.place_title_frame(td,slot,width,height,clip_time,total);
                    }
                    if pose.focus.is_none() {
                        let mut source=td.clone();
                        let target=&mut source.objects[0];
                        target.fx_mut().loops.retain(|l|title_source_loops(std::slice::from_ref(l)));
                        target.opacity*=pose.opacity;
                        target.transform.center.x+=pose.dx/width as f32;
                        target.transform.center.y+=pose.dy/height as f32;
                        if target.opacity<=0.001 {
                            return self.executor.gpu_pipeline_or_init()?.stack_layer_clear(slot).then_some(((0,0),Vec::new()));
                        }
                        return self.place_title_source_in_scene(&source,slot,width,height,clip_time,total,scene,shape_straight);
                    }
                }
            }
        }
        if let [object] = td.objects.as_slice() {
            if !matches!(object.content,klipp_timeline::TitleObjectContent::Shape(_)) && !matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..}) && (object.opacity!=1.0 || !object.opacity_keyframes.is_empty()) {
                let mut source=td.clone();
                source.objects[0].opacity=1.0;
                source.objects[0].opacity_keyframes.clear();
                let (rows,soft)=self.place_title_source_in_scene(&source,slot,width,height,clip_time,total,scene,shape_straight)?;
                if !self.executor.gpu_pipeline_or_init()?.stack_title_source_effects(slot,&soft) { return None; }
                return self.executor.gpu_pipeline_or_init()?.stack_title_alpha(slot,[0.0;4],object.evaluated_opacity(clip_time)).then_some((rows,Vec::new()));
            }
            let edges=object.fx().edge_fade;
            if !matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..}) && !edges.is_none() && title_source_loops(&object.fx().loops) {
                let mut source=td.clone();
                source.objects[0].fx_mut().edge_fade=Default::default();
                let (rows,soft)=self.place_title_source_in_scene(&source,slot,width,height,clip_time,total,scene,shape_straight)?;
                if !soft.is_empty() { return None; }
                return self.executor.gpu_pipeline_or_init()?.stack_title_alpha(slot,[edges.top,edges.bottom,edges.left,edges.right],1.0).then_some((rows,soft));
            }
        }
        // Rotation follows source post-processing, before edge fades and opacity.
        if let [object] = td.objects.as_slice() {
            if let klipp_timeline::TitleObjectContent::TextBox(tb) = &object.content {
                let transform=object.transform.evaluated_at(clip_time);
                if !matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..})
                    && !matches!(tb.text_path,Some(klipp_timeline::TextPath::Layer{..}))
                    && (object.transform.rotation!=0.0 || !object.transform.rotation_keyframes.is_empty()) {
                    let mut source=td.clone();
                    source.objects[0].transform.rotation=0.0;
                    source.objects[0].transform.rotation_keyframes.clear();
                    let (rows,soft)=self.place_title_source_in_scene(&source,slot,width,height,clip_time,total,scene,shape_straight)?;
                    if !soft.is_empty() {return None;}
                    if transform.rotation.abs()<=0.001 {return Some((rows,soft));}
                    let pivot=[(transform.center.x+(transform.anchor_norm.x-0.5)*transform.size.x)*width as f32,
                        (transform.center.y+(transform.anchor_norm.y-0.5)*transform.size.y)*height as f32];
                    return self.executor.gpu_pipeline_or_init()?.stack_title_rotate(slot,pivot,transform.rotation.to_radians()).then_some(((0,height),soft));
                }
            }
        }
        // Render objects independently so each keeps its own source cache,
        // GPU styles and time evaluation before authored-space composition.
        if td.objects.len() > 1 || td.mixes_layers() || matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..}) || td.objects.iter().any(|o|matches!(o.content,klipp_timeline::TitleObjectContent::Shape(_)|klipp_timeline::TitleObjectContent::Image(_)))
 {
            let object_slot = self.executor.gpu_pipeline_or_init()?.stack_title_object_slot(0)?;
            let initialized=match td.background {
                klipp_timeline::TitleBackground::FullFrame{color}=>self.executor.gpu_pipeline_or_init()?.stack_layer_segments(slot,color,[0;4],&[]),
                _=>self.executor.gpu_pipeline_or_init()?.stack_layer_clear(slot),
            };
            if !initialized {return None;}
            // Every text box its own backing, as the CPU title (#2487).
            let backing=matches!(td.background,klipp_timeline::TitleBackground::Color{..}).then(||td.background.clone());
            let mut crop_faded_matte_scene=false;
            for object in td.objects.iter().filter(|o| o.visible && !td.is_matte(o.id)) {
                let opacity = object.evaluated_opacity(clip_time).clamp(0.0,1.0);
                let mut single = td.clone();
                single.objects = vec![object.clone()];
                single.background=if matches!(object.content,klipp_timeline::TitleObjectContent::TextBox(_)) {backing.clone().unwrap_or_default()} else {klipp_timeline::TitleBackground::None};
                single.objects[0].opacity = 1.0;
                single.objects[0].opacity_keyframes.clear();
                single.objects[0].blend=klipp_core::BlendMode::Normal;
                single.objects[0].mask=None;
                let (_,soft) = self.place_title_source_in_scene(&single,object_slot,width,height,clip_time,total,scene,Some(object.blend!=klipp_core::BlendMode::Normal || td.mask_of(object).is_some()))?;
                if !self.executor.gpu_pipeline_or_init()?.stack_title_source_effects(object_slot,&soft) { return None; }
                // Shapes with opacity are byte layers in the CPU painter; at full
                // opacity an undressed normal shape paints fractional colour directly.
                // An image is always a byte layer.
                let plain_shape=matches!(object.content,klipp_timeline::TitleObjectContent::Shape(_)) && object.fx().is_default();
                let image=matches!(object.content,klipp_timeline::TitleObjectContent::Image(_));
                if (plain_shape || image) && (opacity-1.0).abs()>=0.001 && !self.executor.gpu_pipeline_or_init()?.stack_title_precision(object_slot,true) {return None;}
                // Apply opacity before the mask, exactly as the authored stack.
                if !self.executor.gpu_pipeline_or_init()?.stack_title_alpha(object_slot,[0.0;4],opacity) { return None; }
                if let Some((matte,kind)) = td.mask_of(object) {
                    crop_faded_matte_scene |= matches!(matte.content,klipp_timeline::TitleObjectContent::Shape(_))
                        && (!matte.fx().edge_fade.is_none() || matte.fx().motion.is_active());
                    let matte_slot=self.executor.gpu_pipeline_or_init()?.stack_title_object_slot(1)?;
                    let mut mask_title=td.clone();
                    mask_title.objects=vec![matte.clone()];
                    mask_title.background=klipp_timeline::TitleBackground::None;
                    mask_title.objects[0].visible=true;
                    mask_title.objects[0].mask=None;
                    mask_title.objects[0].blend=klipp_core::BlendMode::Normal;
                    let (_,soft)=self.place_title_source_in_scene(&mask_title,matte_slot,width,height,clip_time,total,scene,Some(false))?;
                    if !self.executor.gpu_pipeline_or_init()?.stack_title_source_effects(matte_slot,&soft) { return None; }
                    use klipp_core::layer_mask::LayerMaskKind;
                    let luma=matches!(kind,LayerMaskKind::Luma|LayerMaskKind::InvertedLuma);
                    let invert=matches!(kind,LayerMaskKind::InvertedAlpha|LayerMaskKind::InvertedLuma);
                    if !self.executor.gpu_pipeline_or_init()?.stack_title_mask(matte_slot,object_slot,luma,invert) { return None; }
                }
                // A path is laid in straight alpha on the CPU, never painted
                // into what is under it as a rectangle's colour is (#2201).
                let path=matches!(&object.content,klipp_timeline::TitleObjectContent::Shape(s) if matches!(s.kind,klipp_timeline::ShapeKind::Path(_)));
                let painted=plain_shape && !path && (opacity-1.0).abs()<0.001 && object.blend==klipp_core::BlendMode::Normal && td.mask_of(object).is_none();
                let gpu=self.executor.gpu_pipeline_or_init()?;
                if !(if painted {gpu.stack_title_painted_over(object_slot,slot)} else {gpu.stack_title_over(object_slot,slot,1.0,object.blend)}) {return None;}
            }
            let gpu=self.executor.gpu_pipeline_or_init()?;
            // The CPU keeps only the final scene's alpha bounding box. Preserve
            // faint pixels inside that box, but discard its low-alpha fringe.
            if crop_faded_matte_scene && !gpu.stack_title_crop_to_alpha_bounds(slot) {return None;}
            return gpu.stack_title_precision(slot,false).then_some(((0,height),Vec::new()));
        }
        let rows;
        let mut lead = Vec::new();
                let post = title_post_source(td);
                let td = post.as_ref().map_or(td, |(title, _, _)| title);
                let post_requires_glyph=post.is_some() && td.objects.iter().any(|o|o.fx().motion.is_active());
                let to_working = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, self.working)
                    .unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
                let solid = self.titles.prepare_text3d(td, width, height, clip_time as f32, Some(total));
                // A metal solid (#2706): its text's material, resolved as a
                // flat face's is.
                let scale = klipp_title::frame_scale(width, height);
                let metal = td.primary_text().and_then(|t| t.material.map(|m| title_metal(&m.clamped(), t.font_size * scale, scale)));
                let solid_roi = solid.as_ref().and_then(|solid| {
                    let p = self.executor.gpu_pipeline_or_init()?;
                    if !self.gpu_text3d.as_ref().is_some_and(|g| g.same_device(p)) {
                        self.gpu_text3d = Some(crate::title3d_gpu::GpuText3d::new(p));
                    }
                    self.gpu_text3d.as_mut()?.render(p, slot, solid.geometry.id, &solid.scene(), width, height, to_working, metal.as_ref())
                });
                let placed = if let (Some(solid), Some(mut roi)) = (solid, solid_roi) {
                    let passes = crate::synth::title_style_passes(&solid.fx, klipp_title::frame_scale(width, height), clip_time);
                    if passes.len() > 1 && roi[2] > 0 && roi[3] > 0 {
                        let x1 = roi[0].saturating_add(roi[2]).saturating_add(solid.padding).min(width);
                        let y1 = roi[1].saturating_add(roi[3]).saturating_add(solid.padding).min(height);
                        roi[0] = roi[0].saturating_sub(solid.padding);
                        roi[1] = roi[1].saturating_sub(solid.padding);
                        roi[2] = x1 - roi[0];
                        roi[3] = y1 - roi[1];
                        rows = (roi[1], y1);
                        self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_title_styles(slot, roi, &passes))
                    } else {
                        rows = (roi[1], roi[1] + roi[3]);
                        true
                    }
                } else if let Some(source) = self.titles.glyph_source_in_scene(td, width, height, clip_time as f32, Some(total), Some(scene)) {
                    if source.prepared>0 {
                        residency::record("title.glyph", if source.rerasterized { "Glyph working set exceeded the retained cache" } else { "Bounded glyph source cache miss" }, if source.rerasterized { Work::Fallback } else { Work::Preparation });
                    }
                    rows = source.backing.map_or((source.roi[1], source.roi[1] + source.roi[3]), |(rect, _)| (source.roi[1].min(rect[1]), (source.roi[1] + source.roi[3]).max(rect[1] + rect[3])));
                    let passes=crate::synth::title_style_passes(&source.fx,klipp_title::frame_scale(width,height),clip_time);
                    let rich_slot=if source.pattern.is_none() && source.gradient.is_none() && source.material.is_none() && (!source.face_draws.is_empty() || !source.face_stripes.is_empty()) {
                        Some(self.executor.gpu_pipeline_or_init()?.stack_title_object_slot(2)?)
                    }else{None};
                    // Shadows, glows, outlines and inner shadows read colour
                    // glyphs' coverage, the face does not: the face is a copy
                    // with that coverage taken out (#2209).
                    let face_slot=if rich_slot.is_none() && source.glyphs.iter().any(|g|g.native_colour)
                        && passes.iter().any(|s|!matches!(s,klipp_effects::gpu::TitleStyle::Face)) {
                        Some(self.executor.gpu_pipeline_or_init()?.stack_title_object_slot(2)?)
                    }else{None};
                    let coverage=face_slot.unwrap_or(slot);
                    // The colour glyphs then lie between the face and the
                    // inner shadows, as on the CPU.
                    let colour_slot=match face_slot {Some(_)=>Some(self.executor.gpu_pipeline_or_init()?.stack_title_object_slot(3)?),None=>None};
                    // A bitmap glyph's pose: its alpha and its rows (#2203).
                    let place=|g:&klipp_title::GlyphDraw| klipp_effects::gpu::GlyphPlace {alpha:g.alpha,clip_y:g.clip_y};
                    self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                        p.stack_layer_clear(slot) && source.glyphs.iter().all(|g|match g.target_size {None=>p.stack_title_glyph(slot,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],g.origin,source.color,place(g)),Some(size)=>p.stack_title_resized_colour_glyph(slot,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],size,g.origin,source.color,2,place(g))})
                            && source.outlines.iter().all(|g|p.stack_title_outline(slot,g.id,&g.edges,g.origin,g.size,g.placement,source.color,klipp_effects::gpu::TitleGlyphPose{alpha:g.alpha,clip_y:g.clip_y,source_size:g.source_size,blur:g.blur,radians:g.radians,face:None,path:g.path}))
                            && rich_slot.is_none_or(|rich|p.stack_layer_clear(rich) && source.face_draws.iter().all(|g|p.stack_title_outline(rich,g.id,&g.edges,g.origin,g.size,g.placement,g.color.unwrap_or(source.color),klipp_effects::gpu::TitleGlyphPose{alpha:g.alpha,clip_y:g.clip_y,source_size:g.source_size,blur:g.blur,radians:g.radians,face:Some(source.roi),path:g.path})) && source.face_stripes.iter().all(|(rect,color)|p.stack_title_stripe(rich,*rect,*color,true)))
                            && source.stripes.iter().all(|&rect|p.stack_title_stripe(slot,rect,source.color,false))
                            // Native RGBA is painted after the text fill, but its
                            // coverage first contributes to the max mask, then is
                            // subtracted from that face exactly as on the CPU.
                            && face_slot.is_none_or(|face|p.stack_layer_copy(slot,face))
                            && source.glyphs.iter().filter(|g|g.native_colour).all(|g|match g.target_size {None=>p.stack_title_colour_coverage_subtract(coverage,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],g.origin,place(g)),Some(size)=>p.stack_title_resized_colour_glyph(coverage,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],size,g.origin,source.color,3,place(g))})
                            && colour_slot.is_none_or(|c|p.stack_layer_clear(c) && source.glyphs.iter().filter(|g|g.native_colour).all(|g|match g.target_size {None=>p.stack_title_colour_glyph(c,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],g.origin,place(g)),Some(size)=>p.stack_title_resized_colour_glyph(c,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],size,g.origin,source.color,1,place(g))}))
                            && (source.roi[2]==0 || source.roi[3]==0 || p.stack_title_styles_with_face(slot,source.roi,&passes,klipp_effects::gpu::TitleFace {alpha:source.color[3] as f32/255.0,outline_cutoff:source.outline_cutoff,write_cutoff:source.write_cutoff,backing:source.backing,fill:title_fill(&source,klipp_title::frame_scale(width,height)),bounds:source.bounds,rich_slot,face_slot,colour_slot}))
                            && (colour_slot.is_some() || source.glyphs.iter().filter(|g|g.native_colour).all(|g|match g.target_size {None=>p.stack_title_colour_glyph(slot,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],g.origin,place(g)),Some(size)=>p.stack_title_resized_colour_glyph(slot,g.id,&g.pixels.texels,[g.pixels.w,g.pixels.h],size,g.origin,source.color,1,place(g))}))
                            && source.texture.as_ref().is_none_or(|t|source.roi[2]==0 || source.roi[3]==0 || p.stack_title_texture(slot,source.roi,klipp_effects::gpu::TitleTexture {
                                id:t.id,texels:&t.pixels.texels,size:[t.pixels.w,t.pixels.h],bounds:source.bounds,
                                scale:t.fill.scale,offset:[t.fill.offset_x,t.fill.offset_y],radians:t.fill.rotation.to_radians(),opacity:t.fill.opacity,tint:t.fill.tint,
                                mode:match t.fill.tile_mode {klipp_timeline::TextureTileMode::Stretch=>0,klipp_timeline::TextureTileMode::Tile=>1,klipp_timeline::TextureTileMode::Fit=>2,klipp_timeline::TextureTileMode::Cover=>3},
                            }))
                    })
                } else if post_requires_glyph || matches!(td.text_path(), Some(klipp_timeline::TextPath::Layer { .. })) {
                    // A fallback placed source may defer its blur. It cannot
                    // safely feed post-processing before that blur has run.
                    return None;
                } else if let Some(source) = self.titles.style_source(td, width, height, clip_time as f32, Some(total)) {
                    match source.face_work {
                        klipp_title::StyleFaceWork::Reused => {},
                        klipp_title::StyleFaceWork::Prepared => residency::record("title.style_face", "Bounded style-face cache miss", Work::Preparation),
                        klipp_title::StyleFaceWork::AnimatedRaster => residency::record("title.style_face", "Animated or uncached face rasterized before GPU styling", Work::Fallback),
                    }
                    rows = (source.roi[1], source.roi[1] + source.roi[3]);
                    let passes = crate::synth::title_style_passes(&source.fx, klipp_title::frame_scale(width, height), clip_time);
                    self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                        p.stack_layer_place_sprite_posed(slot, source.id, &source.pixels.texels, sprite_box(&source.pixels), 0, 0, 1.0, 1.0, (0.0, 0.0), None, None, rows.0, rows.1)
                            && p.stack_title_styles(slot, source.roi, &passes)
                    })
                } else {
                    let (frame_rows, frame_lead) = self.place_title_frame(td, slot, width, height, clip_time, total)?;
                    rows = frame_rows;
                    lead = frame_lead;
                    true
                 };
                let ok = placed && post.as_ref().is_none_or(|(_, chromatic, glitch)| {
                    self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                        p.stack_title_post(slot, *chromatic * klipp_title::frame_scale(width, height), *glitch, clip_time as f32)
                    })
                });
                ok.then_some((rows, lead))
    }

    // Keep the complete scene available to cached frames (notably linked paths)
    // when a per-object source cannot represent it. Deferred blur stays before
    // the clip effects and transform, as on the original cached-frame route.
    fn place_title_frame(&mut self, td: &klipp_timeline::TitleData, slot: usize, width: u32, height: u32, clip_time: f64, total: f32) -> Option<((u32,u32),Vec<BuiltinEffect>)> {
        let rows;
        let mut lead = Vec::new();
        let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
        let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
        // A title that is its resting render moved and faded
        // keeps that render on the GPU: no raster, no upload, one
        // pass. Anything else is rendered here and goes up as rows.
        let resting = self.titles.resting_renders();
        let frame = self.titles.frame_with_window(td, width, height, gpu, clip_time as f32, Some(total));
        // Keyed path geometry is rasterised again at each new pose; the
        // cache only serves a pose seen before (#2201).
        if self.titles.resting_renders() > resting {
            if klipp_title::keys_path_geometry(td) {
                residency::record("title.path_geometry", "Keyed vector path geometry rasterized on CPU", Work::Fallback);
            } else {
                // The one-off raster a still title's first frame waits on,
                // 50 to 90 ms at 1080p: counted, so it shows (#2335).
                residency::record("title.resting", "Still title rastered on the CPU once, then kept on the GPU", Work::Preparation);
            }
        }
        let placed = match frame {
            klipp_title::TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, blur, shape, rows: r } => {
                rows = r;
                // The blur entrance, and a roll's focus band going
                // soft (#592): the placed sprite blurred first, as
                // the CPU blurs the title it draws, and then the
                // clip's effects and transform (#995). Added to
                // the end, they ran after them, so a scaled title
                // was blurred at the wrong size.
                let mut soft = Vec::new();
                if blur > 0.5 {
                    soft.push(BuiltinEffect::GaussianBlur { radius: blur });
                }
                if let Some((radius, axis, band)) = self.titles.last_focus_blur() {
                    soft.push(BuiltinEffect::FocusBlur { radius, vertical: axis == klipp_core::title_motion::RampAxis::Vertical, band });
                }
                lead = soft.clone();
                self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                    // The layer's fade by where it is, the sprite
                    // shader's own (#592).
                    let shape = shape.map(|s| klipp_effects::gpu::SpriteShape {
                        top: s.edges.top,
                        bottom: s.edges.bottom,
                        left: s.edges.left,
                        right: s.edges.right,
                        focus_vertical: s.focus.map(|(axis, _)| axis == klipp_core::title_motion::RampAxis::Vertical),
                        band: s.focus.map_or(0.5, |(_, f)| f.band),
                        edge_opacity: s.focus.map_or(1.0, |(_, f)| f.edge_opacity),
                    });
                    p.stack_layer_place_sprite_posed(slot, sprite.id, &sprite.pixels.texels, sprite_box(&sprite.pixels), dx, dy, opacity, scale, anchor, under, shape, r.0, r.1)
                })
            }
            klipp_title::TitleFrame::Glyphs { sprite, poses, rows: r } => {
                rows = r;
                let packed: Vec<[f32; 16]> = poses.iter().map(|p| p.packed()).collect();
                match &sprite.units {
                    Some(u) => self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                        p.stack_layer_place_glyphs(slot, sprite.id, &sprite.pixels.texels, sprite_box(&sprite.pixels), (u.x0, u.y0, u.w, u.h, &u.index), &packed, r.0, r.1)
                    }),
                    None => false,
                }
            }
            klipp_title::TitleFrame::Layers { layers, rows: r } => {
                rows = r;
                self.executor.gpu_pipeline_or_init().is_some_and(|p| {
                    layers.iter().enumerate().all(|(index, (sprite, opacity))| {
                        if index == 0 {
                            p.stack_layer_place_sprite_posed(slot, sprite.id, &sprite.pixels.texels, sprite_box(&sprite.pixels), 0, 0, *opacity, 1.0, sprite.anchor(), None, None, r.0, r.1)
                        } else {
                            p.stack_layer_place_sprite_over(slot, sprite.id, &sprite.pixels.texels, sprite_box(&sprite.pixels), *opacity, r.0, r.1)
                        }
                    })
                })
            }
            klipp_title::TitleFrame::Rendered { pixels, rows: r } => {
                residency::record("title.raster", "Title animation requires CPU raster/composition", Work::Fallback);
                rows = r;
                // Only the box the title covers goes up (#190).
                self.executor.gpu_pipeline_or_init().is_some_and(|p| p.stack_layer_from_srgb_f16_box(slot, &pixels.texels, sprite_box(&pixels)))
            }
        };
        placed.then_some((rows, lead))
    }

    fn raster_generator(&mut self, g: &klipp_core::GeneratorData, width: u32, height: u32, clip_time: f64, total: f64) -> Vec<u32> {
        let start = Instant::now();
        let pixels = crate::synth::render_generator_with_slate_f16(&mut self.titles, g, width, height, clip_time, total);
        CPU_GENERATOR_DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CPU_GENERATOR_US.fetch_add(start.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        pixels
    }

    /// `g`'s picture for the GPU stack, in half floats: a still one from
    /// [`Self::still_generators`] when it is the same at this size, else
    /// rasterised (and kept, when still).
    fn generator_layer(&mut self, clip: ClipId, g: &klipp_core::GeneratorData, width: u32, height: u32, clip_time: f64, total: f64) -> Arc<Vec<u32>> {
        let _clip_residency = residency::clip(clip.0);
        if !is_still_generator(g) {
            residency::record(generator_feature(g), "Time-dependent CPU generator raster/upload", Work::Fallback);
            return Arc::new(self.raster_generator(g, width, height, clip_time, total));
        }
        // A still frame's file as it is now: replaced, rewritten, gone or
        // back, it is read again (#2068).
        let stamp = file_stamp(g);
        if let Some((data, w, h, was, px)) = self.still_generators.get(&clip) {
            if data == g && (*w, *h) == (width, height) && *was == stamp {
                return Arc::clone(px);
            }
        }
        residency::record(generator_feature(g), "Bounded still-generator cache miss", Work::Preparation);
        let px = Arc::new(self.raster_generator(g, width, height, clip_time, total));
        // A handful on screen at once; a long timeline's worth is not kept.
        if self.still_generators.len() >= 16 {
            self.still_generators.clear();
        }
        self.still_generators.insert(clip, (g.clone(), width, height, stamp, Arc::clone(&px)));
        px
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
        self.executor.clear_side_pictures();
        self.note_switched_off(c);
        let clip_time = eval_clip_time(c, time);
        let (mut fx, _) = clip_effects(c, time, fps, transforms.get(&c.id).copied(), self.pins.get(&c.id).copied());
        self.take_light(c.id, &mut fx);
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        let temporal = fx.iter().any(|e| matches!(e, BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. }));
        if has_active_effects(&fx) && temporal {
            return Err("adjustment layer with a CPU-only effect");
        }
        let mut slot_a = true;
        let mut effects_done = false;
        // An ungraded adjustment layer with effects takes the seeded branch:
        // its grade is the passthrough plan the effects are spliced into.
        if c.grade_is_identity() && !has_active_effects(&fx) {
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
            effects_done = self.splice_effects(c, &fx, time, fps, transforms, PlanPart::Whole, stats);
            self.executor.request_output_on_gpu();
            let pixels = self.grade(c, GradeInput::Gpu(seed), clip_time, fps, width, height, None);
            stats.grade_ms += ms_since(t);
            if effects_done && !self.finish_spliced() {
                return Err("an effect could not be recorded");
            }
            stats.effects_followed_grade += u32::from(effects_done);
            PLAN_EFFECT_CLIPS.fetch_add(u64::from(effects_done), std::sync::atomic::Ordering::Relaxed);
            if pixels.is_empty() {
                slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
            } else if let Some(g) = self.executor.gpu_pipeline_or_init() {
                g.upload_chain_input_linear(&pixels);
            }
        }
        if !effects_done && has_active_effects(&fx) {
            // Its effects could not be spliced into the plan: the frame
            // takes the CPU path, effects and all.
            return Err("an adjustment layer's effects could not run as a plan");
        }
        let t = Instant::now();
        if !self.executor.gpu_pipeline_or_init().is_some_and(|g| g.stack_mix_from_chain(slot_a, opacity)) {
            return Err("the adjustment mix failed");
        }
        stats.composite_ms += ms_since(t);
        Ok(())
    }

    /// The sequence's timeline grade, over its composite. Inside a scene
    /// too: a precomposition's scene, or a graded compound drawn as one,
    /// keeps its own grade, applied before the sequence that holds it
    /// grades the result (#370). Only the timeline LUT is left out there,
    /// being the output's, not the scene's.
    fn timeline_grade(&self, seq: &Sequence) -> Option<Clip> {
        Clip::timeline_grade_of(seq)
    }

    /// The stack cannot take this frame: `None`, and the reason in the
    /// log once, until it changes.
    /// Precomposition `id`'s `scene` at `at`, composed on the GPU stack as
    /// `render_scene` composes it on the CPU (its own scale, no shutter,
    /// LUT or inspection of the parent's, alpha kept), and kept for its
    /// layer to seed from (#2757). The parent's frame state is put back
    /// after; a scene the stack declines leaves its layer to the CPU.
    #[allow(clippy::too_many_arguments)]
    fn compose_precomposition(&mut self, id: ClipId, scene: &Sequence, at: Timecode, width: u32, height: u32, src: &mut dyn FrameSource, opts: RenderOptions) {
        if self.nesting >= klipp_timeline::flatten::MAX_COMPOUND_DEPTH {
            return;
        }
        self.nesting += 1;
        let shutter = std::mem::take(&mut self.shutter);
        let title_shutter = std::mem::take(&mut self.title_shutter);
        let lut = self.global_lut.take();
        let inspect = std::mem::take(&mut self.inspect);
        let (px_scale, hdr_peak, title_nits) = (self.px_scale, self.hdr_peak, self.title_nits);
        let (decline, declined_after_work, started) = (self.frame_decline, self.gpu_export_declined_after_work, self.stack_work_started);
        let composed = self.composite_on_gpu(scene, at, width, height, src, RenderOptions { keep_alpha: true, view_channel: klipp_core::ViewChannel::Picture, ..opts }).is_some();
        // The scenes inside this one were composed for it alone: done with
        // now, their buffers go to the next scene composed.
        for inner in scene.all_clips().filter(|c| c.precomp.is_some()) {
            if let Some(buffer) = self.precomp_scenes.remove(&inner.id) {
                self.precomp_ready.remove(&inner.id);
                if self.precomp_spare.len() < SPARE_SCENES {
                    self.precomp_spare.push(buffer);
                }
            }
        }
        (self.px_scale, self.hdr_peak, self.title_nits) = (px_scale, hdr_peak, title_nits);
        (self.frame_decline, self.gpu_export_declined_after_work, self.stack_work_started) = (decline, declined_after_work, started);
        self.inspect = inspect;
        self.global_lut = lut;
        self.shutter = shutter;
        self.title_shutter = title_shutter;
        self.nesting -= 1;
        if !composed {
            return;
        }
        let reuse = self.precomp_scenes.remove(&id).or_else(|| self.precomp_spare.pop());
        let kept = self.executor.gpu_pipeline_or_init().and_then(|g| g.stack_keep_chain(reuse));
        if let Some(kept) = kept {
            if self.precomp_scenes.len() >= 8 {
                self.precomp_scenes.retain(|k, _| self.precomp_ready.contains(k));
            }
            self.precomp_scenes.insert(id, kept);
            self.precomp_ready.insert(id);
        }
    }

    fn gpu_stack_declines<T>(&mut self, why: &'static str) -> Option<T> {
        self.gpu_export_declined_after_work = self.stack_work_started;
        self.frame_decline = Some(why);
        residency::record("composition.gpu_stack", why, Work::Fallback);
        GPU_STACK_DECLINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.last_gpu_stack_decline != Some(why) {
            tracing::debug!("GPU stack declines the frame: {why}");
            self.last_gpu_stack_decline = Some(why);
        }
        None
    }

    /// The primaries around the timeline LUT's lookup. It runs on the
    /// composite before the display transform, so in the working space:
    /// in an ACEScg project, AP1. An untagged `.cube` is Rec.709, as a
    /// Lut3D node's is, and was looked up on sRGB-encoded AP1 (#422).
    fn global_lut_bracket(&self, lut: &GlobalLut) -> klipp_effects::gpu::LutBracket {
        klipp_color::lut_bracket(self.working, lut.lut.input_space, lut.lut.output_space)
    }

    fn global_lut_on_chain(&mut self, width: u32, height: u32) -> bool {
        let Some(lut) = self.global_lut.take() else { return true };
        let bracket = self.global_lut_bracket(&lut);
        let ok = self.executor.apply_global_lut_to_output(
            width,
            height,
            &lut.lut.data,
            lut.lut.size as u32,
            lut.opacity,
            self.lut_tetrahedral,
            bracket,
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
        let mut stack = stack(seq, time);
        // One clip alone for a measurement of it (#719): the clip, out of
        // a transition when it is one side of it, and nothing else.
        if let Some(only) = self.inspect.only {
            stack = stack
                .into_iter()
                .filter_map(|(i, t, item)| match item {
                    TrackItem::Clip(c) if c.id == only => Some((i, t, TrackItem::Clip(c))),
                    TrackItem::Transition { a, .. } if a.id == only => Some((i, t, TrackItem::Clip(a))),
                    TrackItem::Transition { b, .. } if b.id == only => Some((i, t, TrackItem::Clip(b))),
                    _ => None,
                })
                .collect();
        }
        let only = self.inspect.only.is_some();

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
        self.resolve_pins(seq, stack.iter().flat_map(|(_, _, item)| item.clips()), time, fps, &transforms);
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
        // A transition out of or into a matted clip waits until every
        // matte source is rendered, so each side is matted before the
        // blend; it used to blend both sides unmatted (#718).
        let mut matted_transitions: Vec<usize> = Vec::new();
        // A clip that reads what is below it (#1746) is drawn in the
        // stacking loop, over the composite so far, as an adjustment layer
        // is; a transition into or out of one too. Not a matte's source,
        // which every layer may need first, nor a clip measured alone.
        let defers = |c: &Clip| !only && !matte_sources.contains(&c.id) && crate::resolve::reads_below(c);
        let mut deferred: HashSet<usize> = HashSet::new();
        for (i, (_, track, item)) in stack.iter().enumerate() {
            let layer = match item {
                TrackItem::Clip(c) if matches!(c.kind, ClipKind::AdjustmentLayer) => None,
                TrackItem::Clip(c) if defers(c) => {
                    deferred.insert(i);
                    None
                }
                TrackItem::Transition { a, b, .. } if (defers(a) || defers(b)) && a.matte_source.is_none() && b.matte_source.is_none() => {
                    deferred.insert(i);
                    None
                }
                // Measured alone, the clip is read at full strength: a fade
                // or a track's opacity is not the footage's colour.
                TrackItem::Clip(c) if only => self
                    .render_clip(c, time, fps, width, height, src, &transforms)
                    .map(|(pixels, _)| Layer { pixels, opacity: 1.0, blend: BlendMode::Normal, split: None }),
                TrackItem::Clip(c) => self
                    .render_clip(c, time, fps, width, height, src, &transforms)
                    .map(|(pixels, opacity)| Layer { pixels, opacity: opacity * track.opacity, blend: c.blend_mode, split: reflection_split(c, time, fps, height, self.px_scale) }),
                TrackItem::Transition { a, b, .. } if a.matte_source.is_some() || b.matte_source.is_some() => {
                    matted_transitions.push(i);
                    None
                }
                TrackItem::Transition { a, b, kind, progress, params, shutter } => {
                    self.render_transition(a, b, *kind, *progress, params, *shutter, time, fps, width, height, src, &transforms, None)
                        .map(|pixels| Layer { pixels, opacity: track.opacity, blend: BlendMode::Normal, split: None })
                }
            };
            if let (TrackItem::Clip(c), Some(l)) = (item, &layer) {
                if matte_sources.contains(&c.id) {
                    rendered.insert(c.id, l.pixels.clone());
                }
            }
            layers.push(layer);
        }
        for i in matted_transitions {
            if let (_, track, TrackItem::Transition { a, b, kind, progress, params, shutter }) = &stack[i] {
                layers[i] = self
                    .render_transition(a, b, *kind, *progress, params, *shutter, time, fps, width, height, src, &transforms, Some(&rendered))
                    .map(|pixels| Layer { pixels, opacity: track.opacity, blend: BlendMode::Normal, split: None });
            }
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
            // Drawn now, over what is below it (#1746).
            let layer = match deferred.contains(&i) {
                true => {
                    // The one before is put back after: a precomposition's
                    // scene drawn inside this clip has a loop of its own.
                    let outer = self.below.replace(Arc::new(acc.clone().unwrap_or_else(canvas)));
                    let layer = match item {
                        TrackItem::Clip(c) => self.render_clip(c, time, fps, width, height, src, &transforms).map(|(pixels, opacity)| Layer { pixels, opacity: opacity * track.opacity, blend: c.blend_mode, split: reflection_split(c, time, fps, height, self.px_scale) }),
                        TrackItem::Transition { a, b, kind, progress, params, shutter } => self
                            .render_transition(a, b, *kind, *progress, params, *shutter, time, fps, width, height, src, &transforms, None)
                            .map(|pixels| Layer { pixels, opacity: track.opacity, blend: BlendMode::Normal, split: None }),
                    };
                    self.below = outer;
                    layer
                }
                false => layer,
            };
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
            // Measured alone, a matted clip is read without its matte:
            // its source is not in the frame, and the footage is wanted.
            if let (TrackItem::Clip(c), false) = (item, only) {
                if let Some(binding) = self.matte_binding(c, time) {
                    if self.inspect.matte_of == Some(c.id) {
                        self.found.matte_mask = matte_mask(&rendered, &binding, width, height,
                            klipp_color::linear::luma_coefs(self.working));
                    }
                    apply_matte(&mut layer.pixels, &rendered, &binding, width, height,
                    klipp_color::linear::luma_coefs(self.working));
                }
            }
            // A title keeps its text readable over what is below (#2734).
            if let (TrackItem::Clip(c), Some(a)) = (item, acc.as_mut()) {
                klipp_effects::protect::protect_backdrop(a, &layer.pixels, width, height, layer.opacity, contrast_protect(c),
                    klipp_color::linear::luma_coefs(self.working));
                // A pinned layer taking its surface's shade and texture
                // from what is under it (#2751).
                if let Some(pin) = c.surface_pin.filter(|p| p.shade > 0.0 || p.texture > 0.0) {
                    klipp_effects::surface_light::take_surface(&mut layer.pixels, a, width, height, pin.shade, pin.texture, klipp_color::linear::luma_coefs(self.working));
                }
            }
            match &mut acc {
                None if layer.blend == BlendMode::Normal && layer.opacity >= 1.0 && layer.split.is_none() => acc = Some(layer.pixels),
                _ => {
                    let a = acc.get_or_insert_with(canvas);
                    match layer.split {
                        // A reflection's rows mix with the picture below by
                        // its own mode (#2707).
                        Some((row, mode)) => {
                            let at = (row.min(height) * width * 4) as usize;
                            let (above, below) = a.split_at_mut(at);
                            klipp_effects::composite::blend_layer_linear(above, &layer.pixels[..at], layer.opacity, layer.blend);
                            klipp_effects::composite::blend_layer_linear(below, &layer.pixels[at..], layer.opacity, mode);
                        }
                        None => klipp_effects::composite::blend_layer_linear(a, &layer.pixels, layer.opacity, layer.blend),
                    }
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
            let bracket = self.global_lut_bracket(&lut);
            let above_white = if self.hdr_peak.is_some() { split_above_white(&mut acc) } else { None };
            acc = match self.gpu() {
                Some(gpu) => gpu.apply_3d_lut_gpu(&acc, width, height, &lut.lut.data, lut.lut.size as u32, lut.opacity, tetrahedral, bracket),
                None => {
                    klipp_effects::apply_3d_lut_bracketed(&mut acc, &lut.lut.data, lut.lut.size, lut.opacity, bracket.0, bracket.1, tetrahedral);
                    acc
                }
            };
            if let Some(over) = above_white {
                use rayon::prelude::*;
                acc.par_iter_mut().zip(over.par_iter()).for_each(|(v, o)| *v += o);
            }
            self.global_lut = Some(lut);
            self.found.stats.lut_ms += ms_since(t);
        }

        if opts.view_channel != klipp_core::ViewChannel::Picture {
            let t = Instant::now();
            klipp_effects::composite::view_channel(&mut acc, channel_code(opts.view_channel), klipp_color::linear::luma_coefs(self.working));
            composite_ms += ms_since(t);
        } else if !opts.keep_alpha {
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
        let _clip_residency = residency::clip(clip.id.0);
        self.note_switched_off(clip);
        self.note_subject_mattes(clip, src);
        let clip_time = eval_clip_time(clip, time);
        // The rendered frame's presentation time, for its frame blend.
        let mut source_pts = None;
        self.source_decoded = None;
        let (input, hdr) = match &clip.kind {
            ClipKind::Media { .. } => {
                let at = source_secs(clip, time, fps);
                // Blended by its node at the head of the grade's run, from
                // the stream's next frame (M4 step 7).
                let blending = frame_blends(clip, time, src.media_fps(clip), fps);
                // A source reads the frame nearest the time asked; a blend
                // mixes from the frame at or before it toward the next, so
                // it asks half a media frame earlier. Otherwise a time
                // halfway between two frames could read the later one, and
                // the blend would have nothing after it to mix toward.
                let read_at = match src.media_fps(clip).filter(|r| *r > 0.0) {
                    Some(rate) if blending => at - 0.5 / rate,
                    _ => at,
                };
                let f = src.frame(clip, read_at, width, height)?;
                source_pts = f.pts;
                self.source_decoded = Some((f.width, f.height));
                // Temporal noise reduction happens here, on the source,
                // before the grade — so a clip using it gives up the
                // GPU-resident fast path below and comes down to the
                // CPU. That cost is the feature, not an oversight: the
                // neighbours it averages against are CPU-decoded source
                // frames.
                let tnr = temporal_denoise_of(clip, time, fps);
                match f {
                    // Left on the GPU for the grade to start from.
                    SourceFrame { pixels: SourcePixels::Gpu(g), width: w, height: h, .. }
                        if tnr.is_none() && !blending && (w, h) == (width, height) =>
                    {
                        (GradeInput::Gpu(g), None)
                    }
                    // Scene-referred codes at the render size go to the
                    // grade as they are.
                    SourceFrame { pixels: SourcePixels::SceneReferred { codes, transfer, source_to_working }, width: w, height: h, .. }
                        if tnr.is_none() && !blending && (w, h) == (width, height) && codes.len() == (w as usize) * (h as usize) =>
                    {
                        (GradeInput::Scene { codes, transfer, source_to_working }, Some(transfer))
                    }
                    f => {
                        let (pixels, hdr) = to_linear(f, width, height);
                        // Blended and denoised by its node at the head of
                        // the grade's run (`grade_at_source`).
                        (GradeInput::Cpu(pixels), hdr)
                    }
                }
            }
            ClipKind::Title(td) => (GradeInput::Cpu(Arc::new(self.title_linear_at(clip, td, drawn_time(clip, time), width, height))), None),
            ClipKind::Generator(g) => {
                residency::record(generator_feature(g), "CPU composition regenerates source pixels", Work::Fallback);
                let total = (clip.timeline_out - clip.timeline_in).to_secs_f64();
                let clip_time = drawn_time(clip, time);
                let generation_started = Instant::now();
                let packed = self.raster_generator(g, width, height, clip_time, total);
                self.found.stats.cpu_generator_ms += ms_since(generation_started);
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

        // Read here, where the frames come from, for whichever run draws
        // its effects; after a scene's clips, which set their own.
        self.read_media_frames(clip, clip_time, width, height, src);
        let _ = self.put_titles(clip, clip_time, width, height, false);
        if self.inspect.ungraded && self.inspect.clip == Some(clip.id) {
            let mut before = (*input.pixels()).clone();
            if let Some(t) = hdr {
                self.display_render(&mut before, t);
            }
            self.found.ungraded = Some(before);
        }
        let (mut fx, opacity) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied(), self.pins.get(&clip.id).copied());
        self.take_light(clip.id, &mut fx);
        let opacity = opacity * self.pin_presence.get(&clip.id).copied().unwrap_or(1.0);
        // Measured alone, the clip is its own picture (#2475): a fill in
        // its bars, a shadow or a glow drawn around it is not its footage,
        // and a fill hid the bars the measure leaves out. Each is an
        // identity in its place, so the chain keeps its order.
        if self.inspect.only == Some(clip.id) {
            for e in fx.iter_mut().filter(|e| klipp_effects::draws_outside_the_picture(e)) {
                *e = BuiltinEffect::Opacity { value: 1.0 };
            }
        }
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        // A crop to a shape, from the source's size as decoded (#2757).
        if let (ClipKind::Media { .. }, Some(size)) = (&clip.kind, self.source_decoded) {
            klipp_effects::resolve_cover_shapes(&mut fx, size, (width, height));
        }
        self.title_light(clip, &mut fx);
        if crate::resolve::reads_below(clip) {
            self.put_below(clip, &fx, width, height);
        }
        // The grade's result goes on into the effects on the GPU, with no
        // download and upload between them, when nothing in between needs
        // the picture on the CPU (#1089, M2 step 10).
        let on_gpu = self.effects_follow_grade_on_gpu(clip, &fx);
        // What feeds each effect dropped as an identity, for an inspection
        // of one, whichever route runs the effects (#1314).
        if self.inspect.clip == Some(clip.id) {
            let found = clip.picture_v2().and_then(|p| crate::resolve::resolve_effect_nodes_and_identities(clip, &p.picture, time, fps, transforms.get(&clip.id).copied(), self.pins.get(&clip.id).copied(), 1.0));
            match found {
                Ok((_, identities)) => self.identity_feeds.insert(clip.id, identities),
                Err(_) => self.identity_feeds.remove(&clip.id),
            };
        }
        // What runs on the CPU between the grade and the effects keeps them
        // out of the grade's run: a plug-in (step 11c-2), or a shutter
        // that places the picture at every sample and averages them
        // (11c-3). Such a clip runs its grade as a plan, that CPU step, and
        // its effects as plans of their own around it.
        let plugins = clip.has_enabled_plugins();
        let shutter = title_motion_blur(clip).is_some() || self.shutter.contains_key(&clip.id);
        // A picture-truth clip's effects may be masked or mixed, which only
        // a plan draws: those take the plan route, on the GPU or not
        // (step 9c-2).
        let truth = !on_gpu && clip.is_picture_truth() && has_active_effects(&fx) && !fx.iter().any(klipp_effects::registry::needs_source_neighbours);
        // A clip whose grade the tools read inside keeps its effects out of
        // the grade's run, so what they read is the grade's; the effects
        // still run on the GPU, as a plan of their own after it (#1274).
        let inspected = !on_gpu && self.inspects_the_grade_of(clip) && self.effects_can_run_on_gpu(&fx);
        let mut splice = !plugins && !shutter;
        let mut route = on_gpu;
        let mut blended = false;
        let mut apart = None;
        let mut apart_blends = Vec::new();
        if on_gpu || truth || inspected {
            let mut stats = std::mem::take(&mut self.found.stats);
            let planned = self.plan_effects(clip, time, fps, transforms, &fx, PlanPart::Whole, &mut stats);
            self.found.stats = stats;
            let blends = std::mem::take(&mut self.planned_blends);
            blended = blends.iter().any(Option::is_some);
            route = on_gpu || blended || inspected;
            // Masked or mixed effects still run in the grade's plan, its
            // nodes tapped as before (step 9c-2).
            splice &= !(inspected && !blended);
            if route && splice {
                self.run_effects = planned;
                self.run_blends = blends;
            } else if route {
                apart = Some(planned);
                apart_blends = blends;
            }
            // A scene-referred source's display step runs in the plan too,
            // after the grade, where `display_render` runs it below when
            // the plan doesn't (step 11c). Not for a grade the tools read
            // inside whose effects run apart from it (#1274): its taps,
            // thumbnails, solo and matte view show the grade's own output,
            // and `display_render` displays the picture it returns, as on
            // the CPU. Masked or mixed effects spliced into the run keep the
            // step there, ahead of them.
            if route && !(self.inspects_the_grade_of(clip) && !splice) {
                self.run_display = hdr.map(|t| self.scene_to_display(t));
            }
        }
        // Kept for a second grade if the effects can't finish in the plan.
        let retry = (route && splice).then(|| input.clone());
        // What the grade reads, kept for a masked effect apart from it to
        // make its matte from (9c-2 B2).
        self.matte_input = apart_blends.iter().flatten().any(|b| b.mask.is_some()).then(|| input.pixels());
        // The frame's declines before this clip's grade: a run graded again
        // gives back the first run's, and notes its own (step 10).
        let declines_before = self.found.declines.len();
        // The clip's analyses for its runs this frame, from its media.
        let analyses = self.analyses_of(clip, src);
        self.clip_analysis = Some((clip.id, analyses));
        let mut pixels = self.grade_at_source(clip, input, time, clip_time, fps, width, height, src, source_pts, hdr);
        // Whether the grade's run took the display step, so it isn't run
        // twice: not when the run failed and gave back its input.
        let mut displayed = false;
        if route {
            let ran = (std::mem::take(&mut self.run_effects), std::mem::take(&mut self.run_blends));
            displayed = self.run_display.take().is_some() && self.grade_ran;
            if splice {
                if self.grade_ran && self.executor.last_run_effects_complete() {
                    self.found.stats.effects_followed_grade += 1;
                    PLAN_EFFECT_CLIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Some((pixels, opacity));
                }
                self.found.declines.truncate(declines_before);
                if blended || klipp_effects::runs_only_as_a_node(&fx) {
                    // A masked or mixed clip's effects can't be drawn by the
                    // CPU kernels, nor can a frei0r plug-in, which runs only
                    // as its node: the same plan runs again on the CPU
                    // executor, counted (step 9c-2, M4 step 5).
                    (self.run_effects, self.run_blends) = ran;
                    self.run_display = hdr.map(|t| self.scene_to_display(t));
                    let input = retry.expect("kept on the plan route");
                    let pixels = self.on_cpu_executor(|r| r.grade_at_source(clip, input, time, clip_time, fps, width, height, src, source_pts, hdr));
                    self.cpu_kernel_run();
                    self.run_effects.clear();
                    self.run_blends.clear();
                    self.run_display = None;
                    self.cpu_fallback(clip);
                    return Some((pixels, opacity));
                }
                // An effect could not be applied (a lost device), or the run
                // failed before its effects: the frame is graded again and
                // its effects drawn by the CPU kernels below, never shown
                // without them.
                pixels = self.grade_at_source(clip, retry.expect("kept when on_gpu"), time, clip_time, fps, width, height, src, source_pts, hdr);
                displayed = false;
                self.cpu_fallback(clip);
            }
        }
        if let Some(t) = hdr.filter(|_| !displayed) {
            self.display_render(&mut pixels, t);
        }
        // Its OFX plug-ins, each a node of its own after the grade, a
        // temporal one reading the clip's source at its other frames.
        let pixels = self.ofx_nodes(clip, pixels, clip_time, fps, source_secs(clip, time, fps), width, height, Some(&mut *src));
        // The effects as plans of their own (`effects_by`) when the plan
        // route is on, from the picture; the list otherwise.
        let plan = apart.is_some();
        let fx = apart.unwrap_or(fx);
        let missed = self.plan_misses;
        // A title whose own drawing moves is drawn again at every shutter
        // sample, by `title_pixels_at`, which carries a sample through
        // what the frame above was just carried through.
        let redrawn = match &clip.kind {
            ClipKind::Title(td) => title_raster_moves(td),
            _ => None,
        };
        let pixels = match (title_motion_blur(clip), self.shutter.get(&clip.id).cloned()) {
            (Some(_), _) => match self.title_shutter.get(&clip.id).cloned() {
                Some(samples) => self.motion_blurred(clip, pixels, redrawn, &fx, &apart_blends, &samples, time, fps, width, height, plan),
                None => self.effects_by(plan, clip, &fx, &apart_blends, pixels, clip_time, fps, width, height),
            },
            (None, Some(samples)) => self.shuttered(clip, pixels, &fx, &apart_blends, &samples, clip_time, fps, width, height, plan),
            (None, None) => self.effects_by(plan, clip, &fx, &apart_blends, pixels, clip_time, fps, width, height),
        };
        self.matte_input = None;
        // Counted when every run of the clip's effects took the plan.
        if plan && self.plan_misses == missed {
            if shutter {
                self.found.stats.effects_shuttered += 1;
            } else if plugins {
                self.found.stats.effects_after_plugins += 1;
            } else {
                self.found.stats.effects_inspected_apart += 1;
            }
            PLAN_EFFECT_CLIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some((pixels, opacity))
    }

    /// Run `fx` on `pixels` as a plan of their own (step 11c): the effect
    /// nodes between a source and an output, for effects that follow
    /// something the GPU can't run (a plug-in). The picture back, or, when
    /// an effect could not be applied or the run failed, `pixels`
    /// untouched for the CPU kernels.
    #[allow(clippy::too_many_arguments)]
    fn run_effects_plan(&mut self, clip: &Clip, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>], pixels: Vec<f32>, clip_time: f64, fps: f64, width: u32, height: u32) -> Result<Vec<f32>, Vec<f32>> {
        if fx.is_empty() {
            return Ok(pixels);
        }
        self.splice_effects_only(clip, fx, blends);
        let t = Instant::now();
        let frame = grade_frame(clip_time, fps);
        // The pixels go straight into the chain, not copied on the CPU for
        // the Source to upload: a third of a run's time at 720p. A graph
        // that still wants them on the CPU says so, and runs again.
        // The picture's already drawn: a plug-in's instance is lent to the
        // run, and no node of it reads the source.
        let source_at = clip.source_secs_at(clip_time, fps);
        let run = self.in_context(clip, clip_time, fps, source_at, None, &mut klipp_nodes::frame_context::NoSources, |r, ctx| {
            let spliced = std::mem::take(&mut r.spliced);
            let run = match r.executor.prime_chain_from_linear(&pixels, width, height) {
                true => match r.executor.execute_linear_with_context(&[], &spliced, width, height, clip_time, frame, None, None, None, ctx) {
                    Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {
                        r.executor.execute_linear_with_context(&pixels, &spliced, width, height, clip_time, frame, None, None, None, ctx)
                    }
                    run => run,
                },
                false => r.executor.execute_linear_with_context(&pixels, &spliced, width, height, clip_time, frame, None, None, None, ctx),
            };
            r.spliced = spliced;
            run.map(|(out, tap, _)| (out, tap))
        });
        self.found.stats.effects_ms += ms_since(t);
        match run {
            Ok((out, _)) if self.executor.last_run_effects_complete() && out.len() == pixels.len() => {
                // Only a run that is kept: one the CPU takes again notes
                // the retry's.
                self.note_declines(clip.id);
                Ok(out)
            }
            Ok(_) => Err(pixels),
            Err(e) => {
                tracing::warn!("clip {}: its effects after the plug-ins: {e:?}; the CPU kernels take them", clip.id.0);
                self.note_domain_refusal(clip, &e);
                Err(pixels)
            }
        }
    }

    /// A placed layer's effects as a plan of their own over its slot on the
    /// GPU stack (step 11d): the slot seeds the chain, the effects run, and
    /// the result goes back into the slot, with none of the copies a pass
    /// through the CPU would make.
    /// The effects come from the clip's picture, after `lead` (a title's
    /// entrance and focus blur, which are its own). Whether they ran; a
    /// layer they couldn't run on declines the frame.
    #[allow(clippy::too_many_arguments)]
    fn effects_on_layer(
        &mut self,
        clip: &Clip,
        fx: &[BuiltinEffect],
        lead: &[BuiltinEffect],
        slot: usize,
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        transforms: &HashMap<ClipId, ResolvedTransform>,
        at_move: PlanPart,
        stats: &mut RenderStats,
    ) -> bool {
        if !self.effects_follow_grade_on_gpu(clip, fx) {
            return false;
        }
        let mut planned = self.plan_effects(clip, time, fps, transforms, &fx[lead.len().min(fx.len())..], at_move, stats);
        planned.splice(0..0, lead.iter().cloned());
        let mut blends = std::mem::take(&mut self.planned_blends);
        blends.splice(0..0, lead.iter().map(|_| None));
        // A matte this run can't give (the grade's, or a node beside the
        // chain reading the grade's picture): the layer declines to the CPU
        // path, which runs its grade with its effects (step 9c-2).
        use crate::resolve::{MaskSource, SideInput};
        let needs_the_grade = |b: &crate::resolve::Blend| matches!(&b.mask, Some(MaskSource::Grade { .. } | MaskSource::Unavailable(_)) | Some(MaskSource::Side { input: SideInput::Graded, .. }));
        if blends.iter().flatten().any(needs_the_grade) {
            return false;
        }
        let Some(buffer) = self.executor.gpu_pipeline_or_init().and_then(|g| g.stack_layer_buffer(slot)) else { return false };
        if !self.executor.prime_chain_from_gpu_linear_buffer(&buffer, width, height) {
            return false;
        }
        self.splice_effects_only(clip, &planned, &blends);
        self.executor.request_output_on_gpu();
        let clip_time = eval_clip_time(clip, time);
        let run = self.executor.execute_linear(&[], &self.spliced, width, height, clip_time, grade_frame(clip_time, fps), None, None);
        let done = match run {
            Ok((out, _)) if self.executor.last_run_effects_complete() => {
                self.note_declines(clip.id);
                // Left on the GPU, as asked; downloaded only if the run
                // couldn't keep it there.
                let slot_a = self.executor.last_chain_final_slot().unwrap_or(true);
                self.executor.gpu_pipeline_or_init().is_some_and(|g| if out.is_empty() { g.stack_layer_from_chain(slot, slot_a) } else { g.stack_layer_from_linear(slot, &out) })
            }
            Err(e) => {
                self.note_domain_refusal(clip, &e);
                false
            }
            _ => false,
        };
        if done {
            stats.effects_on_layers += 1;
            PLAN_EFFECT_CLIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        done
    }

    /// `self.spliced` as the source straight to the output with `fx`
    /// between them: a plan of the effects alone, ids apart from the
    /// grade run's so each keeps its cached plan.
    ///
    /// The run has no grade: a mask from the grade can't be given here
    /// (bypassed, said); one from beside the chain reads this run's source.
    fn splice_effects_only(&mut self, clip: &Clip, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>]) {
        self.executor.set_working_colorspace(self.working);
        self.executor.set_render_scale(self.px_scale);
        // A tracked window drawn in the plan follows the clip's points.
        self.executor.set_current_clip_track_points(&clip.track_points);
        self.spliced.clone_from(&self.passthrough);
        let source = self.spliced.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).map(|n| n.id);
        let base = clip.id.0.as_u128() ^ (0x00ef_fec8_u128 << 64);
        let unavailable = splice_blended(&mut self.spliced, fx, blends, |i| klipp_nodes::NodeId::from_u128(base ^ i as u128), source, None);
        self.masks_unavailable(clip, unavailable);
    }

    /// `fx` on `pixels`: as a plan of their own when `plan` (step 11c),
    /// falling back to the CPU kernels for this run when they can't be
    /// applied there (counted in `plan_misses` and `effects_on_cpu`), or
    /// on the CPU kernels.
    ///
    /// Effects with `blends` (masked or mixed, a picture-truth clip's) run
    /// as a plan even with no GPU, and fall back to the same plan on the
    /// CPU executor: the CPU kernels can't mask or mix (step 9c-2).
    #[allow(clippy::too_many_arguments)]
    fn effects_by(&mut self, plan: bool, clip: &Clip, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>], pixels: Vec<f32>, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        // A window, or a key of the picture the grade reads, is drawn in
        // the effects' own plan, beside the chain, and the effect blended
        // there (#1265); the key reads that picture handed to the run.
        let moved = self.matte_input.is_some().then(|| mattes_in_the_plan(clip, blends)).flatten();
        let blends = match &moved {
            Some((moved, n)) => {
                self.found.stats.effects_blended_apart += *n;
                EFFECTS_BLENDED_APART.fetch_add(u64::from(*n), std::sync::atomic::Ordering::Relaxed);
                let keyed = moved.iter().flatten().any(|b| matches!(&b.mask, Some(crate::resolve::MaskSource::Side { input: crate::resolve::SideInput::GradeInput, .. })));
                if let (true, Some(input)) = (keyed, self.matte_input.clone()) {
                    self.executor.put_side_picture(klipp_nodes::side_picture::SideKey::GradeInput, Ok(input));
                }
                moved.as_slice()
            }
            None => blends,
        };
        self.effects_by_matte_apart(plan, clip, fx, blends, pixels, clip_time, fps, width, height)
    }

    /// [`Self::effects_by`] once its windows and keys are in the plan: a masked
    /// effect in a run apart from its grade whose matte reads a picture
    /// (9c-2 B2) has the effects before it as usual, it alone and
    /// unmasked, its matte made in a side run of its producer, the two
    /// blended on the CPU; then the rest.
    #[allow(clippy::too_many_arguments)]
    fn effects_by_matte_apart(&mut self, plan: bool, clip: &Clip, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>], pixels: Vec<f32>, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        let blend = |i: usize| blends.get(i).and_then(Option::as_ref);
        // A window or a grade-input key beside the chain is the plan's own
        // (`mattes_in_the_plan`).
        let in_the_plan = |m: &crate::resolve::MaskSource| {
            matches!(m, crate::resolve::MaskSource::Side { node, .. } if node.kind == klipp_nodes::NodeKind::Mask)
                || matches!(m, crate::resolve::MaskSource::Side { input: crate::resolve::SideInput::GradeInput, .. })
        };
        let masked = (0..fx.len()).find(|&i| blend(i).and_then(|b| b.mask.as_ref()).is_some_and(|m| !in_the_plan(m)));
        let (Some(i), true) = (masked, self.matte_input.is_some()) else {
            return self.effects_by_plain(plan, clip, fx, blends, pixels, clip_time, fps, width, height);
        };
        let b = blend(i).cloned().expect("found above");
        let head = self.effects_by_plain(plan, clip, &fx[..i], &blends[..i.min(blends.len())], pixels, clip_time, fps, width, height);
        let drawn = self.effects_by_plain(plan, clip, &fx[i..=i], &[None], head.clone(), clip_time, fps, width, height);
        let out = match self.side_matte(clip, b.mask.as_ref().expect("masked"), clip_time, fps, width, height) {
            Some(matte) if matte.len() == head.len() => {
                let luma = klipp_color::linear::luma_coefs(self.working);
                match klipp_effects::mask_mix::MaskMix::new(b.blend.mix, b.blend.channel, luma) {
                    Ok(mix) => {
                        self.found.stats.effects_blended_on_cpu += 1;
                        residency::record("effect.mask_blend", "Mask and graded effect blended on CPU", Work::Fallback);
                        EFFECTS_BLENDED_ON_CPU.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        head.chunks_exact(4).zip(drawn.chunks_exact(4)).zip(matte.chunks_exact(4)).flat_map(|((p, e), m)| {
                            mix.pixel([p[0], p[1], p[2], p[3]], [e[0], e[1], e[2], e[3]], Some([m[0], m[1], m[2], m[3]]))
                        }).collect()
                    }
                    // A mix that can't be blended (not finite, from a
                    // file): bypassed as any unavailable mask, and said,
                    // never counted as blended (#1268's review).
                    Err(e) => {
                        self.masks_unavailable(clip, vec![format!("its {} can't be blended: {e:?}", effect_tag(&fx[i]))]);
                        head
                    }
                }
            }
            _ => {
                self.masks_unavailable(clip, vec![format!("the matte of its {} couldn't be made apart from its grade", effect_tag(&fx[i]))]);
                head
            }
        };
        self.effects_by_matte_apart(plan, clip, &fx[i + 1..], blends.get(i + 1..).unwrap_or(&[]), out, clip_time, fps, width, height)
    }

    /// The matte `mask` gives, made in a run of its own on the CPU from
    /// the picture the clip's grade reads (`matte_input`), at `clip_time`:
    /// the clip's grade with its output taken from the publishing node,
    /// or the node beside the chain on the clip's source or on the grade's
    /// picture. `None` when it can't be made.
    #[allow(clippy::too_many_arguments)]
    fn side_matte(&mut self, clip: &Clip, mask: &crate::resolve::MaskSource, clip_time: f64, fps: f64, width: u32, height: u32) -> Option<Vec<f32>> {
        use crate::resolve::{MaskSource, SideInput};
        let input = self.matte_input.clone()?;
        let (mut g, from, port) = match mask {
            MaskSource::Grade { node, port } => {
                let g = clip.grade_to_run().into_owned();
                g.nodes.iter().any(|n| n.id == *node).then_some(())?;
                (g, *node, *port)
            }
            MaskSource::Side { node, lead, port, input: reads } => {
                let mut g = if *reads == SideInput::Graded { clip.grade_to_run().into_owned() } else { self.passthrough.clone() };
                let feed = match reads {
                    SideInput::Graded => g.input_of(g.output_node_id, 0)?,
                    // This run's Source is the picture the grade reads.
                    SideInput::Source | SideInput::GradeInput => g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source)?.id,
                };
                crate::resolve::wire_side(&mut g, feed, lead, node);
                (g, node.id, *port)
            }
            MaskSource::Unavailable(_) => return None,
        };
        let out = g.output_node_id;
        g.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
        g.edges.push(klipp_nodes::Edge { from_node: from, from_port: port, to_node: out, to_port: 0 });
        let (encoding, working, scale, tetrahedral) = (self.grade_encoding, self.working, self.px_scale, self.lut_tetrahedral);
        let subject_dirs = self.clip_subject_dirs.get(&clip.id).cloned().unwrap_or_default();
        let media_last = self.clip_media_last.get(&clip.id).copied();
        self.on_cpu_executor(|r| {
            r.executor.prepare_clip(clip.grade());
            r.executor.set_current_clip_subject_mattes(subject_dirs);
            r.executor.set_current_clip_media_last(media_last);
            r.executor.set_grade_encoding(encoding);
            r.executor.set_working_colorspace(working);
            r.executor.set_render_scale(scale);
            r.executor.set_lut_tetrahedral(tetrahedral);
            r.executor.set_current_clip_track_points(&clip.track_points);
            let out = r.executor.execute_linear(&input, &g, width, height, clip_time, grade_frame(clip_time, fps), None, None).ok().map(|(o, _)| o);
            if out.is_some() {
                r.note_declines(clip.id);
            }
            out
        })
    }

    /// [`Self::effects_by`] for a run whose masks, if any, are wired in it.
    #[allow(clippy::too_many_arguments)]
    fn effects_by_plain(&mut self, plan: bool, clip: &Clip, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>], pixels: Vec<f32>, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        let blended = blends.iter().any(Option::is_some);
        // A frei0r plug-in runs as a node, with its instances, on every
        // route: never by the kernels, which would make it one frame's.
        // A media source too (#1746): its frame is the render's to read.
        let frei0r = klipp_effects::runs_only_as_a_node(fx);
        if (!plan && !blended && !frei0r) || fx.is_empty() {
            return self.effects_analysed(clip, fx, pixels, clip_time, fps, width, height);
        }
        if !plan && frei0r {
            self.cpu_kernel_run();
            return self.on_cpu_executor(|r| r.run_effects_plan(clip, fx, blends, pixels, clip_time, fps, width, height)).unwrap_or_else(|back| back);
        }
        match self.run_effects_plan(clip, fx, blends, pixels, clip_time, fps, width, height) {
            Ok(out) => out,
            Err(back) => {
                self.plan_misses += 1;
                self.cpu_fallback(clip);
                if !blended && !frei0r {
                    return self.effects_analysed(clip, fx, back, clip_time, fps, width, height);
                }
                self.cpu_kernel_run();
                self.on_cpu_executor(|r| r.run_effects_plan(clip, fx, blends, back, clip_time, fps, width, height)).unwrap_or_else(|back| back)
            }
        }
    }

    /// Run `f` with the CPU executor in the GPU executor's place: a masked
    /// or mixed clip's plan again where its GPU run couldn't apply an
    /// effect (step 9c-2).
    fn on_cpu_executor<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let mut cpu = self.cpu_executor.take().unwrap_or_else(klipp_nodes::GraphExecutor::new_cpu_only);
        cpu.set_side_pictures(self.executor.side_pictures());
        std::mem::swap(&mut self.executor, &mut cpu);
        let out = f(self);
        std::mem::swap(&mut self.executor, &mut cpu);
        self.cpu_executor = Some(cpu);
        out
    }

    /// The frames `clip`'s media sources draw at `clip_time` (#1746), read
    /// from `src` at the render size: the first of the executor's side
    /// pictures for the clip, the map cleared of the clip before's. An HDR
    /// or log item's frame is given its display step there, from its own
    /// transfer under this render's delivery, as a clip's own source is
    /// ([`Self::display_render`]): display-referred, as every media
    /// source's picture is (#1857).
    fn read_media_frames(&mut self, clip: &Clip, clip_time: f64, width: u32, height: u32, src: &mut dyn FrameSource) {
        self.executor.clear_side_pictures();
        for (media, offset) in crate::resolve::media_sources(clip) {
            let transfer = src.media_transfer(&media);
            let got = src.media_frame(&media, clip_time + f64::from(offset), self.working, width, height).map(|mut frame| {
                if let Some(t) = transfer {
                    self.display_render(&mut frame, t);
                }
                Arc::new(frame)
            });
            self.executor.put_side_picture(klipp_nodes::side_picture::SideKey::Media { item: media, offset_bits: offset.to_bits() }, got);
        }
        self.put_clean_frames(clip, width, height, src);
    }

    /// The frames `clip`'s difference keys pick from its own media as
    /// their clean plates (#2758), for its run: each checked against the
    /// media before anything is decoded, read once at the render size by
    /// the media reader (as a media source's frame is, not the clip's own
    /// decoder) and kept. One refused gives the key no plate, said why.
    fn put_clean_frames(&mut self, clip: &Clip, width: u32, height: u32, src: &mut dyn FrameSource) {
        use klipp_nodes::side_picture::{SideKey, SideMissing};
        let picked = crate::resolve::plate_frames(clip);
        if picked.is_empty() {
            return;
        }
        let media = clip.media_id().map(|m| m.0.to_string());
        let range = src.media_range(clip);
        for secs in picked {
            let key = SideKey::Plate { media: media.clone().unwrap_or_default(), secs_bits: secs.to_bits() };
            let got = match (&media, crate::resolve::clean_frame_refused(secs, range)) {
                (None, _) => Err(SideMissing::Unsupported("the clip has no media to pick a clean frame from".into())),
                (_, Some(why)) => Err(SideMissing::Unsupported(why)),
                (Some(media), None) => match self.clean_frames.get(&(key.clone(), width, height)) {
                    Some(frame) => Ok(Arc::clone(frame)),
                    None => {
                        let transfer = src.media_transfer(media);
                        let read = src.media_frame(media, secs, self.working, width, height).map(|mut frame| {
                            if let Some(t) = transfer {
                                self.display_render(&mut frame, t);
                            }
                            Arc::new(frame)
                        });
                        if let Ok(frame) = &read {
                            if self.clean_frames.len() >= 4 {
                                self.clean_frames.clear();
                            }
                            self.clean_frames.insert((key.clone(), width, height), Arc::clone(frame));
                        }
                        read
                    }
                },
            };
            self.executor.put_side_picture(key, got);
        }
    }

    /// The pictures `clip`'s title sources draw at `clip_time` (#2758), for
    /// its plan: each title drawn as a title clip's would be, timed from
    /// the clip's start. On the GPU stack (`on_gpu`) in the layer kept for
    /// it, handed to the run as it lies there; on the CPU, rastered there.
    /// A title that moves by effects of its own is the CPU route's for now,
    /// said; one its file doesn't read as is missing, said.
    fn put_titles(&mut self, clip: &Clip, clip_time: f64, width: u32, height: u32, on_gpu: bool) -> Result<(), &'static str> {
        use klipp_nodes::side_picture::{SideKey, SideMissing};
        let total = (clip.timeline_out - clip.timeline_in).to_secs_f64() as f32;
        for (k, title) in crate::resolve::title_sources(clip).into_iter().enumerate() {
            let key = SideKey::Title { title: title.clone() };
            let Ok(td) = serde_json::from_str::<klipp_timeline::TitleData>(&title) else {
                self.executor.put_side_picture(key, Err(SideMissing::Unsupported("its title doesn't read".into())));
                continue;
            };
            if !on_gpu {
                // As a title clip of the clip's own span would be drawn.
                let stand_in = Clip { kind: ClipKind::Title(td.clone()), ..Clip::gap(clip.timeline_in, clip.timeline_out) };
                let frame = self.title_linear_at(&stand_in, &td, clip_time, width, height);
                self.executor.put_side_picture(key, Ok(Arc::new(frame)));
                continue;
            }
            let slot = self.side_title_slots.get(&clip.id).copied().ok_or("a title source's layer")? + k;
            match self.place_title_source(&td, slot, width, height, clip_time, total) {
                Some((_, fx)) if fx.is_empty() => {}
                Some(_) => return Err("a title source with effects of its own"),
                None => return Err("a title source the stack couldn't draw"),
            }
            let picture = self.executor.gpu_pipeline_or_init().and_then(|g| g.stack_layer_as_picture(slot)).ok_or("a title source's picture couldn't be kept")?;
            self.executor.put_side_picture(key.clone(), Err(SideMissing::Unavailable));
            self.executor.put_side_picture_on_gpu(key, picture);
        }
        Ok(())
    }

    /// What is below `clip` (#1746), for its Clip Below: the stacking
    /// loop's composite so far ([`Self::below`]), seen through the clip's
    /// placement in `fx` so it lines up with what is under the clip on
    /// screen. A clip drawn outside that loop has none; one placed with a
    /// pin or a 3D tilt can't see it, said.
    fn put_below(&mut self, clip: &Clip, fx: &[BuiltinEffect], width: u32, height: u32) {
        use klipp_nodes::side_picture::SideMissing;
        let still = BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let place = fx.iter().find(|e| matches!(e, BuiltinEffect::Transform { .. })).unwrap_or(&still);
        let got = match &self.below {
            None => Err(SideMissing::Unavailable),
            Some(_) if self.pins.contains_key(&clip.id) || fx.iter().any(|e| matches!(e, BuiltinEffect::CornerPin { .. })) => Err(SideMissing::Unsupported("the clip is pinned to a surface".into())),
            Some(below) => match klipp_effects::corner_pin::seen_through(place, width, height) {
                Some(pin) => Ok(Arc::new(klipp_effects::apply_effects(&[pin], (**below).clone(), width, height))),
                None => Err(SideMissing::Unsupported("the clip is placed with a 3D tilt".into())),
            },
        };
        self.executor.put_side_picture(klipp_nodes::side_picture::SideKey::Below, got);
    }

    /// What is below `c` on the GPU stack (#2758), for its Clip Below: the
    /// composite so far ([`Self::stack_below`]) handed to its run as it is,
    /// for a clip placed where it stands (`place`, as the stack places it).
    /// As on the CPU (`put_below`), a clip pinned to a surface has none.
    /// One moved or tilted, whose below is seen through its placement, is
    /// the CPU stacking loop's for now.
    fn put_below_on_gpu(&mut self, c: &Clip, fx: &[BuiltinEffect], place: Option<BuiltinEffect>) -> Result<(), &'static str> {
        use klipp_nodes::side_picture::{SideKey, SideMissing};
        // The effects its picture runs too: a move there is its placement
        // on the CPU route, what is below seen through it.
        let picture: Vec<BuiltinEffect> = c.picture_effects().map(|es| es.into_iter().filter(|e| e.enabled).map(|e| e.effect).collect()).unwrap_or_default();
        let fx: Vec<&BuiltinEffect> = fx.iter().chain(&picture).collect();
        if self.pins.contains_key(&c.id) || fx.iter().any(|e| matches!(e, BuiltinEffect::CornerPin { .. })) {
            self.executor.put_side_picture(SideKey::Below, Err(SideMissing::Unsupported("the clip is pinned to a surface".into())));
            return Ok(());
        }
        let placed_still = |e: &BuiltinEffect| match *e {
            BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, rotation_x, rotation_y, perspective, .. } => {
                position_x == 0.0 && position_y == 0.0 && scale_x == 1.0 && scale_y == 1.0 && rotation == 0.0 && rotation_x == 0.0 && rotation_y == 0.0 && perspective == 0.0
            }
            _ => true,
        };
        if !fx.iter().copied().chain(place.as_ref()).all(placed_still) {
            return Err("clip below, moved or tilted");
        }
        let Some(picture) = self.stack_below.take() else { return Err("what is below couldn't be kept") };
        self.executor.put_side_picture(SideKey::Below, Err(SideMissing::Unavailable));
        self.executor.put_side_picture_on_gpu(SideKey::Below, picture);
        Ok(())
    }

    /// Splice `fx` into `clip`'s next grade run as plan nodes (step 10b)
    /// when they can follow its grade on the GPU; whether they will.
    /// [`Self::finish_spliced`] ends it after the grade.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn splice_effects(&mut self, clip: &Clip, fx: &[BuiltinEffect], time: Timecode, fps: f64, transforms: &HashMap<ClipId, ResolvedTransform>, at_move: PlanPart, stats: &mut RenderStats) -> bool {
        let on = self.effects_follow_grade_on_gpu(clip, fx);
        if on {
            self.run_effects = self.plan_effects(clip, time, fps, transforms, fx, at_move, stats);
            self.run_blends = std::mem::take(&mut self.planned_blends);
        }
        on
    }

    /// The effects a plan route splices in for `clip` at `time` (step
    /// 10c): read from its lifted picture (`resolve_effects`), in the
    /// working space and at the render scale. `clip_effects`' list,
    /// `fallback`, is what the picture gives (10c-1's parity); it stands
    /// in when the picture can't be read, which is said.
    ///
    /// `at_move`: for a clip smeared across the shutter on the stack
    /// (#1923), the part of its plan before where its picture is placed
    /// (its placement's own node, found by its id, the stack placing it at
    /// each sample), or the part after it, less any transform, which runs
    /// once on the average (#2090). With no placement in the list, the
    /// whole list comes before the move and nothing after it.
    #[allow(clippy::too_many_arguments)]
    fn plan_effects(&mut self, clip: &Clip, time: Timecode, fps: f64, transforms: &HashMap<ClipId, ResolvedTransform>, fallback: &[BuiltinEffect], at_move: PlanPart, stats: &mut RenderStats) -> Vec<BuiltinEffect> {
        self.planned_blends.clear();
        self.planned_nodes.remove(&clip.id);
        self.identity_feeds.remove(&clip.id);
        let resolved = clip.picture_v2().and_then(|p| {
            crate::resolve::resolve_effect_nodes_and_identities(clip, &p.picture, time, fps, transforms.get(&clip.id).copied(), self.pins.get(&clip.id).copied(), 1.0)
        });
        match resolved {
            Ok((triples, identities)) => {
                self.identity_feeds.insert(clip.id, identities);
                let (mut fx, mut blends, mut nodes) = (Vec::new(), Vec::new(), Vec::new());
                for (e, b, n) in triples {
                    fx.push(e);
                    blends.push(b);
                    nodes.push(n);
                }
                self.authored_colours(&mut fx);
                self.at_render_scale(&mut fx);
                // A crop to a shape, from the source's size as read (#2757),
                // the lists beside the effects kept in step.
                if let (ClipKind::Media { .. }, Some(&(size, frame))) = (&clip.kind, self.source_sizes.get(&clip.id)) {
                    for at in klipp_effects::resolve_cover_shapes(&mut fx, size, frame) {
                        blends.insert(at, None);
                        nodes.insert(at, None);
                    }
                }
                // An effect beside the chain (a generator a merge lays
                // over, #1746) is authored as the chain's are.
                for b in blends.iter_mut().flatten() {
                    for side in [&mut b.mask, &mut b.fg].into_iter().flatten() {
                        if let crate::resolve::MaskSource::Side { node, lead, .. } = side {
                            // Its branch's too (#2758).
                            for n in lead.iter_mut().chain(std::iter::once(&mut **node)) {
                                self.authored_colours(&mut n.params.effects);
                                self.at_render_scale(&mut n.params.effects);
                            }
                        }
                    }
                }
                // A title's HDR brightness is the render's, not a node of
                // its picture: put first, as on the list route.
                if let Some(gain) = self.title_gain_effect(clip) {
                    fx.insert(0, gain);
                    blends.insert(0, None);
                    nodes.insert(0, None);
                }
                // Its surface's light, first too (#2751), as on the list
                // route.
                if let Some(gain) = self.pin_light.get(&clip.id) {
                    fx.insert(0, eval::light_gain_effect(*gain));
                    blends.insert(0, None);
                    nodes.insert(0, None);
                }
                if at_move != PlanPart::Whole {
                    let placement = clip.picture_v2().ok().and_then(|p| crate::resolve::placement_node_of(&p.picture));
                    let cut = placement.and_then(|id| nodes.iter().position(|n| *n == Some(id)));
                    match (at_move, cut) {
                        (PlanPart::BeforeMove, Some(cut)) => {
                            fx.truncate(cut);
                            blends.truncate(cut);
                            nodes.truncate(cut);
                        }
                        (PlanPart::AfterMove, Some(cut)) => {
                            let keep: Vec<usize> = (cut + 1..fx.len()).filter(|&i| !matches!(fx[i], BuiltinEffect::Transform { .. })).collect();
                            fx = keep.iter().map(|&i| fx[i].clone()).collect();
                            blends = keep.iter().map(|&i| blends[i].clone()).collect();
                            nodes = keep.iter().map(|&i| nodes[i]).collect();
                        }
                        (PlanPart::AfterMove, None) => (fx, blends, nodes) = (Vec::new(), Vec::new(), Vec::new()),
                        _ => {}
                    }
                }
                self.planned_nodes.insert(clip.id, nodes);
                self.planned_blends = blends;
                fx
            }
            Err(e) => {
                tracing::warn!("clip {}: effects from its picture: {e}; the list stands in", clip.id.0);
                stats.effects_fallback_lists += 1;
                PLAN_EFFECT_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                fallback.to_vec()
            }
        }
    }

    /// End a [`Self::splice_effects`]: whether every effect was applied.
    fn finish_spliced(&mut self) -> bool {
        self.run_effects.clear();
        self.run_blends.clear();
        self.grade_ran && self.executor.last_run_effects_complete()
    }

    /// Drop what a layer queued for its grade's run — its spliced effects,
    /// their mixes and masks, a display step — when the stack refuses it
    /// part-way, so none of it runs in the next grade (#1276).
    fn clear_queued_run(&mut self) {
        self.run_effects.clear();
        self.run_blends.clear();
        self.run_display = None;
    }

    /// Whether `clip`'s effects can take the grade's result where it sits
    /// on the GPU: the grade runs (no shortcut, nothing inspected), and
    /// every effect runs on the GPU. An HDR source's display step runs in
    /// the plan between them (step 11c-1). A clip with a plug-in (11c-2)
    /// or a shutter (11c-3) between them runs its effects as plans of
    /// their own around that CPU step, and the GPU stack, which has
    /// neither, declines it. Otherwise the CPU kernels draw them.
    fn effects_follow_grade_on_gpu(&self, clip: &Clip, fx: &[BuiltinEffect]) -> bool {
        // An effect that needs the source's neighbouring frames can't be a
        // plan node: temporal noise reduction and frame blending run at the
        // head before the grade and are gone from `fx` (`clip_effects`), so
        // this declines only a new such type nobody has moved there. The
        // analysis-driven ones (stabilisation, rolling shutter, deflicker)
        // take the plan route since step 12: their per-frame data reaches
        // the executor as each node's analysis (M4 step 9). Read from the
        // registry, so a new such type is covered.
        self.effects_can_run_on_gpu(fx) && !self.inspects_the_grade_of(clip)
    }

    /// Whether `fx` can run on the GPU as plan nodes at all: something to
    /// run, a GPU, a GPU kernel for each, and none that needs the source's
    /// neighbouring frames.
    fn effects_can_run_on_gpu(&self, fx: &[BuiltinEffect]) -> bool {
        let needs_neighbours = fx.iter().any(klipp_effects::registry::needs_source_neighbours);
        !needs_neighbours && self.allow_gpu && has_active_effects(fx)
    }

    /// Whether the grade tools read inside `clip`'s grade this render: its
    /// node thumbnails, the scope's tap, a matte view or a soloed node.
    /// The viewer names its primary clip in every render (#1274), so being
    /// the inspected clip alone says nothing about what is read.
    fn inspects_the_grade_of(&self, clip: &Clip) -> bool {
        self.inspect.clip == Some(clip.id) && self.inspect.reads_the_grade()
    }


    /// A title as working-space linear pixels at one instant: rasterised
    /// at that time, decoded, its deferred blur entrance applied, and
    /// brought into the working space. The frame's Title branch in
    /// `render_clip` and each shutter sample in `title_pixels_at` both
    /// start here, so the title-specific steps exist once.
    fn title_linear_at(&mut self, clip: &Clip, td: &klipp_timeline::TitleData, t: f64, width: u32, height: u32) -> Vec<f32> {
        residency::record("title.cpu_composition", "Title raster/placement and linear conversion on CPU", Work::Fallback);
        let total = (clip.timeline_out - clip.timeline_in).to_secs_f64() as f32;
        // Unrounded (#190): the title's layers as stacked and posed, in
        // byte units, decoded once here.
        let stack = {
            let mut adapter = self.gpu.as_mut().map(crate::synth::GpuAlphaMaskAdapter);
            let gpu = adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
            self.titles.render_with_window_stack(td, width, height, gpu, t as f32, Some(total))
        };
        if tracing::enabled!(tracing::Level::DEBUG) {
            let opaque = stack.chunks_exact(4).step_by(64).filter(|p| p[3] >= 0.5).count() * 64;
            tracing::debug!(
                "TITLE_LAYER clip={} t={t:.2}/{total:.2} {width}x{height} objects={} text={:?} font={} size={} opaque_px~{opaque}",
                clip.id, td.objects.len(), td.text(), td.font_name(), td.font_size(),
            );
        }
        let mut linear = srgb_stack_to_linear(&stack);
        let blur = self.titles.last_blur();
        if blur > 0.5 {
            // The blur entrance, as the GPU stack blurs the placed sprite.
            let luma = klipp_color::linear::luma_coefs(self.working);
            linear = klipp_effects::apply_effects_with_luma(&[BuiltinEffect::GaussianBlur { radius: blur }], linear, width, height, luma);
        }
        if let Some((radius, axis, band)) = self.titles.last_focus_blur() {
            // A roll's focus band going soft, as the GPU stack softens the
            // placed sprite (#592).
            let luma = klipp_color::linear::luma_coefs(self.working);
            let fx = [BuiltinEffect::FocusBlur { radius, vertical: axis == klipp_core::title_motion::RampAxis::Vertical, band }];
            linear = klipp_effects::apply_effects_with_luma(&fx, linear, width, height, luma);
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
        // Its grade alone, which reads no side picture: the clip's effects,
        // a media source or what is below among them (#1746), run once on
        // the averaged samples at the frame's time, as every effect's keys
        // are read there.
        let graded = self.grade(clip, GradeInput::Cpu(Arc::new(linear)), t, fps, width, height, None);
        self.ofx_nodes(clip, graded, t, fps, t, width, height, None)
    }

    /// A precomposition's scene at `at`, on a transparent surface: its
    /// tracks, camera, masks and shutter resolve here, in the working
    /// space, with nothing of the outer render's applied to them. The
    /// per-render state the outer stack has in hand — its shutter maps,
    /// its global LUT, what it is inspecting — is set aside for the
    /// scene's render and restored after, so a scene neither inherits
    /// nor disturbs it. `None` past the nesting bound.
    fn render_scene(&mut self, scene: &Sequence, at: Timecode, width: u32, height: u32, src: &mut dyn FrameSource) -> Option<Vec<f32>> {
        if self.nesting >= klipp_timeline::flatten::MAX_COMPOUND_DEPTH {
            return None;
        }
        self.nesting += 1;
        let shutter = std::mem::take(&mut self.shutter);
        let title_shutter = std::mem::take(&mut self.title_shutter);
        let lut = self.global_lut.take();
        let inspect = std::mem::take(&mut self.inspect);
        // The scene sets its own render scale, from its own resolution;
        // the parent's layers after it must go on with the parent's. A
        // scene stored at another size than its parent otherwise left
        // its scale behind, and every blur and matte feather above it
        // was sized for the wrong sequence.
        let px_scale = self.px_scale;
        let surface = self.render_stack(scene, at, width, height, src, RenderOptions { keep_alpha: true, hdr_peak_nits: self.hdr_peak, ..Default::default() });
        self.px_scale = px_scale;
        self.inspect = inspect;
        self.global_lut = lut;
        self.shutter = shutter;
        self.title_shutter = title_shutter;
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

    /// A title's brightness in an HDR delivery (#1723): its light scaled
    /// from SDR white (1.0, 203 nits) to the sequence's `title_nits`, held
    /// under the delivery's peak, as a gain at the head of its effects so
    /// the CPU and the GPU stack apply it at the same point. An SDR render,
    /// or a title at 203 nits, has none.
    fn title_gain_effect(&self, clip: &Clip) -> Option<BuiltinEffect> {
        let (ClipKind::Title(_), Some(peak)) = (&clip.kind, self.hdr_peak) else { return None };
        let k = title_gain(self.title_nits, peak);
        ((k - 1.0).abs() > 1e-6).then_some(BuiltinEffect::ChannelMixer {
            red_red: k,
            red_green: 0.0,
            red_blue: 0.0,
            green_red: 0.0,
            green_green: k,
            green_blue: 0.0,
            blue_red: 0.0,
            blue_green: 0.0,
            blue_blue: k,
        })
    }

    /// [`Self::title_gain_effect`] at the head of an effect list. A plan
    /// made from the picture puts it there too (`plan_effects`).
    fn title_light(&self, clip: &Clip, fx: &mut Vec<BuiltinEffect>) {
        if let Some(gain) = self.title_gain_effect(clip) {
            fx.insert(0, gain);
        }
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
    /// A transition's colour (0 to 1, sRGB-encoded, as a dip to a colour
    /// takes it) in the working space, still sRGB-encoded for the shader
    /// to decode: picked in BT.709 like a title's, so in an ACEScg or a
    /// BT.2020 project it means the colour picked (#1653).
    /// The posed-pieces plan of a piece transition at `time`, with the
    /// subject's matte on it when it cuts round one (#985 phase 3): the
    /// matte of the subject side (`a` leaving, `b` assembling) at that
    /// clip's source time, at the frame's size. A matte not made, or a
    /// frame of it missing, is a decline: the viewer says so and an
    /// export refuses; the nearest frame stands in on screen meanwhile,
    /// and with none the pieces are plain. The matte reaches the frame as
    /// the clip's picture does: fitted as its source is, then placed by
    /// its placement effects (Transform, crop, pin, mirror). One matte
    /// serves every shutter sample: a subject moves little in a frame.
    #[allow(clippy::too_many_arguments)]
    fn piece_plan(
        &mut self,
        a: &Clip,
        b: &Clip,
        kind: klipp_core::TransitionKind,
        eased: f32,
        params: &klipp_core::TransitionUserParams,
        shutter: (f32, u32),
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        transforms: &HashMap<ClipId, ResolvedTransform>,
    ) -> klipp_effects::pieces::PiecePlan {
        let kind_id = klipp_core::spec_for_kind(kind).kind_id as u32;
        let p = [params.param0, params.param1, params.param2, params.param3];
        let subject = subject_code(kind, params);
        let mut plan = klipp_effects::pieces::plan_with(kind_id, eased, width, height, p, subject, shutter.0, shutter.1);
        let Some((model, incoming)) = crate::subject_matte::transition_subject_of(kind, params) else { return plan };
        let side = if incoming { b } else { a };
        let decline = |r: &mut Self, reason: &str, why: String| {
            r.found.declines.push(klipp_nodes::Decline {
                clip: Some(side.id),
                node: NodeId::nil(),
                kind: klipp_nodes::NodeKind::ExternalMatte,
                outcome: klipp_nodes::DeclineOutcome::FileMissing,
                name: "Transition subject".into(),
                reason: reason.into(),
                why,
            });
        };
        let Some(dir) = src.subject_matte_dir(side, model) else {
            decline(self, "no subject matte for this media; track its subject", "no matte folder for the clip's media".into());
            return plan;
        };
        // Before the media's start the decoder shows its first frame, past
        // its end its last; so does the matte (and the export's check
        // counts them so).
        let mut secs = source_secs(side, time, fps).max(0.0);
        if let Some((_, end)) = src.media_range(side) {
            let rate = src.media_fps(side).filter(|r| *r > 0.0).unwrap_or(fps);
            secs = secs.min(crate::subject_matte::last_frame_secs(end, rate));
        }
        use klipp_nodes::matte_sequence as seq;
        let fresh = |dir: &std::path::Path| seq::read_manifest(dir).map(|m| (m, seq::frames(dir)));
        let mut known = match self.subject_mattes.get(&dir) {
            Some(k) => Some(k.clone()),
            None => fresh(&dir),
        };
        let mut at = known.as_ref().and_then(|(m, keys)| seq::matte_at(&dir, m, keys, secs));
        // A time with no frame of its own looks again: the pass may have
        // made it since.
        if at.as_ref().is_none_or(|a| !a.exact) {
            known = fresh(&dir);
            at = known.as_ref().and_then(|(m, keys)| seq::matte_at(&dir, m, keys, secs));
        }
        if let Some(k) = known {
            self.subject_mattes.insert(dir.clone(), k);
        }
        let Some(at) = at else {
            decline(self, "no subject matte for this media yet; track its subject", format!("{}: no matte frames", dir.display()));
            return plan;
        };
        if !at.exact {
            decline(
                self,
                &format!("the frame at {secs:.3} s of its media has no subject matte yet"),
                format!("{}: no matte at {secs:.6} s; the one at {:.6} s stands in", dir.display(), at.read as f64 / 1_000_000.0),
            );
        }
        plan.matte = self.placed_matte(side, &at.values, at.width, at.height, time, fps, width, height, transforms);
        plan
    }

    /// A source-space matte `mw × mh` where `clip`'s picture lands in the
    /// frame (#985 phase 3): fitted as the source is (one scale, centred,
    /// bars empty), then through the clip's placement effects, as one
    /// channel. `None` for a malformed matte.
    #[allow(clippy::too_many_arguments)]
    fn placed_matte(&self, clip: &Clip, values: &[f32], mw: u32, mh: u32, time: Timecode, fps: f64, width: u32, height: u32, transforms: &HashMap<ClipId, ResolvedTransform>) -> Option<Vec<f32>> {
        let rgba: Vec<f32> = values.iter().flat_map(|m| [*m, *m, *m, 1.0]).collect();
        let fitted = fit_linear(rgba, mw, mh, width, height)?;
        let (mut fx, _) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied(), self.pins.get(&clip.id).copied());
        self.take_light(clip.id, &mut fx);
        let mut place: Vec<BuiltinEffect> = fx
            .into_iter()
            .filter(|e| matches!(e, BuiltinEffect::Transform { .. } | BuiltinEffect::PictureTransform { .. } | BuiltinEffect::Crop { .. } | BuiltinEffect::CornerPin { .. } | BuiltinEffect::Mirror { .. }))
            .filter(|e| !klipp_effects::is_identity_effect(e))
            .collect();
        self.at_render_scale(&mut place);
        let placed = if place.is_empty() { fitted } else { klipp_effects::apply_effects(&place, fitted, width, height) };
        // Where the placed picture is partly there (its edge, a feather),
        // the matte is as much there.
        Some(placed.chunks_exact(4).map(|p| (p[0] * p[3].clamp(0.0, 1.0)).clamp(0.0, 1.0)).collect())
    }

    /// Where `clip`'s grade finds its tracked subjects' mattes (#1881):
    /// asked of `src` by the clip's media as linked now, for each model
    /// its External Mattes are bound to; nothing asked when none are.
    fn note_subject_mattes(&mut self, clip: &Clip, src: &mut dyn FrameSource) {
        let mut dirs: [Option<std::path::PathBuf>; 2] = [None, None];
        // Its grade's and those beside its chain (#2758).
        let mattes = crate::subject_matte::external_mattes(clip);
        for model in mattes.iter().filter_map(|p| p.matte_subject) {
            if dirs[model.index()].is_none() {
                dirs[model.index()] = src.subject_matte_dir(clip, model.index() as u32);
            }
        }
        if dirs.iter().any(Option::is_some) {
            self.clip_subject_dirs.insert(clip.id, dirs);
        } else {
            self.clip_subject_dirs.remove(&clip.id);
        }
        // A matte sequence, bound either way, is read at the media's last
        // frame past its end, as the picture is.
        let reads_sequence = mattes.iter().any(|p| p.matte_subject.is_some() || p.matte_image_path.as_deref().is_some_and(klipp_nodes::matte_sequence::is_sequence));
        let last = reads_sequence.then(|| src.media_range(clip)).flatten().map(|(_, end)| {
            let rate = src.media_fps(clip).filter(|r| *r > 0.0).unwrap_or(24.0);
            crate::subject_matte::last_frame_secs(end, rate)
        });
        match last {
            Some(last) => self.clip_media_last.insert(clip.id, last),
            None => self.clip_media_last.remove(&clip.id),
        };
    }

    fn authored_colour(&self, c: [f32; 4]) -> [f32; 4] {
        let Some(m) = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, self.working) else {
            return c;
        };
        use klipp_color::transfer::{linear_to_srgb, srgb_to_linear};
        let w = klipp_color::primaries::matrix3_mul_vec(m, [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2])]);
        [linear_to_srgb(w[0]), linear_to_srgb(w[1]), linear_to_srgb(w[2]), c[3]]
    }

    fn authored_colours(&self, fx: &mut [BuiltinEffect]) {
        // A LUT's table and a colour-space transform's, for this working
        // space, on every route the effects take.
        if fx.iter().any(|e| matches!(e, BuiltinEffect::Lut3D { .. } | BuiltinEffect::ColourSpaceTransform { .. })) {
            self.colour_effects.lock().unwrap_or_else(std::sync::PoisonError::into_inner).prepare(fx, self.working);
        }
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
                BuiltinEffect::Generator { color_a, color_b, .. } => {
                    convert(color_a);
                    convert(color_b);
                }
                _ => {}
            }
        }
    }

    /// A scene-referred clip's last step of its own: the SDR display tone
    /// map, or for an HDR delivery its display light.
    fn display_render(&self, pixels: &mut [f32], t: klipp_color::hdr::HdrTransfer) {
        self.scene_to_display(t).apply(pixels);
    }

    /// The display step for a source of transfer `t` under this render's
    /// delivery or monitor: what `display_render` runs on the CPU and the
    /// plan's `SceneToDisplay` node runs in a clip's plan (step 11c).
    fn scene_to_display(&self, t: klipp_color::hdr::HdrTransfer) -> klipp_color::hdr::SceneToDisplay {
        klipp_color::hdr::SceneToDisplay { transfer: t, peak_nits: self.hdr_peak, luma: klipp_color::primaries::luminance_weights(self.working) }
    }

    /// A layer's grade on the GPU stack in the clip's frame context, with
    /// its analyses from its media, as on the CPU: an analysis effect
    /// (a Deflicker) reads its asset here too, or declines, rather than
    /// running at no strength. The stack takes no temporal effect: its
    /// source is lent for its range alone.
    #[allow(clippy::too_many_arguments)]
    /// `blend`: the clip frame-blends here (#2854), with the presentation
    /// time of the frame read: its blend runs at the head of the run, on
    /// the chain, toward the media's next frame.
    #[allow(clippy::too_many_arguments)]
    fn grade_on_stack(&mut self, clip: &Clip, input: GradeInput, time: Timecode, clip_time: f64, fps: f64, width: u32, height: u32, src: &mut dyn FrameSource, blend: Option<Option<f64>>) -> Vec<f32> {
        let t = Instant::now();
        let analyses = self.analyses_of(clip, src);
        self.grade_split.analyses_ms += ms_since(t);
        self.clip_analysis = Some((clip.id, analyses));
        let at = source_secs(clip, time, fps);
        if blend.is_some() {
            self.run_source_nodes = source_nodes(clip, true, None, None);
        }
        // The clip's source, for its range: a node with no analysis for a
        // time inside the media declines as missing, past it as a fallback.
        let mut sources = ClipSources::new(src, clip, self.working);
        if blend.is_some() {
            sources = sources.with_current(at, None, width, height);
        }
        let mut bounded = klipp_nodes::frame_context::BoundedSources::new(&mut sources, width, height);
        let pixels = self.in_context(clip, clip_time, fps, at, blend.flatten(), &mut bounded, |r, ctx| r.grade(clip, input, clip_time, fps, width, height, Some(ctx)));
        self.run_source_nodes.clear();
        self.clip_analysis = None;
        pixels
    }

    /// [`Self::grade`] in the clip's frame context (M4), with its
    /// source-boundary node (step 6): a media clip with temporal noise
    /// reduction runs it straight after
    /// the Source, from the clip's source frames either side (`src`'s
    /// linear reader, bounded to the media and the render size), before
    /// the grade. Its rules are the effect's: the first enabled one, and
    /// none at a strength of 0 or less (`temporal_denoise_of`).
    #[allow(clippy::too_many_arguments)]
    fn grade_at_source(&mut self, clip: &Clip, input: GradeInput, time: Timecode, clip_time: f64, fps: f64, width: u32, height: u32, src: &mut dyn FrameSource, source_pts: Option<f64>, hdr: Option<klipp_color::hdr::HdrTransfer>) -> Vec<f32> {
        use klipp_nodes::frame_context::BoundedSources;
        let denoise = matches!(clip.kind, ClipKind::Media { .. }).then(|| temporal_denoise_of(clip, time, fps)).flatten();
        let blend = matches!(clip.kind, ClipKind::Media { .. }) && frame_blends(clip, time, src.media_fps(clip), fps);
        // A clip the grade tools read inside runs each as the picture's
        // node, so a thumbnail, tap or solo on one reads it (#1305).
        let inspected = self.inspects_the_grade_of(clip).then(|| picture_source_nodes(clip));
        self.run_source_nodes = source_nodes(clip, blend, denoise, inspected);
        // Every clip's run has its context: a plug-in node reads its
        // instance there whether or not anything reads the source, whose
        // readers open only when asked.
        let at = source_secs(clip, time, fps);
        // A parked viewer's frame: its heavy nodes' outputs held for the
        // next run of this clip's grade, and only this run (#1863).
        let held = self.held_source(clip, clip_time, at, source_pts, src);
        self.executor.hold_outputs_for(held);
        let mut sources = ClipSources::new(src, clip, self.working).with_current(at, hdr, width, height);
        let mut bounded = BoundedSources::new(&mut sources, width, height);
        self.grade_source_scene = source_domain(hdr) == klipp_nodes::domains::PictureDomain::Scene;
        let pixels = self.in_context(clip, clip_time, fps, at, source_pts, &mut bounded, |r, ctx| r.grade(clip, input, clip_time, fps, width, height, Some(ctx)));
        self.grade_source_scene = false;
        self.executor.hold_outputs_for(None);
        self.run_source_nodes.clear();
        pixels
    }

    /// What `clip`'s frame at `at` (source seconds; clip time `clip_time`)
    /// is the picture of, when `src` is a parked viewer's: its file, the
    /// decoded frame's own pts and size, and the clip's retime there.
    /// `None` without any of them, and then nothing is held.
    fn held_source(&mut self, clip: &Clip, clip_time: f64, at: f64, source_pts: Option<f64>, src: &mut dyn FrameSource) -> Option<klipp_nodes::held::HeldSource> {
        let (pts, decoded) = (source_pts?, self.source_decoded?);
        let file = src.held_file(clip)?;
        Some(held_source_of(clip, clip_time, at, pts, decoded, file, src.media_fps(clip)))
    }

    /// `clip`'s enabled OFX plug-ins on `pixels`, each a node of its own
    /// (M4 step 3), in the clip's frame context: its instance its own, kept
    /// across frames in its slot, and a temporal one handed the frames it
    /// names from `src` (none for a title's sample or an adjustment layer,
    /// which have no source). A clip with none on keeps its frame at full
    /// precision (#995); a run that fails leaves the frame as it was.
    #[allow(clippy::too_many_arguments)]
    fn ofx_nodes(&mut self, clip: &Clip, pixels: Vec<f32>, clip_time: f64, fps: f64, source_secs: f64, width: u32, height: u32, src: Option<&mut dyn FrameSource>) -> Vec<f32> {
        let Some(graph) = ofx_graph(clip) else { return pixels };
        let t = Instant::now();
        let frame = grade_frame(clip_time, fps);
        let run = match src {
            Some(src) => {
                let mut sources = ClipSources::new(src, clip, self.working);
                let mut bounded = klipp_nodes::frame_context::BoundedSources::new(&mut sources, width, height);
                self.in_context(clip, clip_time, fps, source_secs, None, &mut bounded, |r, ctx| r.executor.execute_linear_with_context(&pixels, &graph, width, height, clip_time, frame, None, None, None, ctx))
            }
            None => self.in_context(clip, clip_time, fps, source_secs, None, &mut klipp_nodes::frame_context::NoSources, |r, ctx| {
                r.executor.execute_linear_with_context(&pixels, &graph, width, height, clip_time, frame, None, None, None, ctx)
            }),
        };
        self.found.stats.ofx_ms += ms_since(t);
        match run {
            Ok((out, _, _)) if out.len() == pixels.len() => {
                // Its own run's declines: the grade's were noted with it.
                self.note_declines(clip.id);
                out
            }
            other => {
                // The frame goes on as it was, and each plug-in says it
                // wasn't drawn: an export refuses it rather than write the
                // frame without them.
                let why = match &other {
                    Ok((out, _, _)) => format!("the run gave {} values for {}", out.len(), pixels.len()),
                    Err(e) => format!("{e:?}"),
                };
                tracing::warn!("clip {}: its OFX plug-ins didn't run ({why}); the frame as it was", clip.id.0);
                for n in graph.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Ofx) {
                    let name = n.params.ofx.as_ref().map_or_else(String::new, |o| o.plugin.clone());
                    self.found.declines.push(klipp_nodes::Decline { clip: Some(clip.id), node: n.id, kind: klipp_nodes::NodeKind::Ofx, outcome: klipp_nodes::DeclineOutcome::RenderFailed, name, reason: "its plug-ins' run failed".into(), why: why.clone() });
                }
                pixels
            }
        }
    }

    /// `f` run with a frame context for `clip` at clip time `clip_time`
    /// (M4): this renderer's plug-in instances, lent for the run, and
    /// `sources` for the clip's source frames at other times.
    #[allow(clippy::too_many_arguments)]
    fn in_context<R>(&mut self, clip: &Clip, clip_time: f64, fps: f64, source_secs: f64, source_pts: Option<f64>, sources: &mut dyn klipp_nodes::frame_context::SourceProvider, f: impl FnOnce(&mut Self, &mut klipp_nodes::frame_context::FrameContext<'_>) -> R) -> R {
        let media = match clip.kind {
            ClipKind::Media { media_id } => media_id,
            _ => klipp_core::MediaId::nil(),
        };
        let mut instances = std::mem::take(&mut self.instances);
        // The OFX host, lent to the run's OFX nodes: a clone of the shared
        // host, so lending it borrows nothing of the renderer (M4 step 3).
        let ofx = crate::ofx_node::OfxNodes(std::sync::Arc::clone(&self.ofx));
        // The clip's analyses, lent for the run.
        let analysis = self.clip_analysis.take();
        let none = ClipAnalysis::default();
        let inputs = analysis.as_ref().filter(|(id, _)| *id == clip.id).map_or(&none, |(_, a)| a);
        let mut ctx = klipp_nodes::frame_context::FrameContext {
            clip: clip.id,
            frame: ((clip.timeline_in.to_secs_f64() + clip_time) * fps).round() as i64,
            source: (media, clip.source_in.to_secs_f64()),
            clip_time_secs: clip_time,
            frame_number: grade_frame(clip_time, fps),
            fps,
            source_secs,
            source_pts,
            sources,
            analysis: inputs,
            instances: &mut instances,
            ofx: Some(&ofx),
            source_at: &|t| clip.source_secs_at(t, fps),
        };
        let out = f(self, &mut ctx);
        self.instances = instances;
        self.clip_analysis = analysis;
        out
    }

    /// Keep analysis assets in `store` rather than the analysis cache.
    pub fn set_asset_store(&mut self, store: Box<dyn klipp_analysis::assets::AssetStore + Send>) {
        self.assets = store;
        self.derived.clear();
    }

    /// `clip`'s analyses for this frame (M4 step 9): for each analysis
    /// effect it runs, its asset found under the key `src` gives it,
    /// derived with the effect's settings (kept while they hold), or stale.
    fn analyses_of(&mut self, clip: &Clip, src: &mut dyn FrameSource) -> ClipAnalysis {
        use klipp_analysis::assets::{lookup, Found};
        let mut out = ClipAnalysis::default();
        // The analysis effects as the clip's picture holds them (M4 step
        // 12a), not its v1 list.
        let effects = match clip.picture_effects() {
            Ok(effects) => effects,
            Err(e) => {
                tracing::warn!("clip {}: its analyses aren't read: {e}", clip.id.0);
                return out;
            }
        };
        for effect in effects.iter().filter(|e| e.enabled).map(|e| &e.effect) {
            let (Some(kind), Some(params)) = (asset_kind(effect), klipp_effects::node_analysis::derivation_params(effect)) else { continue };
            if out.found.iter().any(|(k, _)| *k == (kind, params)) {
                continue;
            }
            let Some(key) = src.analysis_key(clip, kind) else { continue };
            // What's kept, by its stamp: a frame costs a look at the file,
            // never a read. The asset is read and parsed, and derived with
            // the effect's settings, only when that stamp or the settings
            // change; a stamp seen again is the same asset (#1321 review).
            // None at the current algorithm keeps what was found under a
            // stamp no asset has, until an analysis keeps one.
            let stamp = self.assets.stamp(&key).unwrap_or(u64::MAX);
            let kept = match self.derived.get(&(key.clone(), params)) {
                Some((seen, kept)) if *seen == stamp => kept.clone(),
                _ => {
                    let kept = match lookup(self.assets.as_ref(), &key) {
                        Found::Absent => Kept::Absent,
                        Found::Stale { .. } => Kept::Stale,
                        // One that won't derive (its shape changed without
                        // its algorithm) reads as stale: owed again, never
                        // drawn from.
                        Found::Ready(raw) => klipp_effects::node_analysis::derive(effect, &raw).map_or(Kept::Stale, |d| Kept::Ready(Arc::new(d))),
                    };
                    if self.derived.len() > 64 {
                        self.derived.clear();
                    }
                    self.derived.insert((key.clone(), params), (stamp, kept.clone()));
                    kept
                }
            };
            let blob = match kept {
                Kept::Absent => continue,
                Kept::Stale => None,
                Kept::Ready(d) => Some(klipp_nodes::frame_context::AnalysisBlob { schema: klipp_nodes::frame_context::NODE_ANALYSIS_SCHEMA, data: d }),
            };
            out.found.push(((kind, params), blob));
        }
        out
    }

    /// The clip's node graph, at clip-relative time. The inspected clip
    /// also runs the grade tools' captures, its solo and its matte view.
    /// Pixels still on the GPU seed the grade there; read back only when
    /// there is no grade to run, or a node needs them on the CPU.
    fn grade(&mut self, clip: &Clip, input: GradeInput, clip_time: f64, fps: f64, width: u32, height: u32, mut ctx: Option<&mut klipp_nodes::frame_context::FrameContext<'_>>) -> Vec<f32> {
        self.grade_ran = false;
        // Only a grade the tools read inside runs with its taps (#1274).
        let inspecting = self.inspects_the_grade_of(clip);
        let matte_view = if inspecting { self.resolved.matte_view } else { None };
        // An identity grade with effects queued still runs: the effects are
        // the plan's nodes (step 11b). So does one the tools read inside: its
        // run is what takes their thumbnails and taps, and says which of
        // their requests it doesn't hold (M3 step 16a).
        if clip.grade_is_identity() && !inspecting && self.run_effects.is_empty() && self.run_display.is_none() && self.run_source_nodes.is_empty() {
            // Take the buffer rather than clone the handle: a layer this
            // renderer just made is held once, and moves out for free.
            return match input {
                GradeInput::Cpu(p) => unshare(p),
                other => unshare(other.pixels()),
            };
        }
        let t = Instant::now();
        let t_prepare = Instant::now();
        self.executor.prepare_clip(clip.grade());
        self.executor.set_current_clip_subject_mattes(self.clip_subject_dirs.get(&clip.id).cloned().unwrap_or_default());
        self.executor.set_current_clip_media_last(self.clip_media_last.get(&clip.id).copied());
        self.executor.set_grade_encoding(self.grade_encoding);
        self.executor.set_working_colorspace(self.working);
        self.executor.set_render_scale(self.px_scale);
        self.executor.set_lut_tetrahedral(self.lut_tetrahedral);
        self.executor.set_current_clip_track_points(&clip.track_points);
        self.executor.set_solo_node(if inspecting { self.resolved.solo } else { None });
        // A scene-referred clip's grade reads scene light; any other
        // clip's picture is display-referred, an SDR one included (#1867).
        let scene = matches!(input, GradeInput::Scene { .. }) || self.run_display.is_some() || self.grade_source_scene;
        self.executor.set_source_domain(if scene { klipp_nodes::domains::PictureDomain::Scene } else { klipp_nodes::domains::PictureDomain::Display });
        let frame = grade_frame(clip_time, fps);
        self.grade_split.prepare_ms += ms_since(t_prepare);
        let t_seed = Instant::now();
        let seeded = matches!(&input, GradeInput::Gpu(g) if g.seed(&mut self.executor, width, height));
        self.grade_split.seed_ms += ms_since(t_seed);
        let t_graph = Instant::now();
        let out = match &input {
            GradeInput::Cpu(p) => self.run_graph(clip, p, width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut()),
            GradeInput::Gpu(g) if seeded => {
                match self.run_graph(clip, &[], width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut()) {
                    Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {
                        self.run_graph(clip, &g.read(), width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut())
                    }
                    r => r,
                }
            }
            GradeInput::Gpu(g) => self.run_graph(clip, &g.read(), width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut()),
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
                match self.run_graph(clip, &[], width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut()) {
                    Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {
                        self.run_graph(clip, &input.pixels(), width, height, clip_time, frame, inspecting, matte_view, ctx.as_deref_mut())
                    }
                    r => r,
                }
            }
            GradeInput::Scene { .. } => {
                self.run_graph(clip, &input.pixels(), width, height, clip_time, frame, inspecting, matte_view, ctx)
            }
        };
        self.grade_split.graph_ms += ms_since(t_graph);
        self.executor.set_source_domain(klipp_nodes::domains::PictureDomain::Display);
        self.found.stats.grade_ms += ms_since(t);
        // A test's fault (`fault`, #1276): the run, made as any run is,
        // fails as a graph that fails does.
        let out = if crate::fault::grade_fails(clip.id) { Err(klipp_nodes::executor::ExecutorError::MissingNodeOutput) } else { out };
        let out = if crate::fault::grade_gives_nothing(clip.id) { out.map(|_| Vec::new()) } else { out };
        // A CPU run gives the whole picture or fails: a picture of another
        // size is a failed run (#1913), drawn without the grade and said,
        // never handed on to crash whatever shows it.
        let whole = width as usize * height as usize * 4;
        let out = match out {
            Ok(out) if !self.allow_gpu && out.len() != whole => {
                let why = format!("the grade gave {} values for a {width}x{height} picture, which takes {whole}", out.len());
                tracing::error!("clip {}: {why}; drawn without its grade", clip.id.0);
                self.found.declines.push(klipp_nodes::Decline {
                    clip: Some(clip.id),
                    node: clip.grade().output_node_id,
                    kind: klipp_nodes::NodeKind::Output,
                    outcome: klipp_nodes::DeclineOutcome::RenderFailed,
                    name: "grade".into(),
                    reason: "its run gave a picture of the wrong size".into(),
                    why,
                });
                return unshare(input.pixels());
            }
            other => other,
        };
        match out {
            Ok(out) => {
                self.grade_ran = true;
                out
            }
            Err(e) => {
                tracing::error!("node graph failed for clip {:?}: {e:?}", clip.id);
                self.note_domain_refusal(clip, &e);
                // Drawn without its grade from here, nothing to retry it:
                // a matte that isn't there fails the frame, said, rather
                // than show the clip whole (#898).
                self.found.declines.extend(missing_matte_decline(clip, &e));
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
        ctx: Option<&mut klipp_nodes::frame_context::FrameContext<'_>>,
    ) -> Result<Vec<f32>, klipp_nodes::executor::ExecutorError> {
        // The grade's pixel-sized settings are authored at the
        // sequence's size, as the effects' are (#1012).
        self.executor.set_render_scale(self.px_scale);
        // The grade with its corrections' mixes (M3 step 17a).
        let to_run = clip.grade_to_run();
        if !inspecting {
            let graph = if self.run_effects.is_empty() && self.run_display.is_none() && self.run_source_nodes.is_empty() {
                &*to_run
            } else {
                // An identity grade is the source straight to the output:
                // the effects alone (step 11b).
                self.splice_run(clip);
                &self.spliced
            };
            let run = match ctx {
                Some(ctx) => self.executor.execute_linear_with_context(source, graph, width, height, clip_time, frame, None, None, None, ctx),
                None => self.executor.execute_linear_with_thumbs(source, graph, width, height, clip_time, frame, None, None, None),
            };
            if run.is_ok() {
                self.note_declines(clip.id);
            }
            return run.map(|(out, _, _)| out);
        }
        // An inspected clip runs its grade alone, unless its effects are
        // masked or mixed (a picture-truth clip's): those run in the plan
        // with it, the grade's nodes still tapped (step 9c-2). A
        // scene-referred clip's display step runs in it either way.
        let (graph, graded) = if self.run_effects.is_empty() && self.run_display.is_none() && self.run_source_nodes.is_empty() {
            (&*to_run, to_run.input_of(to_run.output_node_id, 0))
        } else {
            self.splice_run(clip);
            (&self.spliced, self.spliced_graded)
        };
        // A request whose node this run doesn't hold (an effect fused
        // into the one before it, a node gone since) is unavailable, never
        // answered by another node or the output (M3 step 16a). An effect
        // dropped as an identity passes its input through unchanged, so it
        // is answered by what feeds it (#1314).
        // The nodes inside its compounds too: the executor answers for
        // them by id (#1475).
        fn all_ids(g: &klipp_nodes::NodeGraph, out: &mut HashSet<NodeId>) {
            for n in &g.nodes {
                out.insert(n.id);
                if let Some(sub) = &n.params.compound_subgraph {
                    all_ids(sub, out);
                }
            }
        }
        let mut held: HashSet<NodeId> = HashSet::new();
        all_ids(graph, &mut held);
        let feeds = self.identity_feeds.get(&clip.id);
        let fed_by = |node: &NodeId| {
            let (_, feed) = feeds?.iter().find(|(n, _)| n == node)?;
            match feed {
                crate::resolve::IdentityFeed::Grade => graded,
                crate::resolve::IdentityFeed::Node(n) => Some(*n),
            }
            .filter(|n| held.contains(n))
        };
        let mut scope_tap = self.resolved.scope_tap.filter(|n| held.contains(n));
        // The set as asked comes back after the run; the run's own adds
        // the nodes that answer for identities.
        let asked_thumbs = self.resolved.thumbs.take();
        let mut thumbs = asked_thumbs.clone();
        let mut fed_thumbs: Vec<(NodeId, InspectAt)> = Vec::new();
        for (node, at) in self.resolved.asked.iter().filter(|(n, _)| !held.contains(n)) {
            if let Some(input) = fed_by(node) {
                if self.resolved.solo == Some(*node) {
                    self.executor.set_solo_node(Some(input));
                }
                if self.resolved.scope_tap == Some(*node) {
                    scope_tap = Some(input);
                }
                if let Some(t) = thumbs.as_mut().filter(|t| t.contains(node)) {
                    t.insert(input);
                    fed_thumbs.push((input, at.clone()));
                }
                continue;
            }
            if !self.found.unavailable.iter().any(|(a, _)| a == at) {
                self.found.unavailable.push((at.clone(), format!("{node} doesn't run in this frame's picture")));
            }
            if self.resolved.solo == Some(*node) {
                self.executor.set_solo_node(None);
            }
        }
        let matte_view = matte_view.filter(|n| held.contains(n));
        let r = match ctx {
            Some(ctx) => self.executor.execute_linear_with_context(source, graph, width, height, clip_time, frame, scope_tap, matte_view, thumbs.as_ref(), ctx),
            None => self.executor.execute_linear_with_thumbs(source, graph, width, height, clip_time, frame, scope_tap, matte_view, thumbs.as_ref()),
        };
        self.resolved.thumbs = asked_thumbs;
        if r.is_ok() {
            self.note_declines(clip.id);
        }
        r.map(|(out, tapped, thumbs)| {
            self.found.scope_tap = tapped;
            for (node, bytes) in thumbs {
                for at in self.resolved.thumb_of.get(&node).into_iter().flatten() {
                    self.found.thumbs.insert(at.clone(), bytes.clone());
                }
                for (_, at) in fed_thumbs.iter().filter(|(n, _)| *n == node) {
                    self.found.thumbs.insert(at.clone(), bytes.clone());
                }
            }
            out
        })
    }

    /// `self.spliced`: `clip`'s grade (the source straight to the output for
    /// an identity grade) with the display step and `run_effects` spliced
    /// in, each with its mix and mask (`run_blends`).
    fn splice_run(&mut self, clip: &Clip) {
        // An identity grade is the source straight to the output: the
        // effects alone (step 11b). Not when a mask reads a node of it: a
        // grade whose picture is the source's can still publish a window,
        // and not when the grade tools read its nodes (#1274).
        let reads_the_grade = self.run_blends.iter().flatten().any(|b| matches!(b.mask, Some(crate::resolve::MaskSource::Grade { .. })));
        let tapped = self.inspects_the_grade_of(clip);
        let to_run = clip.grade_to_run();
        let base = if clip.grade_is_identity() && !reads_the_grade && !tapped { &self.passthrough } else { &*to_run };
        self.spliced.clone_from(base);
        let source = self.spliced.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).map(|n| n.id);
        let graded = self.spliced.input_of(self.spliced.output_node_id, 0);
        self.spliced_graded = graded;
        if let Some(step) = self.run_display {
            self.spliced.splice_before_output(scene_to_display_node(clip, step));
        }
        // Each straight after the Source, the last first, so they run in
        // their order.
        for node in self.run_source_nodes.iter().rev() {
            self.spliced.splice_after_source(node.clone());
        }
        // Each effect's node is the picture's node it came from, so an
        // inspection by path names it (M3 step 16a); an effect without one
        // (the list route) keeps an id derived from its place.
        let base = clip.id.0.as_u128() ^ (0x00ef_fec7_u128 << 64);
        let nodes = self.planned_nodes.get(&clip.id).filter(|n| n.len() == self.run_effects.len()).cloned().unwrap_or_default();
        let id = |i: usize| nodes.get(i).copied().flatten().unwrap_or_else(|| klipp_nodes::NodeId::from_u128(base ^ i as u128));
        let unavailable = splice_blended(&mut self.spliced, &self.run_effects, &self.run_blends, id, source, graded);
        self.masks_unavailable(clip, unavailable);
    }

    /// Say and count masked effects whose matte their run can't give.
    fn masks_unavailable(&mut self, clip: &Clip, reasons: Vec<String>) {
        for why in reasons {
            tracing::warn!("clip {}: a masked effect is bypassed: {why}", clip.id.0);
            self.found.stats.effects_mask_unavailable += 1;
            EFFECT_MASKS_UNAVAILABLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Count a clip's effect run that a plan couldn't apply and the CPU
    /// kernels take instead.
    fn cpu_fallback(&mut self, clip: &Clip) {
        tracing::debug!("clip {}: its effects on the CPU kernels, the plan couldn't apply them", clip.id.0);
        self.found.stats.effects_on_cpu += 1;
        EFFECTS_ON_CPU.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count a clip's effect run drawn on the CPU, by the kernels or by the
    /// CPU executor running its plan again (#1274).
    fn cpu_kernel_run(&mut self) {
        self.found.stats.effects_on_cpu_kernels += 1;
        EFFECTS_ON_CPU_KERNELS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// [`Self::effects`] for `clip`'s list, its analysis effects given
    /// their analysis for the frame (M4 step 9): a deflicker's gain and the
    /// frame's motion, read at the frame's source time from the clip's
    /// analyses. The kernels take one motion for the list.
    #[allow(clippy::too_many_arguments)]
    fn effects_analysed(&mut self, clip: &Clip, fx: &[BuiltinEffect], pixels: Vec<f32>, clip_time: f64, fps: f64, width: u32, height: u32) -> Vec<f32> {
        use klipp_nodes::frame_context::{Analysis, AnalysisInputs, NODE_ANALYSIS_SCHEMA};
        let Some((_, analyses)) = self.clip_analysis.as_ref().filter(|(id, _)| *id == clip.id) else {
            return self.effects(fx, pixels, width, height);
        };
        let source_secs = clip.source_secs_at(clip_time, fps);
        let mut motion = klipp_effects::MotionData::default();
        let mut list = fx.to_vec();
        // An analysis effect with no analysis for the frame passes it
        // through and says so, as its node does on the plan route (step
        // 10), as a neighbour missing: this route has no source range to
        // tell a time past the media's edge apart.
        let decline = |r: &mut Self, effect: &BuiltinEffect, outcome: klipp_nodes::DeclineOutcome, reason: &str| {
            r.found.declines.push(klipp_nodes::Decline { clip: Some(clip.id), node: NodeId::nil(), kind: klipp_nodes::NodeKind::Effect, outcome, name: klipp_nodes::decline::effect_name(effect), reason: reason.into(), why: String::new() });
        };
        let mut missing: Vec<(BuiltinEffect, klipp_nodes::DeclineOutcome, &str)> = Vec::new();
        for effect in &mut list {
            if klipp_effects::node_analysis::derivation_params(effect).is_none() {
                continue;
            }
            let Analysis::Ready(blob) = analyses.lookup_effect(NodeId::nil(), effect, NODE_ANALYSIS_SCHEMA) else {
                missing.push((effect.clone(), klipp_nodes::DeclineOutcome::NeighbourMissing, "its analysis isn't there"));
                continue;
            };
            let Some(a) = blob.data.downcast_ref::<klipp_effects::node_analysis::NodeAnalysis>() else { continue };
            let at = a.at(effect, source_secs, width, height);
            let covered = match effect {
                BuiltinEffect::Deflicker { .. } => at.gain.is_some(),
                BuiltinEffect::VideoStabilization { .. } => at.motion.stabilization.is_some(),
                BuiltinEffect::RollingShutterCorrection { .. } => at.motion.rsc_rows.is_some(),
                _ => true,
            };
            if !covered {
                missing.push((effect.clone(), klipp_nodes::DeclineOutcome::NeighbourMissing, "its analysis has no frame here"));
            }
            if let (BuiltinEffect::Deflicker { gain, .. }, Some(g)) = (&mut *effect, at.gain) {
                *gain = g;
            }
            if at.motion.stabilization.is_some() {
                motion.stabilization = at.motion.stabilization;
            }
            if at.motion.rsc_rows.is_some() {
                motion.rsc_rows = at.motion.rsc_rows;
            }
        }
        for (effect, outcome, reason) in &missing {
            decline(self, effect, *outcome, reason);
        }
        let kept = std::mem::replace(&mut self.motion, motion);
        let out = self.effects(&list, pixels, width, height);
        self.motion = kept;
        out
    }

    /// `fx` on `pixels` by the CPU kernels: a clip whose effects can't run
    /// on the GPU (no GPU, an effect with no GPU kernel), or whose plan
    /// couldn't apply them. Every run is counted (#1274).
    fn effects(&mut self, fx: &[BuiltinEffect], pixels: Vec<f32>, width: u32, height: u32) -> Vec<f32> {
        if !has_active_effects(fx) {
            return pixels;
        }
        self.cpu_kernel_run();
        let t = Instant::now();
        // The working space's luminance weights, for the effects that gate
        // on brightness — a bright pass, a denoise threshold, an edge
        // measure: the numbers the plan's effect nodes get, which is what
        // `cpu_gpu_effect_parity` checks.
        let luma = klipp_color::linear::luma_coefs(self.working);
        let out = klipp_effects::apply_effects_with_motion(fx, pixels, width, height, luma, &self.motion);
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
    /// Where every pinned clip in play is on its surface at `time`, its
    /// host placed as `transforms` resolved it (in sequence pixels, before
    /// the render scale).
    fn resolve_pins<'c>(&mut self, seq: &Sequence, clips: impl Iterator<Item = &'c Clip>, time: Timecode, fps: f64, transforms: &HashMap<ClipId, ResolvedTransform>) {
        self.pins.clear();
        self.pin_presence.clear();
        self.pin_light.clear();
        for c in clips {
            if let Some(q) = eval::surface_pin_corners(c, seq, time, fps, &|host| transforms.get(&host.id).copied()) {
                self.pins.insert(c.id, q);
            }
            let shown = eval::surface_pin_presence(c, seq, time, fps);
            if shown < 1.0 {
                self.pin_presence.insert(c.id, shown);
            }
            let light = eval::surface_pin_light(c, seq, time, fps);
            if (light - 1.0).abs() > 1e-4 {
                self.pin_light.insert(c.id, light);
            }
        }
    }

    fn open_shutter(&mut self, seq: &Sequence, time: Timecode, fps: f64, width: u32, height: u32, transforms: &HashMap<ClipId, ResolvedTransform>) {
        self.shutter.clear();
        self.title_shutter.clear();
        let clips: HashMap<ClipId, &Clip> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        // A title's own motion blur samples its transform the way the
        // camera shutter does (#995): in render pixels, under the camera
        // at the title's depth. It sampled the raw transform, so a
        // blurred title sat elsewhere at a reduced preview scale and
        // ignored a moving camera. Its own shutter, whatever the camera's.
        for id in transforms.keys() {
            let Some(c) = clips.get(id).copied() else { continue };
            let Some(mb) = title_motion_blur(c) else { continue };
            let offsets = shutter_offsets(mb.evaluated_shutter_angle(eval_clip_time(c, time)), fps, mb.samples);
            let samples: Vec<(ResolvedTransform, f64)> = offsets.iter().map(|&o| self.shutter_sample(seq, c, time, o, fps, width, height)).collect();
            self.title_shutter.insert(*id, samples);
        }
        let cam = &seq.camera;
        if cam.shutter_deg <= 0.0 {
            return;
        }
        let offsets = shutter_offsets(cam.shutter_deg, fps, cam.shutter_samples.clamp(2, 32));
        for id in transforms.keys() {
            let Some(c) = clips.get(id).copied() else { continue };
            // A title carrying its own blur keeps it; sampling the
            // sequence shutter over it as well would blur it twice.
            if title_motion_blur(c).is_some() {
                continue;
            }
            if let Some(samples) = self.samples_if_moving(seq, c, time, fps, width, height, &offsets) {
                self.shutter.insert(*id, samples);
            }
        }
    }

    /// Whether `c` really moves while the sequence camera's shutter is
    /// open at `time` (#1922): what decides that it is blurred, and so that
    /// the CPU stack draws it. `may_blur` first, so a clip nothing
    /// animates is never sampled; then its samples, as
    /// `Self::samples_if_moving` compares them. A clip with a title
    /// blur of its own is blurred by that, not by the camera's shutter.
    pub fn moves_in_shutter(&mut self, seq: &Sequence, c: &Clip, time: Timecode, width: u32, height: u32) -> bool {
        if !may_blur(seq, c) || title_motion_blur(c).is_some() {
            return false;
        }
        let fps = seq.fps.as_f64();
        let offsets = shutter_offsets(seq.camera.shutter_deg, fps, seq.camera.shutter_samples.clamp(2, 32));
        self.samples_if_moving(seq, c, time, fps, width, height, &offsets).is_some()
    }

    /// `c`'s transform sampled at each of the shutter's `offsets` from
    /// `time`, when it moves across them: its first and last samples
    /// apart by more than 0.05 of the sequence's pixels in position, or in
    /// scale, rotation or its own tilt. Position is compared in the
    /// sequence's pixels, not the render's, so a preview at a reduced size
    /// and the export decide alike (#1922).
    ///
    /// Only the first and last samples are compared: a move that comes
    /// back within one shutter, a short bounce or a whole turn, reads as
    /// still and is drawn unblurred. A clip carried by a link is judged
    /// by where it is carried and its own tilt, not its target's tilt.
    #[allow(clippy::too_many_arguments)]
    fn samples_if_moving(&mut self, seq: &Sequence, c: &Clip, time: Timecode, fps: f64, width: u32, height: u32, offsets: &[f64]) -> Option<Vec<(ResolvedTransform, f64)>> {
        let samples: Vec<(ResolvedTransform, f64)> = offsets.iter().map(|&o| self.shutter_sample(seq, c, time, o, fps, width, height)).collect();
        let (first, last) = (*samples.first()?, *samples.last()?);
        let [tx, ty] = c.tilt_lanes();
        let tilt_at = |t: f64| (klipp_core::keyframe::evaluate(&tx, t, 0.0), klipp_core::keyframe::evaluate(&ty, t, 0.0));
        let (sx, sy) = render_scale(seq, width, height);
        let moved = ((first.0 .0 - last.0 .0) / sx.max(1e-6)).abs() > 0.05
            || ((first.0 .1 - last.0 .1) / sy.max(1e-6)).abs() > 0.05
            || (first.0 .2 - last.0 .2).abs() > 1e-4
            || (first.0 .4 - last.0 .4).abs() > 0.01
            || tilt_at(first.1) != tilt_at(last.1);
        moved.then_some(samples)
    }

    /// `c`'s transform at shutter offset `o` from `time`, as the frame
    /// would resolve it then: in render pixels, under the camera at the
    /// clip's depth; with the clip time it was taken at.
    ///
    /// A clip that follows another has no transform of its own to
    /// sample — its position at an instant is whatever its target was at
    /// that instant. So it is resolved the same way the frame resolves
    /// it, each sample with a cache of its own: the resolver keys by clip
    /// id for one moment, and reusing one across the shutter would hand
    /// every sample the first sample's answer.
    #[allow(clippy::too_many_arguments)]
    fn shutter_sample(&mut self, seq: &Sequence, c: &Clip, time: Timecode, o: f64, fps: f64, width: u32, height: u32) -> (ResolvedTransform, f64) {
        let t = (eval_clip_time(c, time) + o).max(0.0);
        let own = if c.transform_link.is_some() {
            // Carried at a far finer rate than the sequence's. A Timecode
            // rounds to whole frames and a shutter's offsets are fractions
            // of one, so at the sequence rate every sample lands on one of
            // two frames and the clip smears about half as far as it
            // should.
            let fine = time.rate.saturating_mul(1000);
            let at_sample = Timecode::from_secs_f64(time.to_secs_f64() + o, fine);
            // In the sequence's pixels, as the frame resolves it, then
            // scaled with everything else.
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
        (xf, t)
    }

    /// A clip smeared along its path while the shutter is open. What sits
    /// before the transform in its chain — a crop, rounded corners — is
    /// drawn once, the picture is placed at every sample and the samples
    /// averaged, and what draws outside the picture — a shadow, a glow —
    /// is cast once from the blurred result, so it blurs with it.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn shuttered(&mut self, clip: &Clip, pixels: Vec<f32>, fx: &[BuiltinEffect], blends: &[Option<crate::resolve::Blend>], samples: &[(ResolvedTransform, f64)], clip_time: f64, fps: f64, width: u32, height: u32, plan: bool) -> Vec<f32> {
        let cut = shutter_cut(fx);
        let before = &fx[..cut];
        let after: Vec<BuiltinEffect> = fx[cut..].iter().filter(|e| !matches!(e, BuiltinEffect::Transform { .. })).cloned().collect();
        // Each effect's mix with it, split as the effects are (step 9c-2).
        let blend = |i: usize| blends.get(i).cloned().flatten();
        let before_blends: Vec<_> = (0..cut).map(blend).collect();
        let after_blends: Vec<_> = (cut..fx.len()).filter(|i| !matches!(fx[*i], BuiltinEffect::Transform { .. })).map(blend).collect();
        let base = self.effects_by(plan, clip, before, &before_blends, pixels, clip_time, fps, width, height);
        let mut acc = vec![0.0f32; base.len()];
        for &(xf, t) in samples {
            let placed = self.effects_by(plan, clip, &[transform_effect(xf, tilt(clip, fx, t))], &[], base.clone(), t, fps, width, height);
            add_premultiplied(&mut acc, &placed);
        }
        average_premultiplied(&mut acc, samples.len());
        self.effects_by(plan, clip, &after, &after_blends, acc, clip_time, fps, width, height)
    }

    /// A title smeared by its own motion blur: as [`Self::shuttered`]
    /// does for the camera's shutter (#995), what sits before the
    /// transform in its chain runs on the picture, the picture is placed
    /// at every sample and the samples averaged, and the rest runs once
    /// on the result. It used to place the raw picture and run every
    /// other effect after, so a crop or a rounded corner before the
    /// transform was placed and blurred where it should have been drawn
    /// first.
    #[allow(clippy::too_many_arguments)]
    fn motion_blurred(
        &mut self,
        clip: &Clip,
        pixels: Vec<f32>,
        redrawn: Option<&klipp_timeline::TitleData>,
        fx: &[BuiltinEffect],
        blends: &[Option<crate::resolve::Blend>],
        samples: &[(ResolvedTransform, f64)],
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        plan: bool,
    ) -> Vec<f32> {
        let clip_time = eval_clip_time(clip, time);
        let cut = shutter_cut(fx);
        let before = &fx[..cut];
        let after: Vec<BuiltinEffect> = fx[cut..].iter().filter(|e| !matches!(e, BuiltinEffect::Transform { .. })).cloned().collect();
        // Each effect's mix and mask with it, split as the effects are.
        let blend = |i: usize| blends.get(i).cloned().flatten();
        let before_blends: Vec<_> = (0..cut).map(blend).collect();
        let after_blends: Vec<_> = (cut..fx.len()).filter(|i| !matches!(fx[*i], BuiltinEffect::Transform { .. })).map(blend).collect();
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
        // The picture drawn once and carried through what precedes the
        // transform, when its drawing does not move.
        let still = match redrawn {
            Some(_) => None,
            None => Some(self.effects_by(plan, clip, before, &before_blends, pixels.clone(), clip_time, fps, width, height)),
        };
        let mut acc = vec![0.0f32; pixels.len()];
        for &(xf, t) in samples {
            let picture = match (&still, redrawn) {
                (Some(base), _) => base.clone(),
                (None, Some(td)) => {
                    let drawn = self.title_pixels_at(clip, td, t, fps, width, height);
                    self.effects_by(plan, clip, before, &before_blends, drawn, t, fps, width, height)
                }
                (None, None) => unreachable!("a still picture is drawn when the title's drawing does not move"),
            };
            let placed = self.effects_by(plan, clip, &[transform_effect(xf, tilt(clip, fx, t))], &[], picture, t, fps, width, height);
            add_premultiplied(&mut acc, &placed);
        }
        average_premultiplied(&mut acc, samples.len());
        self.effects_by(plan, clip, &after, &after_blends, acc, clip_time, fps, width, height)
    }

    /// A smooth cut's frame at `progress` (#2219): `from`, the outgoing
    /// shot's last frame, morphed into `to`, the incoming's first, by the
    /// motion between them (`klipp_effects::flow`), eased in and out. The
    /// pair is the same through the whole transition, so its flow is
    /// estimated once and kept. Where too much of it can't be followed the
    /// frames cross-fade, recorded as a fallback; without a GPU the CPU
    /// reference morphs them, recorded the same way.
    #[allow(clippy::too_many_arguments)]
    fn smooth_cut(&mut self, a: &Clip, b: &Clip, from: &[f32], to: &[f32], at: (Timecode, Timecode), progress: f32, width: u32, height: u32) -> Vec<f32> {
        let p = progress.clamp(0.0, 1.0);
        let t = p * p * (3.0 - 2.0 * p);
        let luma = klipp_color::linear::luma_coefs(self.working);
        // Keyed by the cut, both its clips, apart from either clip's own.
        let key = klipp_effects::gpu::FlowKey { clip: a.id.0.as_u128() ^ b.id.0.as_u128().rotate_left(64), from_secs: at.0.to_secs_f64(), to_secs: at.1.to_secs_f64() };
        let made = self.gpu().and_then(|gpu| gpu.interpolate_by_flow(key, from, to, width, height, t, luma));
        let made = made.unwrap_or_else(|| {
            residency::record("transition.smooth_cut", "no GPU: the CPU reference morphed the frames", Work::Fallback);
            klipp_effects::flow::interpolate(from, to, width, height, t, luma)
        });
        if made.cross_faded {
            residency::record("transition.smooth_cut", "the two framings couldn't be followed: the frames cross-faded", Work::Fallback);
        }
        made.pixels
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
        shutter: (f32, u32),
        time: Timecode,
        fps: f64,
        width: u32,
        height: u32,
        src: &mut dyn FrameSource,
        transforms: &HashMap<ClipId, ResolvedTransform>,
        mattes: Option<&HashMap<ClipId, Vec<f32>>>,
    ) -> Option<Vec<f32>> {
        let _clip_residency = residency::clip(a.id.0);
        residency::record(klipp_core::spec_for_kind(kind).string_key, "Transition uses CPU composite inputs/outputs", Work::Fallback);
        let n = (width as usize) * (height as usize) * 4;
        let side = |this: &mut Self, c: &Clip, time: Timecode, src: &mut dyn FrameSource| {
            this.render_clip(c, time, fps, width, height, src, transforms)
                .map(|(mut p, opacity)| {
                    // Opacity is the clip's alpha within the transition.
                    p.chunks_exact_mut(4).for_each(|px| px[3] *= opacity);
                    // A matted side is seen through its matte here too (#718).
                    if let (Some(rendered), Some(binding)) = (mattes, this.matte_binding(c, time)) {
                        apply_matte(&mut p, rendered, &binding, width, height, klipp_color::linear::luma_coefs(this.working));
                    }
                    p
                })
                .unwrap_or_else(|| vec![0.0; n])
        };
        // A smooth cut (#2219) holds the outgoing shot's last frame and the
        // incoming's first, so neither plays its handle (on a jump cut, the
        // pause that was cut out); every other transition plays both.
        let smooth = kind == klipp_core::TransitionKind::SmoothCut;
        let (time_a, time_b) = if smooth {
            // One frame back in whole ticks (1001 at an NTSC rate): from
            // seconds it could round onto the frame beside it.
            let frame = Timecode { value: (f64::from(a.timeline_out.rate) / fps).round() as i64, rate: a.timeline_out.rate };
            ((a.timeline_out - frame).max(a.timeline_in), b.timeline_in)
        } else {
            (time, time)
        };
        let mut from = side(self, a, time_a, src);
        let mut to = side(self, b, time_b, src);
        let kind_id = klipp_core::spec_for_kind(kind).kind_id as u32;
        let eased = if matches!(kind, klipp_core::TransitionKind::CrossDissolve) {
            klipp_transitions::apply_easing(progress, params.param0)
        } else {
            progress
        };
        // The transitions mix straight colour, so a side that is partly or
        // wholly transparent (matted out, faded) would still lend its
        // colour by the blend's weight, not its alpha: a clip hidden by
        // its matte tinted the dissolve (#718). With transparency on
        // either side, mix premultiplied and divide back after.
        let see_through = |p: &[f32]| p.chunks_exact(4).any(|px| px[3] < 0.9999);
        let premultiplied = see_through(&from) || see_through(&to);
        if premultiplied {
            for p in [&mut from, &mut to] {
                p.chunks_exact_mut(4).for_each(|px| { px[0] *= px[3]; px[1] *= px[3]; px[2] *= px[3]; });
            }
        }
        let colour = self.authored_colour(params.color);
        let mut mixed = if smooth {
            self.smooth_cut(a, b, &from, &to, (time_a, time_b), progress, width, height)
        } else if klipp_effects::pieces::is_posed_with(kind_id, params.param3, subject_code(kind, params)) {
            // Posed pieces (#985), blurred across the shutter, cut round
            // the subject when asked.
            let plan = self.piece_plan(a, b, kind, eased, params, shutter, time, fps, width, height, src, transforms);
            match self.gpu() {
                Some(gpu) => gpu.render_pieces_linear(&from, &to, &plan),
                None => klipp_effects::pieces::render(&from, &to, &plan),
            }
        } else { match self.gpu() {
            Some(gpu) => gpu.render_transition_linear(
                &from, &to, kind_id, eased, width, height,
                params.param0, params.param1, params.param2, params.param3, colour,
            ),
            // The same shader, transcribed: the same picture without a GPU.
            None => klipp_effects::render_transition_linear(
                &from, &to, kind_id, eased, width, height,
                params.param0, params.param1, params.param2, params.param3, colour,
            ),
        } };
        if premultiplied {
            mixed.chunks_exact_mut(4).for_each(|px| {
                if px[3] > klipp_effects::UNPREMULTIPLY_MIN_ALPHA {
                    let inv = 1.0 / px[3];
                    px[0] *= inv; px[1] *= inv; px[2] *= inv;
                }
            });
        }
        Some(mixed)
    }

    /// The track matte `c` is seen through at `time`, at render scale.
    fn matte_binding(&self, c: &Clip, time: Timecode) -> Option<klipp_timeline::MatteBinding> {
        let b = c.matte_source.as_ref()?;
        let t = eval_clip_time(c, time);
        Some(klipp_timeline::MatteBinding {
            source_clip_id: b.source_clip_id,
            channel: b.channel,
            invert: b.invert,
            feather_px: klipp_core::keyframe::evaluate(&c.matte_feather_keyframes, t, b.feather_px) * self.px_scale,
            choke_px: klipp_core::keyframe::evaluate(&c.matte_choke_keyframes, t, b.choke_px) * self.px_scale,
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
        // No side pictures: an adjustment layer reads none (#1746), and the
        // clip before's are not its.
        self.executor.clear_side_pictures();
        self.note_switched_off(clip);
        let clip_time = eval_clip_time(clip, time);
        let before = if opacity < 0.999 { Some(below.clone()) } else { None };
        let below = Arc::new(below);
        // What its grade reads, kept for a masked effect to make its matte
        // from: its effects run apart from its grade (9c-2 B2).
        self.matte_input = clip.is_picture_truth().then(|| below.clone());
        let graded = self.grade(clip, GradeInput::Cpu(below), clip_time, fps, width, height, None);
        let graded = self.ofx_nodes(clip, graded, clip_time, fps, clip_time, width, height, None);
        let (mut fx, _) = clip_effects(clip, time, fps, transforms.get(&clip.id).copied(), self.pins.get(&clip.id).copied());
        self.take_light(clip.id, &mut fx);
        // Its effect colours are authored in BT.709, as a clip's are, and
        // converted into the working space the same way (#995): left as
        // they were, an adjustment layer and a clip with the same effect
        // differed in ACEScg and BT.2020 projects.
        self.authored_colours(&mut fx);
        self.at_render_scale(&mut fx);
        // An adjustment layer has no footage of its own to analyse, so its
        // stabilisation and rolling-shutter correction have no analysis
        // and pass through, on both arms (#698).
        // As a plan of its own, from its picture, when it can run on the
        // GPU (step 11d); else the CPU kernels.
        // A picture-truth layer's masked or mixed effects take the plan with
        // no GPU too (step 9c-2).
        let plan = self.effects_follow_grade_on_gpu(clip, &fx);
        let truth = clip.is_picture_truth() && has_active_effects(&fx);
        let (fx, blends) = if plan || truth {
            let mut stats = std::mem::take(&mut self.found.stats);
            let planned = self.plan_effects(clip, time, fps, transforms, &fx, PlanPart::Whole, &mut stats);
            self.found.stats = stats;
            (planned, std::mem::take(&mut self.planned_blends))
        } else {
            (fx, Vec::new())
        };
        let missed = self.plan_misses;
        let mut after = self.effects_by(plan, clip, &fx, &blends, graded, clip_time, fps, width, height);
        self.matte_input = None;
        if plan && self.plan_misses == missed {
            self.found.stats.effects_on_layers += 1;
            PLAN_EFFECT_CLIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
fn active_item<'a>(seq: &'a Sequence, track: &'a Track, time: Timecode) -> Option<TrackItem<'a>> {
    if let Some(item) = track.transitions.iter().find_map(|tr| transition_at(seq, track, tr, time)) {
        return Some(item);
    }
    let clip = track.clip_at_timecode(time).map(|(_, c)| c).filter(|c| c.enabled)?;
    multicam_angle(seq, track, clip, time).map(TrackItem::Clip)
}

/// The kind of the transition running on `track` at `time`, if one is.
pub(crate) fn running_transition(seq: &Sequence, track: &Track, time: Timecode) -> Option<klipp_core::TransitionKind> {
    track.transitions.iter().find_map(|tr| match transition_at(seq, track, tr, time)? {
        TrackItem::Transition { kind, .. } => Some(kind),
        TrackItem::Clip(_) => None,
    })
}

/// `tr`, when it is running at `time` between two enabled clips.
fn transition_at<'a>(seq: &Sequence, track: &'a Track, tr: &klipp_timeline::Transition, time: Timecode) -> Option<TrackItem<'a>> {
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
    // The camera's shutter, as a share of this transition's progress.
    let fps = seq.fps.as_f64();
    let shutter = if seq.camera.shutter_deg > 0.0 && total > 0.0 && fps > 0.0 {
        ((seq.camera.shutter_deg as f64 / 360.0 / (fps * total)) as f32, seq.camera.shutter_samples.clamp(2, 8) as u32)
    } else {
        (0.0, 1)
    };
    Some(TrackItem::Transition { a, b, kind: tr.kind, progress, params: tr.user_params, shutter })
}

/// The clip a multicam group shows at `time`: the angle chosen by the
/// primary (angle 0) clip's cut list — angle 0 until its first cut. Angle
/// clips span the primary's range, so the chosen clip's own timing maps
/// the frame. Clips outside a multicam group are returned as they are.
fn multicam_angle<'a>(seq: &'a Sequence, track: &'a Track, clip: &'a Clip, time: Timecode) -> Option<&'a Clip> {
    let Some(group) = clip.multicam_group else { return Some(clip) };
    let Some(m) = seq.multicam_at(Some(group), time) else { return Some(clip) };
    // The group is shown once: on the primary's track, or, with that
    // track hidden or the primary disabled (a disabled clip's track has
    // nothing in play), on the topmost shown picture track holding one
    // of its clips. A synced multicam puts each angle on tracks of its own,
    // and looking on this track alone, every angle's track showed its
    // own camera, stacked.
    let holds_group = |t: &Track| t.clips.iter().any(|c| c.multicam_group == Some(group) && c.enabled && time >= c.timeline_in && time < c.timeline_out);
    let shown_on = if is_visual(&seq.tracks[m.primary_track]) && m.primary.enabled {
        Some(m.primary_track)
    } else {
        seq.tracks.iter().position(|t| is_visual(t) && t.kind != TrackKind::Audio && holds_group(t))
    };
    if shown_on.is_none_or(|i| !std::ptr::eq(&seq.tracks[i], track)) {
        return None;
    }
    // The angle cut to, else the primary where that angle has ended; a
    // disabled clip shows nothing, as anywhere else.
    seq.multicam_angle_clip(group, m.active, time)
        .or_else(|| seq.multicam_angle_clip(group, 0, time))
}

/// Seconds since the clip's start on the timeline; negative before it
/// (a transition's incoming side), clamped to zero for evaluation.
pub(crate) fn eval_clip_time(clip: &Clip, time: Timecode) -> f64 {
    (time - clip.timeline_in).to_secs_f64().max(0.0)
}

/// When a clip drawn from its own time (a generator, a title) is drawn
/// at `time`, the one rule for the CPU route and the GPU stack alike
/// (#1918). In a transition's overlap a clip is drawn before its start
/// (the incoming side) or past its end (the outgoing side):
/// - a generator that shows no count of its own runs on at its own
///   seconds, negative before its start, so its motion carries through
///   the cut instead of freezing;
/// - one that shows its own time (`GeneratorData::shows_own_time`), and
///   a title, whose entrance and exit are timed from its ends, hold their
///   first frame before the start and their last past the end.
///
/// Any other clip is `eval_clip_time`'s. Keyframed settings (a clip's
/// transform, opacity, grade) are evaluated at `grade_time`, which
/// clamps at zero: before its start they hold their first key.
pub(crate) fn drawn_time(clip: &Clip, time: Timecode) -> f64 {
    let own = (time - clip.timeline_in).to_secs_f64();
    let total = (clip.timeline_out - clip.timeline_in).to_secs_f64().max(0.0);
    match &clip.kind {
        ClipKind::Generator(g) if !g.shows_own_time() => own,
        ClipKind::Generator(_) | ClipKind::Title(_) => own.clamp(0.0, total),
        _ => eval_clip_time(clip, time),
    }
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
/// A reversed clip walks back from the end of its range (#617).
/// The subject a piece transition cuts out, as the renderers take it
/// (`klipp_effects::pieces::plan_with`; #985 phase 3): 0 none.
fn subject_code(kind: klipp_core::TransitionKind, params: &klipp_core::TransitionUserParams) -> u32 {
    params.subject.filter(|_| klipp_core::piece_transition::is_piece_kind(kind)).map_or(0, |c| c.code())
}

pub fn source_secs(clip: &Clip, time: Timecode, fps: f64) -> f64 {
    clip.source_secs_at((time - clip.timeline_in).to_secs_f64(), fps)
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

/// The source straight to the output, with fixed ids.
fn passthrough_graph() -> klipp_nodes::NodeGraph {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
    let node = |id: u128, kind| Node { id: NodeId::from_u128(id), kind, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
    let (source, output) = (node(0x0011_b5ce, NodeKind::Source), node(0x0011_b0ce, NodeKind::Output));
    let edges = vec![Edge { from_node: source.id, from_port: 0, to_node: output.id, to_port: 0 }];
    NodeGraph { output_node_id: output.id, nodes: vec![source, output], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// `blends` with each mask the effects' own plan can draw moved beside the
/// chain of a run apart from the clip's grade (#1265), with how many; `None`
/// when it moved none:
/// - a window (a Mask node, of the grade or beside it), which reads no
///   picture, only its shapes at the run's time and size;
/// - a key (a Qualifier, a luma key, a difference key) of the picture the
///   grade reads, reading it as a side picture
///   (`SideKey::GradeInput`), drawn on the GPU where there is one.
///
/// A key of anything the grade made stays on the CPU side run.
fn mattes_in_the_plan(clip: &Clip, blends: &[Option<crate::resolve::Blend>]) -> Option<(Vec<Option<crate::resolve::Blend>>, u32)> {
    use crate::resolve::{MaskSource, SideInput};
    use klipp_nodes::NodeKind;
    let grade = clip.grade_to_run();
    let grade_source = grade.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id);
    let is_key = |k: NodeKind| matches!(k, NodeKind::Qualifier | NodeKind::LumaKey | NodeKind::DifferenceKey);
    let window = |mask: &MaskSource| -> Option<MaskSource> {
        let (node, port, reads_the_grades_input) = match mask {
            // A grade's node reading the grade's own Source.
            MaskSource::Grade { node, port } => {
                let n = grade.nodes.iter().find(|n| n.id == *node)?.clone();
                let reads = grade_source.is_some() && grade.input_of(n.id, 0) == grade_source;
                (n, *port, reads)
            }
            // Beside the chain, reading the clip's source as the grade does.
            MaskSource::Side { node, port, input, lead } if lead.is_empty() => ((**node).clone(), *port, *input == SideInput::Source),
            MaskSource::Side { .. } | MaskSource::Unavailable(_) => return None,
        };
        match node.kind {
            NodeKind::Mask => Some(MaskSource::Side { node: Box::new(node), lead: Vec::new(), port, input: SideInput::Source }),
            k if is_key(k) && reads_the_grades_input => Some(MaskSource::Side { node: Box::new(node), lead: Vec::new(), port, input: SideInput::GradeInput }),
            _ => None,
        }
    };
    let mut moved = 0;
    let out: Vec<Option<crate::resolve::Blend>> = blends
        .iter()
        .map(|b| {
            let b = b.as_ref()?;
            match b.mask.as_ref().and_then(window) {
                Some(mask) => {
                    moved += 1;
                    Some(crate::resolve::Blend { mask: Some(mask), ..b.clone() })
                }
                None => Some(b.clone()),
            }
        })
        .collect();
    (moved > 0).then_some((out, moved))
}

/// The colour domain of a clip's picture as its grade reads it (#1867):
/// scene-referred when it was decoded so (HDR or camera log, `hdr` its
/// transfer), display-referred otherwise. The renderer's grade and the
/// viewer's GPU path on the parked frame both ask here, so a held
/// output's key (#1863) is the same from either.
pub fn source_domain(hdr: Option<klipp_color::hdr::HdrTransfer>) -> klipp_nodes::domains::PictureDomain {
    match hdr {
        Some(_) => klipp_nodes::domains::PictureDomain::Scene,
        None => klipp_nodes::domains::PictureDomain::Display,
    }
}

/// What `clip`'s frame is the picture of, for its held outputs (#1863):
/// `file`, the decoded frame's own `pts` and `decoded` size, and the
/// clip's retime at `clip_time` (`at` the source time it shows there).
/// One construction for the renderer, which stores the outputs, and the
/// viewer's GPU path, which takes them: the two keys can't drift.
pub fn held_source_of(clip: &Clip, clip_time: f64, at: f64, pts: f64, decoded: (u32, u32), file: HeldFile, media_fps: Option<f64>) -> klipp_nodes::held::HeldSource {
    let retime = klipp_nodes::held::Retime {
        speed: clip.speed_at(clip_time),
        reverse: clip.is_reversed(),
        source_at_clip_time: at,
        media_fps,
        // Media plays at its own rate; there is no conform to say.
        conform_fps: None,
    };
    klipp_nodes::held::HeldSource {
        clip: clip.id,
        media: file.media,
        path: file.path,
        file_len: file.len,
        file_modified: file.modified,
        log_format: file.log_format,
        pts_micros: (pts * 1e6).round() as i64,
        decoded,
        retime,
        proxy: file.proxy,
    }
}

/// `graph`, `clip`'s grade, with the clip's source-boundary node spliced
/// after its Source as the renderer runs it at `time` (#1863): the graph
/// the viewer's GPU path runs when it takes the node's output held. The
/// node's id is the clip's, so the two key it alike. `media_fps` is its
/// file's frame rate, as the renderer's source gives it.
pub fn graph_with_source_nodes(clip: &Clip, time: Timecode, fps: f64, media_fps: Option<f64>, graph: &klipp_nodes::NodeGraph) -> klipp_nodes::NodeGraph {
    let denoise = matches!(clip.kind, ClipKind::Media { .. }).then(|| temporal_denoise_of(clip, time, fps)).flatten();
    let blend = matches!(clip.kind, ClipKind::Media { .. }) && frame_blends(clip, time, media_fps, fps);
    let mut out = graph.clone();
    for node in source_nodes(clip, blend, denoise, None).into_iter().rev() {
        out.splice_after_source(node);
    }
    out
}

/// Whether every effect `clip` draws at `time` runs at its source (a
/// temporal denoise, a frame blend; #1863): nothing of its effects comes
/// after its grade, so a grade run with its source-boundary node is the
/// whole picture.
pub fn only_source_boundary_effects(clip: &Clip, time: Timecode, fps: f64) -> bool {
    let (fx, opacity) = clip_effects(clip, time, fps, None, None);
    (opacity - 1.0).abs() < 1e-6 && fx.iter().all(klipp_effects::registry::needs_source_neighbours)
}

/// A clip's source-boundary nodes (M4 steps 6 and 7): its frame blend and
/// its temporal noise reduction, in the catalog's order
/// (`SOURCE_BOUNDARY_ORDER`), run straight after the Source; none when it
/// has neither. One node runs both, unless `inspected` gives the
/// picture's nodes they come from (`picture_source_nodes`): then each is
/// a node of its own under its picture node's id, so an inspection by
/// path reads the picture after it (#1305). Run one after the other, the
/// two give what the one node gives.
fn source_nodes(clip: &Clip, blend: bool, denoise: Option<(f32, f32)>, inspected: Option<(Option<NodeId>, Option<NodeId>)>) -> Vec<klipp_nodes::Node> {
    use klipp_nodes::{Node, NodeKind, NodeParams};
    let mut effects: Vec<BuiltinEffect> = Vec::new();
    if blend {
        // Cross-faded or followed by its motion (#2215), as the clip's
        // ramp asks.
        effects.push(BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: clip.frame_blend_mode().unwrap_or(1) });
    }
    if let Some((strength, threshold)) = denoise {
        effects.push(BuiltinEffect::TemporalDenoise { strength, threshold });
    }
    effects.sort_by_key(klipp_effects::registry::source_boundary_rank);
    let id = NodeId::from_u128(clip.id.0.as_u128() ^ (0x0050_7c3e_u128 << 64));
    let node = |id, effects| Node { id, kind: NodeKind::Effect, label: String::new(), enabled: true, params: NodeParams { effects, ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: vec![] };
    match inspected {
        _ if effects.is_empty() => Vec::new(),
        None => vec![node(id, effects)],
        Some((blend_node, denoise_node)) => effects
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let picture = if matches!(e, BuiltinEffect::SpeedRamp { .. }) { blend_node } else { denoise_node };
                node(picture.unwrap_or_else(|| NodeId::from_u128(id.as_u128() ^ (i as u128 + 1))), vec![e])
            })
            .collect(),
    }
}

/// The picture's nodes the source-boundary effects that run come from:
/// its first frame blend (a speed ramp blending) and its first temporal
/// denoise switched on, as `frame_blends` and `temporal_denoise_of` take
/// them (#1305).
/// Read from the picture's nodes in list order, not the v1 view.
fn picture_source_nodes(clip: &Clip) -> (Option<NodeId>, Option<NodeId>) {
    let Ok(lifted) = clip.picture_v2() else { return (None, None) };
    // Each entry's node, switched on, with the effect its values make.
    let entries: Vec<(NodeId, BuiltinEffect)> = (0..)
        .map_while(|i| clip.effect_node_at(false, i))
        .filter_map(|id| {
            let node = lifted.picture.graph.nodes.iter().find(|n| n.id == id && n.enabled)?;
            let effect = serde_json::from_value(klipp_graph_lift::effect_params::entry_of(node).ok()?).ok()?;
            Some((id, effect))
        })
        .collect();
    let first = |is: fn(&BuiltinEffect) -> bool| entries.iter().find(|(_, e)| is(e)).map(|(id, _)| *id);
    (first(|e| matches!(e, BuiltinEffect::SpeedRamp { blend_mode: 1 | 2, .. })), first(|e| matches!(e, BuiltinEffect::TemporalDenoise { .. })))
}

/// `clip`'s display step as a plan node (step 11c), with an id fixed per
/// clip so the executor's cached plan holds across frames.
fn scene_to_display_node(clip: &Clip, step: klipp_color::hdr::SceneToDisplay) -> klipp_nodes::Node {
    use klipp_nodes::{Node, NodeId, NodeKind, NodeParams};
    Node {
        id: NodeId::from_u128(clip.id.0.as_u128() ^ (0x0011_c5d0_u128 << 64)),
        kind: NodeKind::SceneToDisplay,
        label: String::new(),
        enabled: true,
        params: NodeParams { scene_to_display: Some(step), ..Default::default() },
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    }
}


/// Splice `fx` into `graph` before its output as effect nodes (step 10b),
/// each with the mix and mask its `blends` entry gives (step 9c-2): a
/// blended effect is a node of its own, its matte wired into its mask
/// port from the grade node that publishes it, or from a node beside the
/// chain built into the run reading `source` (the clip's source) or
/// `graded` (the grade's picture). A matte the run can't give leaves its
/// effect bypassed (its mix 0: the picture as it came in); the reasons
/// come back, one an effect. `id` names the node of the group an effect
/// heads, by that effect's place in `fx`.
fn splice_blended(
    graph: &mut klipp_nodes::NodeGraph,
    fx: &[BuiltinEffect],
    blends: &[Option<crate::resolve::Blend>],
    id: impl Fn(usize) -> klipp_nodes::NodeId,
    source: Option<klipp_nodes::NodeId>,
    graded: Option<klipp_nodes::NodeId>,
) -> Vec<String> {
    use crate::resolve::{MaskSource, SideInput};
    let blend = |i: usize| blends.get(i).and_then(Option::as_ref);
    let spills = |e: &BuiltinEffect| klipp_effects::layer::Spill::of(e).is_some();
    let mut groups: Vec<(klipp_nodes::NodeKind, &[BuiltinEffect], Option<&crate::resolve::Blend>)> = Vec::new();
    // Each group's node is named by its first effect (M3 step 16a).
    let mut starts = Vec::new();
    let mut i = 0;
    while i < fx.len() {
        let kind = match &fx[i] {
            // A plug-in of its own, its instances kept (M4 step 5).
            BuiltinEffect::Frei0r { .. } => klipp_nodes::NodeKind::Frei0r,
            // Another media item's frame, or what is below, handed to the
            // run (#1746).
            BuiltinEffect::MediaSource { .. } | BuiltinEffect::TitleSource { .. } | BuiltinEffect::ClipBelow {} => klipp_nodes::NodeKind::SidePicture,
            // A grade in the chain (#1746): the executor's compound node.
            BuiltinEffect::GradeCompound {} => klipp_nodes::NodeKind::Compound,
            // A merge with a picture to lay over: a Blend drawing it as the
            // grade's layer mixer would, that picture on top (#1746), on
            // the GPU chain with the picture so far (#2758).
            BuiltinEffect::Merge { .. } if blend(i).is_some_and(|b| b.fg.is_some()) => klipp_nodes::NodeKind::Blend,
            e if spills(e) => klipp_nodes::NodeKind::EffectSegment,
            _ => klipp_nodes::NodeKind::Effect,
        };
        // A blended effect stands alone; spills run together otherwise.
        let len = if blend(i).is_some() || !spills(&fx[i]) { 1 } else { fx[i..].iter().enumerate().take_while(|(k, e)| spills(e) && blend(i + k).is_none()).count() };
        groups.push((kind, &fx[i..i + len], blend(i)));
        starts.push(i);
        i += len;
    }
    let id = |g: usize| id(starts[g]);
    let plain: Vec<_> = groups.iter().map(|(k, e, _)| (*k, *e)).collect();
    graph.splice_effects_before_output(&plain, id);
    let mut unavailable = Vec::new();
    // Where a matte or a merge's picture comes from in this run: a node
    // of the grade's, or one beside the chain built into the run.
    let producer_of = |graph: &mut klipp_nodes::NodeGraph, source_of: &MaskSource, what: &str, unavailable: &mut Vec<String>| match source_of {
        MaskSource::Grade { node: p, port } if graph.nodes.iter().any(|n| n.id == *p) => Some((*p, *port)),
        MaskSource::Grade { node: p, .. } => {
            unavailable.push(format!("its {what} comes from the grade's {p}, which isn't in this run"));
            None
        }
        MaskSource::Side { node: side, lead, port, input } => match (input, source, graded) {
            (SideInput::Source, Some(from), _) | (SideInput::Graded, _, Some(from)) => {
                crate::resolve::wire_side(graph, from, lead, side);
                Some((side.id, *port))
            }
            // The picture the grade reads, handed to the run (#1265): a side
            // picture node gives it, sized by the run's own Source.
            (SideInput::GradeInput, Some(from), _) => {
                let picture = klipp_nodes::NodeId::from_u128(side.id.as_u128() ^ (0x0009_4ade_u128 << 64));
                if !graph.nodes.iter().any(|n| n.id == picture) {
                    graph.nodes.push(klipp_nodes::Node {
                        id: picture,
                        kind: klipp_nodes::NodeKind::SidePicture,
                        label: String::new(),
                        enabled: true,
                        params: klipp_nodes::NodeParams { side_picture: Some(klipp_nodes::side_picture::SideKey::GradeInput), ..Default::default() },
                        keyframes: Default::default(),
                        output_link: None,
                        input_links: vec![],
                    });
                    graph.edges.push(klipp_nodes::Edge { from_node: from, from_port: 0, to_node: picture, to_port: 0 });
                }
                crate::resolve::wire_side(graph, picture, lead, side);
                Some((side.id, *port))
            }
            _ => {
                let reads = match input {
                    SideInput::Graded => "the grade's picture",
                    SideInput::Source => "the clip's source",
                    SideInput::GradeInput => "the picture the grade reads",
                };
                unavailable.push(format!("its {what} reads {reads}, which this run hasn't"));
                None
            }
        },
        MaskSource::Unavailable(why) => {
            unavailable.push(why.clone());
            None
        }
    };
    for (g, (kind, effects, b)) in groups.iter().enumerate() {
        let Some(b) = b else { continue };
        let node = id(g);
        // A grade compound's grade, the compound node's body (#1746); at a
        // mix below full, a Blend after it takes the picture it read that
        // far toward what it made.
        if let (klipp_nodes::NodeKind::Compound, Some(run)) = (kind, &b.compound) {
            if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node) {
                n.params.effects.clear();
                n.params.compound_subgraph = Some((**run).clone());
            }
            let mix = b.blend.mix.clamp(0.0, 1.0);
            if mix < 1.0 {
                let read = graph.edges.iter().find(|e| e.to_node == node && e.to_port == 0).map(|e| (e.from_node, e.from_port));
                if let Some((from, from_port)) = read {
                    let blend = klipp_nodes::NodeId::from_u128(node.as_u128() ^ 0x00b1_e4d0_u128);
                    for e in graph.edges.iter_mut().filter(|e| e.from_node == node) {
                        e.from_node = blend;
                    }
                    graph.edges.push(klipp_nodes::Edge { from_node: from, from_port, to_node: blend, to_port: 0 });
                    graph.edges.push(klipp_nodes::Edge { from_node: node, from_port: 0, to_node: blend, to_port: 1 });
                    graph.nodes.push(klipp_nodes::Node {
                        id: blend,
                        kind: klipp_nodes::NodeKind::Blend,
                        label: String::new(),
                        enabled: true,
                        params: klipp_nodes::NodeParams { mix: Some(mix), ..Default::default() },
                        keyframes: Default::default(),
                        output_link: None,
                        input_links: Vec::new(),
                    });
                }
            }
            continue;
        }
        // A merge's picture on the Blend's second port, the picture so far
        // on its first, drawn as a layer mixer's top layer over its base
        // by the merge's blend and opacity (#1746, #2758).
        if let (klipp_nodes::NodeKind::Blend, Some(fg), Some(BuiltinEffect::Merge { blend_mode, opacity })) = (kind, &b.fg, effects.first()) {
            // A masked merge (#2758): its matte on the third port, the
            // merge mixed back over the picture so far by it, as a masked
            // effect is. A mask that can't be had lays nothing.
            let matte = match &b.mask {
                None => Some(None),
                Some(m) => producer_of(graph, m, "matte", &mut unavailable).map(Some),
            };
            match producer_of(graph, fg, "picture", &mut unavailable).zip(matte) {
                Some(((p, port), matte)) => {
                    graph.edges.push(klipp_nodes::Edge { from_node: p, from_port: port, to_node: node, to_port: 1 });
                    if let Some((m, m_port)) = matte {
                        graph.edges.push(klipp_nodes::Edge { from_node: m, from_port: m_port, to_node: node, to_port: 2 });
                    }
                    if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node) {
                        n.params.effects.clear();
                        n.params.merge = Some(klipp_nodes::LayerMixerInput { opacity: *opacity, blend_mode: *blend_mode });
                        n.params.effect_blend = matte.map(|_| b.blend);
                    }
                }
                // No picture to lay over: the picture passes.
                None => {
                    if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node) {
                        n.kind = klipp_nodes::NodeKind::Effect;
                    }
                }
            }
            continue;
        }
        let mut params = b.blend;
        let producer = b.mask.as_ref().and_then(|m| producer_of(graph, m, "matte", &mut unavailable));
        if b.mask.is_some() && producer.is_none() {
            // Bypassed: no matte, no effect.
            params.mix = 0.0;
        }
        if let Some((p, port)) = producer {
            graph.edges.push(klipp_nodes::Edge { from_node: p, from_port: port, to_node: node, to_port: 1 });
        }
        if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node) {
            n.params.effect_blend = Some(params);
        }
    }
    unavailable
}

/// An effect's name as its record writes it (`gaussian_blur`), for what
/// the renderer says about it.
fn effect_tag(effect: &BuiltinEffect) -> String {
    serde_json::to_value(effect).ok().and_then(|v| v.get("type").and_then(serde_json::Value::as_str).map(str::to_owned)).unwrap_or_else(|| "effect".into())
}

/// The failing decline of a grade run refused because a matte it reads
/// isn't there (`MaskUnavailable`): the alpha a grade's Output takes
/// (#898) or an effect's mask. Named against `clip`, so the viewer says
/// why the clip is drawn without its grade and an export refuses the
/// frame. `None` for any other refusal.
fn missing_matte_decline(clip: &Clip, e: &klipp_nodes::executor::ExecutorError) -> Option<klipp_nodes::Decline> {
    let klipp_nodes::executor::ExecutorError::MaskUnavailable(node) = e else { return None };
    let kind = clip.grade_to_run().nodes.iter().find(|n| n.id == *node).map_or(klipp_nodes::NodeKind::Unknown, |n| n.kind);
    let name = if kind == klipp_nodes::NodeKind::OutputAlpha { "grade's alpha" } else { "mask" };
    Some(klipp_nodes::Decline {
        clip: Some(clip.id),
        node: *node,
        kind,
        outcome: klipp_nodes::DeclineOutcome::RenderFailed,
        name: name.into(),
        reason: "its matte isn't there".into(),
        why: e.to_string(),
    })
}

/// Whether the already evaluated pixel-effect list has work left.
fn has_active_effects(fx: &[BuiltinEffect]) -> bool {
    !fx.is_empty()
}

/// A clip's built-in effects evaluated at `time`, with its opacity split
/// out, read from its picture (M4 step 12a): each effect node's values
/// with its keys and modifier stack, the transform replaced by the
/// resolved one (own keyframes, link, focal point), switched-off and
/// identity effects dropped, `Opacity` removed — it is a compositing
/// weight, returned separately — and `SpeedRamp` removed, which acts on
/// time, not pixels. A clip whose picture can't be read is drawn from its
/// list ([`list_effects`]), which is said.
pub fn clip_effects(
    clip: &Clip,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
) -> (Vec<BuiltinEffect>, f32) {
    let from_picture = clip.picture_v2().and_then(|p| crate::resolve::resolve_effects_at(clip, &p.picture, time, fps, resolved, pin, 1.0));
    match from_picture {
        Ok(fx) => (fx, eval::evaluate_clip_opacity(clip, time, fps)),
        Err(e) => {
            tracing::warn!("clip {}: effects from its picture: {e}; its list stands in", clip.id.0);
            list_effects(clip, time, fps, resolved, pin)
        }
    }
}

impl Renderer {
    /// A pinned clip taking its surface's light (#2751): its gain this
    /// frame first in its chain, before its corner pin and the rest.
    fn take_light(&self, clip: ClipId, fx: &mut Vec<BuiltinEffect>) {
        if let Some(gain) = self.pin_light.get(&clip) {
            fx.insert(0, eval::light_gain_effect(*gain));
        }
    }
}

/// A title raster's box as the GPU stack takes it (#190).
fn sprite_box(r: &klipp_title::BoxRaster) -> klipp_effects::gpu::SpriteBox {
    klipp_effects::gpu::SpriteBox { x0: r.x0, y0: r.y0, w: r.w, h: r.h }
}

/// A posed title layer of several blurred by its blur entrance (#2370):
/// decoded to linear light, blurred by the effects `title_linear_at` gives
/// a one-layer title's frame and the GPU stack its layer, in the same
/// straight alpha, and encoded back to byte units.
fn soften_title_layer(stack: &mut [f32], width: u32, height: u32, blur: f32, focus: Option<(f32, klipp_core::title_motion::RampAxis, f32)>) {
    let mut fx = Vec::new();
    if blur > 0.5 {
        fx.push(BuiltinEffect::GaussianBlur { radius: blur });
    }
    if let Some((radius, axis, band)) = focus {
        fx.push(BuiltinEffect::FocusBlur { radius, vertical: axis == klipp_core::title_motion::RampAxis::Vertical, band });
    }
    if fx.is_empty() {
        return;
    }
    let luma = klipp_color::linear::luma_coefs(WorkingColorspace::default());
    let linear = klipp_effects::apply_effects_with_luma(&fx, srgb_stack_to_linear(stack), width, height, luma);
    let encode = |c: f32| {
        let c = c.max(0.0);
        255.0 * if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
    };
    for (out, p) in stack.chunks_exact_mut(4).zip(linear.chunks_exact(4)) {
        out.copy_from_slice(&[encode(p[0]), encode(p[1]), encode(p[2]), p[3].clamp(0.0, 1.0) * 255.0]);
    }
}

/// A title's stack in byte units (straight-alpha sRGB 0..255) as linear
/// light, alpha 0..1: what the GPU stack's sprite decode does, on the CPU.
fn srgb_stack_to_linear(stack: &[f32]) -> Vec<f32> {
    use rayon::prelude::*;
    let decode = |v: f32| {
        let c = (v / 255.0).max(0.0);
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    stack.par_chunks(4).flat_map_iter(|p| [decode(p[0]), decode(p[1]), decode(p[2]), (p[3] / 255.0).clamp(0.0, 1.0)]).collect()
}

/// The gain on a title's light for a white of `nits` in a delivery
/// mastered to `peak`, display light 1.0 being SDR white: never above the
/// peak, and a NaN brightness is the default's.
pub fn title_gain(nits: f32, peak: f32) -> f32 {
    let nits = if nits.is_finite() { nits } else { klipp_timeline::DEFAULT_TITLE_NITS };
    let nits = nits.clamp(*klipp_timeline::TITLE_NITS_RANGE.start(), *klipp_timeline::TITLE_NITS_RANGE.end());
    nits.min(peak.max(1.0)) / klipp_color::hdr::HDR_REFERENCE_WHITE_NITS
}

/// [`clip_effects`] from the clip's effect list rather than its picture:
/// the stand-in for a picture that can't be read, until the lists go
/// (step 12f).
pub fn list_effects(
    clip: &Clip,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
) -> (Vec<BuiltinEffect>, f32) {
    let t = eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let enabled = |i: usize| clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
    let mut fx: Vec<(usize, BuiltinEffect)> =
        clip.builtin_effects().iter().cloned().enumerate().filter(|(i, _)| enabled(*i)).collect();
    let mut pinned = false;
    for (i, effect) in fx.iter_mut() {
        // Inside its safe ranges, whatever wrote the list (#2995).
        klipp_effects::registry::guard_effect(effect);
        // Every keyframable parameter, from the one list the inspector
        // offers keyframes from. Opacity is the exception: it is the
        // clip's compositing weight (`eval::evaluate_clip_opacity`).
        if !matches!(effect, BuiltinEffect::Opacity { .. }) {
            effect.animate(&mut |name, v| {
                let kfs = clip.effect_keyframes().get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
                eval::eval_effect_with_modifiers(kfs, clip.param_modifiers(), *i, name, t, v, &ctx)
            });
        }
        frame_inputs(clip, effect, t, fps, pin, &mut pinned);
    }
    let fx = fx.into_iter().map(|(_, e)| e).collect();
    (place_and_finish(clip, fx, t, &ctx, resolved), eval::evaluate_clip_opacity(clip, time, fps))
}

/// What one frame gives an animated effect (R16): the film effects' seed,
/// a flicker's and particles' time, the deflicker gain, a pinned clip's first corner pin, a colour fade's
/// progress, a surface pin's corners.
pub(crate) fn frame_inputs(
    clip: &Clip,
    effect: &mut BuiltinEffect,
    t: f64,
    fps: f64,
    pin: Option<klipp_timeline::surface::Quad>,
    pinned: &mut bool,
) {
    surface_inputs(clip, effect, t, fps);
    match effect {
        // The animated film effects take a fresh pattern per frame: the
        // seed is the timeline frame number, so a still export and a scrub
        // land on the same grain.
        BuiltinEffect::FilmGrain { seed, .. } | BuiltinEffect::OldFilm { seed, .. } | BuiltinEffect::Vhs { seed, .. } => {
            *seed = (t * fps).round().max(0.0) as u32;
        }
        // A flicker jumps by the clip's time (#989).
        BuiltinEffect::Flicker { time, .. } => *time = t as f32,
        // A displacement moves by the clip's time (#2702), a warp too
        // (#2758).
        BuiltinEffect::Displace { time, .. } | BuiltinEffect::Warp { time, .. } => *time = t as f32,
        // A reflection ripples by the clip's time (#2707).
        BuiltinEffect::Reflection { time, .. } => *time = t as f32,
        // Particles move by the clip's time (#1141).
        BuiltinEffect::Particles { time, .. } => *time = t as f32,
        BuiltinEffect::Deflicker { gain, .. } => {
            // Its analysis gives the frame's gain (M4 step 9, at the
            // effect's node or `effects_analysed`); none here.
            *gain = 1.0;
        }
        // A pinned clip's first corner pin is on its surface.
        BuiltinEffect::CornerPin { tl, tr, br, bl } if pin.is_some() && !*pinned => {
            let q = pin.unwrap_or_default();
            (*tl, *tr, *br, *bl) = (q[0], q[1], q[2], q[3]);
            *pinned = true;
        }
        BuiltinEffect::ColorFade { duration_secs, mode, progress, .. } => {
            let dur = (*duration_secs as f64).max(0.0);
            // `t` is timeline seconds into the clip, so the fade out is
            // measured from the clip's timeline end. Measured from its
            // *source* length, a clip sped up 2× faded out an hour early in
            // the source and never on the timeline.
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

/// A surface pin's corners at `t`, timeline seconds into `clip`: the
/// clip's surface it names (by id, else by name) as the clip's picture
/// shows it then, its inset taken, in fractions of that picture, with
/// the picture's size. On a surface the clip lacks, or one not tracked,
/// they stay unset and the pin draws nothing.
pub(crate) fn surface_inputs(clip: &Clip, effect: &mut BuiltinEffect, t: f64, fps: f64) {
    let BuiltinEffect::SurfacePin { surface, inset, on_source, source } = effect else { return };
    (*on_source, *source) = ([[0.0; 2]; 4], [0; 2]);
    let named = clip.surfaces.iter().find(|s| s.id.to_string() == *surface).or_else(|| clip.surfaces.iter().find(|s| s.name == *surface));
    let Some(s) = named.filter(|s| !s.keys.is_empty()) else { return };
    let shown = s.corners_shown_at(clip.source_secs_at(t.max(0.0), fps));
    if let Some(q) = klipp_timeline::surface::pin_target(&shown, None, None, klipp_timeline::PinFit::Stretch, *inset) {
        (*on_source, *source) = (q, s.source_size);
    }
}

/// The ordering rules on an animated list (R1-R5 as M2 keeps them): the
/// effects that are not pixel effects go, the clip's transform is placed
/// once, and identities are dropped.
pub(crate) fn place_and_finish(
    clip: &Clip,
    fx: Vec<BuiltinEffect>,
    t: f64,
    ctx: &klipp_core::EvalContext,
    resolved: Option<ResolvedTransform>,
) -> Vec<BuiltinEffect> {
    // Opacity is a compositing weight and SpeedRamp a matter of time and
    // frame blending; neither is a pixel effect. (SpeedRamp through the
    // CPU chain was an 8-bit round trip that changed nothing else.)
    let mut fx: Vec<BuiltinEffect> = fx
        .into_iter()
        // Nor is a source-boundary node's (temporal noise reduction): it
        // reads the source's neighbours and runs at the head of the grade's
        // run, never among the graded picture's effects. The catalog says
        // which (`needs_source_neighbours`), SpeedRamp's frame blending
        // among them.
        .filter(|e| !matches!(e, BuiltinEffect::Opacity { .. }) && !klipp_effects::registry::needs_source_neighbours(e))
        .collect();

    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, ctx));
    let first = fx.iter().position(|e| matches!(e, BuiltinEffect::Transform { .. }));
    let three_d = tilt(clip, &fx, t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    // The one placement rule (`placement_cut`), as the picture route and
    // the inspector read it (M3 step 8).
    match crate::resolve::placement_cut(&fx, first, identity) {
        crate::resolve::PlacementCut::Fixed { at } => {
            // The clip's transform goes where its first Transform effect
            // is, once (#995): every Transform effect used to become it,
            // so two moved, scaled and turned the clip twice. Later ones
            // are dropped here, and `check` says so.
            fx[at] = transform_effect(xf, three_d);
            let mut i = 0;
            fx.retain(|e| {
                let keep = i <= at || !matches!(e, BuiltinEffect::Transform { .. });
                i += 1;
                keep
            });
        }
        // A shadow or a glow draws in the transparent margin the transform
        // leaves around a placed layer, so it has to run after the
        // transform that made the margin. Everything else — rounded
        // corners included — runs before it, with the picture, so it
        // scales with the layer.
        crate::resolve::PlacementCut::Auto { at, .. } => fx.insert(at, transform_effect(xf, three_d)),
        crate::resolve::PlacementCut::Identity => {}
    }
    fx.retain(|e| !klipp_effects::is_identity_effect(e));
    fx
}

/// A clip's 3D tilt and perspective at clip time `t`: the tilt tracks,
/// over whatever its own `Transform` effect says. A tilt with no
/// perspective given is seen in moderate perspective (1), since flat it
/// would only squash the picture rather than lean it.
pub(crate) fn tilt(clip: &Clip, fx: &[BuiltinEffect], t: f64) -> (f32, f32, f32) {
    let picture = clip.is_picture_truth().then(|| clip.picture_v2().ok()).flatten();
    let (rx, ry, persp) = picture.as_ref().map(|p| {
        let p = &p.picture.placement;
        (p.tilt_x.unwrap_or(0.0), p.tilt_y.unwrap_or(0.0), p.perspective.unwrap_or(0.0))
    }).unwrap_or_else(|| fx
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Transform { rotation_x, rotation_y, perspective, .. } => Some((*rotation_x, *rotation_y, *perspective)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 0.0)));
    let [tx, ty] = clip.tilt_lanes();
    let rx = klipp_core::keyframe::evaluate(&tx, t, rx);
    let ry = klipp_core::keyframe::evaluate(&ty, t, ry);
    // Keyframed, or set still in the inspector: either way a tilt.
    let tilted = rx != 0.0 || ry != 0.0 || clip.has_tilt_keyframes();
    (rx, ry, if tilted && persp <= 0.0 { 1.0 } else { persp })
}

/// A `Transform` effect with the resolved values and the clip's tilt.
pub(crate) fn transform_effect(xf: ResolvedTransform, (rx, ry, persp): (f32, f32, f32)) -> BuiltinEffect {
    let (px, py, sx, sy, ro, ax, ay) = xf;
    BuiltinEffect::Transform {
        position_x: px, position_y: py, scale_x: sx, scale_y: sy, rotation: ro,
        anchor_x: ax, anchor_y: ay, rotation_x: rx, rotation_y: ry, perspective: persp,
        focal_ref: None,
    }
}

/// Where a shuttered clip's effects split (#1923): what runs once on the
/// picture before it is placed at each sample, and what runs once on the
/// average after. At its Transform, or where none, at the first effect
/// that draws outside the picture (a shadow, a glow); the CPU's and the
/// stack's blur cut alike.
fn shutter_cut(fx: &[BuiltinEffect]) -> usize {
    fx.iter()
        .position(|e| matches!(e, BuiltinEffect::Transform { .. }))
        .unwrap_or_else(|| fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len()))
}

/// `sample`, straight RGBA, added to `acc` premultiplied: its colour
/// weighted by its alpha, so where a moving picture covers a pixel for
/// part of the shutter only what covers it counts (#2017).
fn add_premultiplied(acc: &mut [f32], sample: &[f32]) {
    for (a, s) in acc.chunks_exact_mut(4).zip(sample.chunks_exact(4)) {
        let alpha = s[3];
        a[0] += s[0] * alpha;
        a[1] += s[1] * alpha;
        a[2] += s[2] * alpha;
        a[3] += alpha;
    }
}

/// The mean of `n` samples [`add_premultiplied`] summed into `acc`, back
/// as straight RGBA: the colour of what covered each pixel, and how much
/// of the shutter it covered it. Averaged straight, a pixel a white card
/// covered for half the shutter came out half-grey at half coverage,
/// and a moving picture's blur trail composited darker than the picture
/// all along its path (#2017).
fn average_premultiplied(acc: &mut [f32], n: usize) {
    let inv = 1.0 / n.max(1) as f32;
    for a in acc.chunks_exact_mut(4) {
        let covered = a[3];
        if covered > klipp_effects::UNPREMULTIPLY_MIN_ALPHA {
            let unweight = 1.0 / covered;
            a[0] *= unweight;
            a[1] *= unweight;
            a[2] *= unweight;
        } else {
            a[..3].fill(0.0);
        }
        a[3] = covered * inv;
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
    c.has_changing_transform_keys()
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

fn title_gradient_fill(g:&klipp_timeline::TextGradient)->klipp_effects::gpu::TitleFill {
    klipp_effects::gpu::TitleFill::Gradient{geometry:match g.kind {
        klipp_core::fill_gradient::GradientKind::Linear{angle_deg}=>{let(s,c)=angle_deg.to_radians().sin_cos();[s,c,0.0,0.0]},
        klipp_core::fill_gradient::GradientKind::Radial{centre,radius}=>[centre[0],centre[1],radius.max(1e-6),1.0],
    },stops:g.stops().into_iter().map(|s|(s.position,s.color)).collect()}
}

fn title_fill(source:&klipp_title::GlyphSource,scale:f32)->Option<klipp_effects::gpu::TitleFill> {
    use klipp_effects::gpu::TitleFill;
    if let Some(m)=&source.material {
        return Some(TitleFill::Metal(title_metal(m,source.font_px,scale)));
    }
    if let Some(pattern)=&source.pattern {
        use klipp_timeline::TextPattern;
        let (colors,mode,spacing)=match *pattern {
            TextPattern::HorizontalStripes{color_a,color_b,stripe_width}=>([color_a,color_b],0,stripe_width),
            TextPattern::VerticalStripes{color_a,color_b,stripe_width}=>([color_a,color_b],1,stripe_width),
            TextPattern::Checkerboard{color_a,color_b,cell_size}=>([color_a,color_b],2,cell_size),
            TextPattern::DiagonalStripes{color_a,color_b,stripe_width}=>([color_a,color_b],3,stripe_width),
        };
        return Some(TitleFill::Pattern{colors,mode,spacing:spacing*scale});
    }
    source.gradient.as_ref().map(|g|if g.is_pair() {TitleFill::Pair{top:g.top_color,bottom:g.bottom_color}} else {TitleFill::Gradient{geometry:match g.kind {
        klipp_core::fill_gradient::GradientKind::Linear{angle_deg}=>{let(s,c)=angle_deg.to_radians().sin_cos();[s,c,0.0,0.0]},
        klipp_core::fill_gradient::GradientKind::Radial{centre,radius}=>[centre[0],centre[1],radius.max(1e-6),1.0],
    },stops:g.stops().into_iter().map(|s|(s.position,s.color)).collect()}})
}

/// A title's metal (#2706) as the GPU draws it: lights to directions,
/// colours to linear light, the bevel to frame pixels of a text
/// `font_px` tall.
pub(crate) fn title_metal(m:&klipp_core::title_material::TitleMaterial,font_px:f32,scale:f32)->klipp_effects::gpu::TitleMetal {
    let linear=|c:[u8;3]|c.map(|v|{let x=v as f32/255.0;if x<=0.04045 {x/12.92} else {((x+0.055)/1.055).powf(2.4)}});
    let light=|l:&klipp_core::title_material::MetalLight|(l.direction(),l.strength,linear(l.color));
    let off=([0.0,0.0,-1.0],0.0,[0.0;3]);
    let share=(m.environment.studio+m.environment.sky+m.environment.shot).max(1.0);
    klipp_effects::gpu::TitleMetal {
        tint:m.tint,roughness:m.roughness,edge:m.edge,highlights:m.highlights,glints:m.glints,
        anisotropy:m.anisotropy,anisotropy_radians:m.anisotropy_angle.to_radians(),
        bevel_px:(m.bevel*font_px).max(1.0),depth:m.depth,
        key:light(&m.key_light),rim:m.rim_light.as_ref().map_or(off,light),
        // The three are shares of what the metal sees: more shot is less
        // studio and sky, not more light on top of them.
        studio:m.environment.studio/share,sky:m.environment.sky/share,sky_tint:linear(m.environment.sky_tint),
        shot:m.environment.shot/share,shot_blur_px:m.environment.shot_blur*1080.0*scale,contour:m.contour,
        seed:(m.preset as u32) as f32*7.31+1.0,glint_cell:(font_px*0.55).max(12.0),
    }
}

// These loops alter glyph content/placement before title post-processing.
// Focus and whole-layer opacity loops need their own later ordering stages.
// Whether klipp-title's `frame_at` rasters a lone layer afresh rather than
// placing its resting render, given its loops' `pose` and its `text` now: a
// subset of its render-time overrides, the ones that take no other early path
// there. Anything not named here falls back to the CPU's own frame.
fn title_rasters_each_frame(title: &klipp_timeline::TitleData, text: &klipp_timeline::TextBox, pose: &klipp_timeline::LoopPose) -> bool {
    // A changed colour or text is rastered; a wave with a focus band is too,
    // when its glyphs could be posed one by one.
    let posed_glyphs = |o: &klipp_timeline::TitleObject| text.three_d.as_ref().is_none_or(|t| !t.enabled)
        && o.fx().motion.enter.is_none_or(|m| !m.kind.blurs()) && o.fx().motion.exit.is_none_or(|m| !m.kind.blurs())
        && !o.fx().animators.iter().any(|a| a.enabled && (a.props.blur != 0.0 || a.props.paints()));
    pose.color.is_some_and(|c| c != text.color) || pose.text.is_some()
        || title.objects.iter().any(|o| {
        let fx = o.fx();
        let keyed = match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.glitch_intensity > 0.0
                // Keyed tracking alone takes the tracking path.
                || (tb.has_keyframes() && tb.tracking_keyframes.is_empty())
                || tb.extrusion.as_ref().is_some_and(|e| !e.depth_keyframes.is_empty() || !e.angle_keyframes.is_empty() || !e.lighting_keyframes.is_empty()),
            _ => false,
        };
        o.visible && (keyed || (pose.wave.is_some() && posed_glyphs(o))
            || fx.styles_keyed())
    })
}

fn title_source_loops(loops:&[klipp_timeline::TitleLoop])->bool {
    loops.iter().all(|l|matches!(l,klipp_timeline::TitleLoop::ColorCycle{..}|klipp_timeline::TitleLoop::ColorBlend{..}|klipp_timeline::TitleLoop::Counter{..}|klipp_timeline::TitleLoop::Wave{..}))
}

// Separate post effects from the decorated source. Object rotation is later
// than post-processing in the CPU renderer and needs the scene transform path.
fn title_post_source(td: &klipp_timeline::TitleData) -> Option<(klipp_timeline::TitleData, f32, f32)> {
    let [obj] = td.objects.as_slice() else { return None };
    let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content else { return None };
    if !obj.visible || obj.opacity != 1.0 || !obj.opacity_keyframes.is_empty()
        || obj.transform.rotation != 0.0 || !obj.transform.rotation_keyframes.is_empty()
        || td.mixes_layers() || matches!(td.background,klipp_timeline::TitleBackground::FullFrame{..})
        || !obj.fx().edge_fade.is_none() || (obj.fx().motion.is_active() && tb.three_d.as_ref().is_some_and(|d|d.enabled))
        || !title_source_loops(&obj.fx().loops) || obj.fx().motion_blur.is_some()
        || (tb.chromatic_aberration_px <= 0.0 && tb.glitch_intensity <= 0.0)
    { return None; }
    let mut source = td.clone();
    let text = source.primary_text_mut()?;
    text.chromatic_aberration_px = 0.0;
    text.glitch_intensity = 0.0;
    Some((source, tb.chromatic_aberration_px, tb.glitch_intensity))
}


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

/// Whether a title's own motion blur runs on the stack. A title whose
/// drawing holds still is placed at each sample as the camera's shutter
/// places a picture; one whose drawing moves is drawn at each sample, which
/// the stack does when nothing runs between its drawing and the move: no
/// grade and no effect before it, as `motion_blurred` would apply to each.
fn title_blur_on_stack(c: &Clip, time: Timecode, fps: f64) -> bool {
    let ClipKind::Title(td) = &c.kind else { return false };
    // Drawn once, its grade and effects before the move run as any shuttered
    // layer's do on the stack.
    if title_raster_moves(td).is_none() {
        return true;
    }
    let (fx, _) = clip_effects(c, time, fps, None, None);
    c.grade_is_identity() && !has_active_effects(&fx[..shutter_cut(&fx)])
}

/// A title's motion blur, when on and not overridden by a transform
/// link.
///
/// A linked title still blurs — the sequence shutter samples it now,
/// with its target resolved at each one — but not by its own setting,
/// which samples the title's own animation and would need the link
/// resolved inside that path too. See #196, which is about what the
/// title's own blur samples.
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
mod missing_matte_tests {
    use super::*;
    use klipp_timeline::GradeAccess;

    /// A grade run refused for its alpha's matte (#898) is a failing
    /// decline naming the grade's alpha, so the viewer says so and an
    /// export refuses the frame; any other refusal is left to its own
    /// handling (a grade saved looping passes the picture through).
    #[test]
    fn a_missing_alpha_matte_fails_the_frame() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let mut g = klipp_nodes::NodeGraph::default();
        let source = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).unwrap().id;
        g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: g.output_node_id, to_port: 1 });
        clip.set_grade(g);
        let alpha = clip.grade_to_run().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::OutputAlpha).expect("the run takes the alpha").id;
        let d = missing_matte_decline(&clip, &klipp_nodes::executor::ExecutorError::MaskUnavailable(alpha)).expect("a decline");
        assert!(d.fails(), "the frame fails: {d:?}");
        assert_eq!((d.clip, d.node, d.kind), (Some(clip.id), alpha, klipp_nodes::NodeKind::OutputAlpha));
        assert!(missing_matte_decline(&clip, &klipp_nodes::executor::ExecutorError::CpuSourceRequired).is_none());
    }
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

/// A source frame in linear light at `width × height`, as the renderer
/// takes a frame the grade reads on the CPU: what a frame source hands a
/// temporal node as a neighbour, so the neighbour is the picture the frame
/// beside it would have been (#1988).
pub fn source_linear(f: SourceFrame, width: u32, height: u32) -> Arc<Vec<f32>> {
    to_linear(f, width, height).0
}

fn to_linear(f: SourceFrame, width: u32, height: u32) -> (Arc<Vec<f32>>, Option<klipp_color::hdr::HdrTransfer>) {
    match f.pixels {
        SourcePixels::Linear(p) => (fit(p, f.width, f.height, width, height), None),
        SourcePixels::SceneReferred { codes, transfer, source_to_working } => {
            let p = codes.to_scene_linear_rgba(transfer, source_to_working);
            (fit(Arc::new(p), f.width, f.height, width, height), Some(transfer))
        }
        SourcePixels::Gpu(g) => (fit(g.read(), f.width, f.height, width, height), None),
    }
}

/// The temporal noise reduction settings on `clip` at `time`, keyframes
/// applied, if it has any enabled and they would do something there.
pub fn temporal_denoise_of(clip: &Clip, time: Timecode, fps: f64) -> Option<(f32, f32)> {
    let enabled = |i: usize| clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
    let (i, mut tnr) = clip
        .builtin_effects()
        .iter()
        .enumerate()
        .find(|(i, e)| enabled(*i) && matches!(e, BuiltinEffect::TemporalDenoise { .. }))
        .map(|(i, e)| (i, e.clone()))?;
    let t = eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    tnr.animate(&mut |name, v| {
        let kfs = clip.effect_keyframes().get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
        eval::eval_effect_with_modifiers(kfs, clip.param_modifiers(), i, name, t, v, &ctx)
    });
    match tnr {
        BuiltinEffect::TemporalDenoise { strength, threshold } if strength > 0.0 => Some((strength, threshold)),
        _ => None,
    }
}

/// A still image's picture kept on the GPU (#2092): the file as it was
/// read (media, path, size and time stamp, log format, proxy), the
/// working space and frame size it was read and fitted at, and the
/// linear picture.
struct StillLayer {
    file: HeldFile,
    working: klipp_core::WorkingColorspace,
    size: (u32, u32),
    buffer: Arc<wgpu::Buffer>,
}

/// The most still layers kept: five 4K layers are 660 MB of GPU memory.
const STILL_LAYERS_KEPT: usize = 8;

/// A still generator's raster as drawn: its data, size and file stamp
/// (a still frame's), and the pixels.
type StillGenerator = (klipp_core::GeneratorData, u32, u32, Option<(std::time::SystemTime, u64)>, Arc<Vec<u32>>);

/// Which part of a clip's plan a run takes (#1923, #2090): all of it, or
/// for a clip smeared across the shutter, the part before its move or
/// the part after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanPart {
    Whole,
    BeforeMove,
    AfterMove,
}

/// Whether generator `g` draws the same picture whatever the time: from
/// its data alone, or a still frame from its file, kept until the file
/// changes ([`file_stamp`], #2068). Not a living gradient, grain, a
/// countdown or a timer.
fn is_still_generator(g: &klipp_core::GeneratorData) -> bool {
    use klipp_core::GeneratorData as G;
    match g {
        G::SolidColor { .. } | G::Placeholder { .. } | G::Gradient { .. } | G::ColorBars | G::ColorBars100 | G::ColorBarsSmpte | G::Slate { .. } | G::Polygon { .. } | G::Star { .. } | G::StillFrame { .. } => true,
        G::LivingGradient { .. } | G::Grain { .. } | G::Countdown | G::Timer { .. } => false,
    }
}

/// A still frame's file as it stands: its modified time and size, `None`
/// for a file that isn't there (drawn magenta until it is). Any other
/// generator has no file, and `None`.
fn file_stamp(g: &klipp_core::GeneratorData) -> Option<(std::time::SystemTime, u64)> {
    let klipp_core::GeneratorData::StillFrame { png_path, .. } = g else { return None };
    let meta = std::fs::metadata(png_path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Whether `clip` frame-blends at `time`: a `SpeedRamp` in blend mode 1
/// or 2 (by optical flow), not frozen, either way it plays, where its file gives fewer frames
/// than the timeline at `fps` shows. Slowed, or at real time from a file
/// of a lower frame rate (`media_fps`), conformed by it (#2479). Where each
/// frame falls on one of the file's, nothing is mixed: at real time from a
/// file of the timeline's rate, a blending clip left the GPU stack for the
/// CPU for nothing. A file whose rate is not known is taken to be at the
/// timeline's.
pub fn frame_blends(clip: &Clip, time: Timecode, media_fps: Option<f64>, fps: f64) -> bool {
    let blend = clip.has_frame_blend_entry();
    // Reversed too (#2772): the frames either side of the source time
    // are the same pair whichever way the clip plays.
    let speed = clip.speed_at(eval_clip_time(clip, time)).abs();
    let rate = media_fps.filter(|r| *r > 0.0).unwrap_or(fps);
    blend && clip.hold.is_none() && speed > 1e-6 && speed * rate < fps * (1.0 - 1e-6)
}

/// Whether `clip`'s picture holds a frei0r plug-in node, on or off (M4
/// steps 5 and 12a): the display stack has no frei0r arm, so it declines
/// such a clip. A picture that can't be read is taken to hold one, so the
/// stack declines it rather than leave a plug-in out.
fn has_frei0r_node(clip: &Clip) -> bool {
    clip.picture_effects().map_or(true, |effects| effects.iter().any(|e| matches!(e.effect, BuiltinEffect::Frei0r { .. })))
}

/// `clip`'s enabled OFX entries as a graph of their own (M4 step 3):
/// Source, a node for each in list order, Output. A node's id is its
/// clip's, its plug-in's and which of that plug-in's entries it is, so
/// its instance's slot is the same from frame to frame and follows its
/// plug-in when the list is reordered (an OFX entry keeps no id of its
/// own). `None` when none is on.
fn ofx_graph(clip: &Clip) -> Option<klipp_nodes::NodeGraph> {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
    // The OFX entries as the clip's picture holds them (M4 step 12a), not
    // its v1 list.
    let entries: Vec<klipp_plugin::AppliedEffect> = match clip.picture_ofx() {
        Ok(ofx) => ofx.into_iter().map(|o| o.entry).collect(),
        Err(e) => {
            tracing::error!("clip {}: its OFX plug-ins aren't read: {e}", clip.id.0);
            return None;
        }
    };
    if !entries.iter().any(|e| e.enabled) {
        return None;
    }
    // Apart from any id the clip's own graphs use: an OFX node's mark in
    // the high bits, the rest from what it is.
    let id = |k: u128| NodeId::from_u128(clip.id.0.as_u128() ^ (0x4f46_5800_u128 << 96) ^ k);
    // FNV-1a over the plug-in's name, a hash the same in every process.
    let name = |s: &str| s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let node = |id: NodeId, kind: NodeKind, params: NodeParams| Node { id, kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let mut nodes = vec![node(id(0), NodeKind::Source, NodeParams::default())];
    // Counted over every entry, on or off: switching one off moves no
    // other's slot.
    let mut seen: HashMap<&str, u32> = HashMap::new();
    // An entry's own id, when it has one (#1500): its node follows it
    // through any reorder, past a copy of its own plug-in too. Taken once:
    // a second entry holding the same id (a hand-edited file) runs under
    // the id derived from its place, never on the first's instance.
    let mut owned: std::collections::HashSet<u128> = std::collections::HashSet::new();
    for e in &entries {
        let nth = seen.entry(e.plugin_name.as_str()).or_insert(0);
        let derived = (u128::from(name(&e.plugin_name)) << 32) | u128::from(*nth) | 1 << 31;
        *nth += 1;
        let own = e.id.as_deref().map(|s| s.replace('-', "")).filter(|h| h.len() == 32).and_then(|h| u128::from_str_radix(&h, 16).ok());
        let k = match own {
            Some(u) if owned.insert(u) => u,
            _ => derived,
        };
        if !e.enabled {
            continue;
        }
        let params = NodeParams { ofx: Some(klipp_nodes::plugin::OfxNodeParams { plugin: e.plugin_name.clone(), values: e.param_values.clone(), filter: matches!(e.context, klipp_plugin::OfxCategory::Filter) }), ..Default::default() };
        nodes.push(node(id(k), NodeKind::Ofx, params));
    }
    nodes.push(node(id(u64::MAX as u128), NodeKind::Output, NodeParams::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    let output_node_id = nodes.last().map(|n| n.id).unwrap_or_default();
    Some(NodeGraph { output_node_id, nodes, edges, ..Default::default() })
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
    /// Effects held until the layer is finished (`finish_placed`).
    held: Option<Box<HeldEffects>>,
    /// A reflection's row and mode (#2707): the rows from it down blend
    /// by that mode instead.
    split: Option<(u32, BlendMode)>,
}

/// A clip's reflection's split at `time` (#2707), when it has one: the
/// row of a frame `height` tall from which its layer blends by the
/// reflection's mode, its gap taken to the render's size by `px_scale`.
fn reflection_split(c: &Clip, time: Timecode, fps: f64, height: u32, px_scale: f32) -> Option<(u32, BlendMode)> {
    if !c.enabled_effects().any(|e| matches!(e, BuiltinEffect::Reflection { .. })) {
        return None;
    }
    clip_effects(c, time, fps, None, None).0.into_iter().rev().find_map(|e| e.at_render_scale(px_scale).split_blend(height))
}

/// A layer's effects, and a title's lead-in ones, held for its finish.
struct HeldEffects {
    fx: Vec<BuiltinEffect>,
    lead: Vec<BuiltinEffect>,
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
mod generator_time_tests {
    use super::*;

    fn t(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 25)
    }

    /// A clip from 2 s to 6 s of `kind`.
    fn clip(kind: ClipKind) -> Clip {
        Clip { kind, ..Clip::gap(t(2.0), t(6.0)) }
    }

    /// On a transition's incoming side, before its start, a generator is
    /// drawn at its own (negative) time: a drifting gradient is a picture
    /// there it isn't at its start, and grain a frame of its own rather
    /// than its first. Past its end it runs on too (#1918).
    #[test]
    fn a_generator_runs_on_before_its_start_and_past_its_end() {
        let drifting = klipp_core::GeneratorData::LivingGradient { a: [200, 40, 40, 255], b: [40, 40, 200, 255], angle_deg: 0.0, drift_deg_per_sec: 90.0, glow: 0.0, grain: klipp_core::DEFAULT_GENERATOR_GRAIN };
        let c = clip(ClipKind::Generator(drifting.clone()));
        let at = drawn_time(&c, t(1.6));
        assert!((at + 0.4).abs() < 1e-9, "{at}");
        assert_eq!(eval_clip_time(&c, t(1.6)), 0.0, "what it was held at before");
        assert!((drawn_time(&c, t(6.4)) - 4.4).abs() < 1e-9, "past its end: on");
        assert!((drawn_time(&c, t(3.0)) - 1.0).abs() < 1e-9, "inside the clip: as before");
        let render = |g: &klipp_core::GeneratorData, s: f64| klipp_core::generator::render_generator(g, 32, 18, s, 4.0);
        assert_ne!(render(&drifting, at), render(&drifting, 0.0), "its motion runs on before its start");
        let grain = klipp_core::GeneratorData::Grain { color: [90, 90, 90, 255], amount: 1.0, size_px: 2.0, speed: 24.0 };
        assert_ne!(render(&grain, -0.3), render(&grain, 0.0), "grain before the start is no copy of its first frame");
        assert_ne!(render(&grain, -0.3), render(&grain, -0.6), "and changes as it goes");
    }

    /// A generator showing its own time, and a title, hold their first
    /// frame before the start and their last past the end.
    #[test]
    fn a_count_and_a_title_hold_their_ends() {
        let title = ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hi".into(), ..Default::default() }));
        for kind in [ClipKind::Generator(klipp_core::GeneratorData::Countdown), title] {
            let c = clip(kind);
            assert_eq!(drawn_time(&c, t(1.7)), 0.0, "incoming: its first frame");
            assert_eq!(drawn_time(&c, t(6.4)), 4.0, "outgoing: its last");
            assert!((drawn_time(&c, t(3.0)) - 1.0).abs() < 1e-9);
        }
    }
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
        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Gap,
            ..Clip::gap(tc(0), tc(48))
        };
        klipp_timeline::test_clip::TestClip::of(clip).effects(effects).build()
    }

    /// #1078: a track's Opacity weighs the clip's resolved opacity, still
    /// value and keys alike, rather than replacing it; one of the clip's
    /// own still replaces an earlier one of its own.
    #[test]
    fn a_tracks_opacity_multiplies_the_clips() {
        use klipp_timeline::{Track, TrackKind};
        let baked = |own: Vec<BuiltinEffect>, keys: Vec<klipp_core::Keyframe>, track_fx: Vec<BuiltinEffect>| {
            let mut c = clip_with(own);
            c.kind = klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() };
            let mut b = klipp_timeline::test_clip::TestClip::of(c);
            for k in &keys {
                b = b.lane("opacity", k.time_secs, k.value);
            }
            let c = b.build();
            let mut t = Track::new("V1", TrackKind::Video);
            t.effects.set_video_effect_list(track_fx, vec![]);
            t.clips.push(c);
            let mut seq = klipp_timeline::Sequence::new_default();
            seq.tracks = vec![t];
            seq.baked_for_render().tracks[0].clips[0].clone()
        };
        let opacity = |c: &Clip, frame: i64| clip_effects(c, tc(frame), 24.0, None, None).1;
        let half = || vec![BuiltinEffect::Opacity { value: 0.5 }];
        assert!((opacity(&baked(half(), vec![], half()), 0) - 0.25).abs() < 1e-6, "0.5 on the clip and 0.5 on the track");
        assert!((opacity(&baked(vec![], vec![], half()), 0) - 0.5).abs() < 1e-6, "the track's alone");
        let mut keys = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.2);
        klipp_core::keyframe::upsert_keyframe(&mut keys, 1.0, 0.6);
        assert!((opacity(&baked(half(), keys.clone(), half()), 24) - 0.3).abs() < 1e-6, "the clip's keys still count");
        assert!((opacity(&baked(vec![], keys, half()), 24) - 0.3).abs() < 1e-6, "keys with no Opacity effect of the clip's");
        // Two of the clip's own multiply (#1386), then the track's.
        let own = vec![BuiltinEffect::Opacity { value: 0.2 }, BuiltinEffect::Opacity { value: 0.8 }];
        assert!((opacity(&baked(own, vec![], half()), 0) - 0.08).abs() < 1e-6, "0.2 × 0.8 × 0.5");
        // A track Opacity above 1 can't brighten the clip.
        assert!((opacity(&baked(half(), vec![], vec![BuiltinEffect::Opacity { value: 1.5 }]), 0) - 0.5).abs() < 1e-6);
    }

    /// #995: the clip's transform is applied once, where its first
    /// Transform effect is. Every Transform effect used to become it, so a
    /// clip with two was moved, scaled and turned twice.
    #[test]
    fn two_transform_effects_place_the_clip_once() {
        let t = |scale: f32| BuiltinEffect::Transform {
            position_x: 40.0, position_y: 0.0, scale_x: scale, scale_y: scale,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        };
        let clip = clip_with(vec![t(0.5), BuiltinEffect::Sharpen { amount: 0.5 }, t(0.5)]);
        let (fx, _) = clip_effects(&clip, tc(0), 24.0, None, None);
        let transforms: Vec<usize> = fx.iter().enumerate().filter(|(_, e)| matches!(e, BuiltinEffect::Transform { .. })).map(|(i, _)| i).collect();
        assert_eq!(transforms, vec![0], "one transform, at the first one's place: {fx:?}");
        assert!(matches!(fx[1], BuiltinEffect::Sharpen { .. }), "{fx:?}");
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
        let (fx, _) = clip_effects(&clip, tc(0), 24.0, None, None);
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
                temporal_denoise_of(&clip, tc(0), 24.0),
                Some((0.8, 0.02)),
                "not found at this position",
            );
        }
    }

    #[test]
    fn a_disabled_or_zero_strength_one_is_not_applied() {
        let mut clip = clip_with(vec![BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.02 }]);
        let id = klipp_timeline::test_clip::effect_id(&clip, 0);
        clip.set_effect_enabled(id, false);
        assert_eq!(temporal_denoise_of(&clip, tc(0), 24.0), None, "a disabled effect ran");

        let off = clip_with(vec![BuiltinEffect::TemporalDenoise { strength: 0.0, threshold: 0.02 }]);
        assert_eq!(temporal_denoise_of(&off, tc(0), 24.0), None, "strength 0 still took the CPU route");

        let none = clip_with(vec![BuiltinEffect::Sharpen { amount: 0.5 }]);
        assert_eq!(temporal_denoise_of(&none, tc(0), 24.0), None);
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

#[cfg(test)]
mod active_effect_gate_tests {
    use super::*;

    #[test]
    fn active_effect_gate_uses_the_evaluated_list() {
        let at = Timecode::from_frames(0, 24);
        for (name, entry, enabled, expected) in [
            ("empty", None, true, false),
            ("disabled", Some(BuiltinEffect::GaussianBlur { radius: 2.0 }), false, false),
            ("identity", Some(BuiltinEffect::GaussianBlur { radius: 0.0 }), true, false),
            ("blur", Some(BuiltinEffect::GaussianBlur { radius: 2.0 }), true, true),
            ("opacity is composite weight", Some(BuiltinEffect::Opacity { value: 0.5 }), true, false),
            ("temporal runs earlier", Some(BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.1 }), true, false),
            ("frei0r", Some(BuiltinEffect::Frei0r { type_id: "test".into(), params: vec![] }), true, true),
        ] {
            let mut b = klipp_timeline::test_clip::TestClip::of(Clip::gap(at, Timecode::from_frames(48, 24))).effects(entry);
            if !enabled {
                b = b.off();
            }
            let clip = b.build();
            let (effects, _) = clip_effects(&clip, at, 24.0, None, None);
            assert_eq!(has_active_effects(&effects), expected, "{name}");
        }
        // An otherwise empty clip can still acquire a pixel transform from
        // its resolved parent/camera. The query must see that evaluated list.
        let clip = Clip::gap(at, Timecode::from_frames(48, 24));
        let (effects, _) = clip_effects(&clip, at, 24.0, Some((0.2,0.0,1.0,1.0,0.0,0.5,0.5)), None);
        assert!(has_active_effects(&effects));
    }
}

#[cfg(test)]
mod clip_sources_tests {
    use super::*;
    use klipp_nodes::frame_context::{Neighbour, SourceProvider};

    /// A gradient's frames at 64 × 36, encoded as a plug-in takes them.
    struct Large(Vec<f32>);

    impl FrameSource for Large {
        fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
            None
        }
        fn temporal_frames(&mut self, _: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
            let bytes = klipp_color::ofx::encode(&self.0, WorkingColorspace::Rec709Linear);
            Some(Box::new(move |_| Some((bytes.clone(), 64, 36))))
        }
    }

    /// #2013: a neighbour read at another size than the frame drawn (the
    /// viewer below full size, a proxy) is fitted to the frame's size, as
    /// the linear ones are, so a plug-in still gets it. The neighbour is
    /// resized after its 8-bit encoding and the frame beside it in linear
    /// light, so the two differ by that much: 1 level measured on a
    /// gradient, and held there. Control: the frame unfitted is a size
    /// the plug-in's frame isn't.
    #[test]
    fn a_neighbour_of_another_size_is_fitted_to_the_frame() {
        let full: Vec<f32> = (0..64 * 36).flat_map(|i| {
            let (x, y) = ((i % 64) as f32 / 63.0, (i / 64) as f32 / 35.0);
            [0.02 + 0.8 * x, 0.05 + 0.6 * y, 0.3 * (1.0 - x), 1.0]
        }).collect();
        let clip = Clip::media(klipp_core::MediaId::new(), Timecode::from_frames(0, 24), Timecode::from_frames(24, 24), Timecode::from_frames(0, 24), Timecode::from_frames(24, 24));
        let mut src = Large(full.clone());
        let mut sources = ClipSources::new(&mut src, &clip, WorkingColorspace::Rec709Linear).with_current(0.0, None, 32, 18);
        let Neighbour::Frame(f) = sources.encoded(0.0) else { panic!("a neighbour") };
        assert_eq!((f.width, f.height), (32, 18));
        let beside = klipp_color::ofx::encode(&fit_linear(full, 64, 36, 32, 18).unwrap(), WorkingColorspace::Rec709Linear);
        let off = f.rgba8.iter().zip(&beside).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap();
        eprintln!("a fitted neighbour is {off} levels off the fitted frame");
        assert!(off <= 1, "{off} levels");
        let mut unfitted = ClipSources::new(&mut src, &clip, WorkingColorspace::Rec709Linear);
        let Neighbour::Frame(f) = unfitted.encoded(0.0) else { panic!("a neighbour") };
        assert_eq!((f.width, f.height), (64, 36), "the control: with no frame to fit to, its own size");
    }

    /// A frame source whose encoded reader fills with 200 and linear one
    /// with 0.25, each answering the reader time it was asked; it counts
    /// the readers opened.
    #[derive(Default)]
    struct Readers {
        opened: Vec<&'static str>,
    }

    impl FrameSource for Readers {
        fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
            None
        }
        fn temporal_frames(&mut self, _: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
            self.opened.push("encoded");
            Some(Box::new(|t| (t >= 0.0).then(|| (vec![200, (t * 10.0) as u8, 0, 255], 1, 1))))
        }
        fn temporal_frames_linear(&mut self, _: &Clip, _: WorkingColorspace) -> Option<LinearFrameProviderFn> {
            self.opened.push("linear");
            Some(Box::new(|t| (t >= 0.0).then(|| (Arc::new(vec![0.25, t as f32, 0.0, 1.0]), 1, 1))))
        }
    }

    fn clip() -> Clip {
        let tc = |s| Timecode::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(1.0), tc(2.0), tc(3.0))
    }

    /// A clip with no media (a title, a generator) has no source: an empty
    /// range, so a node asking for another frame falls back.
    #[test]
    fn a_clip_without_media_has_an_empty_range() {
        let tc = |s| Timecode::from_secs_f64(s, 24);
        let c = Clip::gap(tc(0.0), tc(1.0));
        let mut src = Readers::default();
        assert_eq!(ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear).bounds(), (0.0, 0.0));
    }

    /// Each contract from its own reader, asked from the source in-point,
    /// each frame stamped with the source time; the bounds are the clip's
    /// source range, and each reader is opened once.
    #[test]
    fn each_contract_comes_from_its_own_reader() {
        let c = clip();
        let mut src = Readers::default();
        let mut s = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
        assert_eq!(s.bounds(), (0.0, f64::INFINITY), "a source that doesn't know its media's range: only its readers can say");
        match s.encoded(2.5) {
            Neighbour::Frame(f) => assert_eq!((f.rgba8[0], f.rgba8[1], f.source_secs), (200, 5, 2.5)),
            other => panic!("{other:?}"),
        }
        match s.linear(2.5) {
            Neighbour::Frame(f) => assert_eq!((f.rgba[0], f.rgba[1], f.source_secs), (0.25, 0.5, 2.5)),
            other => panic!("{other:?}"),
        }
        let _ = s.linear(2.75);
        let _ = s.encoded(2.75);
        drop(s);
        assert_eq!(src.opened, ["encoded", "linear"]);
    }

    /// The bounds are the media's range, the clip's handles included, as
    /// the source knows it: never the clip's trim.
    #[test]
    fn the_bounds_are_the_medias_range() {
        struct Known(Readers);
        impl FrameSource for Known {
            fn frame(&mut self, c: &Clip, t: f64, w: u32, h: u32) -> Option<SourceFrame> {
                self.0.frame(c, t, w, h)
            }
            fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
                Some((0.0, 10.0))
            }
        }
        let c = clip();
        let mut src = Known(Readers::default());
        assert_eq!(ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear).bounds(), (0.0, 10.0), "not the trim's 2..3");
    }

    /// A source without a reader, or a reader without the frame, is
    /// unavailable: never the current frame instead.
    #[test]
    fn no_reader_or_no_frame_is_unavailable() {
        struct Nothing;
        impl FrameSource for Nothing {
            fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
                None
            }
        }
        let c = clip();
        let mut none = Nothing;
        let mut s = ClipSources::new(&mut none, &c, WorkingColorspace::Rec709Linear);
        let unavailable = klipp_nodes::frame_context::Missing::Unavailable;
        assert_eq!(s.encoded(2.5), Neighbour::Missing(unavailable));
        assert_eq!(s.linear(2.5), Neighbour::Missing(unavailable));
        let mut src = Readers::default();
        let mut s = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
        assert_eq!(s.linear(1.5), Neighbour::Missing(unavailable), "before the media: the reader has none");
    }
}

#[cfg(test)]
mod ofx_graph_tests {
    use super::ofx_graph;
    use klipp_nodes::NodeKind;
    use klipp_plugin::AppliedEffect;

    fn entry(name: &str, enabled: bool) -> AppliedEffect {
        AppliedEffect { plugin_name: name.into(), enabled, ..Default::default() }
    }

    /// Each OFX node's id by its plug-in, in list order.
    fn ids(clip: &klipp_timeline::Clip) -> Vec<(String, klipp_nodes::NodeId)> {
        let g = ofx_graph(clip).expect("a plug-in on");
        g.nodes.iter().filter(|n| n.kind == NodeKind::Ofx).map(|n| (n.params.ofx.as_ref().unwrap().plugin.clone(), n.id)).collect()
    }

    /// Reordering a clip's plug-ins keeps each one's node, so its instance
    /// (its slot, by node) follows it; a copy switched off moves no other.
    #[test]
    fn an_ofx_nodes_id_follows_its_plugin() {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        clip.set_ofx_effects(vec![entry("A", true), entry("B", true)]);
        let before = ids(&clip);
        clip.set_ofx_effects(vec![entry("B", true), entry("A", true)]);
        let after = ids(&clip);
        assert_eq!(after.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["B", "A"], "drawn in the new order");
        for (p, id) in &before {
            assert_eq!(after.iter().find(|(q, _)| q == p).map(|(_, i)| i), Some(id), "{p}'s node moved with it");
        }
        // Two copies of one plug-in: switching the first off leaves the
        // second its node.
        clip.set_ofx_effects(vec![entry("A", true), entry("A", true)]);
        let second = ids(&clip)[1].1;
        clip.set_ofx_effects(vec![entry("A", false), entry("A", true)]);
        assert_eq!(ids(&clip), [("A".to_string(), second)], "the second copy keeps its node");
        // Another clip's nodes are its own.
        let other = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let mut other = other;
        other.set_ofx_effects(vec![entry("A", true), entry("A", true)]);
        assert_ne!(ids(&other)[1].1, second, "another clip, another slot");
    }

    /// #1500: two copies of one plug-in, told apart by a value, swapped
    /// through the app's edit path: each keeps its node, so its instance.
    #[test]
    fn two_copies_of_one_plugin_keep_their_nodes_when_swapped() {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let copy = |tag: i32| AppliedEffect { param_values: [("tag".to_string(), klipp_core::OfxValue::Int(tag))].into(), ..entry("A", true) };
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Add(&copy(1)), klipp_timeline::OfxEdit::Add(&copy(2))]).unwrap();
        let by_tag = |clip: &klipp_timeline::Clip| -> Vec<(i32, klipp_nodes::NodeId)> {
            let g = ofx_graph(clip).expect("plug-ins on");
            g.nodes.iter().filter(|n| n.kind == NodeKind::Ofx).map(|n| match n.params.ofx.as_ref().unwrap().values.get("tag") { Some(klipp_core::OfxValue::Int(t)) => (*t, n.id), other => panic!("{other:?}") }).collect()
        };
        let before = by_tag(&clip);
        assert_eq!(before.iter().map(|(t, _)| *t).collect::<Vec<_>>(), [1, 2]);
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Move { from: 0, to: 1 }]).unwrap();
        let after = by_tag(&clip);
        assert_eq!(after.iter().map(|(t, _)| *t).collect::<Vec<_>>(), [2, 1], "drawn in the new order");
        for (t, id) in &before {
            assert_eq!(after.iter().find(|(u, _)| u == t).map(|(_, i)| i), Some(id), "copy {t} kept its node");
        }
        // A value set, the other copy switched off and on, one updated
        // whole: each keeps its node through all of them.
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::SetParam(1, "gain", &klipp_core::OfxValue::Double(0.5))]).unwrap();
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::SetEnabled(0, false)]).unwrap();
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::SetEnabled(0, true)]).unwrap();
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Update(0, &copy(2))]).unwrap();
        let edited = by_tag(&clip);
        for (t, id) in &before {
            assert_eq!(edited.iter().find(|(u, _)| u == t).map(|(_, i)| i), Some(id), "copy {t} kept its node through the edits");
        }
    }

    /// #1500: a pasted copy (an entry added with its source's id) gets an
    /// id of its own; and if two entries on a clip did hold one id, the
    /// second runs on a node of its own, never on the first's instance.
    #[test]
    fn no_two_entries_share_a_node() {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Add(&entry("A", true))]).unwrap();
        let read = |clip: &klipp_timeline::Clip| clip.picture_ofx().unwrap().into_iter().map(|o| o.entry).collect::<Vec<_>>();
        let first = read(&clip)[0].clone();
        assert!(first.id.is_some(), "an added entry has an id");
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Add(&first)]).unwrap();
        assert_ne!(read(&clip)[1].id, first.id, "a pasted copy gets its own");
        // An entry updated whole with the first's id (as a hand-edited
        // file could hold): two entries, one id.
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Update(1, &first)]).unwrap();
        assert_eq!(read(&clip)[1].id, first.id);
        let g = ids(&clip);
        assert_eq!(g.len(), 2);
        assert_ne!(g[0].1, g[1].1, "one id, two nodes");
    }

    /// #1500: a split clip's halves carry the same entry, id and all, and
    /// run it on two different nodes: each half its own instance.
    #[test]
    fn a_split_clips_halves_run_their_plugin_on_their_own_nodes() {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        clip.apply_ofx_edits(&[klipp_timeline::OfxEdit::Add(&entry("A", true))]).unwrap();
        let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
        let id = clip.id;
        track.clips.push(clip);
        let (left, right) = track.split_at(id, tc(1.0)).expect("split");
        let id_of = |c: &klipp_timeline::Clip| c.picture_ofx().unwrap()[0].entry.id.clone();
        assert_eq!(id_of(&left), id_of(&right), "the same entry on both halves");
        assert_ne!(ids(&left)[0].1, ids(&right)[0].1, "two nodes, two instances");
    }
}

#[cfg(test)]
mod single_path_side_pictures {
    use super::*;
    use klipp_graph_format::lifted::types as lt;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_lift::ops::GraphOp;

    struct NoFrames;
    impl FrameSource for NoFrames {
        fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
            None
        }
    }

    fn effect_node(e: &BuiltinEffect) -> klipp_graph_format::Node {
        let payload = serde_json::to_value(e).unwrap();
        serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
            "legacy": { "payload": payload },
        }))
        .unwrap()
    }

    /// A clip whose Merge reads `side` from beside its chain.
    fn merged(side: BuiltinEffect) -> Clip {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        let picture = c.picture_v2().unwrap().picture.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
        let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
        let (m, s) = (effect_node(&BuiltinEffect::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 }), effect_node(&side));
        c.apply_picture_ops(&[
            GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(s.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(s.id), to_port: "in".into() },
            GraphOp::Connect { from: at(s.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
        ])
        .unwrap();
        c
    }

    /// The single-clip export path grades a clip with the executor's side
    /// pictures cleared, not refilled: it never takes a clip that reads one,
    /// which goes to the stack, where its pictures are read (#1746).
    #[test]
    fn the_single_path_leaves_a_clip_reading_a_side_picture_to_the_stack() {
        let target = klipp_effects::gpu::Yuv420Target { tone_map_op: 0, odt_tag: 0, scene: None, gamut_op: 0, hdr_op: 0, peak_nits: 100.0, luma_coefs: [0.2126, 0.7152, 0.0722], semi_planar: true, bits: 8, sample_shift: 0 };
        for side in [BuiltinEffect::MediaSource { media: klipp_core::MediaId::new().0.to_string(), offset: 0.0 }, BuiltinEffect::ClipBelow {}] {
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: 64, height: 36 };
            let mut track = Track::new("V1", TrackKind::Video);
            track.clips = vec![merged(side.clone())];
            seq.tracks = vec![track];
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            let single = r.render_yuv420_single(&seq, klipp_core::RationalTime::from_secs_f64(1.0, 24), 64, 36, &mut NoFrames, RenderOptions::default(), target);
            assert!(matches!(single, Single::ForTheStack), "{side:?}");
        }
    }
}

fn generator_feature(g: &klipp_core::GeneratorData) -> &'static str {
    use klipp_core::GeneratorData::*;
    match g {
        Grain { .. } => "generator.grain", Countdown => "generator.countdown", Timer { .. } => "generator.timer", StillFrame { .. } => "generator.still_frame",
        LivingGradient { .. } => "generator.living_gradient", SolidColor { .. } => "generator.solid", Placeholder { .. } => "generator.placeholder", Gradient { .. } => "generator.gradient", ColorBars => "generator.color_bars", ColorBars100 => "generator.color_bars_100", ColorBarsSmpte => "generator.color_bars_smpte", Slate { .. } => "generator.slate", Polygon { .. } => "generator.polygon", Star { .. } => "generator.star",
    }
}
