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
//! 3. **[`pipeline`] / [`stage`]** — per-frame effect chain:
//!    [`ColorGradeStage`] → [`OFXEffectsStage`] → [`BuiltinEffectsStage`].
//!    Stage order is fixed; a reorder-able variant is a future item.
//! 4. **[`compositor`]** — multi-layer alpha composite
//!    ([`FrameCompositor`]).
//! 5. **[`audio_mixdown`]** — [`AudioMixdown`] decodes each audio clip
//!    in full (bounded by one clip at a time for export), sums tracks,
//!    applies per-clip fade curves, and produces interleaved f32
//!    stereo for the encoder.
//! 6. **[`encoder`]** — wraps `ffmpeg-next` output contexts. Handles
//!    video (NVENC / software h264/h265/ProRes/AV1), audio (AAC / PCM
//!    / Opus), subtitles (mov_text), and chapter metadata.
//! 7. **[`export_job`]** — orchestrates the whole thing behind a
//!    cancel flag + progress counter.
//!
//! # Conventions
//!
//! * Every failure produces a [`RenderError`] and aborts the job —
//!   partial / drifted output files are never preferred over a loud
//!   failure.
//! * Export is **not** currently atomic: output is written directly to
//!   the target path (a future fix writes to `.tmp` + rename).
//! * Subtitle burn-in and soft-embed are independent flags — with both
//!   on, subtitles render twice. The snapshot path logs a warning in
//!   that case.

pub mod clip_decoder;
pub mod compositor;
pub mod error;
pub mod pipeline;
pub mod snapshot;
pub mod stage;
pub mod stages;

pub mod audio_mixdown;
pub mod encoder;
pub mod export_job;
pub mod gif_filter;

pub use audio_mixdown::AudioMixdown;
pub use clip_decoder::{detect_gpu, GpuVendor, RenderDecoder};
pub use encoder::Encoder;
pub use export_job::ExportJob;
pub use compositor::FrameCompositor;
pub use error::RenderError;
pub use pipeline::RenderPipeline;
pub use snapshot::{
    AudioClipSnapshot, AudioTrackSnapshot, ClipKindSnapshot, ClipSnapshot,
    SequenceSnapshot, SubtitleClipSnapshot, SubtitleTrackSnapshot,
    TransitionSnapshot, VideoTrackSnapshot,
};
pub use stage::{ClipContext, FrameContext, ProcessingStage};
pub use stages::{BuiltinEffectsStage, ColorGradeStage, OFXEffectsStage};
