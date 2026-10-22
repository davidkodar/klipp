//! Offline render / export pipeline (snapshot → composite → encode).
//!
//! Where `klipp-engine` handles real-time playback, `klipp-render` runs
//! the deterministic batch render: the user hits Export, a
//! [`SequenceSnapshot`] is captured from the live timeline, and an
//! [`ExportJob`] walks the snapshot frame-by-frame, composites layers,
//! runs effects, mixes audio, and feeds an [`Encoder`] that writes the
//! final container.
//!
//! # Pipeline
//!
//! 1. **[`snapshot`]** — freeze the current [`klipp_timeline::Sequence`]
//!    into a render-only [`SequenceSnapshot`] (clip paths, timings,
//!    effects, subtitle text, chapter markers). Compound / multicam
//!    clips are flattened here; the render path never re-enters the
//!    timeline crate.
//! 2. **[`clip_decoder`]** — one [`RenderDecoder`] per active clip,
//!    with HW decode negotiation (CUDA → VAAPI → software). Same
//!    two-phase decode invariant as the engine.
//! 3. **[`klipp_compose`]** — the per-frame grade, effects and
//!    multi-layer composite. Export and the viewer share this one
//!    definition of how a timeline frame is rendered, so what the user
//!    saw is what the file contains.
//! 4. **[`audio_mixdown`]** — [`AudioMixdown`] decodes each audio clip
//!    in full (bounded by one clip at a time for export), sums tracks,
//!    applies per-clip fade curves, and produces interleaved f32
//!    stereo for the encoder.
//! 5. **[`encoder`]** — wraps `ffmpeg-next` output contexts. Handles
//!    video (NVENC / software h264/h265/ProRes/AV1), audio (AAC / PCM
//!    / Opus), subtitles (mov_text), and chapter metadata.
//! 6. **[`export_job`]** — orchestrates the whole thing behind a
//!    cancel flag + progress counter.
//!
//! # Conventions
//!
//! * Every failure produces a [`RenderError`] and aborts the job —
//!   partial / drifted output files are never preferred over a loud
//!   failure.
//! * Export publishes atomically: the encode goes to
//!   `<output>.partial` and is renamed into place only once it has
//!   finished, so a cancelled or crashed job never leaves a
//!   half-written file at the target path.
//! * Subtitle burn-in and soft-embed are independent flags — with both
//!   on, subtitles render twice. The snapshot path logs a warning in
//!   that case.

pub mod clip_decoder;
pub mod error;
pub mod snapshot;
mod sws_colour;
mod vaapi;

pub mod a53;
pub mod audio_only;
pub mod hdr10plus;
pub mod normalize;
pub mod audio_mixdown;
pub mod encoder;
pub mod export_frames;
pub mod export_job;
mod frame_source;
pub mod gif_filter;
pub mod pre_analysis;
pub mod banding;
pub mod pixel_check;
pub mod still;

pub use audio_mixdown::AudioMixdown;
pub use clip_decoder::{detect_gpu, hardware_decoders_open, live_render_decoders, GpuVendor, RenderDecoder};
pub use encoder::Encoder;
pub use export_job::{measure_delivery, partial_path_for, ExportJob, LoudnessAnalysis};
/// The export's decoder of a snapshot's media, for tools that render
/// frames as the export does (the real-project pixel check, #1089).
pub use frame_source::{media_analysis_key, ExportFrameSource};
pub use pre_analysis::Analyser;
pub use error::RenderError;
pub use snapshot::{
    AudioClipSnapshot, AudioTrackSnapshot, MediaFile, SequenceSnapshot, SubtitleClipSnapshot,
    SubtitleTrackSnapshot, VideoTimeline,
};
