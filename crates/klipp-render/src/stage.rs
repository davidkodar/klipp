use crate::snapshot::ClipSnapshot;
use klipp_core::ExportSettings;
use std::sync::Arc;

/// A single frame processing operation in the render pipeline.
///
/// Implementations wrap existing Klipp subsystems (color grading, effects, etc.)
/// behind a uniform interface. To add a new processing stage to Klipp, implement
/// this trait and register it in `RenderPipeline::default_stages()`.
pub trait ProcessingStage: Send {
    /// Human-readable name for debugging and progress reporting.
    fn name(&self) -> &'static str;

    /// Process a single frame. Receives owned RGBA bytes, returns owned RGBA bytes.
    /// May modify in-place and return the same Vec, or return a new Vec.
    fn process(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8>;

    /// Called once before the first frame of each clip.
    /// Use to initialise clip-specific state (e.g. pre-build curve LUTs).
    fn begin_clip(&mut self, _ctx: &ClipContext) {}

    /// Called once after the last frame of each clip.
    fn end_clip(&mut self) {}

    /// Called once before the entire export begins.
    fn begin_export(&mut self, _settings: &ExportSettings) {}

    /// Called once after the export completes or is cancelled.
    fn end_export(&mut self) {}
}

/// Per-frame context passed to every processing stage.
/// Contains everything a stage might need without coupling to engine state.
pub struct FrameContext {
    /// Frame number within the entire export (0-based).
    pub export_frame: u64,
    /// Frame number within the current clip (0-based).
    pub clip_frame: u64,
    /// Exact time within the clip in seconds (for keyframe evaluation).
    pub clip_time_secs: f64,
    /// The clip being rendered (read-only access). None during gap/black frames.
    pub clip: Option<Arc<ClipSnapshot>>,
    /// Sequence fps (for grain seed, time calculations).
    pub fps: f64,
    /// Track index this frame belongs to (for multi-track pipeline routing).
    pub track_index: usize,
    /// The source frame's scene-referred codes, for a PQ, HLG or camera-log
    /// clip whose pixels reached this stage unmixed. The grade reads these,
    /// not the 8-bit frame, which is the tone-mapped preview — see
    /// [`SceneReferredFrame`].
    pub scene_referred: Option<Arc<SceneReferredFrame>>,
}

/// A scene-referred source frame (PQ, HLG, camera log) as the viewer's
/// grade reads it: packed 10-bit code values in the source primaries.
///
/// The pipeline between stages is 8-bit sRGB, which cannot hold a scene's
/// highlights, so for these clips the 8-bit frame is only the tone-mapped
/// preview (what the viewer shows ungraded) and the grade stage starts over
/// from the codes: scene-linear, grade, display tone map — the viewer's
/// order.
#[derive(Clone)]
pub struct SceneReferredFrame {
    pub codes: Vec<u32>,
    pub transfer: klipp_color::hdr::HdrTransfer,
    /// Source primaries → the grade's working space.
    pub source_to_working: [[f32; 3]; 3],
    pub width: u32,
    pub height: u32,
}

/// Per-clip context passed to `begin_clip()`.
pub struct ClipContext {
    pub clip: Arc<ClipSnapshot>,
    pub sequence_fps: f64,
    pub sequence_width: u32,
    pub sequence_height: u32,
}
