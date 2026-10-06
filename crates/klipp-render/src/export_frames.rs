//! How an export makes each frame (#1156): one decision, called by
//! [`crate::export_job::ExportJob`] and by anything that must render exactly
//! what an export would (the real-project pixel check), so the two can't
//! drift apart.
//!
//! [`export_renderer`] is the renderer an export draws with, and
//! [`ExportFrameMaker`] makes each frame.
//!
//! A frame is converted to the encoder's planes on the GPU when the encoder
//! takes a GPU-made frame for its delivery (`gpu_yuv420_params`) and the
//! picture fills the output; otherwise it is rendered as linear light and
//! padded to the output, for the encoder's own display transform. A miss
//! that comes after the work (a grade back on the CPU, a stack failing part
//! way) turns the GPU way off for the rest of the export, so no later frame
//! is rendered twice; a miss at the gate is the frame's alone.

use klipp_compose::{RenderOptions, Renderer};
use klipp_core::{ExportSettings, Timecode};
use klipp_effects::gpu::{GpuFrameLight, Yuv420Target};
use klipp_timeline::Sequence;
use tracing::debug;

use crate::encoder::Encoder;
use crate::error::RenderError;
use crate::export_job::{fit_to_target, pad_linear};
use crate::frame_source::ExportFrameSource;
use crate::snapshot::SequenceSnapshot;

/// The renderer an export draws with: the viewer's, set up for a delivery.
/// `label` names the export in the log.
pub fn export_renderer(snapshot: &SequenceSnapshot, label: &str) -> Renderer {
    let timeline = &snapshot.video.sequence;
    // OFX plugin discovery is slow; only when a clip uses a plugin.
    let mut ofx = klipp_plugin::OfxHost::new();
    if timeline.tracks.iter().flat_map(|t| &t.clips).any(|c| c.has_enabled_plugins()) {
        ofx.discover_plugins();
    }
    let mut renderer = Renderer::new(ofx, klipp_title::TitleRenderer::new());
    renderer.set_working_colorspace(snapshot.working_colorspace);
    renderer.set_grade_encoding(snapshot.grade_encoding);
    // Tetrahedral, always: the trilinear option exists so the viewer can
    // stay responsive, and a delivery is not the place to trade LUT
    // accuracy for speed.
    renderer.set_lut_tetrahedral(true);
    if let Some(path) = &snapshot.global_lut_path {
        match klipp_color::lut::load_lut(path) {
            Ok(lut) => renderer.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: snapshot.video.global_lut_opacity })),
            Err(e) => tracing::warn!("{label}: global LUT {path:?} failed to load: {e}"),
        }
    }
    renderer
}

/// Where a `width`×`height` picture sits in a `out_w`×`out_h` export, as
/// the export places it: the picture's size and its offset (letterbox or
/// pillarbox when the aspects differ).
pub fn fit_in(width: u32, height: u32, out_w: u32, out_h: u32) -> (u32, u32, u32, u32) {
    if (width, height) == (out_w, out_h) {
        (out_w, out_h, 0, 0)
    } else {
        fit_to_target(width, height, out_w, out_h)
    }
}

/// One export frame, as the encoder takes it.
pub enum ExportFrame {
    /// Converted on the GPU: the encoder's planes, and the frame's light
    /// for a PQ delivery (`None` otherwise).
    Planes { planes: Vec<u8>, light: Option<GpuFrameLight> },
    /// Rendered as linear RGBA and padded to the output size, for the
    /// encoder's display transform (`encoder::export_display`).
    Linear(Vec<f32>),
}

/// The export's frames, one decision for every caller.
pub struct ExportFrameMaker {
    out: (u32, u32),
    /// The picture's size in the output, and where it sits.
    fit: (u32, u32, u32, u32),
    opts: RenderOptions,
    gpu_yuv: Option<Yuv420Target>,
    label: String,
    /// How long the last [`Self::frame`] spent padding, so a caller timing
    /// the render can keep the pad apart.
    pub last_pad: std::time::Duration,
}

impl ExportFrameMaker {
    /// For an export of `snapshot` with `settings`, into `encoder` (asked
    /// once, here, whether it takes GPU-made frames). `label` names the
    /// export in the log.
    pub fn new(settings: &ExportSettings, snapshot: &SequenceSnapshot, encoder: &Encoder, label: impl Into<String>) -> Self {
        let label = label.into();
        // Output size, and where the sequence's picture sits in it: an
        // export resolution with another aspect is letterboxed or
        // pillarboxed, not stretched.
        let out = settings.output_size(snapshot.width, snapshot.height);
        let fit = fit_in(snapshot.width, snapshot.height, out.0, out.1);
        // Whether a frame can be converted to the encoder's planes on the
        // GPU, and with what. Requires a delivery the shader can finish
        // and a picture that fills the output: a letterboxed export is
        // padded on the CPU, which needs the frame there anyway.
        let gpu_yuv = if (fit.0, fit.1) == out { encoder.gpu_yuv420_params() } else { None };
        debug!(
            "{label}: GPU frame conversion {} (fit {}x{}, out {}x{}, encoder format {:?})",
            if gpu_yuv.is_some() { "available" } else { "unavailable" },
            fit.0,
            fit.1,
            out.0,
            out.1,
            encoder.video_format(),
        );
        // An HLG or PQ delivery renders display light for the encoder's
        // HDR encode (an ACES PQ transform renders its own from SDR).
        let opts = RenderOptions { hdr_peak_nits: settings.renders_hdr().then(|| settings.hdr_peak_nits()), ..Default::default() };
        Self { out, fit, opts, gpu_yuv, label, last_pad: std::time::Duration::ZERO }
    }

    /// The output size frames come at.
    pub fn out_size(&self) -> (u32, u32) {
        self.out
    }

    /// Whether the picture is letterboxed or pillarboxed in the output
    /// (padded on the CPU, so never converted on the GPU).
    pub fn letterboxed(&self) -> bool {
        (self.fit.0, self.fit.1) != self.out
    }

    /// Whether frames are still tried on the GPU.
    pub fn converts_on_gpu(&self) -> bool {
        self.gpu_yuv.is_some()
    }

    /// The frame of `timeline` at `time`, rendered with `renderer` from
    /// `frames`, which it ends as the export does (a decoder that failed is
    /// the error).
    pub fn frame(&mut self, renderer: &mut Renderer, timeline: &Sequence, time: Timecode, frames: &mut ExportFrameSource) -> Result<ExportFrame, RenderError> {
        let (fit_w, fit_h, fit_x, fit_y) = self.fit;
        self.last_pad = std::time::Duration::ZERO;
        // The frame converted on the GPU, when the whole render is one
        // clip's grade and the delivery is one this can finish: then the
        // Y'CbCr planes come back instead of a linear frame, and the
        // per-pixel display-and-convert pass on the CPU does not happen.
        let planes = self.gpu_yuv.and_then(|target| renderer.render_yuv420_on_gpu(timeline, time, fit_w, fit_h, frames, self.opts, target));
        if let Some((planes, light)) = planes {
            frames.end_frame()?;
            return Ok(ExportFrame::Planes { planes, light });
        }
        if self.gpu_yuv.is_some() && renderer.gpu_export_declined_after_work() {
            // The miss came after the frame was rendered, so this frame was
            // rendered twice, and every frame after it would be. A miss at
            // the gate costs nothing and is this frame's alone: a plugin
            // clip's stretch goes the ordinary way and the GPU resumes.
            self.gpu_yuv = None;
            debug!("{}: GPU frame conversion off from {time:?}", self.label);
        }
        let picture = renderer.render(timeline, time, fit_w, fit_h, frames, self.opts);
        frames.end_frame()?;
        let t = std::time::Instant::now();
        let picture = pad_linear(picture, fit_w, fit_h, self.out.0, self.out.1, fit_x, fit_y);
        self.last_pad = t.elapsed();
        Ok(ExportFrame::Linear(picture))
    }
}
