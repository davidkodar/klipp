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
//! way) keeps the GPU way off while the frames hold the same clips, so no
//! frame of that stretch is rendered twice, and the GPU way comes back at
//! the first frame that doesn't (#1276); a lost device keeps it off for the
//! rest of the export. A miss at the gate is the frame's alone.

use klipp_compose::{RenderOptions, Renderer};
use klipp_core::{ClipId, ExportSettings, Timecode};
use klipp_effects::gpu::{GpuFrameLight, Yuv420Target};
use klipp_timeline::Sequence;
use tracing::debug;

use crate::encoder::Encoder;
use crate::error::RenderError;
use crate::export_job::{fit_to_target, pad_linear};
use crate::frame_source::ExportFrameSource;
use crate::snapshot::SequenceSnapshot;

/// Why an export refuses a frame with these declines, if it does: any
/// but a node switched off and a declared fallback (a padded frame, a
/// neighbour past the media's real edge), which draw what the timeline
/// shows (M4 step 10).
pub fn refused(declines: &[klipp_nodes::Decline]) -> Option<String> {
    let failed: Vec<String> = declines.iter().filter(|d| d.fails()).map(|d| d.to_string()).collect();
    (!failed.is_empty()).then(|| failed.join("; "))
}

/// The renderer an export draws with: the viewer's, set up for a delivery.
/// `label` names the export in the log.
pub fn export_renderer(snapshot: &SequenceSnapshot, label: &str) -> Renderer {
    let timeline = &snapshot.video.sequence;
    // OFX plugin discovery is slow; only when a clip uses a plugin.
    let mut ofx = klipp_plugin::OfxHost::new();
    if timeline.tracks.iter().flat_map(|t| &t.clips).any(|c| c.has_enabled_plugins()) {
        ofx.discover_plugins();
    }
    export_renderer_with_host(snapshot, label, ofx)
}

/// The same delivery setup with an explicitly prepared plug-in host. This lets
/// checks use only their fixtures without discovering the user's plug-ins.
pub fn export_renderer_with_host(snapshot: &SequenceSnapshot, label: &str, ofx: klipp_plugin::OfxHost) -> Renderer {
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
    /// What the encoder takes a GPU-made frame with: kept for the whole
    /// export unless the device is lost.
    gpu_yuv: Option<Yuv420Target>,
    /// The clips on screen (every layer's, both sides of a transition) at
    /// the frame the GPU declined after rendering it: while the frames
    /// hold the same ones they go the ordinary way without asking again,
    /// and the first that doesn't asks again (#1276). A stretch the GPU
    /// couldn't take for a passing reason stays off for that stretch.
    declined_stack: Option<Vec<ClipId>>,
    /// How many frames were asked of the GPU way.
    gpu_attempts: u64,
    label: String,
    /// How long the last [`Self::frame`] spent padding, so a caller timing
    /// the render can keep the pad apart.
    pub last_pad: std::time::Duration,
    /// Why the GPU stack declined the stretch in `declined_stack`.
    declined_why: &'static str,
    /// Why frames aren't asked of the GPU at all, when `gpu_yuv` is none.
    no_gpu_why: &'static str,
    /// Why the last frame was composited on the CPU; `None` when the GPU
    /// stack made it (#1924).
    last_cpu_why: Option<&'static str>,
    /// Whether a frame the encoder takes no planes of is composited on the
    /// GPU and brought back (#2488): off once the device is lost.
    gpu_linear: bool,
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
        let no_gpu_why = if (fit.0, fit.1) != out { "the picture is letterboxed in the output, padded on the CPU" } else { "the encoder takes no frames made on the GPU" };
        Self { out, fit, opts, gpu_yuv, declined_stack: None, gpu_attempts: 0, label, last_pad: std::time::Duration::ZERO, declined_why: "", no_gpu_why, last_cpu_why: None, gpu_linear: true }
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

    /// Whether frames are still tried on the GPU: the encoder takes them,
    /// and the device isn't lost. A stretch declined after its work skips
    /// the GPU and still says yes.
    pub fn converts_on_gpu(&self) -> bool {
        self.gpu_yuv.is_some()
    }

    /// Why the last frame was composited on the CPU, or `None` when the
    /// GPU stack made it (#1924).
    pub fn last_cpu_why(&self) -> Option<&'static str> {
        self.last_cpu_why
    }

    /// How many frames were asked of the GPU way so far: a declined
    /// stretch asks once, not at every frame.
    pub fn gpu_attempts(&self) -> u64 {
        self.gpu_attempts
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
        // A stretch declined after its work isn't asked again while its
        // clips are on screen.
        if self.declined_stack.as_ref().is_some_and(|declined| *declined != on_screen(timeline, time)) {
            self.declined_stack = None;
            debug!("{}: GPU frame conversion back from {time:?}", self.label);
        }
        let ask = self.gpu_yuv.filter(|_| self.declined_stack.is_none());
        let planes = ask.and_then(|target| {
            self.gpu_attempts += 1;
            renderer.render_yuv420_on_gpu(timeline, time, fit_w, fit_h, frames, self.opts, target)
        });
        if let Some((planes, light)) = planes {
            frames.end_frame()?;
            self.last_cpu_why = None;
            return Ok(ExportFrame::Planes { planes, light });
        }
        self.last_cpu_why = Some(match ask {
            Some(_) => renderer.last_decline().unwrap_or("the GPU stack declined the frame"),
            None if self.declined_stack.is_some() => self.declined_why,
            None => self.no_gpu_why,
        });
        if ask.is_some() && renderer.gpu_export_declined_after_work() {
            // The miss came after the frame was rendered, so this frame was
            // rendered twice, and every frame of its stretch would be: the
            // GPU isn't asked again while the same clips are on screen. A
            // lost device won't come back, so then it's off for good. A
            // miss at the gate costs nothing and is this frame's alone: a
            // plugin clip's stretch goes the ordinary way and the GPU
            // resumes.
            if renderer.gpu_pipeline().is_none_or(|g| g.is_device_lost()) {
                self.gpu_yuv = None;
                self.no_gpu_why = "the GPU device was lost";
                debug!("{}: GPU frame conversion off from {time:?}: the device is lost", self.label);
            } else {
                self.declined_stack = Some(on_screen(timeline, time));
                self.declined_why = renderer.last_decline().unwrap_or("the GPU stack declined the frame");
                debug!("{}: GPU frame conversion off from {time:?} while its clips are on screen", self.label);
            }
        }
        // No planes made on the GPU for this encoder, or a letterboxed
        // picture: the stack still composites the frame, brought back once
        // to be padded and converted here (#2488). The CPU composition
        // only where the stack declines.
        if ask.is_none() && self.declined_stack.is_none() && self.gpu_linear {
            if let Some(picture) = renderer.render_linear_on_gpu(timeline, time, fit_w, fit_h, frames, self.opts) {
                frames.end_frame()?;
                self.last_cpu_why = None;
                let t = std::time::Instant::now();
                let picture = pad_linear(picture, fit_w, fit_h, self.out.0, self.out.1, fit_x, fit_y);
                self.last_pad = t.elapsed();
                return Ok(ExportFrame::Linear(picture));
            }
            self.last_cpu_why = Some(renderer.last_decline().unwrap_or("the GPU stack declined the frame"));
            if renderer.gpu_export_declined_after_work() {
                if renderer.gpu_pipeline().is_none_or(|g| g.is_device_lost()) {
                    self.gpu_linear = false;
                } else {
                    self.declined_stack = Some(on_screen(timeline, time));
                    self.declined_why = renderer.last_decline().unwrap_or("the GPU stack declined the frame");
                }
            }
        }
        let (picture, found) = renderer.render_inspected(timeline, time, fit_w, fit_h, frames, self.opts, Default::default());
        frames.end_frame()?;
        if let Some(why) = refused(&found.declines) {
            return Err(RenderError::Declined(format!("at {time:?}: {why}")));
        }
        let t = std::time::Instant::now();
        let picture = pad_linear(picture, fit_w, fit_h, self.out.0, self.out.1, fit_x, fit_y);
        self.last_pad = t.elapsed();
        Ok(ExportFrame::Linear(picture))
    }
}

/// The clips on screen in `timeline` at `time`, bottom first: every
/// layer's, both sides of a transition, a precomposition's own.
fn on_screen(timeline: &Sequence, time: Timecode) -> Vec<ClipId> {
    klipp_compose::active_layers(timeline, time).iter().flat_map(|l| l.clips.iter().map(|c| c.id)).collect()
}

#[cfg(test)]
mod tests {
    use super::refused;
    use klipp_nodes::{Decline, DeclineOutcome as Outcome, NodeKind};

    fn decline(outcome: Outcome) -> Decline {
        Decline { clip: None, node: uuid::Uuid::nil(), kind: NodeKind::Frei0r, outcome, name: "Delay0r".into(), reason: "reason".into(), why: "why".into() }
    }

    #[test]
    fn a_neighbour_past_the_media_edge_or_a_switched_off_node_never_stops_an_export() {
        // A denoise at the media's first frame has no frame before it: the
        // node ran with what it had, as it always will there.
        assert_eq!(refused(&[decline(Outcome::Fallback), decline(Outcome::Bypass)]), None);
        assert_eq!(refused(&[]), None);
    }

    #[test]
    fn a_neighbour_unread_inside_the_media_or_a_failed_plug_in_stops_it() {
        for outcome in [Outcome::NeighbourMissing, Outcome::PluginMissing, Outcome::Unsupported, Outcome::RenderFailed] {
            let why = refused(&[decline(Outcome::Fallback), decline(outcome)]).unwrap_or_else(|| panic!("{outcome:?} exported"));
            assert!(why.contains(outcome.label()) && !why.contains("fallback"), "{why}");
        }
    }
}
