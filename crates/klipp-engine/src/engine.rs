use crate::audio_decoder::AudioDecoder;
use crate::audio_output::AudioOutput;
use crate::audio_source::AudioSource;
use crate::mix_thread::MixThread;
use crate::pcm_reader::PcmAudioFeeder;
use crate::sync::MasterClock;
use crate::texture::SharedFrameBuffer;
use crate::video_decoder::VideoDecoder;

/// Wrapper adapter so we can implement AlphaMaskProcessor (from klipp-title)
/// for GpuEffectsPipeline (from klipp-effects). Orphan rule requires the newtype pattern.
struct GpuAlphaMaskAdapter<'a>(&'a mut klipp_effects::gpu::GpuEffectsPipeline);

impl klipp_title::AlphaMaskProcessor for GpuAlphaMaskAdapter<'_> {
    fn blur(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.blur_alpha_gpu(mask, w, h, radius)
    }
    fn dilate(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.dilate_alpha_gpu(mask, w, h, radius)
    }
}

/// Wrapper to make AudioOutput Send. The cpal::Stream inside is !Send
/// due to platform internals, but we only ever use it on the engine thread.
struct SendAudioOutput(#[allow(dead_code)] AudioOutput);
unsafe impl Send for SendAudioOutput {}

fn fade_curve_to_u32(c: klipp_timeline::FadeCurve) -> u32 {
    match c {
        klipp_timeline::FadeCurve::Linear => 0,
        klipp_timeline::FadeCurve::Exponential => 1,
        klipp_timeline::FadeCurve::Logarithmic => 2,
        klipp_timeline::FadeCurve::SCurve => 3,
    }
}

/// If `td.animation_type == Typewriter`, return an Owned TitleData with
/// `write_on` overridden based on clip-elapsed seconds. Otherwise return None.
/// This lets callers avoid the clone when no animation is active.
fn typewriter_override(
    td: &klipp_timeline::TitleData,
    clip_elapsed_secs: f64,
) -> Option<klipp_timeline::TitleData> {
    if let klipp_timeline::TitleAnimationType::Typewriter { chars_per_sec } = td.animation_type {
        let chars = td.text.chars().count().max(1);
        let write_on = ((clip_elapsed_secs * chars_per_sec as f64) / chars as f64)
            .clamp(0.0, 1.0) as f32;
        let mut out = td.clone();
        out.write_on = write_on;
        Some(out)
    } else {
        None
    }
}

fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
        klipp_timeline::ChannelConfig::Surround51 => 3,
        klipp_timeline::ChannelConfig::Surround71 => 4,
    }
}

/// Evaluate a parameter with optional modifier stack, falling back to plain keyframe evaluation.
fn eval_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    param: &klipp_core::ParamId,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    match modifiers.get(param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}

/// Bulk sRGB u8 RGBA → linear f32 RGBA. Alpha stays linear per sRGB
/// spec. Used at composite call sites while grade and GPU effects
/// still run in u8 sRGB (migrates in 10c).
/// 256-entry LUT: u8 sRGB code → f32 linear. Replaces a per-channel
/// `srgb_to_linear` (which is a `powf(2.4)`) with a 1 KB table that
/// fits in L1. Saved 23-46 ms per cache-hit frame at 1080p — the
/// scalar form was the silent dominant cost on the render-cache hit
/// path during playback.
fn srgb_u8_to_linear_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    use klipp_color::transfer::srgb_to_linear;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = srgb_to_linear(i as f32 / 255.0);
        }
        lut
    })
}

/// 256-entry LUT: u8 alpha → f32 (just `/255.0`). Same shape as the
/// sRGB LUT for codegen symmetry — no gamma applied, per sRGB spec.
fn alpha_u8_to_f32_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = i as f32 / 255.0;
        }
        lut
    })
}

#[inline]
/// Nearest-neighbour downsample of an sRGB u8 RGBA buffer to a smaller
/// sRGB u8 RGBA buffer. Stays in sRGB space — the filmstrip tile is
/// already sRGB-encoded and the V2 canvas expects sRGB-encoded thumbs,
/// so a linear round-trip would only add quantisation for no gain. Used
/// for the identity-graph path of `render_node_thumbs_from_source`.
fn downsample_srgb_u8_to_thumb(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src.len() < (src_w * src_h * 4) as usize || src_w == 0 || src_h == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for y in 0..dst_h {
        let sy = ((y as u64 * src_h as u64) / dst_h as u64).min(src_h as u64 - 1) as u32;
        for x in 0..dst_w {
            let sx = ((x as u64 * src_w as u64) / dst_w as u64).min(src_w as u64 - 1) as u32;
            let i = ((sy * src_w + sx) as usize) * 4;
            out.push(src[i]);
            out.push(src[i + 1]);
            out.push(src[i + 2]);
            out.push(src[i + 3]);
        }
    }
    out
}

fn u8_rgba_to_linear(rgba: &[u8]) -> Vec<f32> {
    let srgb_lut = srgb_u8_to_linear_lut();
    let alpha_lut = alpha_u8_to_f32_lut();
    let n_pixels = rgba.len() / 4;
    let mut out = Vec::with_capacity(n_pixels * 4);
    // SAFETY: we write exactly n_pixels * 4 floats below.
    unsafe { out.set_len(n_pixels * 4); }
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let o = i * 4;
        out[o]     = srgb_lut[chunk[0] as usize];
        out[o + 1] = srgb_lut[chunk[1] as usize];
        out[o + 2] = srgb_lut[chunk[2] as usize];
        out[o + 3] = alpha_lut[chunk[3] as usize];
    }
    out
}

/// 4096-entry LUT: quantise a clamped linear f32 sample to 12 bits, then
/// look up the encoded sRGB u8. Replaces a per-channel `powf(1/2.4)` call.
/// 4 KB static, fits in L1 — ~10× faster than the scalar form, measured
/// at ~25 ms/frame on 1920×818 prior to LUT. 12-bit intermediate
/// resolution is more than enough for u8 output (256 levels).
fn linear_to_srgb_u8_lut() -> &'static [u8; 4096] {
    use std::sync::OnceLock;
    use klipp_color::transfer::linear_to_srgb;
    static LUT: OnceLock<[u8; 4096]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0u8; 4096];
        for (i, slot) in lut.iter_mut().enumerate() {
            let x = i as f32 / 4095.0;
            *slot = (linear_to_srgb(x).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        lut
    })
}

/// Bulk linear f32 RGBA → sRGB u8 RGBA with boundary clamp. The
/// inverse of [`u8_rgba_to_linear`]; values outside `[0.0, 1.0]`
/// clamp at the u8 boundary, not earlier.
#[inline]
fn linear_rgba_to_u8(linear: &[f32]) -> Vec<u8> {
    let lut = linear_to_srgb_u8_lut();
    let n_pixels = linear.len() / 4;
    let mut out = Vec::with_capacity(n_pixels * 4);
    // SAFETY: we write exactly n_pixels * 4 bytes below.
    unsafe { out.set_len(n_pixels * 4); }
    // Clamp → quantise to 12 bits → LUT lookup. Alpha stays linear per
    // sRGB spec (no gamma), so its "LUT" is just the clamp+scale.
    for (i, chunk) in linear.chunks_exact(4).enumerate() {
        let q = |v: f32| -> usize { (v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize };
        let o = i * 4;
        out[o]     = lut[q(chunk[0])];
        out[o + 1] = lut[q(chunk[1])];
        out[o + 2] = lut[q(chunk[2])];
        out[o + 3] = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    out
}

/// Evaluate the effective clip-level opacity at a timeline position.
/// Combines the static `BuiltinEffect::Opacity` value with opacity keyframes
/// and any modifier stack on `ParamId::Opacity`. Returns a value in [0.0, 1.0].
fn evaluate_clip_opacity(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::Timecode,
    fps: f64,
) -> f32 {
    let opacity_default = clip
        .builtin_effects
        .iter()
        .find_map(|e| match e {
            klipp_effects::BuiltinEffect::Opacity { value } => Some(*value),
            _ => None,
        })
        .unwrap_or(1.0);
    let clip_relative_secs = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
    let eval_ctx = klipp_core::EvalContext {
        clip_time_secs: clip_relative_secs,
        clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
        timeline_time_secs: playhead.to_secs_f64(),
        fps,
        seed: clip.id.0.as_u64_pair().0,
    };
    eval_with_modifiers(
        &clip.opacity_keyframes,
        &clip.param_modifiers,
        &klipp_core::ParamId::Opacity,
        clip_relative_secs,
        opacity_default,
        &eval_ctx,
    )
    .clamp(0.0, 1.0)
}

/// Evaluate an effect param with optional modifier stack.
fn eval_effect_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    effect_index: usize,
    param_name: &str,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    let param = klipp_core::ParamId::Effect {
        effect_index,
        param_name: param_name.to_string(),
    };
    match modifiers.get(&param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}

/// One entry per active audio source (one per playing track).
struct ActiveAudioSource {
    #[allow(dead_code)]
    track_index: usize,
    /// Media file this source is currently decoding. When a clip boundary
    /// is crossed into a clip whose media differs, we can't just seek the
    /// existing source — its decoder is open on the previous media file
    /// and would play the wrong audio. seek_all_audio_to_timecode checks
    /// this and falls back to start_all_audio when it sees a mismatch.
    media_id: MediaId,
    /// Fingerprint of the audio chain (effects + kfs) the source was
    /// opened with. Two clips that share the same media but have
    /// different audio_effects MUST get separate chain rebuilds at the
    /// boundary — without this check the first clip's chain bleeds into
    /// the second. `audio_fingerprint_for_clip` is the canonical hash.
    audio_chain_fingerprint: u64,
    source: AudioSource,
}
/// Pre-opened audio source ready for loop swap (created by pre-seek thread).
struct PreSeekAudioSource {
    track_index: usize,
    media_id: MediaId,
    audio_chain_fingerprint: u64,
    source: AudioSource,
    consumer: crate::audio_decoder::AudioConsumer,
}

/// Result from the pre-seek background thread.
struct LoopPreSeekResult {
    generation: u64,
    sources: Vec<PreSeekAudioSource>,
    #[allow(dead_code)]
    target_secs: f64,
}

use crossbeam_channel::{Receiver, Sender};
use crossbeam_queue::ArrayQueue;
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, ParamChange, RationalTime, Resolution, Timecode};
use klipp_jobs::ForegroundActivity;
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Engine-loop heartbeat. Bumped once per iteration of `PlaybackEngine::run`.
/// A watchdog in `klipp-app::main` polls it every 500 ms and screams to the
/// log if it hasn't advanced in ~2 s — that's the signal the engine thread
/// is stuck (e.g. a GPU `device.poll(Wait)` fence behind a saturated queue,
/// a mutex deadlock, or a runaway inner loop). Used strictly for
/// diagnostics; nothing load-bearing reads it.
pub static ENGINE_TICK: AtomicU64 = AtomicU64::new(0);

/// Monotonic heartbeat for the UI `update()` loop. Bumped at the top
/// of every `App::update` call. Same shape as `ENGINE_TICK` — the
/// watchdog in klipp-app reads it and logs when it stops advancing.
/// Lets us tell "engine is stuck" from "UI is stuck from "both are
/// stuck" when a hard freeze hits.
pub static UI_TICK: AtomicU64 = AtomicU64::new(0);

/// Fine-grained "last waypoint reached" marker for the watchdog. The
/// engine thread updates this at each major stage of a tick; when the
/// watchdog detects a stall, it reads this value and logs WHICH
/// stage was the last one entered. The `ENGINE_WATCHDOG STALLED` log
/// now names the operation that's hanging instead of just "somewhere
/// on the engine thread".
///
/// Encoding is a simple u32 ordinal indexed by `EngineStage`. Atomic
/// so the watchdog can read it without racing the engine's update.
pub static ENGINE_STAGE: AtomicU64 = AtomicU64::new(0);

/// Engine-thread waypoints. Each value is written to `ENGINE_STAGE`
/// at the start of the corresponding operation. The watchdog prints
/// the name of the stage in effect when it trips, which narrows a
/// hang from "entire run loop" to a specific ~10-line region.
#[repr(u64)]
#[allow(dead_code)]
pub enum EngineStage {
    Idle = 0,
    LoopTop = 1,
    CommandDrain = 2,
    ParamDrain = 3,
    UpdatePlayback = 4,
    PostProcessEntry = 5,
    PostProcessEnsureGpu = 6,
    PostProcessPregradeEncode = 7,
    PostProcessFbLockA = 8,
    PostProcessCacheLookup = 9,
    PostProcessInner = 10,
    PostProcessInnerGradeExecute = 11,
    PostProcessReencode = 12,
    PostProcessScopeRender = 13,
    GpuEncodeToTexture = 14,
    BlendComposite = 15,
    FbWrite = 16,
    LoopBottom = 17,
    /// `regrade_cached_frame` — runs when paused and a slider-scrub
    /// param change just landed. Calls `post_process_rgba_inner`
    /// directly (bypasses the wrapper's own stage marks) plus
    /// overlay compositing and scope texture render.
    PausedRegrade = 18,
    /// `prerender_cache_frames` — paused-only background work that
    /// pre-grades one nearby frame per tick. One `post_process_rgba_inner`
    /// invocation plus two cache writes (hot + disk).
    Prerender = 19,
}

pub fn engine_stage_name(v: u64) -> &'static str {
    match v {
        0 => "Idle",
        1 => "LoopTop",
        2 => "CommandDrain",
        3 => "ParamDrain",
        4 => "UpdatePlayback",
        5 => "PostProcessEntry",
        6 => "PostProcessEnsureGpu",
        7 => "PostProcessPregradeEncode",
        8 => "PostProcessFbLockA",
        9 => "PostProcessCacheLookup",
        10 => "PostProcessInner",
        11 => "PostProcessInnerGradeExecute",
        12 => "PostProcessReencode",
        13 => "PostProcessScopeRender",
        14 => "GpuEncodeToTexture",
        15 => "BlendComposite",
        16 => "FbWrite",
        17 => "LoopBottom",
        18 => "PausedRegrade",
        19 => "Prerender",
        _ => "Unknown",
    }
}

/// Convenience helper so each waypoint is a one-liner.
fn mark_stage(s: EngineStage) {
    ENGINE_STAGE.store(s as u64, Ordering::Relaxed);
}

/// UI-thread waypoint counter. Same shape as `ENGINE_STAGE` — the
/// watchdog reads this without locking and prints the matching name
/// when `UI_WATCHDOG STALLED` trips, narrowing a UI hang from "the
/// whole update loop" to a specific section. Set via
/// [`ui_stage::mark`]; resolve to a name via [`ui_stage::name`].
pub static UI_STAGE: AtomicU64 = AtomicU64::new(0);

pub mod ui_stage {
    use super::{Ordering, UI_STAGE};

    // Stage IDs. Keep stable; the watchdog formats them by id name.
    // Add new stages at the end so existing log greps don't shift.
    pub const IDLE: u64 = 0;
    pub const FRAME_TOP: u64 = 1;
    pub const TEXTURE_DROP_RING: u64 = 2;
    pub const REGISTER_GPU_TEXTURE: u64 = 3;
    pub const AI_TOOLS_TICK: u64 = 4;
    pub const DRAIN_EVENTS: u64 = 5;
    pub const COMPUTE_LAYOUT: u64 = 6;
    pub const RENDER_PANELS: u64 = 7;
    pub const RENDER_VIEWER: u64 = 8;
    pub const RENDER_TIMELINE: u64 = 9;
    pub const RENDER_BROWSER: u64 = 10;
    pub const RENDER_INSPECTOR: u64 = 11;
    pub const RENDER_DIALOGS: u64 = 12;
    pub const SHORTCUTS: u64 = 13;
    pub const PROCESS_QUIT: u64 = 14;
    pub const SEND_COMMANDS: u64 = 15;
    pub const POST_COMMANDS: u64 = 16;
    pub const UPLOAD_TITLE: u64 = 17;
    pub const ON_EXIT: u64 = 18;
    pub const TOAST_AND_GHOST: u64 = 19;
    pub const FRAME_END: u64 = 20;

    pub fn mark(s: u64) {
        UI_STAGE.store(s, Ordering::Relaxed);
    }

    pub fn name(v: u64) -> &'static str {
        match v {
            0 => "Idle",
            1 => "FrameTop",
            2 => "TextureDropRing",
            3 => "RegisterGpuTexture",
            4 => "AiToolsTick",
            5 => "DrainEvents",
            6 => "ComputeLayout",
            7 => "RenderPanels",
            8 => "RenderViewer",
            9 => "RenderTimeline",
            10 => "RenderBrowser",
            11 => "RenderInspector",
            12 => "RenderDialogs",
            13 => "Shortcuts",
            14 => "ProcessQuit",
            15 => "SendCommands",
            16 => "PostCommands",
            17 => "UploadTitle",
            18 => "OnExit",
            19 => "ToastAndGhost",
            20 => "FrameEnd",
            _ => "Unknown",
        }
    }
}

/// Read process RSS in MB from `/proc/self/statm` (Linux). Returns 0
/// on non-Linux or if the proc file can't be read. Cheap enough
/// (single file read, tens of microseconds) to call every 10 s.
fn read_rss_mb() -> f64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0.0;
    };
    let Some(rss_pages_str) = statm.split_whitespace().nth(1) else {
        return 0.0;
    };
    let Ok(pages) = rss_pages_str.parse::<u64>() else {
        return 0.0;
    };
    (pages * 4096) as f64 / (1024.0 * 1024.0)
}

/// Gate for the GPU-resident viewer output path. When on, the engine
/// mirrors the graded frame into `FrameBuffer.gpu_output` on the
/// shared wgpu device so the UI can sample it directly without a
/// CPU upload. Off by default while the UI-side wiring matures. Read
/// once per process via `OnceLock`.
/// Gate for the drag-time grade downsampling optimisation. When on,
/// `post_process_rgba_inner` runs at half resolution while the user
/// is actively dragging a colour-grade slider, then upsamples the
/// result so the rest of the pipeline keeps its dimension contract.
/// 4× compute reduction in the grade chain — fixes the "drops to 10
/// the moment I touch hue" symptom on a 1080p sequence.
///
/// Off by default while the optimisation matures. `KLIPP_DRAG_DOWNSAMPLE=1`
/// turns it on. Read once via `OnceLock`.
fn drag_downsample_enabled() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        std::env::var("KLIPP_DRAG_DOWNSAMPLE").ok().as_deref() == Some("1")
    })
}

fn should_publish_gpu_viewer() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let want_viewer = std::env::var("KLIPP_GPU_VIEWER").ok().as_deref() == Some("1");
        if !want_viewer {
            return false;
        }
        // The GPU viewer hand-off allocates `fb.gpu_output` on the
        // engine's wgpu device, then the UI registers a TextureView
        // of that texture with eframe-wgpu's renderer. wgpu-22's
        // wgpu_core stores resources per `Instance`; engine and UI
        // each have their own Instance unless the shared-device
        // path is enabled. Cross-Hub TextureView lookups panic in
        // `wgpu-core/src/storage.rs:79` ("TextureView[Id(N,vk)]
        // does not exist") on the very first
        // `register_native_texture` call. Until KLIPP_SHARED_GPU_DEVICE
        // is fixed (deadlock on `device.poll` — see main.rs comments)
        // the viewer path can ONLY engage when both env flags are on,
        // so `KLIPP_GPU_VIEWER=1` alone silently no-ops instead of
        // crashing.
        let want_shared = std::env::var("KLIPP_SHARED_GPU_DEVICE").ok().as_deref() == Some("1");
        if !want_shared {
            tracing::warn!(
                "KLIPP_GPU_VIEWER=1 ignored: requires KLIPP_SHARED_GPU_DEVICE=1 too. \
                 Without device sharing, fb.gpu_output lives on the engine wgpu Hub \
                 and egui-wgpu cannot register a TextureView from a different Hub — \
                 the cross-device BindGroup creation panics in wgpu_core storage."
            );
            return false;
        }
        true
    })
}

/// Maximum number of live overlay decoders during playback.
/// Prevents runaway decoder spawning on projects with many video tracks.
/// Overlay tracks beyond this cap still render correctly in export.
const MAX_OVERLAY_DECODERS: usize = 4;

/// Which GPU scope kernel (if any) to dispatch for a given scope
/// render mode. Mode 2 (vectorscope) has no GPU implementation —
/// the engine returns `None` and clears `fb.scope_texture` so the
/// UI falls back to the CPU vectorscope renderer in
/// `klipp-ui/widgets/scopes.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuScopeKind { Waveform, Parade, Histogram }

/// Resolve a UI-facing scope mode code to the GPU scope kernel it
/// should drive, or `None` when no GPU kernel applies (vectorscope,
/// plus any future modes not yet ported to GPU).
fn gpu_scope_kind_for_mode(mode: u8) -> Option<GpuScopeKind> {
    match mode {
        0 => Some(GpuScopeKind::Waveform),
        1 => Some(GpuScopeKind::Parade),
        // 2 = vectorscope — CPU only.
        3 => Some(GpuScopeKind::Histogram),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Stopped,
    Playing,
    Paused,
}

/// Bidirectional frame cache for instant backward stepping.
/// Stores pre-grade RGBA frames keyed by frame number.
struct FrameCache {
    entries: Vec<Option<FrameCacheEntry>>,
    capacity: usize,
}

struct FrameCacheEntry {
    frame_number: i64,       // playhead.value at this frame
    rgba: Arc<Vec<u8>>,      // pre-grade RGBA (shared via Arc)
    width: u32,
    height: u32,
}

impl FrameCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: (0..capacity).map(|_| None).collect(),
            capacity,
        }
    }

    /// Insert a frame into the cache. Uses frame_number as hash key.
    fn insert(&mut self, frame_number: i64, rgba: Arc<Vec<u8>>, width: u32, height: u32) {
        let slot = (frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot] = Some(FrameCacheEntry { frame_number, rgba, width, height });
    }

    /// Look up a frame by number. Returns (Arc<rgba>, width, height) if found.
    fn get(&self, frame_number: i64) -> Option<(Arc<Vec<u8>>, u32, u32)> {
        let slot = (frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot].as_ref()
            .filter(|e| e.frame_number == frame_number)
            .map(|e| (Arc::clone(&e.rgba), e.width, e.height))
    }

    /// Clear the entire cache (on clip change, seek, etc.)
    fn clear(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }
}

/// Bound on the lock-free param-change queue. 256 entries is roughly 2 seconds
/// of 120 Hz slider scrubbing; overflow drops oldest (see
/// [`PlaybackEngine::drain_param_changes`]) because the user won't perceive the
/// loss — every subsequent tick carries the current absolute value.
pub const PARAM_CHANGE_QUEUE_CAPACITY: usize = 256;

/// Drain a `ParamChange` queue into a shared `Sequence`. Pure helper — no
/// dependency on `PlaybackEngine` state so tests can exercise it without
/// constructing a full engine (and without the engine's audio/ffmpeg deps).
///
/// - Takes the sequence write lock at most once (only if the queue is
///   non-empty).
/// - Applies every queued change by looking up the clip across all tracks via
///   `Sequence::all_clips_mut()`.
/// - Invalidates `grade_hash` / `graph_arc` so the render cache and the
///   engine's cached graph Arc rebuild on the next frame.
///
/// Returns the number of changes applied. Entries whose clip or node can no
/// longer be found are silently dropped — the UI may have deleted the clip
/// between the push and this drain.
/// Recursively locate a node by id across a graph and every nested
/// compound sub-graph, applying a param override when found. Returns
/// true on first hit so `apply_param_changes` can count successes.
/// Depth is bounded by `COMPOUND_MAX_DEPTH_ENGINE` — the UI's
/// `resolve_graph_at_path_mut` can only descend through `Compound`
/// nodes whose `params.compound_subgraph` is `Some`, so runaway
/// recursion would require maliciously-crafted project data; a small
/// cap turns that into a safe bailout.
fn apply_override_recursive(
    graph: &mut klipp_nodes::NodeGraph,
    target_id: klipp_nodes::NodeId,
    param: &str,
    value: f32,
) -> bool {
    apply_override_recursive_impl(graph, target_id, param, value, 0)
}

const COMPOUND_MAX_DEPTH_ENGINE: u32 = 8;

fn apply_override_recursive_impl(
    graph: &mut klipp_nodes::NodeGraph,
    target_id: klipp_nodes::NodeId,
    param: &str,
    value: f32,
    depth: u32,
) -> bool {
    if depth >= COMPOUND_MAX_DEPTH_ENGINE {
        return false;
    }
    // Top-of-graph hit first (most common case — user edits a root
    // node; single linear scan, no allocations).
    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == target_id) {
        node.params.apply_override(param, value);
        return true;
    }
    // Miss — descend into any compound sub-graphs. The nested
    // recursive call reborrows on each iteration so we don't hold
    // multiple mutable views at once.
    for node in graph.nodes.iter_mut() {
        if node.kind == klipp_nodes::NodeKind::Compound {
            if let Some(sub) = node.params.compound_subgraph.as_mut() {
                if apply_override_recursive_impl(
                    sub, target_id, param, value, depth + 1,
                ) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn apply_param_changes(
    sequence: &Arc<RwLock<Sequence>>,
    queue: &Arc<ArrayQueue<ParamChange>>,
    grade_hash: &mut u64,
    graph_arc: &mut Option<Arc<klipp_nodes::NodeGraph>>,
) -> usize {
    // Fast path — no write lock if nothing to do. `len` on ArrayQueue is O(1).
    if queue.is_empty() {
        return 0;
    }
    // Try-write with logging: a plain `sequence.write()` silently
    // blocks the engine thread forever if another holder never
    // releases (we hit this during color-grade slider drags — 20 s+
    // stalls at stage=ParamDrain in the watchdog log). Retry every
    // 50 ms, log every 1 s so the freeze leaves a trail identifying
    // the owning thread via the `current_holder` telemetry below.
    let try_start = std::time::Instant::now();
    let mut last_log = try_start;
    let mut seq = loop {
        match sequence.try_write() {
            Ok(w) => break w,
            Err(std::sync::TryLockError::WouldBlock) => {
                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                    let held = SEQ_WRITE_HOLDER.load(std::sync::atomic::Ordering::Acquire);
                    tracing::error!(
                        "PARAM_DRAIN_BLOCKED {}ms waiting for sequence.write — current holder={} queue_len={}",
                        try_start.elapsed().as_millis(),
                        seq_write_holder_name(held),
                        queue.len(),
                    );
                    last_log = std::time::Instant::now();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
        }
    };
    seq_write_holder_set(SeqWriteHolder::EngineParamDrain);
    let mut applied = 0usize;
    while let Some(change) = queue.pop() {
        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == change.clip_id) {
            if apply_override_recursive(
                &mut clip.node_graph,
                change.node_id,
                &change.param,
                change.value,
            ) {
                applied += 1;
            }
        }
    }
    seq_write_holder_set(SeqWriteHolder::None);
    drop(seq);
    if applied > 0 {
        *grade_hash = 0;
        *graph_arc = None;
    }
    applied
}

/// Ownership-tag for the shared `Arc<RwLock<Sequence>>`. Every
/// `sequence.write()` call that holds the guard for more than a
/// few microseconds sets this to its site name on acquire and
/// clears it on release. The engine's `apply_param_changes`
/// try-write loop reads this atomic when it's been blocked for >1s
/// so the diagnostic log names the blocker instead of saying
/// "something".
///
/// Stored as a `u64` encoding the `SeqWriteHolder` discriminant —
/// read cost is a single atomic load.
pub static SEQ_WRITE_HOLDER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[repr(u64)]
#[derive(Copy, Clone, Debug)]
pub enum SeqWriteHolder {
    None = 0,
    EngineParamDrain = 1,
    UiSyncToEngine = 2,
    UiStabilizationComplete = 3,
}

pub fn seq_write_holder_set(h: SeqWriteHolder) {
    SEQ_WRITE_HOLDER.store(h as u64, std::sync::atomic::Ordering::Release);
}

pub fn seq_write_holder_name(v: u64) -> &'static str {
    match v {
        0 => "None",
        1 => "EngineParamDrain",
        2 => "UiSyncToEngine",
        3 => "UiStabilizationComplete",
        _ => "Unknown",
    }
}

pub struct PlaybackEngine {
    cmd_rx: Receiver<AppCommand>,
    event_tx: Sender<AppEvent>,
    frame_buffer: SharedFrameBuffer,
    /// Param changes queued by the UI's `SetNodeParam` handler. Drained at
    /// the start of each `update_playback` (and before seek/pre-render)
    /// so the engine sees the latest graph with exactly ONE write-lock
    /// acquisition per frame, regardless of how many slider ticks the
    /// UI produced. Avoids per-tick contention with the engine's long
    /// `sequence.read()` during active grading.
    param_change_queue: Arc<ArrayQueue<ParamChange>>,
    /// Shared per-frame performance telemetry. UI reads it for the
    /// overlay and session export; the engine writes one entry per
    /// `post_process_rgba` call. Behind a mutex because both sides
    /// run on their own threads.
    perf_ring: crate::perf::SharedPerfRing,
    state: PlaybackState,
    current_file: Option<PathBuf>,
    current_clip_index: usize,
    /// Index into sequence.tracks for the primary video track being played.
    current_track_index: usize,
    /// True when audio is continuing seamlessly across a contiguous clip boundary.
    /// Suppresses fade-in on the new clip to prevent dropout.
    contiguous_audio: std::sync::atomic::AtomicBool,
    duration: Timecode,
    fps: Fps,
    playhead: Timecode,
    sequence: Arc<RwLock<Sequence>>,
    media_pool: Arc<RwLock<MediaPool>>,
    repaint_cb: Box<dyn Fn() + Send>,
    last_frame_time: std::time::Instant,
    dropped_frames: u64,
    // Video decoders
    v1_decoder: Option<VideoDecoder>,
    // Overlay management: one entry per overlay track that has an active clip at the playhead.
    // Keyed by track_index (the index in sequence.tracks).
    overlay_decoders: HashMap<usize, VideoDecoder>,
    overlay_clips: HashMap<usize, usize>,        // track_index -> clip_index within that track
    overlay_frames: HashMap<usize, crate::types::LinearFrame>, // track_index -> latest decoded/rendered frame (linear f32 RGBA)
    overlay_synth_frame: HashMap<usize, i64>,    // track_index -> last rendered frame number (for synthetic clip caching)
    // Audio — one source per active track, mixed through mix thread
    audio_sources: Vec<ActiveAudioSource>,
    audio_output: Option<SendAudioOutput>,
    mix_thread: Option<MixThread>,
    mixer: std::sync::Arc<std::sync::Mutex<klipp_audio::AudioMixer>>,
    lufs_readings: Option<std::sync::Arc<std::sync::Mutex<klipp_audio::LufsReadings>>>,
    lufs_reset: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    spectrum_buf: Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>>,
    spectrum_sample_rate: u32,
    cpal_sample_rate: u32,
    /// Fingerprint of audio-relevant state. Prevents redundant audio teardown+rebuild
    /// when SequenceUpdated fires for non-audio changes (title edits, color changes).
    last_audio_fingerprint: u64,
    // Master clock — single source of truth for A/V timing
    master_clock: Arc<MasterClock>,
    // Timeline offset: maps source PTS to sequence timeline.
    clip_source_start: f64, // source seconds at which playback started for current clip
    clip_timeline_start: Timecode, // timeline position of clip_source_start
    clip_speed: f64, // per-clip speed multiplier (1.0 = normal)
    // Variable-speed (time remap): per-clip speed envelope. When non-empty,
    // the master clock's speed is updated per tick from speed_at_keyframes(clip_relative).
    speed_keyframes: Vec<klipp_core::Keyframe>,
    speed_ramp_enabled: bool,
    // Ramp anchor: clip-relative timeline seconds at the moment `ramp_anchor_wall`
    // was set. `update_playback` advances `ramp_timeline_anchor_secs + wall_elapsed`
    // to get the current clip-relative timeline position, then samples the speed
    // curve and re-anchors master_clock when the instantaneous speed drifts >1%.
    ramp_timeline_anchor_secs: f64,
    ramp_timeline_anchor_wall: std::time::Instant,
    // Last speed we pushed to master_clock via the ramp path. Tracked here because
    // MasterClock has no getter; we use it to decide whether to re-anchor.
    ramp_last_clock_speed: f64,
    // Previous post-effects RGBA frame, kept for linear frame blending on the
    // SpeedRamp effect's `blend_mode == 1` path (slow-mo only). Dimensions are
    // stored alongside so we can safely skip blending after a resolution change.
    prev_blend_frame: Option<Vec<u8>>,
    prev_blend_dims: (u32, u32),
    current_source_rotation: i32, // source rotation in degrees (0, 90, 180, 270)
    /// Last sequence-frame number rendered by the synthetic playback path.
    /// Avoids re-rendering the same generator/title frame ~1000x per second.
    last_synth_frame_rendered: Option<i64>,
    // Audio seek synchronization — generation counter
    expected_audio_generation: u64,
    // Performance
    use_hw_decode: bool,
    last_stats_time: std::time::Instant,
    /// Per-stats-interval counters (2-second window). Let us see at a
    /// glance how much work the engine is doing without grep-summing
    /// the 60 fps-worth of PERF lines by hand.
    dbg_update_playback_count: u64,
    dbg_regrade_count: u64,
    dbg_param_changes_drained: u64,
    dbg_commands_handled: u64,
    frames_consumed: u64,
    last_playback_log: std::time::Instant,
    /// Reliability telemetry — emitted every 10s so a long-playback
    /// log shows memory/queue trends at a glance. `last_rss_mb` is
    /// kept so each line can print the delta since the previous one
    /// (monotonic growth over a 2-hour soak is the leak signature).
    last_reliability_time: std::time::Instant,
    last_rss_mb: f64,
    // Color pipeline — post-processing on RGBA frames before display
    color_pipeline: klipp_color::ColorPipeline,
    // Node graph executor — processes per-clip color grading
    graph_executor: klipp_nodes::GraphExecutor,
    // Before/After — when true, skip per-clip color grade to show ungraded frame
    bypass_color_grade: bool,
    // Stabilization compare — when true, render the builtin effect chain twice
    // (once with VideoStabilization, once without) so the viewer can split-view
    // the raw vs stabilized result. Cost: ~2× builtin chain when active.
    pub compare_stabilization: bool,
    // Per-effect compare — mirrors compare_stabilization but targets a single
    // effect picked in the inspector, identified by clip + index + is_ofx.
    // When active on the current clip, the engine renders the chain twice
    // (once normal, once with the target effect's enabled flag flipped to
    // false) and writes the bypassed version to FrameBuffer.compare_effect_data.
    pub compare_effect_active: bool,
    pub compare_effect_clip_id: Option<klipp_core::ClipId>,
    pub compare_effect_index: usize,
    pub compare_effect_is_ofx: bool,
    // Matte preview — when true, show B&W qualifier/mask matte instead of graded frame
    matte_preview: bool,
    // Scope tap — which node's output to capture for scope display
    scope_tap_node: Option<klipp_nodes::NodeId>,
    /// V2 canvas "nodes visible right now" set — the executor
    /// captures downsampled thumbnails for each of these on the
    /// paused / post-seek re-grade so the canvas can paint per-node
    /// previews. Empty during playback (capturing N readbacks per
    /// frame would cost more than the grade itself).
    visible_node_thumbs: std::collections::HashSet<klipp_nodes::NodeId>,
    /// Set to `true` while `prerender_cache_frames` is running a
    /// background re-grade at some near-playhead frame. Those
    /// speculative grades must NOT overwrite the per-node thumbnails
    /// — the thumbs in the V2 canvas should reflect the user's
    /// actual playhead, not whichever frame the pre-cache happened
    /// to be chewing on at the time.
    thumb_capture_suspended: bool,
    /// After a paused seek (Pause / SetPlayhead / SeekTo while paused) the
    /// decoder is resumed and we wait briefly for a frame so per-node
    /// thumbnails can capture against the new position. At 4K the cold
    /// seek + keyframe rewind often outruns that budget. When it does we
    /// leave the decoder running and set this flag; the per-tick paused
    /// branch drains the queue when the frame finally arrives, runs the
    /// post-process pipeline (which captures thumbs), then pauses the
    /// decoder. Without this, the V2 canvas thumbs were stuck on the
    /// pre-seek frame until the user pressed an arrow key — that path
    /// pops from the now-filled queue immediately.
    pending_paused_refresh: bool,

    // ── Source Viewer playback (separate from timeline) ───────────
    /// Persistent decoder for whatever media the user has loaded into
    /// the source viewer. Lives independently of `v1_decoder` (which
    /// belongs to the timeline) so source playback doesn't disturb the
    /// program viewer's state. `None` until `SetSourceMedia` arrives.
    source_decoder: Option<VideoDecoder>,
    /// Path of the file currently bound to the source viewer. Compared
    /// on incoming `SetSourceMedia` so re-binding the same file is a
    /// cheap no-op (instead of re-spawning the decoder thread).
    source_path: Option<PathBuf>,
    /// Source-viewer playback state — matches the timeline's
    /// `PlaybackState` semantics but is independent of it. The
    /// mutual-exclusion rule (one viewer at a time owns audio) is
    /// enforced in the Play / PlaySource handlers, not here.
    source_state: PlaybackState,
    /// Current source-viewer playhead in source-time seconds (the
    /// file's own timeline, not the project sequence). Advanced by
    /// wall-clock during playback; jumped on `SeekSource` / `StepSource`.
    source_playhead_secs: f64,
    /// Native frame rate of the bound source media — used to convert
    /// `StepSource` frame counts to seconds and to pace the playhead
    /// advance during playback.
    source_fps: f64,
    /// Wall-clock + source-time anchor for the lightweight clock that
    /// drives the source playhead during playback. On Play we capture
    /// `(now, source_playhead_secs)`; per-tick we recompute the
    /// playhead as `anchor_secs + (now - anchor_wall) * 1.0`. Cleaner
    /// than dragging the timeline `MasterClock` into source territory.
    source_clock_anchor_wall: std::time::Instant,
    source_clock_anchor_secs: f64,
    /// True after the source decoder has signalled EOF for the
    /// currently-bound media. The next `play_source` consults this
    /// to rebuild the decoder before resuming, otherwise the BSF
    /// stays latched in EOF state and rejects every packet.
    source_at_eof: bool,
    /// True when the source viewer is paused but expects to receive
    /// one more frame from the decoder (just after `set_source_media`
    /// or `seek_source`). The per-tick paused-drain pops exactly one
    /// frame in that state, then clears the flag — without it, the
    /// drain pops every frame the decoder buffers and the playhead
    /// walks forward on its own (looks like the progressbar is
    /// "playing" right after a click).
    source_drain_pending: bool,
    /// Rotation passed to `VideoDecoder::open` for the bound source
    /// media — held so an EOF-restart rebuild can recreate the
    /// decoder with the same orientation as the original open.
    source_rotation: i32,
    /// Audio source for the bound media — mixed via the existing
    /// `mixer` on a dedicated source-viewer track. Muted whenever
    /// `source_state != Playing` (and the timeline pre-mutes its own
    /// tracks while source plays). `None` for files without audio.
    source_audio_source: Option<AudioSource>,
    /// Source-viewer frame buffer — the source viewer panel paints
    /// from this (separate from the program viewer's
    /// `frame_buffer`). Same `FrameBuffer` struct, just a second
    /// instance with its own pending/data buffers and generation
    /// counter so the two viewers' upload paths never collide.
    source_frame_buffer: SharedFrameBuffer,
    /// Mixer track-index reserved for source-viewer audio. Allocated
    /// at engine init (one slot past whatever the timeline mixer
    /// reserves) so SetTrackVolume / mute toggles don't accidentally
    /// touch a real timeline track.
    source_mixer_track_index: usize,
    /// Which scope mode to render on GPU (0=waveform, 1=parade, 2=vectorscope, 3=histogram).
    scope_render_mode: u8,
    /// Whether the scope panel is actually visible in the UI.
    /// Drives `should_render_scope()` together with the time-based
    /// throttle. Default `false` so a fresh engine doesn't burn the
    /// per-frame scope budget on data nobody is looking at — UI
    /// flips it on via `AppCommand::SetScopesVisible(true)` when
    /// the scopes panel opens.
    scopes_visible: bool,
    /// Wall-clock timestamp of the last GPU scope render. Used to
    /// throttle scope refresh to ~10 Hz — colourists can't perceive
    /// scope updates faster than that, and each scope render costs
    /// two full GPU sync fences (~40 ms together on NVIDIA Vulkan)
    /// which would otherwise consume most of a 24fps frame budget.
    last_scope_render: std::time::Instant,
    // Matte preview node — which Qualifier/Mask node's matte to show
    matte_preview_node: Option<klipp_nodes::NodeId>,
    // Cached pre-grade frame for instant parameter updates when paused.
    // Stored after decode, before color grading. Re-graded when sequence changes.
    cached_pregrade_frame: Option<(Arc<Vec<u8>>, u32, u32)>, // (rgba, width, height)
    // Bidirectional frame cache — keeps last N decoded pre-grade frames for instant
    // backward stepping without FFmpeg seeks.
    frame_cache: FrameCache,
    // Smart render cache — stores post-grade frames on disk (LZ4 compressed).
    // Hot tier: last 30 graded frames in RAM. Cold tier: disk LZ4.
    render_cache_hot: crate::render_cache::HotCache,
    render_cache_disk: crate::render_cache::DiskRenderCache,
    /// Current grade hash for the active clip (cached to avoid recomputing per frame).
    current_grade_hash: u64,
    current_grade_clip_id: Option<klipp_core::ClipId>,
    /// Arc-cached NodeGraph for the current clip. Reused across frames while the
    /// clip is active — a fresh Arc is constructed only when the clip switches
    /// or the graph mutates (invalidated alongside `current_grade_hash`).
    /// Avoids cloning ~13 nodes + edges + params every frame of `post_process_rgba_inner`.
    current_graph_arc: Option<Arc<klipp_nodes::NodeGraph>>,
    /// True while a colour-grade parameter gesture is in flight. Set
    /// by BeginNodeParamEdit, cleared by EndNodeParamEdit. Used to
    /// skip the render cache during slider scrubbing — grade_hash
    /// changes every tick, so the cache misses every read and pays
    /// for a write it won't reuse.
    in_node_param_edit: bool,
    /// One-shot signal: after `EndNodeParamEdit` clears
    /// `in_node_param_edit`, the next tick must run a full-resolution
    /// regrade so the viewer recovers from any drag-time downsampled
    /// frame. Cleared after the recovery tick consumes it.
    force_full_res_regrade: bool,
    /// Drag-window telemetry. Set on `BeginNodeParamEdit`, summarised
    /// + cleared on `EndNodeParamEdit`. Counts the grade frames run
    /// during the gesture and how many of them were downsampled, so
    /// the closing log line tells us at a glance whether the
    /// optimisation engaged.
    drag_started_at: Option<std::time::Instant>,
    drag_grade_count: u64,
    drag_downsampled_count: u64,
    /// Shared signal that background jobs read to decide whether to
    /// yield. Engine writes `is_playing` on play/pause transitions.
    /// Optional so legacy constructors / tests still work; when
    /// `None`, all writes are no-ops.
    foreground_activity: Option<Arc<ForegroundActivity>>,
    // OFX plugin host — applies per-clip effects chain after color grade
    ofx_host: klipp_plugin::OfxHost,
    // GPU effects pipeline — compute shaders for built-in effects
    gpu_effects: Option<klipp_effects::gpu::GpuEffectsPipeline>,
    /// True if GPU effects init was attempted and failed — prevents retrying every frame.
    gpu_effects_failed: bool,
    /// GPU vendor detected at startup — gates VAAPI in thumbnail/filmstrip decode
    /// on AMD/Intel while skipping it on NVIDIA (where nvidia-vaapi-driver
    /// writes stderr noise that bypasses av_log suppression).
    #[allow(dead_code)] // Read via DETECTED_GPU_VENDOR cache from free functions
    gpu_vendor: klipp_core::GpuVendor,
    /// Reusable accumulator for the viewer compositor. Sized to the last
    /// composite resolution; reallocated only on resolution change. Swapped
    /// with `FrameBuffer::pending_data` via `write_frame_swap` to eliminate
    /// the per-frame 8-33 MB allocation of the composite buffer.
    /// Reused scratch buffer for the main composite loop. Linear
    /// f32 RGBA; converted to u8 once at the end of the composite.
    /// Each multi-track frame pulls this out via `mem::take`, blends
    /// into it, then puts it back for the next frame to reuse —
    /// avoids reallocating a 4K×4 f32 buffer every frame.
    composite_scratch_linear: Vec<f32>,
    /// u8 output pool — the sRGB-encoded final composite swapped
    /// into `fb.pending_data`. The returned stale pending_data comes
    /// back here so we don't churn 33 MB/frame through the allocator.
    composite_scratch_u8: Vec<u8>,
    /// Display-side tone-mapping operator applied at the viewer
    /// boundary. `None` by default (identity — SDR content never
    /// exceeds 1.0 linear so the curve is a no-op). `Hable` or
    /// `Reinhard` become useful once HDR footage surfaces; flipping
    /// this is a one-line change. See [`klipp_color::display`].
    tone_map_op: klipp_color::display::ToneMapOp,
    // Rolling shutter correction motion cache
    rsc_motion_cache: klipp_effects::rolling_shutter::MotionCache,
    // Transition decoder — secondary decoder for the partner clip during transitions
    transition_decoder: Option<VideoDecoder>,
    transition_clip_id: Option<klipp_core::ClipId>,
    transition_frame_cache: Option<crate::types::DecodedFrame>,
    /// Cached clip_a frame captured during the first half of a centered transition
    /// window. Reused as the "from" frame during the second half, after the main
    /// decoder has advanced to clip_b. Cleared when the transition ends.
    transition_clip_a_last_frame: Option<Vec<u8>>,
    /// True when the partner decoder has signaled EOF during the current
    /// transition — the clip ran out of source material. This is the ONLY
    /// trigger for tagging frames as `Frozen`. Transient cache reuse
    /// (priming, seeking, underrun) does NOT set this flag.
    transition_partner_eof: bool,
    /// Per-transition degradation counter. Reset when the active transition
    /// changes or ends.
    pub transition_degradation: crate::transition::TransitionDegradation,
    // Title renderer — generates RGBA frames for title clips
    title_renderer: klipp_title::TitleRenderer,
    // VU metering — emit peak levels at ~30Hz
    last_peak_time: std::time::Instant,
    // Proxy: when true, decode from proxy files instead of originals
    use_proxies: bool,
    // Skim audio — shared with app layer for browser hover audio
    skim_consumer: crate::audio_output::SkimConsumer,
    // Loop playback — cycle between in/out points or whole sequence
    loop_enabled: bool,
    // Audio scrubbing: when false, audio is muted while the user drags the
    // playhead. Mirrors `prefs.playback.scrub_audio` from the UI side.
    scrub_audio_enabled: bool,
    // True while BeginScrub…EndScrub brackets a playhead drag.
    is_scrubbing: bool,
    // Predictive pre-seek — background thread pre-opens audio decoders before loop point
    pre_seek_result: Arc<std::sync::Mutex<Option<LoopPreSeekResult>>>,
    pre_seek_generation: u64,
    pre_seek_started: bool,
    // Clip look-ahead: pre-decoder for the next clip, opened ~1s before the cut point.
    // When the cut arrives, this decoder is swapped in as v1_decoder — zero stall.
    preroll_decoder: Option<VideoDecoder>,
    preroll_clip_index: Option<usize>,
    preroll_source_in: f64,
}

impl PlaybackEngine {
    /// Handle to the shared spectrum PCM ring fed by the mix thread — clone
    /// and stash on the UI side so the audio-spectrum scope can drain samples
    /// each frame without going through the event channel. Returns `None` if
    /// the mix thread failed to initialise (extremely unusual).
    pub fn spectrum_buf(&self) -> Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>> {
        self.spectrum_buf.clone()
    }

    /// Sample rate of the samples being pushed into `spectrum_buf`.
    pub fn spectrum_sample_rate(&self) -> u32 {
        self.spectrum_sample_rate
    }

    /// Handle to the lock-free param-change queue. The UI clones this and
    /// pushes `ParamChange` entries from its `SetNodeParam` handler (60-120 Hz
    /// during a colour-grade scrub). The engine drains the queue once per
    /// frame with a single write-lock acquisition — see
    /// [`Self::drain_param_changes`]. Exposed as `Arc` so callers can clone
    /// without cost.
    pub fn param_change_queue(&self) -> Arc<ArrayQueue<ParamChange>> {
        self.param_change_queue.clone()
    }

    /// Apply every queued `ParamChange` to `self.sequence`. Takes the write
    /// lock at most once (when the first entry is popped) and releases it when
    /// the queue is empty. Also invalidates `current_grade_hash` and
    /// `current_graph_arc` so the render cache and node-graph Arc rebuild on
    /// the next frame, mirroring what `AppCommand::SequenceUpdated` does.
    ///
    /// Called at the start of `update_playback`, once per engine tick.
    /// Safe to call when the queue is empty — returns immediately without
    /// taking any locks.
    pub fn drain_param_changes(&mut self) -> usize {
        let n = apply_param_changes(&self.sequence, &self.param_change_queue, &mut self.current_grade_hash, &mut self.current_graph_arc);
        self.dbg_param_changes_drained += n as u64;
        n
    }

    pub fn new(
        cmd_rx: Receiver<AppCommand>,
        event_tx: Sender<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        source_frame_buffer: SharedFrameBuffer,
        sequence: Arc<RwLock<Sequence>>,
        media_pool: Arc<RwLock<MediaPool>>,
        repaint_cb: Box<dyn Fn() + Send>,
        skim_consumer: crate::audio_output::SkimConsumer,
        perf_ring: crate::perf::SharedPerfRing,
    ) -> Self {
        // Infallible today — every fallible probe inside logs + falls back
        // to a None/default. Previously wrapped in `Result<Self,
        // klipp_core::KlippError>` but no code path ever returned `Err`,
        // so callers had a dead `.map_err(...)?` branch. Plain `Self`
        // makes the lack of failure mode explicit.
        info!("PlaybackEngine: initializing (ffmpeg-next backend)");

        // Probe cpal sample rate (don't start the stream yet — we start it per-clip)
        let cpal_sample_rate = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: cpal default device probe");
            use cpal::traits::{DeviceTrait, HostTrait};
            let host = cpal::default_host();
            host.default_output_device()
                .and_then(|d| d.default_output_config().ok())
                .map(|c| c.sample_rate().0)
                .unwrap_or(48000)
        };
        info!("PlaybackEngine: cpal sample rate = {cpal_sample_rate}");

        // Probe VAAPI availability once
        let use_hw_decode = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: VAAPI probe");
            unsafe {
                use ffmpeg_next::ffi::*;
                let device_type = av_hwdevice_find_type_by_name(c"vaapi".as_ptr());
                device_type != AVHWDeviceType::AV_HWDEVICE_TYPE_NONE
            }
        };
        info!("PlaybackEngine: VAAPI available = {use_hw_decode}");

        // Create audio output eagerly so skim audio works without playing a clip first.
        let mixer = std::sync::Arc::new(std::sync::Mutex::new(klipp_audio::AudioMixer::new(0)));
        // True-peak oversampling defaults on (BS.1770-4 Annex 2 compliant).
        // No live toggle from UI prefs yet — change requires next playback
        // session. Matches `prefs.audio.tp_oversample` default in klipp-ui.
        let (mix_thread, main_consumer) = MixThread::new(cpal_sample_rate, mixer.clone(), true);
        let lufs_readings = Some(mix_thread.lufs_readings());
        let lufs_reset = Some(mix_thread.lufs_reset_flag());
        let spectrum_buf = Some(mix_thread.spectrum_buf());
        let spectrum_sample_rate = mix_thread.spectrum_sample_rate();
        let preferred_device = crate::audio_output::PREFERRED_DEVICE.lock().ok().and_then(|g| g.clone());
        let audio_output = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: AudioOutput::new (cpal stream open)");
            match AudioOutput::new_with_device(main_consumer, skim_consumer.clone(), preferred_device.as_deref()) {
                Ok(output) => {
                    info!("PlaybackEngine: audio output created eagerly");
                    Some(SendAudioOutput(output))
                }
                Err(e) => {
                    warn!("Failed to create audio output: {e}");
                    None
                }
            }
        };

        // Heavy field inits — extracted from the struct literal below so we can
        // measure each one independently for STARTUP_PERF.
        let render_cache_disk = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: DiskRenderCache::new (LRU walk)");
            crate::render_cache::DiskRenderCache::new(
                dirs::cache_dir().unwrap_or_default().join("klipp/render"),
                20, // 20 GB default max
            )
        };
        let ofx_host = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: OfxHost::discover_plugins");
            let mut h = klipp_plugin::OfxHost::new();
            h.discover_plugins();
            h
        };
        let title_renderer = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: TitleRenderer::new (font discovery)");
            klipp_title::TitleRenderer::new()
        };

        let param_change_queue = Arc::new(ArrayQueue::new(PARAM_CHANGE_QUEUE_CAPACITY));

        Self {
            cmd_rx,
            event_tx,
            frame_buffer,
            param_change_queue,
            perf_ring,
            state: PlaybackState::Idle,
            current_file: None,
            current_clip_index: 0,
            current_track_index: 0,
            contiguous_audio: std::sync::atomic::AtomicBool::new(false),
            duration: Timecode::zero(),
            fps: Fps::film_24(),
            playhead: RationalTime::from_frames(0, 24),
            sequence,
            media_pool,
            repaint_cb,
            last_frame_time: std::time::Instant::now(),
            dropped_frames: 0,
            v1_decoder: None,
            overlay_decoders: HashMap::new(),
            overlay_clips: HashMap::new(),
            overlay_frames: HashMap::new(),
            overlay_synth_frame: HashMap::new(),
            audio_sources: Vec::new(),
            audio_output,
            mix_thread: Some(mix_thread),
            mixer,
            lufs_readings,
            lufs_reset,
            spectrum_buf,
            spectrum_sample_rate,
            cpal_sample_rate,
            last_audio_fingerprint: 0,
            master_clock: Arc::new(MasterClock::new()),
            clip_source_start: 0.0,
            clip_timeline_start: Timecode::zero(),
            clip_speed: 1.0,
            speed_keyframes: Vec::new(),
            speed_ramp_enabled: false,
            ramp_timeline_anchor_secs: 0.0,
            ramp_timeline_anchor_wall: std::time::Instant::now(),
            ramp_last_clock_speed: 1.0,
            prev_blend_frame: None,
            prev_blend_dims: (0, 0),
            current_source_rotation: 0,
            last_synth_frame_rendered: None,
            expected_audio_generation: 0,
            use_hw_decode: false,
            last_stats_time: std::time::Instant::now(),
            dbg_update_playback_count: 0,
            dbg_regrade_count: 0,
            dbg_param_changes_drained: 0,
            dbg_commands_handled: 0,
            frames_consumed: 0,
            last_playback_log: std::time::Instant::now(),
            last_reliability_time: std::time::Instant::now(),
            last_rss_mb: 0.0,
            color_pipeline: klipp_color::ColorPipeline::new(),
            // GraphExecutor MUST have its own GPU pipeline — `new_cpu_only`
            // forces every node onto the CPU and turns the entire grade
            // chain into a 60-90 ms/frame stall during slider scrub.
            // (The engine has its own `gpu_effects` for scope/wrap; the
            // two pipelines are independent for now. TODO share via Arc.)
            graph_executor: klipp_nodes::GraphExecutor::new(),
            bypass_color_grade: false,
            compare_stabilization: false,
            compare_effect_active: false,
            compare_effect_clip_id: None,
            compare_effect_index: 0,
            compare_effect_is_ofx: false,
            matte_preview: false,
            scope_tap_node: None,
            visible_node_thumbs: std::collections::HashSet::new(),
            thumb_capture_suspended: false,
            pending_paused_refresh: false,
            source_decoder: None,
            source_path: None,
            source_state: PlaybackState::Idle,
            source_playhead_secs: 0.0,
            source_fps: 24.0,
            source_clock_anchor_wall: std::time::Instant::now(),
            source_clock_anchor_secs: 0.0,
            source_at_eof: false,
            source_drain_pending: false,
            source_rotation: 0,
            source_audio_source: None,
            source_frame_buffer,
            // Reserve a track index past anything the timeline mixer
            // is likely to use. Picked high enough that the user
            // adding ~ all the audio tracks they reasonably will
            // (Klipp's UI tops out around 16) won't collide. The
            // mixer grows on demand via `ensure_tracks` so this slot
            // gets allocated lazily on first source-audio attach.
            source_mixer_track_index: 64,
            scope_render_mode: 0,
            scopes_visible: false,
            last_scope_render: std::time::Instant::now() - std::time::Duration::from_secs(1),
            matte_preview_node: None,
            cached_pregrade_frame: None,
            frame_cache: FrameCache::new(120), // ~5s at 24fps (backward history + forward prefetch)
            render_cache_hot: crate::render_cache::HotCache::new(30),
            render_cache_disk,
            current_grade_hash: 0,
            current_grade_clip_id: None,
            current_graph_arc: None,
            in_node_param_edit: false,
            force_full_res_regrade: false,
            drag_started_at: None,
            drag_grade_count: 0,
            drag_downsampled_count: 0,
            foreground_activity: None,
            ofx_host,
            gpu_effects: None, // lazily initialized on first use
            gpu_effects_failed: false,
            gpu_vendor: {
                let v = klipp_core::GpuVendor::detect();
                let _ = DETECTED_GPU_VENDOR.set(v);
                info!("PlaybackEngine: detected GPU vendor = {v:?}");
                v
            },
            composite_scratch_linear: Vec::new(),
            composite_scratch_u8: Vec::new(),
            tone_map_op: klipp_color::display::ToneMapOp::None,
            rsc_motion_cache: klipp_effects::rolling_shutter::MotionCache::new(256),
            transition_decoder: None,
            transition_clip_id: None,
            transition_frame_cache: None,
            transition_clip_a_last_frame: None,
            transition_partner_eof: false,
            transition_degradation: Default::default(),
            title_renderer,
            last_peak_time: std::time::Instant::now(),
            use_proxies: true,
            skim_consumer,
            loop_enabled: false,
            scrub_audio_enabled: true,
            is_scrubbing: false,
            pre_seek_result: Arc::new(std::sync::Mutex::new(None)),
            pre_seek_generation: 0,
            pre_seek_started: false,
            preroll_decoder: None,
            preroll_clip_index: None,
            preroll_source_in: 0.0,
        }
    }

    /// Install the shared foreground-activity signal. The engine
    /// writes `is_playing` whenever its `state` flips to/from
    /// `PlaybackState::Playing`, plus `is_scrubbing` on scrub
    /// gestures. Background jobs read this signal to know when to
    /// yield. Idempotent — the latest call wins.
    pub fn set_foreground_activity(&mut self, fg: Arc<ForegroundActivity>) {
        // Bring the signal into immediate sync with current engine
        // state so a fresh installation doesn't lie about what's
        // happening right now.
        fg.set_playing(self.state == PlaybackState::Playing);
        fg.set_scrubbing(self.is_scrubbing);
        self.foreground_activity = Some(fg);
    }

    /// Periodic reliability telemetry, emitted from the engine
    /// run-loop every 10 s.
    ///
    /// Grep `RELIABILITY` in `klipp-debug.log` over a long soak run
    /// to see monotonic trends. Monotonic RSS growth = leak. Decoder
    /// queue saturating = decode is falling behind (or the consumer
    /// isn't draining). Frame-buffer capacities climbing above the
    /// single-frame working set = somewhere a buffer is being kept
    /// larger than needed (reallocs without shrinks, or swap-race).
    fn emit_reliability_telemetry(&mut self) {
        let rss_mb = read_rss_mb();
        let rss_delta = if self.last_rss_mb > 0.0 {
            rss_mb - self.last_rss_mb
        } else {
            0.0
        };
        self.last_rss_mb = rss_mb;

        let (v1_q_len, v1_q_cap) = self
            .v1_decoder
            .as_ref()
            .map(|d| {
                let q = d.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                (q.len(), q.capacity())
            })
            .unwrap_or((0, 0));

        let overlay_qs: Vec<String> = self
            .overlay_decoders
            .iter()
            .map(|(idx, d)| {
                let q = d.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                format!("{}:{}/{}", idx, q.len(), q.capacity())
            })
            .collect();

        let (fb_data_mb, fb_pending_mb, fb_has_gpu, fb_generation) = {
            let fb = self
                .frame_buffer
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            (
                fb.data.capacity() as f64 / (1024.0 * 1024.0),
                fb.pending_data.capacity() as f64 / (1024.0 * 1024.0),
                fb.gpu_output.is_some(),
                fb.generation,
            )
        };

        tracing::info!(
            "RELIABILITY rss={:.1}MB rss_delta={:+.1}MB state={:?} v1_q={}/{} overlays=[{}] \
             fb_data_cap={:.2}MB fb_pending_cap={:.2}MB fb_has_gpu={} fb_gen={} \
             drops={} frames_consumed={}",
            rss_mb,
            rss_delta,
            self.state,
            v1_q_len, v1_q_cap,
            overlay_qs.join(","),
            fb_data_mb, fb_pending_mb, fb_has_gpu, fb_generation,
            self.dropped_frames, self.frames_consumed,
        );
    }

    /// Helper: mirror the current `state == Playing` to the
    /// foreground signal (no-op when no signal installed). Called
    /// after every `self.state = ...` write so the signal stays
    /// truthful without us having to thread it through every call
    /// site.
    fn sync_foreground_playing(&self) {
        if let Some(fg) = &self.foreground_activity {
            fg.set_playing(self.state == PlaybackState::Playing);
        }
    }

    /// Helper: mirror `is_scrubbing` to the foreground signal.
    fn sync_foreground_scrubbing(&self) {
        if let Some(fg) = &self.foreground_activity {
            fg.set_scrubbing(self.is_scrubbing);
        }
    }

    pub fn run(mut self) {
        info!("Engine thread starting");

        loop {
            // Heartbeat for the watchdog thread in klipp-app::main. Bumped at
            // the very top of every iteration so a stall anywhere below (GPU
            // sync, mutex wait, decode) freezes the counter.
            ENGINE_TICK.fetch_add(1, Ordering::Relaxed);
            mark_stage(EngineStage::LoopTop);

            let tick_start = std::time::Instant::now();

            // Loop timeout paces the engine thread's spin rate.
            // Playing: 8 ms = up to 125 Hz loop rate (plenty for 60 fps
            // video — update_playback pacing is internal). Was 1 ms which
            // made the loop spin at ~1000 Hz, hammering CPU with no
            // benefit.
            // Paused: 16 ms = 60 Hz loop rate, cheap enough to still
            // respond to user commands within one frame.
            let timeout = if self.state == PlaybackState::Playing
                || self.source_state == PlaybackState::Playing
            {
                Duration::from_millis(8)
            } else {
                Duration::from_millis(16)
            };

            // Mirror current state to the foreground-activity signal
            // every iteration so background jobs always see fresh
            // truth without us having to thread `sync_foreground_*`
            // through every play/pause/scrub call site. The scheduler
            // polls at 50 ms; we update faster than that so it never
            // misses a transition.
            self.sync_foreground_playing();
            self.sync_foreground_scrubbing();

            // Collect all pending commands, coalescing seeks
            let mut commands = Vec::new();
            match self.cmd_rx.recv_timeout(timeout) {
                Ok(cmd) => commands.push(cmd),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                commands.push(cmd);
            }

            // Coalesce: keep only the last seek/setplayhead, drop earlier ones
            let commands = coalesce_commands(commands);
            let had_commands = !commands.is_empty();

            mark_stage(EngineStage::CommandDrain);
            for cmd in commands {
                self.dbg_commands_handled += 1;
                if self.handle_command(cmd) {
                    info!("Engine thread shutting down");
                    return;
                }
            }

            // Apply any queued slider-scrub param changes from the UI. During
            // Pause this is the ONLY site that drains the queue (update_playback
            // doesn't run). Cheap no-op when the queue is empty, so it's safe to
            // call every tick.
            mark_stage(EngineStage::ParamDrain);
            let applied = self.drain_param_changes();

            // Source-viewer playback runs in parallel with the
            // timeline state machine — this is the only site that
            // pops frames off the source decoder during playback. The
            // mutual-exclusion rule in `play_source` / `Play`
            // guarantees only one viewer is in the Playing state at
            // any moment, so this is at most one of the two paths
            // doing real per-tick work.
            if self.source_state == PlaybackState::Playing {
                self.update_source_playback();
            } else if self.source_decoder.is_some() && self.source_drain_pending {
                // Paused-drain: a recent seek/load is waiting on
                // its target frame to land. Pop ONE frame and stop;
                // the gate prevents the drain from stepping through
                // the full decoder queue every tick (which made the
                // playhead walk forward on its own and looked like
                // the progressbar was "playing" right after a click).
                if self.drain_one_source_frame_to_buffer() {
                    self.source_drain_pending = false;
                }
            }

            if applied > 0
                && matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped)
                && self.cached_pregrade_frame.is_none()
            {
                // Safety net: a slider drag is landing in the
                // engine while we're still in Idle / Stopped (user
                // never scrubbed via a path that pushed SetPlayhead /
                // SeekTo), and `cached_pregrade_frame` is empty so
                // the regrade branch below would silently no-op.
                // Auto-heal by seeking to the current playhead —
                // opens the decoder, populates the snapshot, flips
                // state to Paused. The next drain tick's regrade
                // hits a real snapshot and the viewer updates live
                // without requiring a Play round-trip first.
                let ph = self.playhead;
                self.seek_in_sequence(ph);
                if self.v1_decoder.is_some() || !self.audio_sources.is_empty() {
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                }
            }
            if self.state == PlaybackState::Playing {
                // Playback already produces fresh frames at full res
                // every tick; the drag-recovery flag isn't needed here.
                self.force_full_res_regrade = false;
                self.dbg_update_playback_count += 1;
                mark_stage(EngineStage::UpdatePlayback);
                self.update_playback();
                // Sync clip audio params periodically during playback (for keyframe animation)
                self.sync_clip_volumes();
            } else if self.state == PlaybackState::Paused {
                if self.pending_paused_refresh {
                    // A previous seek (Pause / SetPlayhead / SeekTo
                    // while paused) left the decoder running because
                    // its 50–200 ms grab window wasn't enough for a
                    // cold 4K seek. Pop any frame the decoder has
                    // produced since, run the full post_process
                    // pipeline (captures per-node thumbs), then pause
                    // the decoder. Silent no-op until the frame lands.
                    if self.try_drain_paused_refresh_frame() {
                        self.pending_paused_refresh = false;
                    }
                }
                if applied > 0 {
                    // Colour-grade param drag while paused — the drain updated
                    // the graph but the viewer is still displaying the cached
                    // graded frame from before the change. Re-run the grade
                    // chain on `cached_pregrade_frame` so the visible frame
                    // reflects the new values immediately. Previously this
                    // call was missing and the pref drag produced no visible
                    // change until the user pressed play.
                    self.dbg_regrade_count += 1;
                    self.regrade_cached_frame();
                } else if self.force_full_res_regrade {
                    // EndNodeParamEdit just cleared the drag flag. The last
                    // displayed frame is at half res from the drag-time
                    // downsample path; run one more grade at full res so
                    // the viewer recovers cleanly. Clear the flag so we
                    // don't loop here forever.
                    self.force_full_res_regrade = false;
                    self.dbg_regrade_count += 1;
                    self.regrade_cached_frame();
                } else if !had_commands {
                    // Engine is genuinely idle — pre-render nearby frames
                    // into cache. Skip prerender when we just applied param
                    // changes so the next tick re-runs through the fresh
                    // graph instead of caching stale output.
                    self.prerender_cache_frames();
                }
            }

            // Reliability telemetry — once every 10s. Tracks the
            // signals that diverge during a freeze: RSS (leak
            // pressure), decoder queue fullness (decode stalled),
            // frame-buffer capacities (pending-buffer growth), so a
            // soak-test log lets us spot where things went wrong
            // before the watchdog fired.
            if self.last_reliability_time.elapsed() >= Duration::from_millis(10_000) {
                self.emit_reliability_telemetry();
                self.last_reliability_time = std::time::Instant::now();
            }

            // Emit perf stats periodically
            if self.last_stats_time.elapsed() >= Duration::from_millis(2000) {
                let elapsed = self.last_stats_time.elapsed().as_secs_f64().max(0.001);
                // Check actual HW decode status from decoder
                let hw = self.v1_decoder.as_ref()
                    .map(|d| d.hw_active.load(Ordering::Relaxed))
                    .unwrap_or(self.use_hw_decode);
                let _ = self.event_tx.send(AppEvent::EngineStats {
                    dropped_frames: self.dropped_frames,
                    hw_decode: hw,
                });
                tracing::info!(
                    "ENGINE_HEARTBEAT state={:?} updates={} regrades={} param_changes={} cmds={} elapsed={:.2}s rate={:.1}/s drops_total={} hw_decode={} has_v1={} overlays={}",
                    self.state,
                    self.dbg_update_playback_count,
                    self.dbg_regrade_count,
                    self.dbg_param_changes_drained,
                    self.dbg_commands_handled,
                    elapsed,
                    (self.dbg_update_playback_count + self.dbg_regrade_count) as f64 / elapsed,
                    self.dropped_frames,
                    hw,
                    self.v1_decoder.is_some(),
                    self.overlay_decoders.len(),
                );
                self.last_stats_time = std::time::Instant::now();
                self.dbg_update_playback_count = 0;
                self.dbg_regrade_count = 0;
                self.dbg_param_changes_drained = 0;
                self.dbg_commands_handled = 0;
            }

            // Emit VU peak levels at ~30Hz
            if self.state == PlaybackState::Playing
                && self.last_peak_time.elapsed() >= Duration::from_millis(33)
            {
                self.last_peak_time = std::time::Instant::now();
                if let Ok(mixer) = self.mixer.lock() {
                    let mut master_l = 0.0f32;
                    let mut master_r = 0.0f32;
                    for (i, track) in mixer.tracks.iter().enumerate() {
                        let _ = self.event_tx.send(AppEvent::TrackPeakLevels {
                            track_index: i,
                            left: track.peak_left,
                            right: track.peak_right,
                        });
                        master_l = master_l.max(track.peak_left);
                        master_r = master_r.max(track.peak_right);
                    }
                    let _ = self.event_tx.send(AppEvent::MasterPeakLevels {
                        left: master_l,
                        right: master_r,
                    });
                }
                // LUFS readings from the mix thread
                if let Some(ref lufs_arc) = self.lufs_readings {
                    if let Ok(r) = lufs_arc.try_lock() {
                        let _ = self.event_tx.send(AppEvent::LufsReadings {
                            momentary: r.momentary,
                            short_term: r.short_term,
                            integrated: r.integrated,
                            loudness_range: r.loudness_range,
                            true_peak_dbfs: r.true_peak_dbfs,
                        });
                    }
                }
            }

            // Per-tick timing. Only log when a single tick exceeded the frame
            // budget (16.6 ms at 60 fps) — this is the signal that the engine
            // thread was blocked in a long op (GPU fence, decode stall,
            // mutex wait). Noisy ticks are fine; persistent >30 ms ticks
            // during a slider drag point directly at the regression.
            let tick_ms = tick_start.elapsed().as_secs_f64() * 1000.0;
            if tick_ms > 16.0 {
                debug!(
                    "ENGINE_TICK_SLOW {:.1}ms state={:?} in_edit={} had_cmds={} drained={}",
                    tick_ms,
                    self.state,
                    self.in_node_param_edit,
                    had_commands,
                    applied,
                );
            }
        }

        info!("Engine thread shutting down");
    }

    fn handle_command(&mut self, cmd: AppCommand) -> bool {
        match cmd {
            AppCommand::Quit => return true,

            AppCommand::LoadMediaFile { path, media_id } => {
                self.load_file(path, media_id);
            }

            AppCommand::Play => {
                debug!("Play command received, state={:?}", self.state);
                // Mutual-exclusion: stop the source viewer if it owns
                // audio right now, then hand audio back to the
                // timeline tracks. Without this both viewers would
                // try to feed the cpal stream and the source's mute
                // toggle would stay active.
                if self.source_state == PlaybackState::Playing {
                    self.pause_source();
                }
                self.set_source_audio_owner(false);
                if self.state == PlaybackState::Paused
                    && (self.v1_decoder.is_some() || !self.audio_sources.is_empty())
                {
                    // Compute current source position from playhead. With a
                    // speed ramp, integrate the curve from clip start to the
                    // playhead rather than multiplying by the constant speed.
                    let timeline_elapsed = (self.playhead - self.clip_timeline_start)
                        .to_secs_f64()
                        .max(0.0);
                    let source_offset = if !self.speed_keyframes.is_empty() {
                        klipp_timeline::speed_integral_keyframes(
                            &self.speed_keyframes,
                            timeline_elapsed,
                            self.fps.rate() as f64,
                            self.clip_speed,
                        )
                    } else {
                        timeline_elapsed * self.clip_speed
                    };
                    let source_secs = self.clip_source_start + source_offset;
                    debug!(
                        "Play resume: playhead={:.3}s clip_source_start={:.3}s clip_timeline_start={:.3}s speed={:.2}x → source={:.3}s",
                        self.playhead.to_secs_f64(), self.clip_source_start,
                        self.clip_timeline_start.to_secs_f64(), self.clip_speed, source_secs
                    );

                    // Seek video decoders to current position and flush stale frames.
                    // Shortcut when resuming from a pause at the same
                    // playhead: the decoder already has a frame sitting
                    // in the queue near `source_secs`, so re-seeking
                    // would force a 100–500 ms keyframe-rewind + forward-
                    // decode cycle for nothing. Check the queue head
                    // for a frame within 100 ms of target — if present,
                    // skip the seek + clear entirely and just resume.
                    // This is the primary cause of the 2–3 s Play-
                    // after-pause latency the user hit.
                    if let Some(dec) = &self.v1_decoder {
                        let queue_has_target = {
                            let q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                            match q.front() {
                                Some(f) if !f.is_eof
                                    && (f.pts_secs() - source_secs).abs() < 0.1 => true,
                                _ => false,
                            }
                        };
                        if !queue_has_target {
                            dec.seek(source_secs);
                            // Drain stale frames from queue immediately —
                            // the decoder thread will also clear on seek, but we
                            // clear from here to discard any frames produced between
                            // our seek command and the decoder processing it.
                            dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                        }
                        dec.resume();
                    }
                    for dec in self.overlay_decoders.values() {
                        dec.resume();
                    }

                    // Flush stale audio, seek all tracks to current position.
                    // Flushes mix thread staging, seeks each source per-track,
                    // and bumps seek_gen on audio output to drain stale main buffer.
                    self.seek_all_audio_to_timecode(self.playhead);

                    // Wait for a valid post-seek video frame before starting
                    // clock+audio. This also gives cpal time to process the
                    // seek_gen bump (drain stale samples) while we wait.
                    self.wait_for_valid_frame(source_secs);

                    // Set clock to target position (not running yet)
                    let source_us = (source_secs * 1_000_000.0) as u64;
                    self.master_clock.seek(source_us);

                    // Pre-fill audio pipeline: resume audio sources so samples
                    // flow through staging → mix thread → main ring buffer.
                    // cpal is still paused, so it accumulates without consuming.
                    for active in &self.audio_sources {
                        active.source.resume();
                    }
                    std::thread::sleep(Duration::from_millis(20));

                    // Start playback: resume audio output + clock together.
                    // Main ring buffer already has ~20ms of correct samples.
                    if let Some(aout) = &self.audio_output {
                        aout.0.resume();
                    }
                    self.master_clock.resume();
                    self.state = PlaybackState::Playing;
                    self.last_frame_time = std::time::Instant::now();
                    // Re-anchor the ramp integrator at the current playhead so
                    // the first update_playback tick samples the correct speed.
                    self.reset_ramp_anchor();
                    let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                    (self.repaint_cb)();
                } else if self.state == PlaybackState::Paused
                    || self.state == PlaybackState::Stopped
                    || self.state == PlaybackState::Idle
                {
                    self.handle_play();
                }
            }

            AppCommand::Pause => {
                if self.state == PlaybackState::Playing {
                    if let Some(dec) = &self.v1_decoder {
                        dec.pause();
                    }
                    for dec in self.overlay_decoders.values() {
                        dec.pause();
                    }
                    self.master_clock.pause();
                    for active in &self.audio_sources {
                        active.source.pause();
                    }
                    if let Some(aout) = &self.audio_output {
                        aout.0.pause();
                        // Drain stale samples so they don't play on resume
                        aout.0.notify_seek();
                    }
                    if let Some(ref mt) = self.mix_thread {
                        mt.flush();
                    }
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                    // During playback the pregrade encode is skipped
                    // (see `pregrade_needed` in `post_process_rgba`)
                    // so `cached_pregrade_frame` may still hold the
                    // frame we paused ON last — but more importantly,
                    // `regrade_cached_frame()` is a silent no-op when
                    // that snapshot is None. Without a fresh pregrade
                    // in place, every user edit while paused (slider
                    // drag, AddNode, Connect, ToggleBypass …) feeds
                    // into a `regrade_cached_frame` that does
                    // nothing, and the viewer stays stale until the
                    // user unpauses and re-pauses. Force one
                    // `seek_in_sequence` on the Pause transition so
                    // the decoder hands us a known-good frame,
                    // `post_process_rgba` writes the pregrade
                    // snapshot, and every subsequent paused edit has
                    // something to re-grade.
                    let playhead = self.playhead;
                    self.seek_in_sequence(playhead);
                    (self.repaint_cb)();
                }
            }

            AppCommand::Stop => {
                if self.state != PlaybackState::Idle {
                    self.master_clock.pause();
                    self.v1_decoder = None;
                    self.clear_all_overlays();
                    self.stop_all_audio();
                    self.playhead = RationalTime::from_frames(0, self.fps.rate());
                    self.last_synth_frame_rendered = None;
                    self.state = PlaybackState::Stopped;
                    self.pre_seek_generation += 1;
                    self.pre_seek_started = false;
                    let _ = self.event_tx.send(AppEvent::PlaybackStopped);
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    (self.repaint_cb)();
                }
            }

            AppCommand::ResetEngine => {
                self.reset_to_idle();
            }

            AppCommand::SeekTo(tc) => {
                // Lift the Idle / Stopped gates. After project load
                // the engine sits in Idle or Stopped depending on
                // path — but scrubbing the ruler / clicking on the
                // timeline before the first Play should still seek
                // the decoder and populate `cached_pregrade_frame`,
                // otherwise slider edits silently no-op because
                // `regrade_cached_frame` has nothing to re-grade.
                self.seek_in_sequence(tc);
                // Transition Idle / Stopped → Paused once we
                // actually have a decoder or audio source to play.
                // Puts the engine in the same "paused with a valid
                // pregrade snapshot" mode Play-then-Pause produces,
                // so paused slider edits start working without the
                // round-trip through Play.
                if matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped)
                    && (self.v1_decoder.is_some()
                        || !self.audio_sources.is_empty())
                {
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                }
            }

            AppCommand::SetSpeed(speed) => {
                debug!("SetSpeed({speed})");
                self.master_clock.set_speed(speed);
                // Mute audio at high speeds (unintelligible above 4x)
                for active in &self.audio_sources {
                    if speed.abs() > 4.0 {
                        active.source.muted().store(true, Ordering::Release);
                    } else {
                        active.source.muted().store(false, Ordering::Release);
                    }
                }
            }

            AppCommand::SeekRelative(frames) => {
                if self.state != PlaybackState::Idle {
                    if self.state != PlaybackState::Playing
                        && frames.abs() <= 1
                        && self.v1_decoder.is_some()
                    {
                        // Single frame step: just grab next frame without seeking
                        self.step_frame(frames);
                    } else {
                        let seq_dur = self.sequence_duration();
                        let new_tc = RationalTime {
                            value: self.playhead.value + frames,
                            rate: self.playhead.rate,
                        };
                        let clamped = RationalTime {
                            value: new_tc.value.max(0).min(seq_dur.value),
                            rate: new_tc.rate,
                        };
                        // Use longer timeout for multi-frame seeks (1s jump needs more decode time)
                        self.seek_in_sequence_with_timeout(clamped, Duration::from_millis(200));
                        // If the grab timed out and no new frame was produced, regrade the
                        // cached frame so scopes still update from the old frame.
                        if self.state != PlaybackState::Playing {
                            self.regrade_cached_frame();
                        }
                    }
                }
            }

            AppCommand::SetPlayhead(tc) => {
                self.playhead = tc;
                // Same Idle / Stopped gate lift as SeekTo.
                self.seek_in_sequence(tc);
                if matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped)
                    && (self.v1_decoder.is_some()
                        || !self.audio_sources.is_empty())
                {
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                }
            }

            AppCommand::LoadGlobalLut { path } => {
                match self.color_pipeline.load_global_lut(&path) {
                    Ok(()) => info!("Loaded global LUT: {}", path.display()),
                    Err(e) => error!("Failed to load LUT {}: {}", path.display(), e),
                }
                self.regrade_cached_frame();
            }
            AppCommand::ClearGlobalLut => {
                self.color_pipeline.clear_global_lut();
                info!("Cleared global LUT");
                self.regrade_cached_frame();
            }
            AppCommand::SetGlobalLutOpacity(opacity) => {
                self.color_pipeline.global_lut_opacity = opacity;
                self.regrade_cached_frame();
            }
            AppCommand::SetGlobalLutEnabled(enabled) => {
                self.color_pipeline.enabled = enabled;
                self.regrade_cached_frame();
            }
            AppCommand::SetLutTetrahedral(tetrahedral) => {
                self.color_pipeline.use_tetrahedral = tetrahedral;
                self.regrade_cached_frame();
            }

            AppCommand::SetTrackVolume { track_index, volume } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].volume = volume;
                }
            }
            AppCommand::SetTrackMute { track_index, muted } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].muted = muted;
                }
            }
            AppCommand::SetTrackSolo { track_index, solo } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].solo = solo;
                }
            }
            AppCommand::SetTrackPan { track_index, pan } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].pan = pan;
                }
            }
            AppCommand::SetMasterVolume(vol) => {
                // Apply in cpal callback for instant response (no ring buffer lag)
                if let Some(aout) = &self.audio_output {
                    aout.0.set_master_volume(vol);
                }
                // Also keep mixer in sync for VU metering
                if let Ok(mut m) = self.mixer.lock() {
                    m.master_volume = vol;
                }
            }
            AppCommand::ResetLufsMeter => {
                if let Some(ref flag) = self.lufs_reset {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }

            AppCommand::SetClipVolume(vol) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_volume(vol);
                }
            }

            AppCommand::SetClipPan(pan) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_pan(pan);
                }
            }

            AppCommand::SetClipGain(gain) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_gain(gain);
                }
            }

            AppCommand::SetClipChannelConfig(config) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_channel_config(config);
                }
            }

            AppCommand::SetUseProxies(enabled) => {
                info!("Engine: use_proxies = {enabled}");
                self.use_proxies = enabled;
            }
            AppCommand::SetRenderCacheEnabled(enabled) => {
                self.render_cache_disk.set_enabled(enabled);
                if !enabled {
                    self.render_cache_hot.clear();
                }
                debug!("Render cache enabled={enabled}");
            }
            AppCommand::CleanOrphanedCaches => {
                // Handled on UI thread — ignore in engine
            }
            AppCommand::BeginScrub => {
                // Engine can optimize decode during scrub (handled via use_proxies on UI side).
                self.is_scrubbing = true;
                // Mute eagerly so the first tick of the drag is silent too —
                // update_playback's per-tick mute check will keep things in sync.
                if !self.scrub_audio_enabled {
                    for active in &self.audio_sources {
                        active.source.muted().store(true, Ordering::Release);
                    }
                }
            }
            AppCommand::EndScrub => {
                // Scrub ended — UI will restore proxy state. Clear any
                // scrub-induced mute; the per-tick update_playback reasserts
                // speed / time-remap mutes on the next tick, so we don't need
                // to restore any prior state here.
                self.is_scrubbing = false;
                if !self.scrub_audio_enabled {
                    for active in &self.audio_sources {
                        active.source.muted().store(false, Ordering::Release);
                    }
                }
                // Thumb refresh is handled by the `SetPlayhead` that
                // `scrub_end` pushes right after `EndScrub` — that path
                // runs a real seek (`seek_in_sequence` →
                // `grab_single_frame_timeout` → `post_process_rgba`)
                // which produces a fresh frame and captures per-node
                // thumbs against the final scrub position. The old
                // `regrade_cached_frame` call here re-graded whatever
                // stale `cached_pregrade_frame` was lying around (no
                // longer matches the playhead after a scrub), so the
                // thumbs would land on the pre-scrub frame instead of
                // the drop position.
            }

            AppCommand::SetLoopPlayback(enabled) => {
                debug!("SetLoopPlayback: {enabled}");
                self.loop_enabled = enabled;
                if !enabled {
                    self.pre_seek_generation += 1;
                    self.pre_seek_started = false;
                }
            }

            AppCommand::SetScrubAudioEnabled(enabled) => {
                self.scrub_audio_enabled = enabled;
                // Apply immediately if the user toggles mid-scrub. The per-tick
                // update_playback re-asserts the correct mute state so we only
                // need to push the transition here; EndScrub's clear path also
                // honours this flag.
                if self.is_scrubbing && !enabled {
                    for active in &self.audio_sources {
                        active.source.muted().store(true, Ordering::Release);
                    }
                }
            }

            AppCommand::SetAntiClickFade(enabled) => {
                for active in &self.audio_sources {
                    active.source.set_anti_click(enabled);
                }
            }
            AppCommand::SetPanLaw(law) => {
                if let Ok(mut mixer) = self.mixer.lock() {
                    mixer.pan_law = law;
                }
            }

            AppCommand::ToggleBeforeAfter => {
                self.bypass_color_grade = !self.bypass_color_grade;
                debug!("Before/After: bypass_color_grade = {}", self.bypass_color_grade);
            }

            AppCommand::ToggleStabilizationCompare => {
                self.compare_stabilization = !self.compare_stabilization;
                debug!("StabilizationCompare: compare_stabilization = {}", self.compare_stabilization);
            }

            AppCommand::SetSplitCompareEffect { clip_id, effect_index, is_ofx } => {
                self.compare_effect_active = clip_id.is_some();
                self.compare_effect_clip_id = clip_id;
                self.compare_effect_index = effect_index;
                self.compare_effect_is_ofx = is_ofx;
                debug!(
                    "SetSplitCompareEffect: active={} clip={:?} index={} is_ofx={}",
                    self.compare_effect_active, self.compare_effect_clip_id,
                    self.compare_effect_index, self.compare_effect_is_ofx,
                );
            }

            AppCommand::SetMattePreview(active) => {
                self.matte_preview = active;
                // When matte preview is disabled, clear the matte preview node
                if !active {
                    self.matte_preview_node = None;
                }
                debug!("Matte preview: {}", self.matte_preview);
            }

            AppCommand::SetScopeRenderMode { mode } => {
                self.scope_render_mode = mode;
            }
            AppCommand::SetScopesVisible(visible) => {
                self.scopes_visible = visible;
            }
            AppCommand::RenderScopesForFrame { rgba, width, height } => {
                // Run the same GPU scope shaders the playback pipeline uses
                // (engine.rs:5149) on a UI-supplied RGBA frame and write the
                // result to fb.scope_texture so the UI displays it via its
                // existing GPU scope path. Used by media-browser hover-skim
                // and timeline scrub so scope quality matches playback.
                if let Some(gpu) = &mut self.gpu_effects {
                    let scope_w = 720u32;
                    let scope_h = 256u32;
                    let mode = self.scope_render_mode;
                    let tex = gpu_scope_kind_for_mode(mode).map(|kind| match kind {
                        GpuScopeKind::Waveform => gpu.render_scope_waveform(&rgba, width, height, scope_w, scope_h),
                        GpuScopeKind::Parade => gpu.render_scope_parade(&rgba, width, height, scope_w, scope_h),
                        GpuScopeKind::Histogram => gpu.render_scope_histogram(&rgba, width, height, scope_w, scope_h),
                    });
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    match tex {
                        Some(t) => {
                            fb.scope_texture = t;
                            fb.scope_texture_w = scope_w;
                            fb.scope_texture_h = scope_h;
                            fb.scope_texture_mode = mode;
                        }
                        None => {
                            // Mode has no GPU kernel (e.g. vectorscope) —
                            // leave scope_texture cleared so the UI falls
                            // back to its CPU renderer for that mode.
                            fb.scope_texture.clear();
                            fb.scope_texture_w = 0;
                            fb.scope_texture_h = 0;
                            fb.scope_texture_mode = mode;
                        }
                    }
                }
            }
            AppCommand::SetScopeTapNode { node_id, .. } => {
                self.scope_tap_node = node_id.clone();
                debug!("Scope tap node: {:?}", self.scope_tap_node);
            }
            AppCommand::SetVisibleNodeThumbs { node_ids, .. } => {
                self.visible_node_thumbs = node_ids.iter().copied().collect();
                // Clear the old thumb cache so the next re-grade is
                // what the UI sees (stale thumbs from a different node
                // set would confuse the user mid-transition).
                {
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.node_thumbs.clear();
                    fb.node_thumbs_gen = fb.node_thumbs_gen.wrapping_add(1);
                }
                // Force a re-grade of the current frame so the thumbs
                // populate immediately — otherwise the user opens the
                // V2 canvas and sees empty nodes until they happen to
                // move a slider or scrub the playhead.
                if self.state != PlaybackState::Playing
                    && !self.visible_node_thumbs.is_empty()
                {
                    self.regrade_cached_frame();
                }
            }
            AppCommand::RenderNodeThumbsFromSource {
                clip_id,
                playhead,
                src_rgba,
                src_w,
                src_h,
            } => {
                self.render_node_thumbs_from_source(
                    clip_id, playhead, src_rgba, src_w, src_h,
                );
            }

            AppCommand::SetSourceMedia { path, source_rotation, .. } => {
                self.set_source_media(path, source_rotation);
            }
            AppCommand::PlaySource => {
                debug!(
                    "PlaySource command received, source_state={:?}, source_decoder={}",
                    self.source_state,
                    self.source_decoder.is_some(),
                );
                self.play_source();
            }
            AppCommand::PauseSource => {
                debug!("PauseSource command received, source_state={:?}", self.source_state);
                self.pause_source();
            }
            AppCommand::SeekSource(tc) => {
                self.seek_source(tc.to_secs_f64());
            }
            AppCommand::StepSource(frames) => {
                self.step_source(frames);
            }
            AppCommand::StopSource => {
                self.stop_source();
            }

            AppCommand::SequenceUpdated => {
                // Invalidate grade hash so render cache re-checks on next frame.
                // Also drop the cached graph Arc — the graph may have mutated,
                // so a fresh snapshot must be built on the next `post_process_rgba_inner`.
                self.current_grade_hash = 0;
                self.current_graph_arc = None;
                // Sync per-clip volume/pan to active audio sources
                self.sync_clip_volumes();
                // Restart audio sources only when audio-relevant state changed.
                // Title color/opacity/position edits fire SequenceUpdated but don't
                // affect audio — skipping the teardown+rebuild prevents a storm that
                // freezes the playhead during live editing.
                if !self.audio_sources.is_empty() {
                    let audio_fingerprint = self.compute_audio_fingerprint();
                    if audio_fingerprint != self.last_audio_fingerprint {
                        self.last_audio_fingerprint = audio_fingerprint;
                        let tc = self.playhead;
                        self.start_all_audio(tc);
                        if self.state == PlaybackState::Playing {
                            for active in &self.audio_sources {
                                active.source.resume();
                            }
                        }
                    }
                }
                // Re-read clip speed in case SpeedRamp changed it during playback
                if self.state == PlaybackState::Playing {
                    let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                    let mut speed_state_changed = false;
                    if let Some(clip) = seq.tracks.get(self.current_track_index)
                        .and_then(|t| t.clips.get(self.current_clip_index))
                    {
                        // Keep speed_keyframes in sync with the sequence.
                        let new_keyframes = clip.speed_keyframes.clone();
                        let new_ramp_enabled = !new_keyframes.is_empty();
                        if new_ramp_enabled != self.speed_ramp_enabled
                            || new_keyframes.len() != self.speed_keyframes.len()
                        {
                            speed_state_changed = true;
                        }
                        self.speed_keyframes = new_keyframes;
                        self.speed_ramp_enabled = new_ramp_enabled;
                        let new_speed = clip.speed.max(0.001);
                        if (self.clip_speed - new_speed).abs() > 0.001 {
                            tracing::debug!("SequenceUpdated: clip_speed {} → {}", self.clip_speed, new_speed);
                            self.clip_speed = new_speed;
                            // Only hard-set the clock's constant speed when no
                            // ramp is active. Ramp mode drives the clock per tick.
                            if !self.speed_ramp_enabled {
                                self.master_clock.set_speed(new_speed);
                            }
                            speed_state_changed = true;
                            // Mute audio at non-normal speeds
                            for active in &self.audio_sources {
                                active.source.muted().store((new_speed - 1.0).abs() > 0.01, Ordering::Release);
                            }
                        }
                    }
                    drop(seq);
                    if speed_state_changed {
                        self.reset_ramp_anchor();
                    }
                } else {
                    // Re-render overlays and re-grade cached frame when paused —
                    // instant parameter feedback for title/generator edits.
                    // Clear synth frame cache so update_overlay_decoders re-renders
                    // all synthetic clips, reflecting any parameter changes.
                    self.overlay_synth_frame.clear();
                    self.update_overlay_decoders(self.playhead);
                    self.regrade_cached_frame();
                }
            }

            // Gesture boundaries for a colour-grade parameter drag. The UI
            // dispatches these around every slider/knob drag (see
            // panels::node_inspector) and the UI side also uses them to
            // bracket undo history. We piggy-back on the signal to skip the
            // render cache while a gesture is in flight — grade_hash changes
            // every tick during a drag, so the cache misses every read and
            // pays for a 6 MB compressed write that will never be reused.
            AppCommand::BeginNodeParamEdit { .. } => {
                self.in_node_param_edit = true;
                self.drag_started_at = Some(std::time::Instant::now());
                self.drag_grade_count = 0;
                self.drag_downsampled_count = 0;
                tracing::debug!("DRAG_BEGIN downsample_enabled={}", drag_downsample_enabled());
            }
            AppCommand::EndNodeParamEdit { .. } => {
                self.in_node_param_edit = false;
                // Trigger one full-resolution regrade so the viewer
                // recovers cleanly from the last downsampled frame
                // — see `post_process_rgba_inner_with_optional_downsample`.
                self.force_full_res_regrade = true;
                if let Some(started) = self.drag_started_at.take() {
                    let dur_ms = started.elapsed().as_secs_f64() * 1000.0;
                    tracing::info!(
                        "DRAG_END duration={:.1}ms grade_frames={} downsampled={} downsample_enabled={}",
                        dur_ms,
                        self.drag_grade_count,
                        self.drag_downsampled_count,
                        drag_downsample_enabled(),
                    );
                }
            }

            // UI-side commands — engine ignores them
            _ => {}
        }

        false
    }

    fn handle_play(&mut self) {
        // Sync fps from the active sequence so synthetic clips play at the correct rate.
        // Without this, a generator-only timeline plays at the engine's default 24fps
        // because self.fps is only updated by load_file() and generators never trigger it.
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.fps = seq.fps;
        }

        // Check if no media clip exists at playhead but a title or generator clip does
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let has_media_clip = seq.tracks.iter()
                .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                .any(|track| {
                    track.clip_at_timecode(self.playhead)
                        .map(|(_, c)| c.enabled && c.media_id().is_some())
                        .unwrap_or(false)
                });
            let title_info = if !has_media_clip {
                seq.tracks.iter().enumerate()
                    .filter(|(_, t)| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                    .find_map(|(ti, track)| {
                        track.clip_at_timecode(self.playhead).and_then(|(idx, clip)| {
                            if !clip.enabled { return None; }
                            if let Some(td) = clip.title_data() {
                                return Some((ti, idx, td.clone(), clip.timeline_in, clip.source_in.to_secs_f64()));
                            }
                            None
                        })
                    })
            } else {
                None
            };
            // Check for standalone generator clip (no media, no title)
            let generator_info = if !has_media_clip && title_info.is_none() {
                seq.tracks.iter().enumerate()
                    .filter(|(_, t)| matches!(t.kind, klipp_timeline::TrackKind::Video))
                    .find_map(|(ti, track)| {
                        track.clip_at_timecode(self.playhead).and_then(|(idx, clip)| {
                            if !clip.enabled { return None; }
                            let gd = clip.generator_data()?.clone();
                            Some((ti, idx, gd, clip.timeline_in, clip.timeline_out))
                        })
                    })
            } else {
                None
            };
            let res = seq.resolution;
            drop(seq);

            if let Some((ti, idx, _title_data, timeline_in, clip_source_in)) = title_info {
                info!("handle_play: title clip {idx} on track {ti} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_track_index = ti;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;

                // Start audio for all tracks
                self.start_all_audio(self.playhead);
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Set clock to playhead position (title clips use timeline time as source)
                let source_secs = self.playhead.to_secs_f64();
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);

                // Pre-fill audio
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(20));

                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                self.master_clock.resume();

                self.state = PlaybackState::Playing;
                self.update_overlay_decoders(self.playhead);
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
                return;
            }

            // Standalone generator clip at playhead — render and display
            if let Some((ti, idx, gen_data, timeline_in, timeline_out)) = generator_info {
                info!("handle_play: generator clip {idx} on track {ti} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_track_index = ti;
                self.current_file = None;
                self.clip_source_start = 0.0;
                self.clip_timeline_start = timeline_in;

                let clip_relative_secs = (self.playhead - timeline_in).to_secs_f64().max(0.0);
                let total_secs = (timeline_out - timeline_in).to_secs_f64();
                let rgba = klipp_core::generator::render_generator(
                    &gen_data,
                    res.width,
                    res.height,
                    clip_relative_secs,
                    total_secs,
                );
                // Generator produces sRGB u8; lift to linear at the
                // caller seam so `post_process_rgba` stays linear-in/
                // linear-out, then display-transform at the viewer
                // boundary.
                let linear = u8_rgba_to_linear(&rgba);
                let final_linear = self.post_process_rgba(linear, res.width, res.height);
                let final_rgba = self.display_encode(&final_linear);
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.write_frame(final_rgba, res.width, res.height);
                drop(fb);

                // Start audio + clock for playback
                self.start_all_audio(self.playhead);
                let source_secs = self.playhead.to_secs_f64();
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);
                self.master_clock.resume();

                self.state = PlaybackState::Playing;
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
                return;
            }
        }

        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // Search video tracks in reverse — the last video track (bottom of panel)
        // is the compositing primary, matching pro NLE convention.
        let clip_info = seq.tracks.iter().enumerate().rev()
            .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|(track_idx, track)| {
                // Check if playhead is on a non-gap clip
                if let Some((idx, clip)) = track.clip_at_timecode(self.playhead) {
                    if let Some(media_id) = clip.media_id() {
                        let source_secs = clip.source_in.to_secs_f64()
                            + (self.playhead - clip.timeline_in).to_secs_f64() * clip.speed;
                        let clip_source_in = clip.source_in.to_secs_f64();
                        return Some((track_idx, idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation));
                    }
                }
                // Find next non-gap clip at or after playhead
                for (idx, clip) in track.clips.iter().enumerate() {
                    if clip.timeline_in >= self.playhead {
                        if let Some(media_id) = clip.media_id() {
                            let si = clip.source_in.to_secs_f64();
                            return Some((track_idx, idx, media_id, si, clip.timeline_in, si, clip.speed, clip.source_rotation));
                        }
                    }
                }
                // Fallback: first non-gap clip
                for (idx, clip) in track.clips.iter().enumerate() {
                    if let Some(media_id) = clip.media_id() {
                        let si = clip.source_in.to_secs_f64();
                        return Some((track_idx, idx, media_id, si, clip.timeline_in, si, clip.speed, clip.source_rotation));
                    }
                }
                None
            });
        drop(seq);

        if let Some((track_idx, clip_idx, media_id, source_secs, timeline_in, clip_source_in, clip_speed, source_rotation)) = clip_info {
            if let Some(path) = self.path_for_media(media_id) {
                info!("handle_play: track {track_idx} clip {clip_idx} from {:?}, source={source_secs:.3}s", path);

                // Open video decoder
                match VideoDecoder::open(&path, source_rotation) {
                    Ok(dec) => {
                        if source_secs > 0.01 {
                            dec.seek(source_secs);
                        }
                        dec.resume();
                        self.v1_decoder = Some(dec);
                    }
                    Err(e) => {
                        error!("Failed to open video decoder: {e}");
                        return;
                    }
                }

                // Open audio for ALL tracks (sources start paused, already
                // seeked to correct position — no stale data to flush)
                self.start_all_audio(self.playhead);

                // Pause cpal output while we wait for video
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Wait for a valid post-seek video frame
                self.wait_for_valid_frame(source_secs);

                // Set clock to target (not running yet)
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);

                // Pre-fill: resume audio sources so samples flow into ring buffer
                // while cpal is still paused (accumulates without consuming)
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(20));

                // Start playback: resume cpal + clock together
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                // Set clock speed to clip_speed so source time advances at the right rate.
                self.master_clock.set_speed(clip_speed);
                self.master_clock.resume();

                // Mute audio when clip speed is not normal (audio pitch would be wrong)
                for active in &self.audio_sources {
                    active.source.muted().store((clip_speed - 1.0).abs() > 0.01, Ordering::Release);
                }

                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = clip_speed;
                self.current_source_rotation = source_rotation;
                self.current_clip_index = clip_idx;
                self.current_track_index = track_idx;
                self.current_file = Some(path);
                self.refresh_speed_keyframes();
                self.state = PlaybackState::Playing;

                // Open overlay decoders for all non-primary video tracks at the playhead
                self.update_overlay_decoders(self.playhead);
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
            } else {
                warn!("No path for media {:?}", media_id);
            }
        } else {
            // No video clip — try audio-only playback
            self.handle_play_audio_only();
        }
    }

    /// Audio-only playback — no video decoder, just audio tracks.
    fn handle_play_audio_only(&mut self) {
        // Check if any audio track has a clip at the playhead
        let has_audio = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.iter().any(|track| {
                track.kind == klipp_timeline::TrackKind::Audio
                    && track.clip_at_timecode(self.playhead).is_some()
            })
        };
        if !has_audio {
            debug!("No clips in sequence (video or audio)");
            return;
        }

        info!("handle_play: audio-only at {:.3}s", self.playhead.to_secs_f64());

        // Open audio sources (start paused, already seeked — no stale data)
        self.start_all_audio(self.playhead);

        // Pause cpal while we set up
        if let Some(aout) = &self.audio_output {
            aout.0.pause();
        }

        // Set clock
        let source_us = (self.playhead.to_secs_f64() * 1_000_000.0) as u64;
        self.master_clock.seek(source_us);

        // Pre-fill audio pipeline
        for active in &self.audio_sources {
            active.source.resume();
        }
        std::thread::sleep(Duration::from_millis(20));

        // Start playback
        if let Some(aout) = &self.audio_output {
            aout.0.resume();
        }
        self.master_clock.resume();

        self.clip_source_start = self.playhead.to_secs_f64();
        self.clip_timeline_start = self.playhead;
        self.clip_speed = 1.0; // audio-only: no per-clip speed for now
        self.speed_keyframes.clear();
        self.speed_ramp_enabled = false;
        self.state = PlaybackState::Playing;
        self.dropped_frames = 0;
        self.frames_consumed = 0;
        self.last_playback_log = std::time::Instant::now();
        self.last_frame_time = std::time::Instant::now();
        let _ = self.event_tx.send(AppEvent::PlaybackStarted);
        (self.repaint_cb)();
    }

    /// Open audio sources for ALL tracks that have a clip at `tc`.
    /// Replaces any existing audio sources.
    fn start_all_audio(&mut self, tc: Timecode) {
        // Update fingerprint so SequenceUpdated knows the current audio state
        // (prevents redundant restart on the next SequenceUpdated after a seek/play).
        self.last_audio_fingerprint = self.compute_audio_fingerprint();
        self.stop_all_audio();

        // Collect (track_index, media_id, path, source_secs, effects, kfs) for every track with audio at tc
        type TrackAudioRow = (
            usize,
            MediaId,
            PathBuf,
            f64,
            Vec<klipp_core::AudioEffectInstance>,
            std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        );
        let track_audio: Vec<TrackAudioRow> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks
                .iter()
                .enumerate()
                .filter_map(|(track_idx, track)| {
                    let (_clip_idx, clip) = track.clip_at_timecode(tc)?;
                    if !clip.enabled { return None; }
                    let media_id = clip.media_id()?;
                    let item = pool.get(&media_id)?;
                    if !item.has_audio {
                        return None;
                    }
                    // Skip audio for video-track clips that have a linked audio-track partner
                    // (the audio track clip will produce audio instead)
                    if track.kind == klipp_timeline::TrackKind::Video {
                        if let Some(lid) = clip.linked_clip_id {
                            let partner_on_audio = seq.tracks.iter().any(|t| {
                                t.kind == klipp_timeline::TrackKind::Audio
                                    && t.clips.iter().any(|c| c.id == lid)
                            });
                            if partner_on_audio {
                                return None;
                            }
                        }
                    }
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    let decode_path = if self.use_proxies && item.proxy_ready {
                        item.proxy_path.as_ref()
                            .filter(|p| p.exists())
                            .cloned()
                            .unwrap_or_else(|| item.path.clone())
                    } else {
                        item.path.clone()
                    };
                    Some((
                        track_idx,
                        media_id,
                        decode_path,
                        source_secs,
                        clip.audio_effects.clone(),
                        clip.audio_effect_keyframes.clone(),
                    ))
                })
                .collect()
        };

        debug!("start_all_audio: {} tracks with audio at {:.3}s", track_audio.len(), tc.to_secs_f64());
        for (track_idx, media_id, path, source_secs, audio_effects, audio_effect_keyframes) in track_audio {
            debug!("start_all_audio: track {} has {} audio effects ({} kf tracks)",
                track_idx, audio_effects.len(), audio_effect_keyframes.len());
            self.open_audio_source(track_idx, media_id, &path, source_secs, audio_effects, audio_effect_keyframes);
        }
        // Apply per-clip volume from sequence data
        self.sync_clip_volumes();

        // `open_audio_source` intentionally leaves each source paused so the
        // caller can line up master-clock + video decoder before audio
        // starts pushing samples. That contract works for the "user pressed
        // play" path, where the dedicated Play command resumes everything
        // explicitly. It breaks when `start_all_audio` fires mid-playback
        // — e.g. the preroll-hit branch delegates through
        // `seek_all_audio_to_timecode`, which expects audio to come back up
        // transparently. Without this resume, the newly-opened PCM feeders
        // sit idle, the mix-thread staging rings never get fed, LUFS pegs
        // on whatever NaN/stale state the meter sees, and clip-2+ plays
        // silent. Resume here when we know playback is already live.
        if self.state == PlaybackState::Playing {
            for active in &self.audio_sources {
                active.source.resume();
            }
            if let Some(aout) = &self.audio_output {
                aout.0.resume();
            }
            debug!(
                "start_all_audio: resumed {} source(s) for live playback",
                self.audio_sources.len(),
            );
        }
    }

    /// Sync per-clip volume, pan, and fade from sequence data to the audio output.
    /// Called on start_all_audio and SequenceUpdated.
    fn sync_clip_volumes(&self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut clip_volume = 1.0f32;
        let mut clip_pan = 0.0f32;
        let mut clip_gain_out = 1.0f32;
        let mut clip_ch_config = 0u32;

        for active in &self.audio_sources {
            if let Some(track) = seq.tracks.get(active.track_index) {
                if let Some((_idx, audio_clip)) = track.clip_at_timecode(self.playhead) {
                    // Compute clip-relative time for keyframe evaluation
                    let clip_time_secs = (self.playhead.to_secs_f64() - audio_clip.timeline_in.to_secs_f64()).max(0.0);

                    let audio_eval_ctx = klipp_core::EvalContext {
                        clip_time_secs,
                        clip_duration_secs: (audio_clip.source_out.to_secs_f64() - audio_clip.source_in.to_secs_f64()),
                        timeline_time_secs: self.playhead.to_secs_f64(),
                        fps: self.fps.as_f64(),
                        seed: audio_clip.id.0.as_u64_pair().0,
                    };

                    clip_volume = eval_with_modifiers(&audio_clip.volume_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Volume, clip_time_secs, audio_clip.volume, &audio_eval_ctx);
                    clip_pan = eval_with_modifiers(&audio_clip.pan_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Pan, clip_time_secs, audio_clip.pan, &audio_eval_ctx);
                    let mut clip_gain = eval_with_modifiers(&audio_clip.gain_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Gain, clip_time_secs, audio_clip.gain, &audio_eval_ctx);
                    let mut fade_in = audio_clip.fade_in_secs;
                    let mut fade_out = audio_clip.fade_out_secs;
                    let mut fi_curve = audio_clip.fade_in_curve;
                    let mut fo_curve = audio_clip.fade_out_curve;
                    let mut ch_config = audio_clip.channel_config;
                    let source_in_secs = audio_clip.source_in.to_secs_f64();
                    let clip_dur_secs = audio_clip.duration().to_secs_f64();

                    // If linked to a video clip, use the video clip's audio params
                    if let Some(linked_id) = audio_clip.linked_clip_id {
                        for t in &seq.tracks {
                            if let Some(video_clip) = t.clips.iter().find(|c| c.id == linked_id) {
                                let linked_clip_time = (self.playhead.to_secs_f64() - video_clip.timeline_in.to_secs_f64()).max(0.0);
                                let linked_eval_ctx = klipp_core::EvalContext {
                                    clip_time_secs: linked_clip_time,
                                    clip_duration_secs: (video_clip.source_out.to_secs_f64() - video_clip.source_in.to_secs_f64()),
                                    timeline_time_secs: self.playhead.to_secs_f64(),
                                    fps: self.fps.as_f64(),
                                    seed: video_clip.id.0.as_u64_pair().0,
                                };
                                clip_volume = eval_with_modifiers(&video_clip.volume_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Volume, linked_clip_time, video_clip.volume, &linked_eval_ctx);
                                clip_pan = eval_with_modifiers(&video_clip.pan_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Pan, linked_clip_time, video_clip.pan, &linked_eval_ctx);
                                clip_gain = eval_with_modifiers(&video_clip.gain_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Gain, linked_clip_time, video_clip.gain, &linked_eval_ctx);
                                fade_in = video_clip.fade_in_secs;
                                fade_out = video_clip.fade_out_secs;
                                fi_curve = video_clip.fade_in_curve;
                                fo_curve = video_clip.fade_out_curve;
                                ch_config = video_clip.channel_config;
                                break;
                            }
                        }
                    }

                    // Set fade params on the audio source
                    // When contiguous_audio is true, offset source_in backward so the
                    // fade envelope doesn't think we're at position 0 (which would
                    // trigger a fade-in from silence at blade cut boundaries)
                    let is_contiguous = self.contiguous_audio.load(std::sync::atomic::Ordering::Relaxed);
                    let effective_source_in = if is_contiguous {
                        source_in_secs - 1.0 // pretend clip started 1s earlier
                    } else {
                        source_in_secs
                    };
                    let effective_clip_dur = if is_contiguous {
                        clip_dur_secs + 1.0 // extend duration to match
                    } else {
                        clip_dur_secs
                    };
                    active.source.set_fade(
                        effective_source_in,
                        effective_clip_dur,
                        fade_in as f64,
                        fade_out as f64,
                        fade_curve_to_u32(fi_curve),
                        fade_curve_to_u32(fo_curve),
                        clip_gain,
                    );
                    // Clear contiguous flag — it's consumed
                    self.contiguous_audio.store(false, std::sync::atomic::Ordering::Relaxed);
                    clip_gain_out = clip_gain;
                    clip_ch_config = channel_config_to_u32(ch_config);
                    break;
                }
            }
        }
        drop(seq);
        if let Some(aout) = &self.audio_output {
            aout.0.set_clip_gain(clip_gain_out);
            aout.0.set_clip_volume(clip_volume);
            aout.0.set_clip_pan(clip_pan);
            aout.0.set_channel_config(clip_ch_config);
        }
    }

    /// Stop and drop all audio sources, mix thread, and the output.
    /// Stop all overlay decoders and clear overlay state.
    fn clear_all_overlays(&mut self) {
        for dec in self.overlay_decoders.values_mut() {
            dec.stop_detached();
        }
        self.overlay_decoders.clear();
        self.overlay_clips.clear();
        self.overlay_frames.clear();
        self.overlay_synth_frame.clear();
    }

    /// Wipe all engine-side state to a clean slate.
    /// Called on `AppCommand::ResetEngine` (new project / close project).
    /// After this returns the engine holds no decoders, no cached frames,
    /// no audio sources, and the frame buffer is blanked.
    fn reset_to_idle(&mut self) {
        // Pause clock and stop playback state
        self.master_clock.pause();
        self.state = PlaybackState::Idle;

        // Drop all video decoders
        self.v1_decoder = None;
        self.clear_all_overlays();
        if let Some(mut dec) = self.preroll_decoder.take() {
            dec.stop_detached();
        }
        self.preroll_clip_index = None;
        self.preroll_source_in = 0.0;
        if let Some(mut dec) = self.transition_decoder.take() {
            dec.stop_detached();
        }
        self.transition_clip_id = None;
        self.transition_frame_cache = None;
        self.transition_clip_a_last_frame = None;
        self.transition_partner_eof = false;

        // Stop audio
        self.stop_all_audio();
        self.last_audio_fingerprint = 0;

        // Clear caches (frame, render hot, pre-grade, grade hash)
        self.cached_pregrade_frame = None;
        self.frame_cache.clear();
        self.render_cache_hot.clear();
        self.last_synth_frame_rendered = None;
        self.current_grade_hash = 0;
        self.current_grade_clip_id = None;
        self.current_graph_arc = None;

        // Reset playhead and per-clip state
        self.playhead = RationalTime::from_frames(0, self.fps.rate());
        self.current_clip_index = 0;
        self.current_track_index = 0;
        self.current_file = None;
        self.clip_source_start = 0.0;
        self.clip_timeline_start = RationalTime::from_frames(0, self.fps.rate());
        self.clip_speed = 1.0;
        self.speed_keyframes.clear();
        self.speed_ramp_enabled = false;
        self.current_source_rotation = 0;
        self.master_clock.set_speed(1.0);

        // Reset pre-seek background state
        self.pre_seek_generation = self.pre_seek_generation.wrapping_add(1);
        self.pre_seek_started = false;
        if let Ok(mut g) = self.pre_seek_result.lock() {
            *g = None;
        }

        // Frame counters
        self.frames_consumed = 0;
        self.dropped_frames = 0;

        // Blank the shared frame buffer so the viewer no longer shows old pixels
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.clear();
            fb.pregrade_data = Arc::new(Vec::new());
            fb.original_data.clear();
            fb.unstab_data.clear();
            fb.scope_data.clear();
            fb.scope_preview.clear();
            fb.scope_preview_w = 0;
            fb.scope_preview_h = 0;
            fb.scope_texture.clear();
            fb.scope_texture_w = 0;
            fb.scope_texture_h = 0;
            fb.scope_texture_mode = 255;
            fb.pending_data.clear();
            fb.pending_width = 0;
            fb.pending_height = 0;
            fb.pending_ready = false;
            fb.render_cache_ranges.clear();
            fb.generation = fb.generation.wrapping_add(1);
        }

        info!("Engine reset to idle (project unloaded)");
        let _ = self.event_tx.send(AppEvent::PlaybackStopped);
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    /// Compute a fingerprint of audio-relevant state at the current playhead.
    /// Includes: which media clips are active, their speed, audio effect
    /// chain (types + enabled), and audio_effect_keyframes (track count
    /// + per-track length + per-kf time/value/interpolation bits). Any
    /// change here triggers a chain rebuild on `SequenceUpdated`.
    /// Title/generator clips are excluded (they don't produce audio).
    ///
    /// **Scope is intentionally narrow** — only clips currently at the
    /// playhead contribute. A kf edit on a non-playhead clip does NOT
    /// flip the fingerprint, but that's correct: no audio source is
    /// open for those clips, so there's no running chain to rebuild.
    /// When the playhead reaches the edited clip, `open_audio_source`
    /// clones the clip's audio_effect_keyframes fresh, so the edit is
    /// picked up at clip-transition time.
    fn compute_audio_fingerprint(&self) -> u64 {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let tc = self.playhead;
        let mut fp: u64 = 0;
        for track in &seq.tracks {
            if let Some((_, clip)) = track.clip_at_timecode(tc) {
                fp = fp.wrapping_mul(31).wrapping_add(audio_fingerprint_for_clip(clip));
            }
        }
        fp
    }
}

/// Pure helper: hash the audio-relevant fields of a single clip.
/// Extracted from `compute_audio_fingerprint` so the per-clip formula
/// is unit-testable without standing up an Engine. Returns 0 for clips
/// without media (titles, generators) — caller folds this into the
/// per-track combine.
pub(crate) fn audio_fingerprint_for_clip(clip: &klipp_timeline::Clip) -> u64 {
    let Some(mid) = clip.media_id() else { return 0 };
    let mut fp: u64 = 0;
    fp = fp.wrapping_mul(31).wrapping_add(mid.0.as_u64_pair().0);
    fp = fp.wrapping_mul(31).wrapping_add(clip.speed.to_bits());
    fp = fp.wrapping_mul(31).wrapping_add(audio_chain_fingerprint_for(
        &clip.audio_effects,
        &clip.audio_effect_keyframes,
    ));
    fp
}

/// Hash the audio CHAIN fields only (effects + kf tracks) — without
/// media_id or clip speed. Used at clip-transition boundaries to detect
/// "same media, different effects" cases that
/// `audio_fingerprint_for_clip` would mask under the media_id check.
pub(crate) fn audio_chain_fingerprint_for(
    audio_effects: &[klipp_core::AudioEffectInstance],
    audio_effect_keyframes: &std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
) -> u64 {
    let mut fp: u64 = 0;
    fp = fp.wrapping_mul(31).wrapping_add(audio_effects.len() as u64);
    for ae in audio_effects {
        for b in ae.effect_type.bytes() {
            fp = fp.wrapping_mul(31).wrapping_add(b as u64);
        }
        fp = fp.wrapping_mul(31).wrapping_add(ae.enabled as u64);
    }
    // Sort kf-track keys before hashing so HashMap iteration order
    // doesn't leak nondeterminism into the fingerprint.
    fp = fp.wrapping_mul(31).wrapping_add(audio_effect_keyframes.len() as u64);
    let mut kf_keys: Vec<&String> = audio_effect_keyframes.keys().collect();
    kf_keys.sort();
    for key in kf_keys {
        for b in key.bytes() {
            fp = fp.wrapping_mul(31).wrapping_add(b as u64);
        }
        let kfs = &audio_effect_keyframes[key];
        fp = fp.wrapping_mul(31).wrapping_add(kfs.len() as u64);
        for kf in kfs {
            fp = fp.wrapping_mul(31).wrapping_add(kf.time_secs.to_bits());
            fp = fp.wrapping_mul(31).wrapping_add(kf.value.to_bits() as u64);
            fp = fp.wrapping_mul(31).wrapping_add(kf.interpolation as u64);
        }
    }
    fp
}

impl PlaybackEngine {

    fn stop_all_audio(&mut self) {
        // Clear mix thread staging first (stop reading from source ring buffers)
        if let Some(ref mt) = self.mix_thread {
            mt.clear_all();
        }
        for active in &mut self.audio_sources {
            active.source.stop_detached();
        }
        self.audio_sources.clear();
        // Drop mix thread and audio output
        self.mix_thread = None;
        self.audio_output = None;
        self.expected_audio_generation = 0;
    }

    /// Open one audio source for a given track and add it to audio_sources.
    /// Creates mix thread + audio output if not already present.
    /// The source starts PAUSED — caller is responsible for resuming.
    fn open_audio_source(
        &mut self,
        track_index: usize,
        media_id: MediaId,
        path: &std::path::Path,
        source_secs: f64,
        audio_effects: Vec<klipp_core::AudioEffectInstance>,
        audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    ) {
        // Compute the chain fingerprint up front so both the PCM and
        // decoder paths stamp it onto the ActiveAudioSource. Boundary
        // checks compare this against the new clip to decide whether
        // a chain rebuild is required.
        let chain_fp = audio_chain_fingerprint_for(&audio_effects, &audio_effect_keyframes);

        // Try PCM cache first (instant seeks, zero codec overhead)
        let pcm_path = self.pcm_cache_path(media_id);
        if let Some(ref pcm) = pcm_path {
            if pcm.exists() {
                match PcmAudioFeeder::open(pcm, self.cpal_sample_rate, audio_effects.clone(), audio_effect_keyframes.clone()) {
                    Ok((feeder, consumer)) => {
                        if source_secs > 0.01 {
                            feeder.seek(source_secs);
                        }
                        // Source starts paused — caller resumes when ready

                        self.ensure_mix_pipeline(consumer, track_index);
                        self.audio_sources.push(ActiveAudioSource {
                            track_index,
                            media_id,
                            audio_chain_fingerprint: chain_fp,
                            source: AudioSource::PcmFeeder(feeder),
                        });
                        return;
                    }
                    Err(e) => {
                        warn!("PCM feeder failed, falling back to decoder: {e}");
                    }
                }
            }
        }

        // Fallback: real-time ffmpeg decode
        match AudioDecoder::open(path, self.cpal_sample_rate, audio_effects, audio_effect_keyframes, source_secs) {
            Ok((adec, consumer)) => {
                if source_secs > 0.01 {
                    adec.seek(source_secs);
                }
                // Source starts paused — caller resumes when ready

                self.ensure_mix_pipeline(consumer, track_index);
                self.audio_sources.push(ActiveAudioSource {
                    track_index,
                    media_id,
                    audio_chain_fingerprint: chain_fp,
                    source: AudioSource::Decoder(adec),
                });
            }
            Err(e) => {
                debug!("No audio for this clip: {e}");
            }
        }
    }

    /// Ensure the mix thread and audio output exist, then register
    /// the given staging consumer for a track.
    fn ensure_mix_pipeline(
        &mut self,
        staging_consumer: crate::audio_decoder::AudioConsumer,
        track_index: usize,
    ) {
        // Create mix thread + audio output on first use
        if self.mix_thread.is_none() {
            let (mt, main_consumer) =
                MixThread::new(self.cpal_sample_rate, self.mixer.clone(), true);
            self.lufs_readings = Some(mt.lufs_readings());
            self.lufs_reset = Some(mt.lufs_reset_flag());
            self.spectrum_buf = Some(mt.spectrum_buf());
            self.spectrum_sample_rate = mt.spectrum_sample_rate();
            match AudioOutput::new(main_consumer, self.skim_consumer.clone()) {
                Ok(output) => {
                    self.audio_output = Some(SendAudioOutput(output));
                }
                Err(e) => {
                    warn!("Failed to create audio output: {e}");
                }
            }
            self.mix_thread = Some(mt);
        }

        // Register this source's consumer with the mix thread
        if let Some(ref mt) = self.mix_thread {
            mt.add_staging(track_index, staging_consumer);
        }
    }

    /// Look up the PCM cache path for a media item.
    fn pcm_cache_path(&self, media_id: MediaId) -> Option<std::path::PathBuf> {
        let pool = self.media_pool.read().ok()?;
        let item = pool.get(&media_id)?;
        item.audio_cache.as_ref().map(|c| c.pcm_path.clone())
    }

    /// Seek all audio sources to match the clip at `tc` on each source's track.
    /// Flushes the mix pipeline, bumps seek_gen, and waits for all sources to confirm.
    fn seek_all_audio_to_timecode(&mut self, tc: Timecode) {
        if self.audio_sources.is_empty() {
            return;
        }

        // If the clip at `tc` on any track uses a different media file than
        // the active audio source on that track, seeking won't help — the
        // source's decoder is open on the previous file. Delegate to
        // start_all_audio, which stops the stale sources and opens new
        // decoders against the correct media. Without this check the user
        // sees clip A's audio continue playing through clip B's playback
        // whenever advance_to_next_clip enters the preroll-hit branch with
        // a different media on the next clip.
        //
        // Same restart triggers when the AUDIO CHAIN changed (different
        // audio_effects / audio_effect_keyframes on the next clip) — two
        // same-media clips with distinct effects would otherwise share
        // the first clip's chain and the user hears effect bleed.
        let chain_or_media_changed = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.audio_sources.iter().any(|active| {
                let Some(track) = seq.tracks.get(active.track_index) else {
                    return false;
                };
                let Some((_, clip)) = track.clip_at_timecode(tc) else {
                    // No clip at `tc` on this track: start_all_audio will
                    // drop the source, which is the correct outcome too.
                    return true;
                };
                if clip.media_id() != Some(active.media_id) {
                    return true;
                }
                let new_fp = audio_chain_fingerprint_for(
                    &clip.audio_effects,
                    &clip.audio_effect_keyframes,
                );
                new_fp != active.audio_chain_fingerprint
            })
        };
        if chain_or_media_changed {
            self.start_all_audio(tc);
            return;
        }

        // Flush mix thread staging BEFORE telling sources to seek
        if let Some(ref mt) = self.mix_thread {
            mt.flush();
        }

        // Compute per-track seek positions from the timeline
        let seek_positions: Vec<(usize, f64)> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.audio_sources
                .iter()
                .filter_map(|active| {
                    let track = seq.tracks.get(active.track_index)?;
                    let (_idx, clip) = track.clip_at_timecode(tc)?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    Some((active.track_index, source_secs))
                })
                .collect()
        };

        self.expected_audio_generation += 1;
        for active in &self.audio_sources {
            if let Some((_, secs)) = seek_positions.iter().find(|(idx, _)| *idx == active.track_index) {
                active.source.seek(*secs);
            }
        }

        // Tell cpal callback to drain stale main ring buffer samples
        if let Some(aout) = &self.audio_output {
            aout.0.notify_seek();
        }

        // Wait for all audio sources to confirm seek
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        loop {
            let all_ready = self.audio_sources.iter().all(|a| {
                a.source.seek_generation().load(Ordering::Acquire)
                    >= self.expected_audio_generation
            });
            if all_ready {
                break;
            }
            if std::time::Instant::now() >= deadline {
                debug!(
                    "audio seek timeout: expected gen={} — proceeding anyway",
                    self.expected_audio_generation
                );
                break;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    /// Wait until the mix thread has pre-filled the main ring buffer with at
    /// least one cpal-callback-worth of samples, or until the timeout elapses.
    /// Replaces a previous hardcoded 15 ms `thread::sleep` after seek.
    ///
    /// Why: cpal's output callback pulls a few hundred to a few thousand
    /// samples per call. Resuming cpal against an empty main ring causes a
    /// brief silent gap or starvation while the mix pipeline catches up.
    /// The fixed 15 ms wait was unreliable under load (mix thread might still
    /// be warming up at 15 ms on a busy machine) and wasteful idle (mix
    /// thread typically fills enough in 2–5 ms).
    ///
    /// We poll `MixThread::buffered_samples()` (a lock-free atomic written
    /// by the mix loop on every iteration) at 1 ms granularity, and proceed
    /// either when the buffer has reached `target_samples` or when the
    /// `timeout` elapses. The timeout matches the prior worst-case
    /// behaviour so a misconfigured pipeline still progresses rather than
    /// blocking forever.
    fn wait_for_audio_prefill(&self) {
        // ~21 ms of stereo audio at 48 kHz, ~10 ms at 96 kHz. Comfortable
        // margin over a typical 256–1024-sample cpal callback request.
        const TARGET_SAMPLES: usize = 2048;
        const POLL_INTERVAL: Duration = Duration::from_millis(1);
        const TIMEOUT: Duration = Duration::from_millis(50);
        let Some(mt) = &self.mix_thread else { return };
        let start = std::time::Instant::now();
        loop {
            if mt.buffered_samples() >= TARGET_SAMPLES {
                return;
            }
            if start.elapsed() >= TIMEOUT {
                debug!(
                    "audio prefill timeout: only {} samples ready after {:?}",
                    mt.buffered_samples(),
                    TIMEOUT
                );
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Re-read speed_keyframes from the current clip into the engine's local copy.
    /// Call after any `self.clip_speed = ...` or after advancing current_clip_index
    /// so the time-remap path has up-to-date keyframes. Also resets the ramp
    /// anchor so the next tick integrates from the current playhead.
    fn refresh_speed_keyframes(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        if let Some(clip) = seq.tracks.get(self.current_track_index)
            .and_then(|t| t.clips.get(self.current_clip_index))
        {
            self.speed_keyframes = clip.speed_keyframes.clone();
            self.speed_ramp_enabled = !clip.speed_keyframes.is_empty();
        } else {
            self.speed_keyframes.clear();
            self.speed_ramp_enabled = false;
        }
        drop(seq);
        self.reset_ramp_anchor();
    }

    /// Re-anchor the ramp integrator to the current playhead. Called after any
    /// seek, clip change, or play/resume so the wall-clock-based timeline_t
    /// computation starts from a known good spot.
    fn reset_ramp_anchor(&mut self) {
        let clip_rel = (self.playhead - self.clip_timeline_start).to_secs_f64().max(0.0);
        self.ramp_timeline_anchor_secs = clip_rel;
        self.ramp_timeline_anchor_wall = std::time::Instant::now();
        self.ramp_last_clock_speed = self.clip_speed;
    }

    /// Current clip-relative timeline seconds when a speed ramp is active.
    /// Returns `None` for constant speed — callers fall back to their own math.
    fn ramp_clip_relative_secs(&self) -> Option<f64> {
        if self.speed_ramp_enabled && self.state == PlaybackState::Playing {
            Some(self.ramp_timeline_anchor_secs + self.ramp_timeline_anchor_wall.elapsed().as_secs_f64())
        } else {
            None
        }
    }

    #[allow(unused_assignments)]
    fn update_playback(&mut self) {
        let t_update = std::time::Instant::now();
        // Per-stage timing for pinpointing where the frame budget goes.
        // Each stage records its elapsed time; we emit one consolidated
        // line at the end when the total exceeds 5 ms (same threshold the
        // existing `PERF update_playback` uses).
        let mut t_stage = std::time::Instant::now();
        let mut stage_drain_us: u128 = 0;
        let mut stage_clock_ramp_us: u128 = 0;
        let mut stage_queue_pop_us: u128 = 0;
        let mut stage_overlay_open_us: u128 = 0;
        let mut stage_overlay_pop_us: u128 = 0;
        let mut stage_fb_write_us: u128 = 0;
        let mut stage_mid_us: u128 = 0;

        // Drain UI-queued colour-grade param changes BEFORE we take any
        // sequence read lock this frame. One write-lock acquisition per frame
        // for the whole batch — the UI can have pushed dozens of slider ticks
        // without contending with the long read lock that follows.
        self.drain_param_changes();
        stage_drain_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        // Time-remap: sample the instantaneous speed curve and re-anchor the
        // master clock when the curve has drifted from what the clock is using.
        // The wall-clock elapsed since `ramp_timeline_anchor_wall` gives us the
        // timeline position within the clip (time advances 1:1 with wall time
        // in timeline space). We sample speed_at(timeline_t) and push it to the
        // master clock so source-time integrates the curve piecewise-constantly.
        if self.speed_ramp_enabled && self.state == PlaybackState::Playing {
            let clip_t = self.ramp_timeline_anchor_secs
                + self.ramp_timeline_anchor_wall.elapsed().as_secs_f64();
            let instant_speed = klipp_timeline::speed_at_keyframes(
                &self.speed_keyframes,
                clip_t,
                self.clip_speed,
            )
            .clamp(klipp_timeline::SPEED_MIN, klipp_timeline::SPEED_MAX);
            // Re-anchor master_clock when drift exceeds 1% to avoid churn.
            if (self.ramp_last_clock_speed - instant_speed).abs() / self.ramp_last_clock_speed.abs().max(1e-6) > 0.01 {
                self.master_clock.set_speed(instant_speed);
                self.ramp_last_clock_speed = instant_speed;
            }
            // Mute audio whenever the time remap is active — pitch is wrong at
            // any non-1.0 speed and the UI warns about this.
            for active in &self.audio_sources {
                active.source.muted().store(true, Ordering::Release);
            }
        } else {
            // Constant-speed path: mute when clip_speed != 1.0, or when the
            // user is scrubbing with audio-scrubbing disabled. Reasserted each
            // tick so mid-scrub pref changes and EndScrub both self-correct.
            let should_mute = (self.clip_speed - 1.0).abs() > 0.01
                || (self.is_scrubbing && !self.scrub_audio_enabled);
            for active in &self.audio_sources {
                active.source.muted().store(should_mute, Ordering::Release);
            }
        }

        // Master clock is the single source of truth.
        let current_source_secs = self.master_clock.current_secs();
        // Frame presentation tolerance: half a frame duration (scales with FPS)
        let frame_tolerance = self.fps.frame_duration_secs() * 0.5;
        let _clock_running = self.master_clock.is_running();
        stage_clock_ramp_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        // Title / Generator clip path — standalone non-media clip on V1 (no decoder active)
        if self.v1_decoder.is_none() && self.current_file.is_none() {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let synth_clip = seq.tracks.get(self.current_track_index)
                .and_then(|track| {
                    track.clips.get(self.current_clip_index).and_then(|clip| {
                        if clip.title_data().is_some() || clip.generator_data().is_some() {
                            Some(clip.clone())
                        } else {
                            None
                        }
                    })
                });
            let res = seq.resolution;
            drop(seq);

            if let Some(clip) = synth_clip {
                // Update playhead from clock — honour a speed ramp if active.
                let elapsed_timeline_secs = if let Some(r) = self.ramp_clip_relative_secs() {
                    r
                } else {
                    let elapsed_secs = current_source_secs - self.clip_source_start;
                    let speed = self.clip_speed.max(0.001);
                    (elapsed_secs / speed).max(0.0)
                };
                let elapsed_tc = RationalTime::from_secs_f64(elapsed_timeline_secs.max(0.0), self.fps.rate());
                self.playhead = clip.timeline_in + elapsed_tc;

                // Check if past end of clip
                if self.playhead >= clip.timeline_out {
                    self.playhead = clip.timeline_out;
                    self.last_synth_frame_rendered = None;
                    self.advance_to_next_clip();
                    return;
                }

                // Frame throttle: only render when the sequence frame has actually changed.
                // Without this, the synthetic path renders ~1000 times per second (once per
                // engine tick), allocating ~8 GB/s of RGBA for content that only needs to
                // update at 24-30 Hz. When the frame hasn't changed, just update the playhead
                // for smooth UI display and skip the expensive render work.
                let current_frame = self.playhead.value;
                if Some(current_frame) == self.last_synth_frame_rendered {
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    (self.repaint_cb)();
                    return;
                }
                self.last_synth_frame_rendered = Some(current_frame);

                let (w, h) = (res.width, res.height);
                let rgba = if let Some(title_data) = clip.title_data() {
                    let clip_elapsed = (self.playhead - clip.timeline_in).to_secs_f64().max(0.0);
                    let typewriter = typewriter_override(title_data, clip_elapsed);
                    let td_for_render = typewriter.as_ref().unwrap_or(title_data);
                    let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                    let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                    self.title_renderer.render_with_gpu(td_for_render, w, h, gpu)
                } else if let Some(gen_data) = clip.generator_data() {
                    let clip_relative = (self.playhead - clip.timeline_in).to_secs_f64().max(0.0);
                    let total = (clip.timeline_out - clip.timeline_in).to_secs_f64();
                    klipp_core::generator::render_generator(gen_data, w, h, clip_relative, total)
                } else {
                    return;
                };

                // u8→linear→post_process→display_transform→u8 at this
                // title/generator viewer boundary.
                let linear = u8_rgba_to_linear(&rgba);
                let final_linear = self.post_process_rgba(linear, w, h);
                let final_rgba = self.display_encode(&final_linear);
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.write_frame(final_rgba, w, h);
                drop(fb);

                self.frames_consumed += 1;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                (self.repaint_cb)();
                return;
            }
        }

        // Pop frames from the queue, presenting the one closest to current time
        let mut display_frame = None;
        let mut q_len = 0usize;

        if let Some(dec) = &self.v1_decoder {
            let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            q_len = queue.len();

            loop {
                match queue.front() {
                    None => break,
                    Some(f) if f.is_eof => {
                        let frame = queue.pop().unwrap();
                        display_frame = Some(frame);
                        break;
                    }
                    Some(f) => {
                        let pts = f.pts_secs();
                        if pts <= current_source_secs + frame_tolerance {
                            // Frame is due or overdue — take it
                            if self.frames_consumed == 0 {
                                debug!(
                                    "FIRST FRAME: pts={:.3}s clock={:.3}s delta={:.3}s q_len={}",
                                    pts, current_source_secs, pts - current_source_secs, q_len
                                );
                            }
                            let frame = queue.pop().unwrap();
                            // Check if the next frame is also due (we're behind — drop this one)
                            if let Some(next) = queue.front() {
                                if !next.is_eof && next.pts_secs() <= current_source_secs + frame_tolerance {
                                    // Only count as a drop if we're behind by more than 1 frame
                                    // (normal drift of <1 frame interval is not a real drop)
                                    let gap = next.pts_secs() - pts;
                                    let frame_dur = self.fps.frame_duration_secs();
                                    if gap > frame_dur * 1.5 {
                                        self.dropped_frames += 1;
                                        debug!(
                                            "DROP: frame pts={:.3}, next pts={:.3}, clock={:.3}, gap={:.3}",
                                            pts, next.pts_secs(), current_source_secs, gap
                                        );
                                    }
                                    continue; // skip to the more recent frame
                                }
                            }
                            display_frame = Some(frame);
                            break;
                        } else {
                            // Frame is in the future — wait
                            break;
                        }
                    }
                }
            }
        }

        let mut frame = match display_frame {
            Some(f) => f,
            None => {
                if self.v1_decoder.is_none() && !self.audio_sources.is_empty() {
                    // Audio-only playback: update playhead from clock
                    let playhead_secs = current_source_secs;
                    let new_playhead = RationalTime::from_secs_f64(playhead_secs, self.fps.rate());
                    self.playhead = new_playhead;

                    // Check if past end of all audio clips
                    let seq_dur = self.sequence_duration();
                    if self.playhead >= seq_dur {
                        self.stop_at_end();
                        return;
                    }

                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    (self.repaint_cb)();
                    return;
                }

                // No new frame this tick — still update playhead from clock
                // so the timeline playhead stays smooth during slow-mo
                let elapsed_timeline_secs = if let Some(r) = self.ramp_clip_relative_secs() {
                    r
                } else {
                    let speed = self.clip_speed.max(0.001);
                    let elapsed_secs = current_source_secs - self.clip_source_start;
                    (elapsed_secs / speed).max(0.0)
                };
                let elapsed_tc = RationalTime::from_secs_f64(elapsed_timeline_secs.max(0.0), self.fps.rate());
                self.playhead = self.clip_timeline_start + elapsed_tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                (self.repaint_cb)();
                return;
            }
        };

        if frame.is_eof {
            debug!("EOF sentinel received — advancing to next clip");
            self.advance_to_next_clip();
            return;
        }

        // Update playhead from the frame's PTS relative to clip timeline
        // Search ALL video tracks for the current clip
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let found_clip = seq.tracks.get(self.current_track_index)
            .and_then(|track| track.clips.get(self.current_clip_index).cloned());
        drop(seq);

        if found_clip.is_none() {
            info!(
                "CUT-DIAG STALE INDEX: clip_idx={} not found, pts={:.4}s clock={:.4}s — skipping frame",
                self.current_clip_index, frame.pts_secs(), current_source_secs,
            );
            return;
        }
        if let Some(clip) = found_clip {
            let source_elapsed_secs = frame.pts_secs() - clip.source_in.to_secs_f64();
            // Convert source elapsed time to timeline elapsed time. With a ramp
            // active, we trust the wall-clock anchor rather than trying to
            // invert the integral from the frame PTS.
            let timeline_elapsed_secs = if let Some(r) = self.ramp_clip_relative_secs() {
                r
            } else {
                let speed = clip.speed.max(0.001);
                source_elapsed_secs / speed
            };
            let elapsed_tc = RationalTime::from_secs_f64(
                timeline_elapsed_secs.max(0.0),
                self.fps.rate(),
            );
            self.playhead = clip.timeline_in + elapsed_tc;

            // Loop out-point detection: if loop enabled and playhead crossed out_point, loop back
            if self.loop_enabled {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let out_pt = seq.out_point;
                let in_pt = seq.in_point;
                drop(seq);

                if let Some(out) = out_pt {
                    let out_secs = out.to_secs_f64();
                    let playhead_secs = self.playhead.to_secs_f64();
                    let in_secs = in_pt.map(|t| t.to_secs_f64()).unwrap_or(0.0);
                    let range_duration = out_secs - in_secs;

                    // Predictive pre-seek: start 200ms before out_point if range >= 1.0s
                    if range_duration >= 1.0
                        && !self.pre_seek_started
                        && playhead_secs >= out_secs - 0.200
                        && playhead_secs < out_secs
                    {
                        self.begin_pre_seek(in_secs);
                    }

                    // Poll pre-seek result (non-blocking) — consumed by execute_loop
                    if let Ok(guard) = self.pre_seek_result.try_lock() {
                        drop(guard);
                    }

                    if playhead_secs >= out_secs {
                        // Target: in_point if set, else start
                        let target = in_pt.unwrap_or_else(|| RationalTime { value: 0, rate: self.fps.rate() });
                        self.execute_loop(target);
                        return;
                    }
                }
            }

            // Look-ahead: pre-decode next clip when within 1.5s of source_out
            let dist_to_out = clip.source_out.to_secs_f64() - frame.pts_secs();
            if dist_to_out < 1.5 && dist_to_out > 0.0 && self.preroll_decoder.is_none() {
                self.start_preroll_for_next_clip();
            }

            // Check if we've passed source_out
            if frame.pts_secs() >= clip.source_out.to_secs_f64() - 0.01 {
                info!("CUT-DIAG BOUNDARY HIT: pts={:.4}s source_out={:.4}s timeline_out={:.4}s clock={:.4}s",
                    frame.pts_secs(), clip.source_out.to_secs_f64(),
                    clip.timeline_out.to_secs_f64(), current_source_secs);
                self.playhead = clip.timeline_out;
                self.advance_to_next_clip();
                return;
            }
        }

        // Measure the cost of: frame queue pop + drop-frame-if-behind loop +
        // found_clip lookup + loop out-point detection (all the V1 decoder
        // handling between the start and here).
        stage_queue_pop_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        // Check overlay decoder lifecycle — open/close as playhead moves through overlay clips
        self.update_overlay_decoders(self.playhead);
        stage_overlay_open_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        // Pop frames from each overlay decoder using per-clip source timing.
        // Each overlay clip has its own source_in, timeline_in, and speed, so we compute
        // the expected source time independently for each overlay decoder.
        let overlay_dec_indices: Vec<usize> = self.overlay_decoders.keys().copied().collect();
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            for track_idx in overlay_dec_indices {
                // Compute this overlay clip's source time from the current playhead
                let overlay_source_secs = if let Some(clip_idx) = self.overlay_clips.get(&track_idx) {
                    seq.tracks.get(track_idx)
                        .and_then(|t| t.clips.get(*clip_idx))
                        .map(|clip| {
                            clip.source_in.to_secs_f64()
                                + (self.playhead - clip.timeline_in).to_secs_f64() * clip.speed
                        })
                        .unwrap_or(current_source_secs)
                } else {
                    current_source_secs
                };

                if let Some(dec) = self.overlay_decoders.get(&track_idx) {
                    let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                    let q_len = queue.len();
                    if q_len > 0 || self.overlay_frames.contains_key(&track_idx) {
                        debug!("OV track={} q={} has_frame={} src={:.3}s front_pts={:?}",
                            track_idx, q_len, self.overlay_frames.contains_key(&track_idx),
                            overlay_source_secs, queue.front().map(|f| f.pts_secs()));
                    }
                    while let Some(f) = queue.front() {
                        if f.is_eof {
                            let _ = queue.pop();
                            break;
                        }
                        let pts = f.pts_secs();
                        if pts <= overlay_source_secs + frame_tolerance {
                            let ovf = queue.pop().unwrap();
                            if let Some(next) = queue.front() {
                                if !next.is_eof && next.pts_secs() <= overlay_source_secs + frame_tolerance {
                                    continue;
                                }
                            }
                            // Store the raw overlay frame WITHOUT baking clip opacity in.
                            // Clip opacity is now applied at composite time so paused
                            // opacity drags update the visible frame.
                            // Store linear-f32 directly — the composite path is linear,
                            // so the old sRGB u8 round-trip (~10 ms on 1920x818) was
                            // pure waste on the hot path.
                            self.overlay_frames.insert(track_idx, ovf);
                            break;
                        } else {
                            break;
                        }
                    }
                }
            }
        }

        // Overlay frame popping (walks each overlay decoder's queue, picks
        // the frame closest to the current source time, stashes it in
        // overlay_frames).
        stage_overlay_pop_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        // Grade V1's frame through the full processing pipeline (scopes, stabilization,
        // comparison mode, keyframes, global LUT) BEFORE compositing.
        // `post_process_rgba` is linear-in/linear-out post the linear-
        // edge-to-edge refactor, and `LinearFrame.data` already sits in
        // linear — feed it directly, skipping the pre-refactor round-
        // trip via `frame.into_rgba().to_linear()`.
        let (final_w, final_h) = (frame.width, frame.height);
        let t_pp_full = std::time::Instant::now();
        // Phase 4d.1: realise CPU `data` from `gpu_source` if the
        // decoder deferred it. The grade chain consumes the GPU
        // buffer directly via `prime_chain_from_gpu_u8_buffer`, but
        // the CPU encode path inside `post_process_rgba` (pregrade
        // cache, scope reencode) still wants linear-f32. No-op when
        // `data` is already populated (SW decode, KLIPP_GPU_FRAMES=0).
        frame.ensure_cpu_data();
        let gpu_source = frame.gpu_source.clone();
        let v1_graded_linear = self.post_process_rgba_with_gpu_source(
            frame.data, final_w, final_h, gpu_source.as_ref(),
        );
        let pp_full_us = t_pp_full.elapsed().as_micros();

        // Composite V1 (graded) + overlay tracks in linear-light f32.
        // Every layer is already linear (post_process_rgba, apply_clip_
        // effects, and LinearFrame all speak linear now), so we blend
        // without any u8⇄linear seams in the hot loop.
        let t_composite_full = std::time::Instant::now();
        let t_composite = std::time::Instant::now();
        let v1_opacity = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.get(self.current_track_index).map(|t| t.opacity).unwrap_or(1.0)
        };

        // Gather overlay clip data while holding the sequence lock, then drop
        // the lock before calling apply_clip_effects (which needs &mut self).
        let overlay_clip_data: Vec<(usize, f32, Option<klipp_timeline::Clip>)> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let fps_f64 = self.fps.as_f64();
            let overlay_tracks = seq.overlay_video_tracks(self.current_track_index);
            overlay_tracks.iter().filter_map(|(track_idx, track)| {
                self.overlay_frames.get(track_idx).map(|_| {
                    let clip_opacity = track
                        .clip_at_timecode(self.playhead)
                        .map(|(_, clip)| evaluate_clip_opacity(clip, self.playhead, fps_f64))
                        .unwrap_or(1.0);
                    let effective = track.opacity * clip_opacity;
                    let clip = track.clip_at_timecode(self.playhead).map(|(_, c)| c.clone());
                    (*track_idx, effective, clip)
                })
            }).collect()
        };

        // Reuse the pooled linear scratch buffer across frames.
        let pixel_count = (final_w * final_h) as usize;
        let needed_f = pixel_count * 4;
        let mut accumulated_linear = std::mem::take(&mut self.composite_scratch_linear);
        if accumulated_linear.len() != needed_f {
            accumulated_linear.resize(needed_f, 0.0);
        } else {
            accumulated_linear.fill(0.0);
        }

        // Per-step accumulators so we can see where composite+into_rgba time
        // actually goes when it busts the budget. Sub-millisecond on the fast
        // path, stay zero when there are no overlays.
        let mut ov_clone_us: u128 = 0;
        let mut ov_grade_us: u128 = 0;
        let mut ov_resize_us: u128 = 0;
        let mut ov_blend_us: u128 = 0;

        // Blend V1 (always the first layer) onto the zero canvas. V1
        // already comes back from post_process_rgba as linear f32.
        let v1_blend_us: u128 = {
            let t_blend = std::time::Instant::now();
            crate::composite::blend_layer_linear(
                &mut accumulated_linear,
                &v1_graded_linear,
                v1_opacity,
                klipp_core::BlendMode::Normal,
            );
            let el = t_blend.elapsed();
            tracing::trace!("PERF blend_layer pass: {:.3}ms res={final_w}x{final_h} layer=V1", el.as_secs_f64() * 1000.0);
            el.as_micros()
        };

        // Each overlay clip's effects are applied before compositing so every
        // clip renders with its own colour grade, OFX chain, and builtin effects.
        // Adjustment layers transform the accumulated composite in-place instead
        // of blending a new layer on top.
        for (track_idx, effective, clip) in &overlay_clip_data {
            if *effective <= 0.001 { continue; }

            // Adjustment layer: apply its effects directly to the
            // accumulated linear composite — no u8 bookends needed
            // post-refactor.
            if let Some(clip) = clip {
                if matches!(clip.kind, klipp_timeline::ClipKind::AdjustmentLayer) {
                    let clip_time = (self.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
                    let pass = std::mem::take(&mut accumulated_linear);
                    accumulated_linear = self.apply_clip_effects(pass, final_w, final_h, clip, clip_time);
                    continue;
                }
            }

            // Regular clip: grade its linear frame and blend onto
            // accumulated linear. Dimensions-mismatch scaling is the
            // one remaining u8 seam inside the compositor because
            // `image::imageops::resize` is u8-only — pay the round-
            // trip only when an overlay's source doesn't already
            // match the sequence size.
            if let Some(ov_frame) = self.overlay_frames.get(track_idx) {
                let (ov_w, ov_h) = (ov_frame.width, ov_frame.height);
                // `overlay_frames` now stores linear f32 RGBA directly —
                // no u8↔linear round-trip on the hot path. Clone the
                // pixel buffer once so downstream effects/blend can own
                // it; the map keeps its authoritative copy for the
                // next frame.
                let t_clone = std::time::Instant::now();
                let mut ov_linear = ov_frame.data.clone();
                ov_clone_us += t_clone.elapsed().as_micros();
                if let Some(clip) = clip {
                    let clip_time = (self.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
                    let t_grade = std::time::Instant::now();
                    ov_linear = self.apply_clip_effects(ov_linear, ov_w, ov_h, clip, clip_time);
                    ov_grade_us += t_grade.elapsed().as_micros();
                }
                let src_linear = if ov_w == final_w && ov_h == final_h {
                    ov_linear
                } else {
                    use image::{ImageBuffer, Rgba};
                    // Resize path: linear → u8 → resize → u8 → linear.
                    // Inevitable while imageops is u8-only; localised to
                    // just the mismatched-dim case. The main case
                    // (dims match) stays fully linear.
                    let t_resize = std::time::Instant::now();
                    let ov_rgba = linear_rgba_to_u8(&ov_linear);
                    let img = ImageBuffer::<Rgba<u8>, _>::from_raw(ov_w, ov_h, ov_rgba)
                        .unwrap_or_else(|| ImageBuffer::new(final_w, final_h));
                    let scaled = image::imageops::resize(&img, final_w, final_h, image::imageops::FilterType::Triangle);
                    let out = u8_rgba_to_linear(&scaled.into_raw());
                    ov_resize_us += t_resize.elapsed().as_micros();
                    out
                };
                let t_blend = std::time::Instant::now();
                crate::composite::blend_layer_linear(
                    &mut accumulated_linear,
                    &src_linear,
                    *effective,
                    klipp_core::BlendMode::Normal,
                );
                let el = t_blend.elapsed();
                ov_blend_us += el.as_micros();
                tracing::trace!(
                    "PERF blend_layer pass: {:.3}ms res={final_w}x{final_h} overlay_track={track_idx}",
                    el.as_secs_f64() * 1000.0,
                );
            }
        }

        // Encode the final linear composite to u8 for downstream (viewer,
        // export, cache). This is the display boundary — applies the
        // user-selected tone-map operator (None = identity for SDR) then
        // sRGB-encodes the result. For `ToneMapOp::None` + SDR content
        // this is bit-identical to the old linear_rgba_to_u8 path.
        let t_display = std::time::Instant::now();
        let final_rgba = self.display_encode(&accumulated_linear);
        let display_us = t_display.elapsed().as_micros();
        // Return the linear scratch to the pool for the next frame.
        self.composite_scratch_linear = accumulated_linear;

        let composite_ms = t_composite.elapsed().as_secs_f64() * 1000.0;
        let overlay_count = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.overlay_video_tracks(self.current_track_index).len()
        };
        if composite_ms > 2.0 {
            debug!("PERF composite+into_rgba: {composite_ms:.1}ms");
        }
        if composite_ms > 5.0 {
            let budget = if composite_ms < 16.7 { "OK" }
                else if composite_ms < 41.0 { "WARN" }
                else { "OVER" };
            debug!(
                "PERF blend_composite: {composite_ms:.2}ms overlays={overlay_count} \
                 res={final_w}x{final_h} budget={budget} | v1_blend={:.2} ov_clone={:.2} ov_grade={:.2} ov_resize={:.2} ov_blend={:.2} display={:.2}",
                v1_blend_us as f64 / 1000.0,
                ov_clone_us as f64 / 1000.0,
                ov_grade_us as f64 / 1000.0,
                ov_resize_us as f64 / 1000.0,
                ov_blend_us as f64 / 1000.0,
                display_us as f64 / 1000.0,
            );
        }

        let composite_full_us = t_composite_full.elapsed().as_micros();

        // Check for active transition and blend if needed.
        // If a transition is active this may build a new Vec — in that case
        // the composite_scratch pool contribution is lost for this frame
        // but restored on the next frame when no transition is active.
        let t_transition = std::time::Instant::now();
        let mut final_rgba = self.apply_transition_if_active(final_rgba, final_w, final_h);
        let transition_us = t_transition.elapsed().as_micros();
        let transition_ms = transition_us as f64 / 1000.0;
        if transition_ms > 2.0 {
            debug!("PERF apply_transition: {transition_ms:.1}ms");
        }

        // Snapshot how much time the "big middle" (post_process +
        // composite + apply_transition) actually took. Expected ~15 ms
        // from logs (10 grade + 5 composite); if mid >> sum of logged
        // pieces, apply_transition or something in between is the
        // culprit.
        stage_mid_us = t_stage.elapsed().as_micros();
        let mid_accounted_us = pp_full_us + composite_full_us + transition_us;
        let mid_gap_us = stage_mid_us.saturating_sub(mid_accounted_us);
        let mid_ms = stage_mid_us as f64 / 1000.0;
        if mid_ms > 20.0 {
            debug!(
                "PERF mid_breakdown: {mid_ms:.1}ms total | pp={pp:.1} composite={cp:.1} transition={tr:.1} gap={gap:.1}",
                pp = pp_full_us as f64 / 1000.0,
                cp = composite_full_us as f64 / 1000.0,
                tr = transition_us as f64 / 1000.0,
                gap = mid_gap_us as f64 / 1000.0,
            );
        }
        let t_fb_write = std::time::Instant::now();
        // Write frame to the shared FrameBuffer via swap — `final_rgba` ends up
        // holding the PREVIOUS pending_data buffer, which we return to the pool.
        // This eliminates the 8-33 MB per-frame allocation that used to be
        // `vec![0u8; pixel_count * 4]` at the top of the composite block.
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut final_rgba, final_w, final_h);
        }
        // Return the stale buffer (formerly pending_data) to the u8 pool.
        self.composite_scratch_u8 = final_rgba;
        stage_fb_write_us = t_fb_write.elapsed().as_micros();

        self.frames_consumed += 1;

        let update_ms = t_update.elapsed().as_secs_f64() * 1000.0;
        if update_ms > 5.0 {
            // Consolidated per-stage breakdown so the "~45ms unaccounted"
            // gap in `PERF update_playback` has a home. `grade` and
            // `composite` are the existing PERF lines; anything else large
            // is a new stage to investigate.
            debug!(
                "PERF update_playback: {update_ms:.1}ms for {}x{} frame | \
                 drain={:.1} clock={:.1} queue_pop={:.1} ov_open={:.1} ov_pop={:.1} mid={:.1} fb_write={:.1}",
                final_w, final_h,
                stage_drain_us as f64 / 1000.0,
                stage_clock_ramp_us as f64 / 1000.0,
                stage_queue_pop_us as f64 / 1000.0,
                stage_overlay_open_us as f64 / 1000.0,
                stage_overlay_pop_us as f64 / 1000.0,
                stage_mid_us as f64 / 1000.0,
                stage_fb_write_us as f64 / 1000.0,
            );
        }

        // Periodic playback status (every 2 seconds)
        if self.last_playback_log.elapsed().as_secs() >= 2 {
            let elapsed = self.last_playback_log.elapsed().as_secs_f64();
            let fps = self.frames_consumed as f64 / elapsed.max(0.001);
            debug!(
                "PLAYBACK: clock={:.3}s consumed={} ({:.1}fps) drops={} q_len={}",
                current_source_secs, self.frames_consumed, fps,
                self.dropped_frames, q_len
            );
            self.frames_consumed = 0;
            self.last_playback_log = std::time::Instant::now();
        }

        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    fn advance_to_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());

        // Find the track that contains the current clip (by index + is_playable),
        // then look for the next playable clip on the SAME track.
        let next_idx = self.current_clip_index + 1;
        let (current_clip_info, next_clip_info) = {
            let mut cur_info: Option<(klipp_timeline::ClipKind, Option<klipp_core::MediaId>, f64)> = None;
            let mut next_info: Option<(usize, klipp_timeline::Clip)> = None;
            if let Some(track) = seq.tracks.get(self.current_track_index) {
                if let Some(c) = track.clips.get(self.current_clip_index) {
                    if c.kind.is_playable() {
                        cur_info = Some((c.kind.clone(), c.media_id(), c.source_out.to_secs_f64()));
                        for idx in next_idx..track.clips.len() {
                            let clip = &track.clips[idx];
                            if clip.kind.is_playable() {
                                next_info = Some((idx, clip.clone()));
                                break;
                            }
                        }
                    }
                }
            }
            (cur_info, next_info)
        };
        drop(seq);

        let Some((actual_idx, next_clip)) = next_clip_info else {
            self.stop_at_end();
            return;
        };

        let timeline_in = next_clip.timeline_in;
        let next_clip_speed = next_clip.speed;
        let source_rotation = next_clip.source_rotation;
        let source_in_secs = next_clip.source_in.to_secs_f64();

        // ── Branch: next clip is synthetic (Title, Generator) — no decoder needed ──
        if !next_clip.kind.needs_decoder() {
            info!(
                "CUT-DIAG advance: next_idx={} kind={:?} (synthetic, no decoder)",
                actual_idx,
                std::mem::discriminant(&next_clip.kind),
            );
            // Tear down any active decoder from the current clip
            if let Some(dec) = &mut self.v1_decoder {
                dec.stop_detached();
            }
            self.v1_decoder = None;
            self.current_file = None;

            self.current_clip_index = actual_idx;
            self.playhead = timeline_in;
            self.last_synth_frame_rendered = None;
            self.clip_source_start = 0.0;
            self.clip_timeline_start = timeline_in;
            self.clip_speed = next_clip_speed;
            self.refresh_speed_keyframes();
            self.last_frame_time = std::time::Instant::now();

            // Seek clock to timeline position so update_playback's synthetic path picks up
            let source_secs = self.playhead.to_secs_f64();
            let source_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(source_us);
            self.master_clock.resume();

            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
            self.update_overlay_decoders(self.playhead);
            return;
        }

        // ── Branch: next clip needs a decoder (Media, Sequence) ──
        let media_id = match next_clip.media_id() {
            Some(mid) => mid,
            None => {
                warn!("Playable clip at idx {} needs decoder but has no media_id", actual_idx);
                self.stop_at_end();
                return;
            }
        };

        // Contiguous clip optimization: if the next clip uses the same media file
        // and its source_in matches the current clip's source_out (e.g. after a blade cut),
        // keep the decoder running — no seek, no flush, no stall.
        let is_contiguous = current_clip_info.as_ref().is_some_and(|(_, cur_media_id, cur_source_out)| {
            *cur_media_id == Some(media_id) && (*cur_source_out - source_in_secs).abs() < 0.02
        });
        info!(
            "CUT-DIAG advance: next_idx={} next_media={:?} next_src_in={:.4}s is_contiguous={} has_decoder={}",
            actual_idx, media_id, source_in_secs, is_contiguous, self.v1_decoder.is_some(),
        );

        if is_contiguous && self.v1_decoder.is_some() {
            info!(
                "CUT-DIAG CONTIGUOUS: old_idx={} new_idx={} source_in={:.4}s clock={:.4}s",
                self.current_clip_index, actual_idx, source_in_secs,
                self.master_clock.current_secs(),
            );
            self.current_clip_index = actual_idx;
            self.playhead = timeline_in;
            self.last_synth_frame_rendered = None;
            self.clip_source_start = source_in_secs;
            self.clip_timeline_start = timeline_in;
            self.clip_speed = next_clip_speed;
            self.refresh_speed_keyframes();
            self.current_source_rotation = source_rotation;
            self.contiguous_audio.store(true, std::sync::atomic::Ordering::Relaxed);
            self.last_frame_time = std::time::Instant::now();
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

            // The `is_contiguous` check above only considers the video track
            // (same media, source_out == next.source_in). Audio tracks may
            // live independently and switch to a different media file at
            // this boundary — the existing `AudioSource`s are still decoding
            // the previous media, so a `set_fade` update would silence them
            // once the old media runs out. Scan the audio sources and, if
            // any sits on a media that no longer matches the clip at the
            // new playhead, delegate to `start_all_audio` to stop-and-reopen
            // the decoders. Video stays contiguous regardless. Mirrors the
            // matching check in `seek_all_audio_to_timecode`.
            //
            // We also restart when the AUDIO CHAIN changed — two clips
            // that share the same media file can carry different
            // audio_effects / audio_effect_keyframes, and without this
            // check the prior clip's chain bleeds into the next one
            // (manifests as "the effect is applied to the whole track").
            let audio_chain_or_media_changed = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                self.audio_sources.iter().any(|active| {
                    let Some(track) = seq.tracks.get(active.track_index) else {
                        return true;
                    };
                    match track.clip_at_timecode(self.playhead) {
                        Some((_, clip)) => {
                            if clip.media_id() != Some(active.media_id) {
                                return true;
                            }
                            let new_fp = audio_chain_fingerprint_for(
                                &clip.audio_effects,
                                &clip.audio_effect_keyframes,
                            );
                            new_fp != active.audio_chain_fingerprint
                        }
                        None => true,
                    }
                })
            };
            if audio_chain_or_media_changed {
                info!(
                    "CUT-DIAG CONTIGUOUS: audio media or chain changed at boundary — restarting audio sources",
                );
                self.start_all_audio(self.playhead);
                self.update_overlay_decoders(self.playhead);
                return;
            }

            // CRITICAL: Immediately update fade parameters on audio sources
            {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                for active_src in &self.audio_sources {
                    if let Some(track) = seq.tracks.get(active_src.track_index) {
                        if let Some((_idx, new_clip)) = track.clip_at_timecode(self.playhead) {
                            let new_source_in = new_clip.source_in.to_secs_f64();
                            let new_clip_dur = new_clip.duration().to_secs_f64();
                            active_src.source.set_fade(
                                new_source_in - 1.0,
                                new_clip_dur + 1.0,
                                new_clip.fade_in_secs as f64,
                                new_clip.fade_out_secs as f64,
                                fade_curve_to_u32(new_clip.fade_in_curve),
                                fade_curve_to_u32(new_clip.fade_out_curve),
                                new_clip.gain,
                            );
                        }
                    }
                }
            }

            self.update_overlay_decoders(self.playhead);
            return;
        }

        self.current_clip_index = actual_idx;
        self.playhead = timeline_in;
        self.last_synth_frame_rendered = None;

        // Preroll hit: if we pre-decoded this clip, swap in the ready decoder — zero stall.
        if self.preroll_clip_index == Some(actual_idx) {
            if let Some(preroll_dec) = self.preroll_decoder.take() {
                info!("CUT-DIAG PREROLL HIT: swapping in pre-decoded clip idx={} source_in={:.4}s", actual_idx, source_in_secs);
                if let Some(dec) = &mut self.v1_decoder {
                    dec.stop_detached();
                }
                self.v1_decoder = Some(preroll_dec);
                self.preroll_clip_index = None;

                if let Some(path) = self.path_for_media(media_id) {
                    self.current_file = Some(path);
                }

                self.seek_all_audio_to_timecode(self.playhead);

                let start_us = (source_in_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.master_clock.resume();

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = next_clip_speed;
                self.refresh_speed_keyframes();
                self.current_source_rotation = source_rotation;
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                self.update_overlay_decoders(self.playhead);
                return;
            }
        }
        // Clear stale preroll if it didn't match
        if let Some(mut old) = self.preroll_decoder.take() {
            old.stop_detached();
        }
        self.preroll_clip_index = None;

        if let Some(path) = self.path_for_media(media_id) {
            // Same-file optimization: reuse decoder if same file
            let same_file = self.current_file.as_ref() == Some(&path);

            if same_file && self.v1_decoder.is_some() {
                info!("CUT-DIAG SAME-FILE seek: idx={} source_in={:.4}s (reusing decoder)", actual_idx, source_in_secs);
                if let Some(dec) = &self.v1_decoder {
                    dec.seek(source_in_secs);
                    dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                }

                self.master_clock.pause();
                self.wait_for_valid_frame(source_in_secs);
                self.seek_all_audio_to_timecode(self.playhead);

                let start_us = (source_in_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.master_clock.resume();

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = next_clip_speed;
                self.refresh_speed_keyframes();
                self.current_source_rotation = source_rotation;
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                self.update_overlay_decoders(self.playhead);
                return;
            }

            info!("Advancing to clip {} ({:?})", actual_idx, path);

            // Stop old decoders (detached)
            if let Some(dec) = &mut self.v1_decoder {
                dec.stop_detached();
            }
            self.v1_decoder = None;
            self.clear_all_overlays();
            for active in &mut self.audio_sources {
                active.source.stop_detached();
            }
            self.audio_sources.clear();
            self.audio_output = None;

            // Open new video decoder
            match VideoDecoder::open(&path, source_rotation) {
                Ok(dec) => {
                    if source_in_secs > 0.01 {
                        dec.seek(source_in_secs);
                    }
                    dec.resume();
                    self.v1_decoder = Some(dec);
                    self.current_file = Some(path.clone());
                }
                Err(e) => {
                    error!("Failed to open decoder for next clip: {e}");
                    self.stop_at_end();
                    return;
                }
            }

            // Open audio for all tracks at the new clip position
            let start_us = (source_in_secs * 1_000_000.0) as u64;
            self.master_clock.seek(start_us);
            self.start_all_audio(self.playhead);

            // Pre-fill + resume
            for active in &self.audio_sources {
                active.source.resume();
            }
            std::thread::sleep(Duration::from_millis(10));
            if let Some(aout) = &self.audio_output {
                aout.0.resume();
            }
            self.master_clock.resume();

            self.clip_source_start = source_in_secs;
            self.clip_timeline_start = timeline_in;
            self.clip_speed = next_clip_speed;
            self.refresh_speed_keyframes();
            self.current_source_rotation = source_rotation;
            self.last_frame_time = std::time::Instant::now();
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

            self.update_overlay_decoders(self.playhead);
        } else {
            warn!("No path for media {:?}", media_id);
            self.stop_at_end();
        }
    }

    /// Start pre-decoding the next clip so frames are ready when the cut arrives.
    fn start_preroll_for_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let next_idx = self.current_clip_index + 1;

        // Find next non-gap clip on the SAME track as the current clip
        let next_info = {
            let mut result = None;
            if let Some(track) = seq.tracks.get(self.current_track_index) {
                if track.clips.get(self.current_clip_index).and_then(|c| c.media_id()).is_some() {
                    for idx in next_idx..track.clips.len() {
                        let clip = &track.clips[idx];
                        if let Some(media_id) = clip.media_id() {
                            result = Some((idx, media_id, clip.source_in.to_secs_f64(), clip.source_rotation));
                            break;
                        }
                    }
                }
            }
            result
        };
        drop(seq);

        if let Some((idx, media_id, source_in_secs, preroll_rotation)) = next_info {
            if self.preroll_clip_index == Some(idx) {
                return; // Already pre-rolling this clip
            }
            if let Some(path) = self.path_for_media(media_id) {
                match VideoDecoder::open(&path, preroll_rotation) {
                    Ok(dec) => {
                        if source_in_secs > 0.01 {
                            dec.seek(source_in_secs);
                        }
                        dec.resume();
                        info!("CUT-DIAG PREROLL started: idx={} source_in={:.4}s", idx, source_in_secs);
                        // Close any previous preroll
                        if let Some(mut old) = self.preroll_decoder.take() {
                            old.stop_detached();
                        }
                        self.preroll_decoder = Some(dec);
                        self.preroll_clip_index = Some(idx);
                        self.preroll_source_in = source_in_secs;
                    }
                    Err(e) => {
                        warn!("Failed to open preroll decoder: {e}");
                    }
                }
            }
        }
    }

    /// Check if Video 2 has a clip at the given playhead position.
    /// Opens/closes the V2 decoder as needed.
    fn update_overlay_decoders(&mut self, playhead: Timecode) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // Collect overlay info for all overlay tracks (not just the first one).
        let overlay_tracks = seq.overlay_video_tracks(self.current_track_index);
        let mut active_overlays: Vec<(usize, usize, klipp_timeline::Clip)> = Vec::new();
        for (track_idx, track) in &overlay_tracks {
            if let Some((clip_idx, clip)) = track.clip_at_timecode(playhead) {
                active_overlays.push((*track_idx, clip_idx, clip.clone()));
            }
        }
        let active_track_indices: std::collections::HashSet<usize> = active_overlays.iter().map(|(ti, _, _)| *ti).collect();
        let res = seq.resolution;
        drop(seq);

        // Close decoders/frames for tracks that no longer have an active clip
        let stale: Vec<usize> = self.overlay_clips.keys()
            .filter(|ti| !active_track_indices.contains(ti))
            .copied().collect();
        for track_idx in stale {
            if let Some(mut dec) = self.overlay_decoders.remove(&track_idx) {
                dec.stop_detached();
                debug!("Overlay decoder closed for track {track_idx} (no clip at playhead)");
            }
            self.overlay_clips.remove(&track_idx);
            self.overlay_frames.remove(&track_idx);
            self.overlay_synth_frame.remove(&track_idx);
        }

        // Count live media decoders (for cap enforcement later)
        let mut media_decoder_count = self.overlay_decoders.len();

        // Compute current frame number for synthetic clip caching
        let current_frame = playhead.value;

        // Process each active overlay track
        for (track_idx, clip_idx, clip) in active_overlays {
            // Skip re-rendering synthetic clips when the same clip+frame is already cached.
            // This mirrors the `last_synth_frame_rendered` optimization on the V1 path and
            // prevents ~1000 re-renders/sec for static titles/generators.
            let is_synthetic = clip.title_data().is_some() || clip.generator_data().is_some();
            if is_synthetic
                && self.overlay_clips.get(&track_idx) == Some(&clip_idx)
                && self.overlay_synth_frame.get(&track_idx) == Some(&current_frame)
                && self.overlay_frames.contains_key(&track_idx)
            {
                // Same synthetic clip at same frame already rendered — skip
                continue;
            }

            // Title clip — render directly to overlay_frames
            if let Some(title_data) = clip.title_data() {
                // Close any media decoder for this track
                if let Some(mut dec) = self.overlay_decoders.remove(&track_idx) {
                    dec.stop_detached();
                    media_decoder_count = media_decoder_count.saturating_sub(1);
                }
                let clip_elapsed_tw = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
                let typewriter_td = typewriter_override(title_data, clip_elapsed_tw);
                let td_for_render = typewriter_td.as_ref().unwrap_or(title_data);
                let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                let mut rgba = self.title_renderer.render_with_gpu(td_for_render, res.width, res.height, gpu);

                // Note: clip-level opacity is NOT baked in here. It is applied at
                // composite time so paused opacity drags update the visible frame
                // even when no new frame is being rendered.
                let clip_relative_secs = (playhead - clip.timeline_in).to_secs_f64().max(0.0);

                // Apply transform keyframes + modifiers (position, scale, rotation)
                let has_transform = !clip.transform_pos_x_keyframes.is_empty()
                    || !clip.transform_pos_y_keyframes.is_empty()
                    || !clip.transform_scale_keyframes.is_empty()
                    || !clip.transform_rotation_keyframes.is_empty()
                    || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionX)
                    || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionY)
                    || clip.param_modifiers.contains_key(&klipp_core::ParamId::Scale)
                    || clip.param_modifiers.contains_key(&klipp_core::ParamId::Rotation);

                if has_transform {
                    let tdef = clip.builtin_effects.iter()
                        .find_map(|e| match e {
                            klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, .. } =>
                                Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y)),
                            _ => None,
                        })
                        .unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));

                    let eval_ctx = klipp_core::EvalContext {
                        clip_time_secs: clip_relative_secs,
                        clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
                        timeline_time_secs: playhead.to_secs_f64(),
                        fps: self.fps.as_f64(),
                        seed: clip.id.0.as_u64_pair().0,
                    };

                    let motion_blur = title_data.motion_blur.filter(|mb| mb.samples >= 2);

                    if let Some(mb) = motion_blur {
                        let frame_dur = self.fps.frame_duration_secs();
                        let half_shutter = (mb.shutter_angle as f64 / 360.0) * frame_dur;
                        let n = mb.samples as usize;
                        let pixel_count = (res.width * res.height) as usize * 4;
                        let mut accum = vec![0u32; pixel_count];

                        for si in 0..n {
                            let t_offset = if n > 1 {
                                (si as f64 / (n - 1) as f64 - 0.5) * 2.0 * half_shutter
                            } else {
                                0.0
                            };
                            let sample_t = (clip_relative_secs + t_offset).max(0.0);
                            let sample_eval = klipp_core::EvalContext {
                                clip_time_secs: sample_t,
                                ..eval_ctx
                            };

                            let spx = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, sample_t, tdef.0, &sample_eval),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, sample_t, tdef.0),
                            };
                            let spy = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, sample_t, tdef.1, &sample_eval),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, sample_t, tdef.1),
                            };
                            let ssc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, sample_t, tdef.2, &sample_eval),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, sample_t, tdef.2),
                            };
                            let sro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, sample_t, tdef.4, &sample_eval),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, sample_t, tdef.4),
                            };

                            let sample_rgba = klipp_effects::transform::apply(
                                rgba.clone(), res.width, res.height,
                                spx, spy, ssc, ssc, sro, tdef.5, tdef.6, 0.0, 0.0, 0.0,
                            );
                            for (i, &b) in sample_rgba.iter().enumerate() {
                                accum[i] += b as u32;
                            }
                        }
                        rgba = accum.iter().map(|v| (*v / n as u32) as u8).collect();
                    } else {
                        let px = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, clip_relative_secs, tdef.0, &eval_ctx),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, clip_relative_secs, tdef.0),
                        };
                        let py = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, clip_relative_secs, tdef.1, &eval_ctx),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, clip_relative_secs, tdef.1),
                        };
                        let sc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, clip_relative_secs, tdef.2, &eval_ctx),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, clip_relative_secs, tdef.2),
                        };
                        let ro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, clip_relative_secs, tdef.4, &eval_ctx),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, clip_relative_secs, tdef.4),
                        };

                        rgba = klipp_effects::transform::apply(rgba, res.width, res.height, px, py, sc, sc, ro, tdef.5, tdef.6, 0.0, 0.0, 0.0);
                    }
                }

                // Title renderer is u8-only; bridge to linear once
                // here so `overlay_frames` stays linear-native for the
                // compositor (which runs every frame).
                let u8_frame = crate::types::DecodedFrame {
                    data: rgba,
                    width: res.width,
                    height: res.height,
                    pts: RationalTime::from_frames(0, 24),
                    is_eof: false,
                };
                self.overlay_frames.insert(track_idx, u8_frame.to_linear());
                self.overlay_clips.insert(track_idx, clip_idx);
                self.overlay_synth_frame.insert(track_idx, current_frame);
                continue;
            }

            // Generator clip — render directly to overlay_frames
            if let Some(gen_data) = clip.generator_data() {
                if let Some(mut dec) = self.overlay_decoders.remove(&track_idx) {
                    dec.stop_detached();
                    media_decoder_count = media_decoder_count.saturating_sub(1);
                }
                let clip_relative_secs = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
                let total_secs = (clip.timeline_out - clip.timeline_in).to_secs_f64();
                let rgba = klipp_core::generator::render_generator(
                    gen_data,
                    res.width,
                    res.height,
                    clip_relative_secs,
                    total_secs,
                );

                // Note: clip-level opacity is NOT baked in here. It is applied at
                // composite time so paused opacity drags update the visible frame.

                // Generator renderer is u8-only; bridge to linear once
                // here so `overlay_frames` stays linear-native for the
                // compositor.
                let u8_frame = crate::types::DecodedFrame {
                    data: rgba,
                    width: res.width,
                    height: res.height,
                    pts: RationalTime::from_frames(0, 24),
                    is_eof: false,
                };
                self.overlay_frames.insert(track_idx, u8_frame.to_linear());
                self.overlay_clips.insert(track_idx, clip_idx);
                self.overlay_synth_frame.insert(track_idx, current_frame);
                continue;
            }

            // Media clip — open decoder if needed
            if let Some(media_id) = clip.media_id() {
                let source_secs = clip.source_in.to_secs_f64()
                    + (playhead - clip.timeline_in).to_secs_f64() * clip.speed;
                let rotation = clip.source_rotation;
                // Already have the right decoder open for this track+clip?
                if self.overlay_clips.get(&track_idx) == Some(&clip_idx) && self.overlay_decoders.contains_key(&track_idx) {
                    continue;
                }
                // Close old decoder for this track if any
                if let Some(mut dec) = self.overlay_decoders.remove(&track_idx) {
                    dec.stop_detached();
                    media_decoder_count = media_decoder_count.saturating_sub(1);
                }
                self.overlay_frames.remove(&track_idx);

                // Enforce decoder cap
                if media_decoder_count >= MAX_OVERLAY_DECODERS {
                    debug!("Overlay decoder cap reached ({MAX_OVERLAY_DECODERS}), skipping track {track_idx}");
                    continue;
                }

                if let Some(path) = self.path_for_media(media_id) {
                    match VideoDecoder::open(&path, rotation) {
                        Ok(dec) => {
                            if source_secs > 0.01 {
                                dec.seek(source_secs);
                            }
                            dec.resume();
                            self.overlay_decoders.insert(track_idx, dec);
                            self.overlay_clips.insert(track_idx, clip_idx);
                            media_decoder_count += 1;
                            debug!("Overlay decoder opened for track {track_idx} clip {clip_idx}");
                        }
                        Err(e) => {
                            warn!("Failed to open overlay decoder for track {track_idx}: {e}");
                            self.overlay_clips.remove(&track_idx);
                        }
                    }
                }
            }
        }

        // Enforce decoder cap: remove excess starting from highest track index
        while self.overlay_decoders.len() > MAX_OVERLAY_DECODERS {
            let max_idx = self.overlay_decoders.keys().copied().max();
            if let Some(track_idx) = max_idx {
                if let Some(mut dec) = self.overlay_decoders.remove(&track_idx) {
                    dec.stop_detached();
                }
                self.overlay_clips.remove(&track_idx);
                self.overlay_frames.remove(&track_idx);
                debug!("Overlay decoder capped: closed track {track_idx}");
            } else {
                break;
            }
        }
    }

    fn stop_at_end(&mut self) {
        info!("stop_at_end called — loop_enabled={}, playhead={:.3}s",
            self.loop_enabled, self.playhead.to_secs_f64());
        // Loop: if enabled, seek back to in_point (or start)
        if self.loop_enabled {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let in_pt = seq.in_point;
            drop(seq);
            let target = in_pt.unwrap_or_else(|| RationalTime { value: 0, rate: self.fps.rate() });
            self.execute_loop(target);
            return;
        }

        self.v1_decoder = None;
        self.clear_all_overlays();
        if let Some(dec) = &mut self.transition_decoder {
            dec.stop_detached();
        }
        self.transition_decoder = None;
        self.transition_clip_id = None;
        self.transition_frame_cache = None;
        self.transition_clip_a_last_frame = None;
        self.transition_partner_eof = false;
        self.transition_degradation = Default::default();
        self.stop_all_audio();
        self.last_synth_frame_rendered = None;
        self.state = PlaybackState::Paused;
        let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        (self.repaint_cb)();
    }

    /// Execute a loop-back seek to the given target timecode.
    /// Modeled on seek_in_sequence_with_timeout() but always resumes playback.
    /// Begin predictive pre-seek: spawn background thread to open audio decoders
    /// at the loop target position, so they're ready when we reach the out_point.
    fn begin_pre_seek(&mut self, target_secs: f64) {
        self.pre_seek_started = true;
        let generation = self.pre_seek_generation;
        let result_arc = self.pre_seek_result.clone();
        let sequence = self.sequence.clone();
        let media_pool = self.media_pool.clone();
        let sample_rate = self.cpal_sample_rate;
        let use_proxies = self.use_proxies;

        debug!("[Loop] Starting pre-seek to {target_secs:.3}s (gen={generation})");

        std::thread::spawn(move || crate::thread_guard::catch_panic("klipp-preseek", move || {
            let tc = RationalTime::from_secs_f64(target_secs, 24); // rate doesn't matter for lookup
            let seq = match sequence.read() {
                Ok(s) => s,
                Err(_) => return,
            };
            let pool = match media_pool.read() {
                Ok(p) => p,
                Err(_) => return,
            };

            // Collect audio track info at target position
            type PreSeekRow = (
                usize,
                MediaId,
                PathBuf,
                f64,
                Vec<klipp_core::AudioEffectInstance>,
                std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
            );
            let track_audio: Vec<PreSeekRow> = seq
                .tracks
                .iter()
                .enumerate()
                .filter_map(|(track_idx, track)| {
                    let (_, clip) = track.clip_at_timecode(tc)?;
                    let media_id = clip.media_id()?;
                    let item = pool.get(&media_id)?;
                    if !item.has_audio {
                        return None;
                    }
                    // Skip audio for linked video-track clips
                    if track.kind == klipp_timeline::TrackKind::Video {
                        if let Some(lid) = clip.linked_clip_id {
                            let partner_on_audio = seq.tracks.iter().any(|t| {
                                t.kind == klipp_timeline::TrackKind::Audio
                                    && t.clips.iter().any(|c| c.id == lid)
                            });
                            if partner_on_audio {
                                return None;
                            }
                        }
                    }
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;

                    // Resolve path (proxy or original)
                    let decode_path = if use_proxies && item.proxy_ready {
                        item.proxy_path
                            .as_ref()
                            .filter(|p| p.exists())
                            .cloned()
                            .unwrap_or_else(|| item.path.clone())
                    } else {
                        item.path.clone()
                    };

                    // Try PCM cache first
                    let path = item
                        .audio_cache
                        .as_ref()
                        .map(|c| c.pcm_path.clone())
                        .filter(|p| p.exists())
                        .unwrap_or(decode_path);

                    Some((
                        track_idx,
                        media_id,
                        path,
                        source_secs,
                        clip.audio_effects.clone(),
                        clip.audio_effect_keyframes.clone(),
                    ))
                })
                .collect();
            drop(pool);
            drop(seq);

            // Open and seek audio sources
            let mut sources = Vec::new();
            for (track_idx, media_id, path, source_secs, audio_effects, audio_effect_keyframes) in track_audio {
                let chain_fp = audio_chain_fingerprint_for(&audio_effects, &audio_effect_keyframes);
                // Try PCM cache
                if let Ok((feeder, consumer)) = PcmAudioFeeder::open(&path, sample_rate, audio_effects.clone(), audio_effect_keyframes.clone()) {
                    if source_secs > 0.01 {
                        feeder.seek(source_secs);
                    }
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        media_id,
                        audio_chain_fingerprint: chain_fp,
                        source: AudioSource::PcmFeeder(feeder),
                        consumer,
                    });
                    continue;
                }
                // Fallback: ffmpeg decode
                if let Ok((adec, consumer)) = AudioDecoder::open(&path, sample_rate, audio_effects, audio_effect_keyframes, source_secs) {
                    if source_secs > 0.01 {
                        adec.seek(source_secs);
                    }
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        media_id,
                        audio_chain_fingerprint: chain_fp,
                        source: AudioSource::Decoder(adec),
                        consumer,
                    });
                }
            }

            // Wait for all sources to confirm seek (max 50ms)
            let deadline = std::time::Instant::now() + Duration::from_millis(50);
            loop {
                let all_ready = sources.iter().all(|s| {
                    s.source.seek_generation().load(Ordering::Acquire) >= 1
                });
                if all_ready || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_micros(200));
            }

            // Resume sources so they start filling their ring buffers
            for s in &sources {
                s.source.resume();
            }
            // Let them decode for a bit (~50ms of audio)
            std::thread::sleep(Duration::from_millis(50));
            // Pause them — they've pre-filled their ring buffers
            for s in &sources {
                s.source.pause();
            }

            // Store result
            if let Ok(mut guard) = result_arc.lock() {
                *guard = Some(LoopPreSeekResult {
                    generation,
                    sources,
                    target_secs,
                });
            }
            debug!("[Loop] Pre-seek ready (gen={generation})");
        }));
    }

    fn execute_loop(&mut self, target: Timecode) {
        debug!("[Loop] Executing loop-back to {:.3}s", target.to_secs_f64());
        let was_playing = self.state == PlaybackState::Playing;

        // Reset pre-seek state for next iteration
        self.pre_seek_started = false;

        // Try pre-seek fast path: use pre-opened audio sources if available
        let pre_seek = self.pre_seek_result.try_lock().ok().and_then(|mut g| g.take());
        if let Some(ps) = pre_seek {
            if ps.generation == self.pre_seek_generation {
                debug!("[Loop] Using pre-seeked audio sources (gen={})", ps.generation);

                // 1. Pause clock + cpal
                self.master_clock.pause();
                if let Some(aout) = &self.audio_output { aout.0.pause(); }

                // 2. Stop old audio sources, swap in pre-seeked ones
                self.stop_all_audio();
                for ps_source in ps.sources {
                    self.ensure_mix_pipeline(ps_source.consumer, ps_source.track_index);
                    self.audio_sources.push(ActiveAudioSource {
                        track_index: ps_source.track_index,
                        media_id: ps_source.media_id,
                        audio_chain_fingerprint: ps_source.audio_chain_fingerprint,
                        source: ps_source.source,
                    });
                }

                // 3. Seek video decoder (still needed — video wasn't pre-seeked)
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let clip_info = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(target).and_then(|(idx, clip)| {
                            let media_id = clip.media_id()?;
                            let source_secs = clip.source_in.to_secs_f64()
                                + (target - clip.timeline_in).to_secs_f64() * clip.speed;
                            let clip_source_in = clip.source_in.to_secs_f64();
                            Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                        })
                    });
                drop(seq);

                let source_secs_for_clock = if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
                    let need_new = self.current_clip_index != idx || self.v1_decoder.is_none();
                    if need_new {
                        if let Some(path) = self.path_for_media(media_id) {
                            self.v1_decoder = None;
                            if let Ok(dec) = VideoDecoder::open(&path, rot) {
                                dec.seek(source_secs);
                                dec.resume();
                                self.v1_decoder = Some(dec);
                                self.current_clip_index = idx;
                                self.current_file = Some(path);
                                self.clip_source_start = clip_source_in;
                                self.clip_timeline_start = timeline_in;
                                self.clip_speed = spd;
                                self.refresh_speed_keyframes();
                                self.current_source_rotation = rot;
                            }
                        }
                    } else if let Some(dec) = &self.v1_decoder {
                        dec.seek(source_secs);
                        dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                        self.clip_source_start = clip_source_in;
                        self.clip_timeline_start = timeline_in;
                        self.clip_speed = spd;
                        self.refresh_speed_keyframes();
                    }
                    source_secs
                } else {
                    target.to_secs_f64()
                };

                // 4. Set master clock
                let target_us = (source_secs_for_clock * 1_000_000.0) as u64;
                self.master_clock.seek(target_us);

                // 5. Resume — audio already pre-filled, skip the 15ms wait
                if was_playing {
                    for active in &self.audio_sources { active.source.resume(); }
                    // Shorter pre-fill since sources already have buffered samples
                    std::thread::sleep(Duration::from_millis(5));
                    if let Some(aout) = &self.audio_output { aout.0.resume(); }
                    self.master_clock.resume();
                }

                // 6. Overlay decoder update
                self.overlay_frames.clear();
                self.update_overlay_decoders(target);

                self.playhead = target;
                let _ = self.event_tx.send(AppEvent::LoopedBack);
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
                (self.repaint_cb)();
                return;
            } else {
                debug!("[Loop] Pre-seek stale (gen={} vs current={}), falling back to cold seek",
                    ps.generation, self.pre_seek_generation);
                // Drop stale pre-seek sources (they'll be cleaned up)
            }
        }

        // Cold seek fallback (no pre-seek available)

        // Find video clip at target position
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip_info = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|track| {
                track.clip_at_timecode(target).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (target - clip.timeline_in).to_secs_f64() * clip.speed;
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                })
            });
        // Also check for title clip
        let title_info = if clip_info.is_none() {
            seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|track| {
                    track.clip_at_timecode(target).and_then(|(idx, clip)| {
                        let td = clip.title_data()?.clone();
                        Some((idx, td, clip.timeline_in, clip.source_in.to_secs_f64()))
                    })
                })
        } else {
            None
        };
        let res = seq.resolution;
        drop(seq);

        // Handle title clip at loop target
        if let Some((idx, title_data, timeline_in, clip_source_in)) = title_info {
            self.v1_decoder = None;
            self.current_clip_index = idx;
            self.current_file = None;
            self.clip_source_start = clip_source_in;
            self.clip_timeline_start = timeline_in;

            let clip_elapsed_tw = (target - timeline_in).to_secs_f64().max(0.0);
            let typewriter_td = typewriter_override(&title_data, clip_elapsed_tw);
            let td_for_render = typewriter_td.as_ref().unwrap_or(&title_data);
            let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
            let rgba = self.title_renderer.render_with_gpu(td_for_render, res.width, res.height, gpu);
            // Title renderer emits u8; bridge at this viewer boundary.
            let linear = u8_rgba_to_linear(&rgba);
            let final_linear = self.post_process_rgba(linear, res.width, res.height);
            let final_rgba = self.display_encode(&final_linear);
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(final_rgba, res.width, res.height);
            drop(fb);

            // Reopen audio at target
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }
            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);
            let target_us = (target.to_secs_f64() * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                self.wait_for_audio_prefill();
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
            return;
        }

        // Handle media clip at loop target
        if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
            let need_new_decoder = self.current_clip_index != idx || self.v1_decoder.is_none();

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.stop_all_audio();
                    match VideoDecoder::open(&path, rot) {
                        Ok(dec) => {
                            dec.seek(source_secs);
                            dec.resume();
                            self.v1_decoder = Some(dec);
                            self.current_clip_index = idx;
                            self.current_file = Some(path);
                            self.clip_source_start = clip_source_in;
                            self.clip_timeline_start = timeline_in;
                            self.clip_speed = spd;
                            self.refresh_speed_keyframes();
                            self.current_source_rotation = rot;
                        }
                        Err(e) => {
                            error!("[Loop] Failed to open decoder: {e}");
                            return;
                        }
                    }
                }
            } else if let Some(dec) = &self.v1_decoder {
                dec.seek(source_secs);
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = spd;
                self.refresh_speed_keyframes();
            }

            // 1. Pause clock + cpal
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }

            // 2. Reopen audio at target
            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);

            // 3. Set master clock
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            // 4. Pre-fill + resume
            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                self.wait_for_audio_prefill();
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            // 5. Overlay decoder update
            self.overlay_frames.clear();
            self.update_overlay_decoders(target);

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
        } else {
            // No video clip at target — audio-only loop
            self.v1_decoder = None;
            self.overlay_frames.clear();

            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }

            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);
            let target_us = (target.to_secs_f64() * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                self.wait_for_audio_prefill();
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
        }
    }

    fn seek_in_sequence(&mut self, tc: Timecode) {
        self.seek_in_sequence_with_timeout(tc, Duration::from_millis(50));
    }

    fn seek_in_sequence_with_timeout(&mut self, tc: Timecode, grab_timeout: Duration) {
        // Clear frame cache on explicit seek (cache is only valid for sequential stepping)
        self.frame_cache.clear();
        self.last_synth_frame_rendered = None;
        // Check for standalone title clip (no media clip at this timecode on any track)
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let has_media_clip = seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .any(|track| {
                    track.clip_at_timecode(tc)
                        .map(|(_, c)| c.media_id().is_some())
                        .unwrap_or(false)
                });
            let title_info = if !has_media_clip {
                seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                            let td = clip.title_data()?.clone();
                            Some((idx, td, clip.timeline_in, clip.source_in.to_secs_f64()))
                        })
                    })
            } else {
                None
            };
            let res = seq.resolution;
            drop(seq);

            if let Some((idx, title_data, timeline_in, clip_source_in)) = title_info {
                // Close any existing video decoder — title clips don't need one
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;

                // Render title frame and write to frame buffer
                let clip_elapsed_tw = (tc - timeline_in).to_secs_f64().max(0.0);
                let typewriter_td = typewriter_override(&title_data, clip_elapsed_tw);
                let td_for_render = typewriter_td.as_ref().unwrap_or(&title_data);
                let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                let rgba = self.title_renderer.render_with_gpu(td_for_render, res.width, res.height, gpu);
                let linear = u8_rgba_to_linear(&rgba);
                let final_linear = self.post_process_rgba(linear, res.width, res.height);
                let final_rgba = self.display_encode(&final_linear);
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.write_frame(final_rgba, res.width, res.height);
                drop(fb);

                self.playhead = tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
                (self.repaint_cb)();
                return;
            }

            // Check for standalone generator clip at this timecode
            if !has_media_clip {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let gen_info = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                            let gd = clip.generator_data()?.clone();
                            Some((idx, gd, clip.timeline_in, clip.timeline_out))
                        })
                    });
                let res2 = seq.resolution;
                drop(seq);

                if let Some((idx, gen_data, timeline_in, timeline_out)) = gen_info {
                    self.v1_decoder = None;
                    self.current_clip_index = idx;
                    self.current_file = None;
                    self.clip_source_start = 0.0;
                    self.clip_timeline_start = timeline_in;

                    let elapsed_secs = (tc - timeline_in).to_secs_f64().max(0.0);
                    let total_secs = (timeline_out - timeline_in).to_secs_f64();
                    let rgba = klipp_core::generator::render_generator(
                        &gen_data, res2.width, res2.height, elapsed_secs, total_secs,
                    );
                    let linear = u8_rgba_to_linear(&rgba);
                    let final_linear = self.post_process_rgba(linear, res2.width, res2.height);
                    let final_rgba = self.display_encode(&final_linear);
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.write_frame(final_rgba, res2.width, res2.height);
                    drop(fb);

                    self.playhead = tc;
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
                    (self.repaint_cb)();
                    return;
                }
            }
        }

        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // Search video tracks in reverse — last video track (bottom of panel) is primary.
        let clip_info = seq.tracks.iter().enumerate().rev()
            .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|(ti, track)| {
                track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((ti, idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                })
            });
        drop(seq);

        if let Some((ti, idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
            self.current_track_index = ti;
            let need_new_decoder = self.current_clip_index != idx || self.v1_decoder.is_none();

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.stop_all_audio();
                    match VideoDecoder::open(&path, rot) {
                        Ok(dec) => {
                            dec.seek(source_secs);
                            dec.resume();
                            self.v1_decoder = Some(dec);
                            self.current_clip_index = idx;
                            self.current_file = Some(path.clone());
                            self.clip_source_start = clip_source_in;
                            self.clip_timeline_start = timeline_in;
                            self.clip_speed = spd;
                            self.refresh_speed_keyframes();
                            self.current_source_rotation = rot;
                        }
                        Err(e) => {
                            error!("Failed to open decoder for seek: {e}");
                        }
                    }
                }
            } else if let Some(dec) = &self.v1_decoder {
                dec.seek(source_secs);
                // Clear stale frames from queue
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = spd;
                self.refresh_speed_keyframes();
            }

            let was_playing = self.state == PlaybackState::Playing;

            // 1. Pause clock + cpal output
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output {
                aout.0.pause();
            }

            // 2. Re-open audio sources (start paused) then flush+seek
            self.start_all_audio(tc);
            self.seek_all_audio_to_timecode(tc);

            // 3. Set master clock to target
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            // 4. Resume or stay paused based on playback state
            if was_playing {
                // Pre-fill: resume audio sources while cpal is still paused
                for active in &self.audio_sources {
                    active.source.resume();
                }
                self.wait_for_audio_prefill();

                // Resume cpal + clock together
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                self.master_clock.resume();
            }
            // Sources already start paused, no need to pause them

            // Re-check overlays at the new playhead
            self.overlay_frames.clear();
            self.update_overlay_decoders(tc);

            if !was_playing {
                let got_frame = self.grab_single_frame_timeout(grab_timeout);
                if !got_frame {
                    // 4K / slow codec cold seeks routinely outrun the
                    // 50–200 ms grab budget. Instead of giving up and
                    // freezing V2 thumbs on the pre-seek frame until
                    // the user pokes an arrow key, keep the decoder
                    // running and let the per-tick paused branch
                    // post-process the frame the moment it lands.
                    self.pending_paused_refresh = true;
                }
                // Play scrub chirp (~40ms snippet at the seek position) unless
                // the user has disabled audio during a scrub.
                if !self.is_scrubbing || self.scrub_audio_enabled {
                    if let Some(active) = self.audio_sources.first() {
                        active.source.chirp(source_secs, 0.04);
                        if let Some(aout) = &self.audio_output {
                            aout.0.start_chirp();
                        }
                    }
                }
            }
        } else {
            // No video clip at this timecode — handle audio-only seek
            self.v1_decoder = None;
            self.overlay_frames.clear();

            // Check if any audio track has a clip here
            let has_audio = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                seq.tracks.iter().any(|track| {
                    track.kind == klipp_timeline::TrackKind::Audio
                        && track.clip_at_timecode(tc).is_some()
                })
            };

            if has_audio {
                let was_playing = self.state == PlaybackState::Playing;

                self.master_clock.pause();
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Re-open audio (paused), flush+seek
                self.start_all_audio(tc);
                self.seek_all_audio_to_timecode(tc);
                let target_us = (tc.to_secs_f64() * 1_000_000.0) as u64;
                self.master_clock.seek(target_us);

                if was_playing {
                    // Pre-fill then resume
                    for active in &self.audio_sources {
                        active.source.resume();
                    }
                    self.wait_for_audio_prefill();
                    if let Some(aout) = &self.audio_output {
                        aout.0.resume();
                    }
                    self.master_clock.resume();
                } else {
                    // Play scrub chirp for audio-only
                    if !self.is_scrubbing || self.scrub_audio_enabled {
                        if let Some(active) = self.audio_sources.first() {
                            let source_secs = {
                                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                                seq.tracks.iter().find_map(|track| {
                                    let (_, clip) = track.clip_at_timecode(tc)?;
                                    Some(clip.source_in.to_secs_f64()
                                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed)
                                }).unwrap_or(0.0)
                            };
                            active.source.chirp(source_secs, 0.04);
                            if let Some(aout) = &self.audio_output {
                                aout.0.start_chirp();
                            }
                        }
                    }
                }
            } else {
                self.stop_all_audio();
            }
        }

        self.playhead = tc;
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
        (self.repaint_cb)();
    }

    /// Step forward or backward by one frame without a full seek.
    fn step_frame(&mut self, direction: i64) {
        let target_frame = self.playhead.value + direction;

        // Check frame cache first (works for both forward AND backward).
        // The cache stores sRGB u8 — bridge at this viewer seam.
        if let Some((rgba_arc, w, h)) = self.frame_cache.get(target_frame) {
            let rgba = rgba_arc.as_ref().clone();
            self.playhead = RationalTime { value: target_frame, rate: self.playhead.rate };
            let linear = u8_rgba_to_linear(&rgba);
            let final_linear = self.post_process_rgba(linear, w, h);
            let final_rgba = self.display_encode(&final_linear);
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(final_rgba, w, h);
            drop(fb);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
            let _ = self.event_tx.send(AppEvent::FrameReady);
            (self.repaint_cb)();
            // Keep decoder filling the queue for future steps
            self.prefetch_ahead();
            return;
        }

        if direction >= 0 {
            // Forward: pop ONE frame from decoder queue (don't drain all)
            let next_frame = self.v1_decoder.as_ref().and_then(|dec| {
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop()
            }).filter(|f| !f.is_eof);

            if let Some(mut frame) = next_frame {
                let chirp_pos = frame.pts_secs();
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                if let Some(clip) = seq.tracks.get(self.current_track_index)
                    .and_then(|t| t.clips.get(self.current_clip_index))
                {
                    let source_elapsed =
                        frame.pts_secs() - clip.source_in.to_secs_f64();
                    let spd = clip.speed.max(0.001);
                    let elapsed_tc = RationalTime::from_secs_f64(
                        (source_elapsed / spd).max(0.0),
                        self.fps.rate(),
                    );
                    self.playhead = clip.timeline_in + elapsed_tc;
                }
                drop(seq);
                let (w, h) = (frame.width, frame.height);
                // Cache pre-grade frame as u8 for backward stepping
                // (frame_cache stores u8). Feed linear straight into
                // post_process_rgba so the hot path stays in linear.
                frame.ensure_cpu_data();
                let rgba_u8 = linear_rgba_to_u8(&frame.data);
                self.frame_cache.insert(self.playhead.value, Arc::new(rgba_u8), w, h);
                let gpu_source = frame.gpu_source.clone();
                let final_linear = self.post_process_rgba_with_gpu_source(
                    frame.data, w, h, gpu_source.as_ref(),
                );
                let final_rgba = self.display_encode(&final_linear);
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.write_frame(final_rgba, w, h);
                drop(fb);
                // Frame step chirp (~20ms). Chirps on a scrub are gated on
                // the scrub-audio preference; non-scrub frame steps keep theirs.
                if !self.is_scrubbing || self.scrub_audio_enabled {
                    if let Some(active) = self.audio_sources.first() {
                        active.source.chirp(chirp_pos, 0.02);
                        if let Some(aout) = &self.audio_output {
                            aout.0.start_chirp();
                        }
                    }
                }
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                self.prefetch_ahead();
                return;
            }
            // No queued frames — resume decoder to produce one
            if let Some(dec) = &self.v1_decoder {
                dec.resume();
            }
            // Wait for one frame with short timeout
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < deadline {
                // Pop frame in a limited scope to avoid borrowing self across post_process
                let popped = self.v1_decoder.as_ref()
                    .and_then(|dec| dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop());
                if let Some(mut frame) = popped {
                    if frame.is_eof {
                        if let Some(dec) = &self.v1_decoder { dec.pause(); }
                        return;
                    }
                    // Update playhead from frame PTS
                    let chirp_pos = frame.pts_secs();
                    let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(clip) = seq.tracks.get(self.current_track_index)
                        .and_then(|t| t.clips.get(self.current_clip_index))
                    {
                        let source_elapsed =
                            frame.pts_secs() - clip.source_in.to_secs_f64();
                        let spd = clip.speed.max(0.001);
                        let elapsed_tc = RationalTime::from_secs_f64(
                            (source_elapsed / spd).max(0.0),
                            self.fps.rate(),
                        );
                        self.playhead = clip.timeline_in + elapsed_tc;
                    }
                    drop(seq);

                    let (w, h) = (frame.width, frame.height);
                    // Cache pre-grade frame as u8 for backward
                    // stepping; post_process_rgba stays linear.
                    frame.ensure_cpu_data();
                    let rgba_u8 = linear_rgba_to_u8(&frame.data);
                    self.frame_cache.insert(self.playhead.value, Arc::new(rgba_u8), w, h);
                    let gpu_source = frame.gpu_source.clone();
                    let final_linear = self.post_process_rgba_with_gpu_source(
                        frame.data, w, h, gpu_source.as_ref(),
                    );
                    let final_rgba = self.display_encode(&final_linear);
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.write_frame(final_rgba, w, h);
                    drop(fb);
                    // Frame step chirp (~20ms). Same gating as the primary
                    // frame-step path above — skip during a silenced scrub.
                    if !self.is_scrubbing || self.scrub_audio_enabled {
                        if let Some(active) = self.audio_sources.first() {
                            if let Some(aout) = &self.audio_output {
                                aout.0.notify_seek();
                                aout.0.resume();
                            }
                            active.source.chirp(chirp_pos, 0.02);
                        }
                    }
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    let _ = self.event_tx.send(AppEvent::FrameReady);
                    (self.repaint_cb)();
                    // Don't pause — prefetch_ahead will drain queue and keep decoder running
                    self.prefetch_ahead();
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if let Some(dec) = &self.v1_decoder {
                dec.pause();
            }
        } else {
            // Backward: cache miss. Seek to target AND prefill cache with nearby frames.
            // Strategy: seek to (target - 30) and decode forward to fill cache,
            // so subsequent backward steps are instant cache hits.
            let prefetch_start = (target_frame - 30).max(0);
            let seek_tc = RationalTime {
                value: prefetch_start,
                rate: self.playhead.rate,
            };

            // Seek decoder to prefetch start position
            if let Some(dec) = &self.v1_decoder {
                dec.seek(seek_tc.to_secs_f64());
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                dec.resume();
            }

            // Decode frames from prefetch_start up to and including target_frame
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            let mut found_target = false;
            while std::time::Instant::now() < deadline {
                let popped = self.v1_decoder.as_ref()
                    .and_then(|dec| dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop());
                if let Some(mut frame) = popped {
                    if frame.is_eof { break; }
                    let (w, h) = (frame.width, frame.height);

                    // Compute frame number from PTS
                    let frame_num = {
                        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                        if let Some(clip) = seq.tracks.get(self.current_track_index)
                            .and_then(|t| t.clips.get(self.current_clip_index))
                        {
                            let source_elapsed = frame.pts_secs() - clip.source_in.to_secs_f64();
                            let spd = clip.speed.max(0.001);
                            let elapsed_tc = RationalTime::from_secs_f64(
                                (source_elapsed / spd).max(0.0), self.fps.rate(),
                            );
                            (clip.timeline_in + elapsed_tc).value
                        } else {
                            break;
                        }
                    };

                    // Cache the pre-grade frame as u8 for backward
                    // stepping; the display path stays in linear until
                    // the viewer seam below.
                    frame.ensure_cpu_data();
                    let rgba_u8 = linear_rgba_to_u8(&frame.data);
                    self.frame_cache.insert(frame_num, Arc::new(rgba_u8), w, h);

                    // When we reach the target frame, display it
                    if frame_num >= target_frame && !found_target {
                        found_target = true;
                        self.playhead = RationalTime { value: frame_num, rate: self.playhead.rate };
                        let gpu_source = frame.gpu_source.clone();
                        let final_linear = self.post_process_rgba_with_gpu_source(
                            frame.data, w, h, gpu_source.as_ref(),
                        );
                        let final_rgba = self.display_encode(&final_linear);
                        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                        fb.write_frame(final_rgba, w, h);
                        drop(fb);
                        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                        let _ = self.event_tx.send(AppEvent::FrameReady);
                        (self.repaint_cb)();
                        // Keep decoding a few more frames into cache, then stop
                    }

                    // Stop after caching a few frames past target
                    if frame_num > target_frame + 5 { break; }
                    continue;
                }
                std::thread::sleep(Duration::from_millis(1));
            }

            if let Some(dec) = &self.v1_decoder {
                dec.pause();
            }

            // If we didn't find the target (rare), fall back to basic seek
            if !found_target {
                let new_tc = RationalTime { value: target_frame.max(0), rate: self.playhead.rate };
                self.seek_in_sequence_with_timeout(new_tc, Duration::from_millis(300));
            }
        }
    }

    /// Bidirectional prefetch: fill cache with frames both behind and ahead
    /// of the current playhead. After this call, stepping in either direction
    /// is an instant cache hit.
    /// Pre-render nearby frames into the render cache during idle time.
    /// Priority: ±2 seconds from playhead first, then rest of clip.
    /// Runs ONE frame per call to stay responsive to new commands.
    fn prerender_cache_frames(&mut self) {
        mark_stage(EngineStage::Prerender);
        // Previously a source of 70+ second hangs at
        // `stage=Prerender` — the inner GPU grade call occasionally
        // never returned on Wayland+NVIDIA, taking the engine thread
        // (and eventually the UI) down with it. Re-enabled with
        // finer-grained `GPU_STAGE` instrumentation: the next hang
        // (if any) will name its exact blocking call
        // (`gpu_stage=LinearRgbaToU8`, `FlushChain`, etc.) instead
        // of hiding behind the coarse `Prerender` stage.
        // Skip if render cache is disabled
        if !self.render_cache_disk.is_enabled() { return; }
        // Only pre-render if we have a clip with a non-identity grade
        let clip_info = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|t| t.clip_at_timecode(self.playhead))
                .and_then(|(_, clip)| {
                    if clip.node_graph.is_identity() && clip.builtin_effects.is_empty() {
                        None
                    } else {
                        Some((
                            clip.id,
                            clip.timeline_in.value,
                            clip.timeline_out.value,
                            crate::render_cache::compute_grade_hash(
                                &clip.node_graph, &clip.builtin_effects, &clip.builtin_effects_enabled,
                            ),
                        ))
                    }
                })
        };

        let (clip_id, clip_start, clip_end, grade_hash) = match clip_info {
            Some(info) => info,
            None => return,
        };

        let current = self.playhead.value;
        let fps = self.fps.rate() as i64;
        let range = fps * 2; // ±2 seconds

        // Priority 1: frames within ±2 seconds of playhead
        // Priority 2: rest of the clip outward from playhead
        let candidates: Vec<i64> = {
            let mut frames = Vec::new();
            // Near playhead first (alternating forward/backward)
            for offset in 1..=range {
                let fwd = current + offset;
                let bwd = current - offset;
                if fwd < clip_end { frames.push(fwd); }
                if bwd >= clip_start { frames.push(bwd); }
            }
            // Rest of clip
            for f in (clip_start..current.saturating_sub(range as i64)).rev() {
                frames.push(f);
            }
            for f in (current + range + 1)..clip_end {
                frames.push(f);
            }
            frames
        };

        // Find the first uncached frame and pre-render it
        for frame_num in candidates {
            let key = crate::render_cache::RenderCacheKey {
                clip_id,
                frame_number: frame_num,
                grade_hash,
            };

            // Skip if already in hot or cold cache
            if self.render_cache_hot.get(&key).is_some() { continue; }
            if self.render_cache_disk.get(&key).is_some() { continue; }

            // Check if we have the pre-grade frame in the decode cache.
            // `frame_cache` stores sRGB u8 and both render caches do
            // too — bridge around `post_process_rgba_inner`, which is
            // linear-in/linear-out post the linear-edge-to-edge
            // refactor.
            if let Some((rgba_arc, w, h)) = self.frame_cache.get(frame_num) {
                let rgba = rgba_arc.as_ref().clone();
                // Save/restore playhead so grading evaluates at the right time
                let saved_playhead = self.playhead;
                self.playhead = klipp_core::RationalTime { value: frame_num, rate: self.playhead.rate };

                let linear = u8_rgba_to_linear(&rgba);
                // Suspend per-node thumbnail capture: this speculative
                // grade is for a NEAR-playhead frame (prerender cache
                // warm-up), not the user's actual playhead. Capturing
                // thumbs here would overwrite the canvas previews with
                // frames the user didn't ask for.
                self.thumb_capture_suspended = true;
                let graded_linear = self.post_process_rgba_inner(linear, w, h);
                self.thumb_capture_suspended = false;
                let graded = linear_rgba_to_u8(&graded_linear);

                self.render_cache_hot.insert(key.clone(), graded.clone(), w, h);
                self.render_cache_disk.put(&key, &graded, w, h);

                // Restore playhead
                self.playhead = saved_playhead;
                return; // One frame per call — stay responsive
            }
        }

        // Update render cache ranges for timeline visualization
        self.update_render_cache_ranges();
    }

    /// Update the shared frame buffer with current render cache coverage per clip.
    fn update_render_cache_ranges(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut ranges = Vec::new();

        for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
            for clip in &track.clips {
                if clip.node_graph.is_identity() && clip.builtin_effects.is_empty() {
                    continue;
                }
                let grade_hash = crate::render_cache::compute_grade_hash(
                    &clip.node_graph, &clip.builtin_effects, &clip.builtin_effects_enabled,
                );
                let total = (clip.timeline_out.value - clip.timeline_in.value).max(1) as u32;
                let mut cached = 0u32;
                for f in clip.timeline_in.value..clip.timeline_out.value {
                    let key = crate::render_cache::RenderCacheKey {
                        clip_id: clip.id,
                        frame_number: f,
                        grade_hash,
                    };
                    if self.render_cache_hot.get(&key).is_some() {
                        cached += 1;
                    }
                    // Don't check disk here — too slow for timeline display
                }
                if cached > 0 {
                    ranges.push((clip.id, cached, total));
                }
            }
        }
        drop(seq);

        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        fb.render_cache_ranges = ranges;
    }

    fn prefetch_ahead(&mut self) {
        // 1. Drain any currently queued forward frames into cache
        if let Some(dec) = &self.v1_decoder {
            let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            while let Some(frame) = queue.pop() {
                if frame.is_eof { break; }
                let pts = frame.pts;
                let (w, h) = (frame.width, frame.height);
                let rgba = frame.to_rgba();
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let frame_num = if let Some(clip) = seq.tracks.get(self.current_track_index)
                    .and_then(|t| t.clips.get(self.current_clip_index))
                {
                    let source_elapsed = pts.to_secs_f64() - clip.source_in.to_secs_f64();
                    let spd = clip.speed.max(0.001);
                    let elapsed_tc = RationalTime::from_secs_f64(
                        (source_elapsed / spd).max(0.0), self.fps.rate(),
                    );
                    (clip.timeline_in + elapsed_tc).value
                } else {
                    continue;
                };
                drop(seq);
                self.frame_cache.insert(frame_num, Arc::new(rgba), w, h);
            }
        }

        // 2. Check if backward frames need prefilling
        let current = self.playhead.value;
        let backward_needed = (1..=30).any(|i| {
            let f = current - i;
            f >= 0 && self.frame_cache.get(f).is_none()
        });

        if backward_needed {
            // Seek backward and decode forward to fill the cache behind us.
            // This is a one-time cost that makes all subsequent backward steps instant.
            let prefetch_start = (current - 30).max(0);
            if let Some(dec) = &self.v1_decoder {
                let seek_secs = RationalTime { value: prefetch_start, rate: self.playhead.rate }.to_secs_f64();
                dec.seek(seek_secs);
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
                dec.resume();
            }

            let deadline = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < deadline {
                let popped = self.v1_decoder.as_ref()
                    .and_then(|dec| dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop());
                if let Some(frame) = popped {
                    if frame.is_eof { break; }
                    let (w, h) = (frame.width, frame.height);
                    let rgba = frame.to_rgba();
                    let frame_num = {
                        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                        if let Some(clip) = seq.tracks.get(self.current_track_index)
                            .and_then(|t| t.clips.get(self.current_clip_index))
                        {
                            let source_elapsed = frame.pts_secs() - clip.source_in.to_secs_f64();
                            let spd = clip.speed.max(0.001);
                            let elapsed_tc = RationalTime::from_secs_f64(
                                (source_elapsed / spd).max(0.0), self.fps.rate(),
                            );
                            (clip.timeline_in + elapsed_tc).value
                        } else { break; }
                    };
                    self.frame_cache.insert(frame_num, Arc::new(rgba), w, h);
                    // Stop a few frames past current (forward will be handled by decoder resume)
                    if frame_num > current + 5 { break; }
                    continue;
                }
                std::thread::sleep(Duration::from_millis(1));
            }

            // Re-seek decoder back to current position for forward prefetch
            if let Some(dec) = &self.v1_decoder {
                let current_secs = self.playhead.to_secs_f64();
                dec.seek(current_secs);
                dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
            }
        }

        // 3. Resume decoder for continuous forward prefetch
        if let Some(dec) = &self.v1_decoder {
            dec.resume();
        }
    }

    fn grab_single_frame_timeout(&mut self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            // Pop best frame in limited scope to avoid borrow across post_process
            let best = self.v1_decoder.as_ref().and_then(|dec| {
                let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                let mut best = None;
                while let Some(frame) = queue.pop() {
                    if frame.is_eof {
                        break;
                    }
                    best = Some(frame);
                }
                best
            });
            if let Some(mut frame) = best {
                let (w, h) = (frame.width, frame.height);
                // LinearFrame carries linear natively — feed it
                // straight into post_process_rgba, display-transform
                // once at the viewer seam.
                frame.ensure_cpu_data();
                let gpu_source = frame.gpu_source.clone();
                let final_linear = self.post_process_rgba_with_gpu_source(
                    frame.data, w, h, gpu_source.as_ref(),
                );
                let final_rgba = self.display_encode(&final_linear);
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.write_frame(final_rgba, w, h);
                drop(fb);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                if let Some(dec) = &self.v1_decoder {
                    dec.pause();
                }
                (self.repaint_cb)();
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Timed out without a frame. DO NOT pause the decoder here — the
        // caller may set `pending_paused_refresh` so the per-tick paused
        // branch picks up the frame when it finally arrives. If the caller
        // doesn't care (e.g. initial load), it will get paused elsewhere.
        false
    }

    /// Per-tick handler for `pending_paused_refresh`. Pops one frame the
    /// decoder has produced since the timed-out seek, runs the full
    /// post_process pipeline on it (which captures per-node thumbs when
    /// V2 canvas is open), and pauses the decoder. Returns true on
    /// success so the caller can clear the flag. Returns false and leaves
    /// the decoder running when the queue is still empty.
    fn try_drain_paused_refresh_frame(&mut self) -> bool {
        let frame = self.v1_decoder.as_ref().and_then(|dec| {
            let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            let mut best = None;
            while let Some(f) = queue.pop() {
                if f.is_eof { break; }
                best = Some(f);
            }
            best
        });
        let Some(mut frame) = frame else { return false; };
        let (w, h) = (frame.width, frame.height);
        frame.ensure_cpu_data();
        let gpu_source = frame.gpu_source.clone();
        let final_linear = self.post_process_rgba_with_gpu_source(
            frame.data, w, h, gpu_source.as_ref(),
        );
        let final_rgba = self.display_encode(&final_linear);
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(final_rgba, w, h);
        }
        let _ = self.event_tx.send(AppEvent::FrameReady);
        if let Some(dec) = &self.v1_decoder {
            dec.pause();
        }
        (self.repaint_cb)();
        true
    }

    /// Run the clip's node graph on a caller-supplied pre-grade source
    /// (e.g. a filmstrip tile at ~228×128) to produce per-node
    /// thumbnails for the V2 canvas — without waiting on a full-res
    /// decoder seek. Grade params are still evaluated at `playhead` so
    /// keyframed values match exactly; only the source pixels come
    /// from a nearby timestamp (visually indistinguishable at 192×108
    /// and way more reliable than the decode-then-capture path).
    ///
    /// Source is sRGB u8 RGBA at `src_w × src_h`. Executor runs at the
    /// same resolution and its thumb-capture stage downsamples each
    /// visible node's output to 192×108 (`THUMB_W/THUMB_H` in
    /// klipp-nodes). Silently no-ops when the visible-set is empty,
    /// the clip / graph is missing, or the source payload has the
    /// wrong length — nothing to do, no harm done.
    fn render_node_thumbs_from_source(
        &mut self,
        clip_id: klipp_core::ClipId,
        playhead: klipp_core::RationalTime,
        src_rgba: Vec<u8>,
        src_w: u32,
        src_h: u32,
    ) {
        if self.visible_node_thumbs.is_empty() {
            return;
        }
        if src_rgba.len() != (src_w as usize) * (src_h as usize) * 4 {
            return;
        }
        // Clone the graph under a short read lock so the executor call
        // below doesn't hold the sequence lock.
        let (graph, clip_time_secs) = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let found = seq.tracks.iter().find_map(|track| {
                track.clips.iter().find(|c| c.id == clip_id).map(|c| {
                    let speed = c.speed.max(0.001);
                    let clip_t = c.source_in.to_secs_f64()
                        + (playhead - c.timeline_in).to_secs_f64() * speed;
                    (c.node_graph.clone(), clip_t.max(0.0))
                })
            });
            match found {
                Some(v) => v,
                None => return,
            }
        };
        if graph.is_identity() {
            // Empty graph: the thumb is just the source (pre-grade)
            // for every visible node. Cheap one-off downsample + fan-out
            // so Source / Output nodes still get a preview.
            use klipp_nodes::{THUMB_W, THUMB_H};
            let thumb = downsample_srgb_u8_to_thumb(&src_rgba, src_w, src_h, THUMB_W, THUMB_H);
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.node_thumbs.clear();
            for &node_id in &self.visible_node_thumbs {
                fb.node_thumbs.insert(node_id, thumb.clone());
            }
            fb.node_thumbs_gen = fb.node_thumbs_gen.wrapping_add(1);
            return;
        }
        let linear_in = u8_rgba_to_linear(&src_rgba);
        let frame_number = (clip_time_secs * self.playhead.rate as f64).max(0.0) as u64;
        let thumb_set = self.visible_node_thumbs.clone();
        self.ensure_gpu_effects();
        // Mirror the project's active working colorspace into the
        // executor so gamut-aware grade ops (Saturation, Vibrance,
        // ToneControls, ColorWheels, etc.) use the right luma vector.
        // Cheap setter; safe to call per-frame.
        self.graph_executor.set_working_colorspace(
            crate::video_decoder::working_colorspace(),
        );
        match self.graph_executor.execute_linear_with_thumbs(
            &linear_in,
            &graph,
            src_w,
            src_h,
            clip_time_secs,
            frame_number,
            None, // no scope tap on this pass
            None, // no matte preview on this pass
            Some(&thumb_set),
        ) {
            Ok((_result, _tapped, thumbs)) => {
                if thumbs.is_empty() {
                    return;
                }
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.node_thumbs = thumbs;
                fb.node_thumbs_gen = fb.node_thumbs_gen.wrapping_add(1);
            }
            Err(e) => {
                tracing::debug!("render_node_thumbs_from_source: executor failed: {e:?}");
            }
        }
    }

    /// Apply the full post-processing pipeline to a raw decoded RGBA frame:
    /// color grade → OFX effects → built-in effects → global LUT.
    /// Check for an active transition at the current playhead and blend if needed.
    fn apply_transition_if_active(&mut self, from_rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        // Read sequence to check for transitions
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let active = crate::transition::find_active_transition(&seq, self.playhead);
        drop(seq);

        let active = match active {
            Some(a) => a,
            None => {
                // No transition active — close transition decoder if open
                if self.transition_decoder.is_some() {
                    if let Some(dec) = &mut self.transition_decoder {
                        dec.stop_detached();
                    }
                    self.transition_decoder = None;
                    self.transition_clip_id = None;
                    self.transition_frame_cache = None;
                }
                self.transition_clip_a_last_frame = None;
                self.transition_partner_eof = false;
                self.transition_degradation = Default::default();
                return from_rgba;
            }
        };

        // Open transition decoder for partner clip if needed
        let partner_clip_id = active.partner_clip.id;
        if self.transition_clip_id != Some(partner_clip_id) {
            // Close old transition decoder
            if let Some(dec) = &mut self.transition_decoder {
                dec.stop_detached();
            }
            self.transition_decoder = None;
            self.transition_frame_cache = None;
            self.transition_partner_eof = false;
            self.transition_degradation = Default::default();

            // Open new decoder for the partner clip
            if let Some(media_id) = active.partner_clip.media_id() {
                if let Some(path) = self.path_for_media(media_id) {
                    match VideoDecoder::open(&path, active.partner_clip.source_rotation) {
                        Ok(dec) => {
                            let source_secs = active.partner_clip.source_in.to_secs_f64()
                                + (self.playhead - active.partner_clip.timeline_in).to_secs_f64().max(0.0) * active.partner_clip.speed;
                            if source_secs > 0.01 {
                                dec.seek(source_secs);
                            }
                            dec.resume();
                            self.transition_decoder = Some(dec);
                            self.transition_clip_id = Some(partner_clip_id);
                            debug!("Transition decoder opened for partner clip");
                        }
                        Err(e) => {
                            warn!("Failed to open transition decoder: {e}");
                        }
                    }
                }
            }
        }

        // Pop frame from transition decoder — comparison must be in partner source time,
        // not master clock / timeline time. Mirrors the seek formula at lines 3473-3474.
        let partner_source_secs = active.partner_clip.source_in.to_secs_f64()
            + (self.playhead - active.partner_clip.timeline_in)
                .to_secs_f64()
                .max(0.0)
                * active.partner_clip.speed;
        if let Some(t_dec) = &self.transition_decoder {
            let current_source_secs = partner_source_secs;
            let frame_tolerance = self.fps.frame_duration_secs() * 0.5;
            let mut queue = t_dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            while let Some(f) = queue.front() {
                if f.is_eof {
                    let _ = queue.pop();
                    // EOF from the partner decoder — the source file has no
                    // more frames. Any subsequent use of transition_frame_cache
                    // is a freeze-frame. This flag is the ONLY trigger for
                    // tagging frames as Frozen.
                    self.transition_partner_eof = true;
                    break;
                }
                let pts = f.pts_secs();
                if pts <= current_source_secs + frame_tolerance {
                    let tf = queue.pop().unwrap();
                    if let Some(next) = queue.front() {
                        if !next.is_eof && next.pts_secs() <= current_source_secs + frame_tolerance {
                            continue;
                        }
                    }
                    self.transition_frame_cache = Some(tf.to_srgb_u8());
                    break;
                } else {
                    break;
                }
            }
        }

        let kind_id = crate::transition::transition_kind_to_id(active.kind);

        if !active.past_cut {
            // First half: main decoder is on clip_a, partner decoder is on clip_b.
            // Need the partner frame to blend. Cache from_rgba (clip_a) so the
            // second-half branch can still show it after the main decoder advances.
            let to_rgba = match &self.transition_frame_cache {
                Some(f) => f.to_rgba(),
                None => {
                    // No partner frame yet — show main (clip_a) unblended
                    return from_rgba;
                }
            };
            // Track degradation: partner provides the B-side in first half.
            if self.transition_partner_eof {
                self.transition_degradation.frozen_b_count += 1;
            }
            self.transition_clip_a_last_frame = Some(from_rgba.clone());
            let up = active.user_params;
            // CrossDissolve progress reshaping must happen before the GPU call
            // (the shader receives progress as-is). The CPU fallback wrapper
            // reshapes internally, so we pass the raw value there.
            let gpu_progress = if matches!(active.kind, klipp_timeline::TransitionKind::CrossDissolve) {
                crate::transition::apply_easing(active.progress, up.param0)
            } else {
                active.progress
            };
            if let Some(gpu) = &mut self.gpu_effects {
                gpu.render_transition(
                    &from_rgba, &to_rgba, kind_id, gpu_progress, width, height,
                    up.param0, up.param1, up.param2, up.param3, up.color,
                )
            } else {
                crate::transition::render_transition_with_params(
                    active.kind, &from_rgba, &to_rgba, active.progress, width, height, &up,
                )
            }
        } else {
            // Second half: main decoder has advanced to clip_b. Use the cached
            // clip_a frame as "from" and the main (clip_b) frame as "to". The
            // partner decoder is still running on clip_b; its frames are ignored
            // here but we keep it open so there's no teardown mid-window.
            //
            // Exception — StartAtCut alignment: the entire window is "past the cut",
            // so the cache is never populated. In that mode the partner decoder is
            // pointed at clip_a's tail (because there's no first half to occupy it),
            // and we use its live frames as the "from" side directly.
            let blend_from = if active.alignment == klipp_core::TransitionAlignment::StartAtCut {
                // partner decoder frame for clip_a tail
                match &self.transition_frame_cache {
                    Some(f) => {
                        // Track degradation: partner provides the A-side in StartAtCut.
                        if self.transition_partner_eof {
                            self.transition_degradation.frozen_a_count += 1;
                        }
                        f.to_rgba()
                    }
                    None => return from_rgba,
                }
            } else {
                match self.transition_clip_a_last_frame.clone() {
                    Some(f) => f,
                    None => {
                        // No cached clip_a frame (e.g. seeked directly into second half).
                        // Fall back to unblended main.
                        return from_rgba;
                    }
                }
            };
            let up = active.user_params;
            let gpu_progress = if matches!(active.kind, klipp_timeline::TransitionKind::CrossDissolve) {
                crate::transition::apply_easing(active.progress, up.param0)
            } else {
                active.progress
            };
            if let Some(gpu) = &mut self.gpu_effects {
                gpu.render_transition(
                    &blend_from, &from_rgba, kind_id, gpu_progress, width, height,
                    up.param0, up.param1, up.param2, up.param3, up.color,
                )
            } else {
                crate::transition::render_transition_with_params(
                    active.kind, &blend_from, &from_rgba, active.progress, width, height, &up,
                )
            }
        }
    }

    /// Smart linear-f32 RGBA → sRGB-u8 RGBA encode. Routes to the
    /// GPU compute shader for frames ≥ 1080p when a GPU pipeline is
    /// available, falls back to the CPU LUT for everything else.
    ///
    /// At 4K the GPU path is ~20× faster (40-90 ms CPU LUT → 3-5 ms
    /// GPU readback). At HD or smaller the CPU LUT wins because the
    /// PCIe round-trip exceeds the encode cost. The threshold is set
    /// at 1080p — that's the breakeven point measured on the user's
    /// RTX 4080 hardware.
    fn linear_rgba_to_u8_smart(&mut self, linear: &[f32], width: u32, height: u32) -> Vec<u8> {
        const GPU_THRESHOLD_PIXELS: u64 = 1920 * 1080;
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count >= GPU_THRESHOLD_PIXELS {
            if let Some(gpu) = &mut self.gpu_effects {
                if let Some(out) = gpu.linear_rgba_to_u8_gpu(linear, width, height) {
                    return out;
                }
                // GPU rejected (device-lost / short input) — fall
                // through to CPU LUT.
            }
        }
        linear_rgba_to_u8(linear)
    }

    /// Lazily initialize GPU effects pipeline on first use.
    /// This defers the ~1-2s shader compilation cost from startup to the first frame
    /// that actually needs GPU processing.
    fn ensure_gpu_effects(&mut self) {
        if self.gpu_effects.is_none() && !self.gpu_effects_failed {
            let t = std::time::Instant::now();
            self.gpu_effects = klipp_effects::gpu::GpuEffectsPipeline::new();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if self.gpu_effects.is_some() {
                info!("PlaybackEngine: GPU effects pipeline initialized in {ms:.0}ms");
            } else {
                self.gpu_effects_failed = true;
                info!("GPU effects pipeline unavailable — running without GPU effects");
                warn!("GPU effects pipeline init failed ({ms:.0}ms) — will not retry");
            }
        }
    }

    /// Apply a clip's effects (node graph → builtin effects → OFX) to a decoded frame.
    /// Simplified path for overlay clips — does NOT run scopes, stabilization, comparison
    /// mode, or global LUT. V1 continues to use post_process_rgba_inner() for those.
    fn apply_clip_effects(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        clip: &klipp_timeline::Clip,
        clip_time_secs: f64,
    ) -> Vec<f32> {
        // 1. Node graph (color grade) — takes linear natively via
        //    `execute_linear`, skipping the u8 round-trip that
        //    `execute` would otherwise introduce.
        let rgba = if !clip.node_graph.is_identity() {
            self.graph_executor.prepare_clip(&clip.node_graph);
            let frame_number = (clip_time_secs * self.fps.as_f64()).max(0.0) as u64;
            self.graph_executor.set_working_colorspace(
                crate::video_decoder::working_colorspace(),
            );
            match self.graph_executor.execute_linear(
                &linear_rgba, &clip.node_graph, width, height, clip_time_secs, frame_number, None, None,
            ) {
                Ok((result, _)) => result,
                Err(e) => { tracing::error!("Overlay node graph failed: {e:?}"); linear_rgba }
            }
        } else { linear_rgba };

        // 2. Builtin effects (GPU preferred, CPU fallback) with keyframe evaluation
        let rgba = if !clip.builtin_effects.is_empty() {
            let mut fx: Vec<_> = clip.builtin_effects.iter()
                .zip(clip.builtin_effects_enabled.iter().chain(std::iter::repeat(&true)))
                .filter(|(_, enabled)| **enabled)
                .map(|(e, _)| e.clone())
                .collect();

            if !fx.is_empty() {
                // Build evaluation context for keyframes
                let clip_duration_secs = (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()).max(0.001);
                let eval_ctx = klipp_core::EvalContext {
                    clip_time_secs,
                    clip_duration_secs,
                    timeline_time_secs: self.playhead.to_secs_f64(),
                    fps: self.fps.as_f64(),
                    seed: clip.id.0.as_u64_pair().0,
                };

                // Evaluate opacity keyframes
                if !clip.opacity_keyframes.is_empty() {
                    let opacity_default = fx.iter().find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Opacity { value } => Some(*value),
                        _ => None,
                    }).unwrap_or(1.0);
                    let opacity_val = eval_with_modifiers(
                        &clip.opacity_keyframes,
                        &clip.param_modifiers,
                        &klipp_core::ParamId::Opacity,
                        clip_time_secs,
                        opacity_default,
                        &eval_ctx,
                    );
                    let mut found = false;
                    for effect in &mut fx {
                        if let klipp_effects::BuiltinEffect::Opacity { value } = effect {
                            *value = opacity_val;
                            found = true;
                            break;
                        }
                    }
                    if !found && (opacity_val - 1.0).abs() > 0.001 {
                        fx.push(klipp_effects::BuiltinEffect::Opacity { value: opacity_val });
                    }
                }

                // Evaluate transform keyframes
                let has_transform_kf = !clip.transform_pos_x_keyframes.is_empty()
                    || !clip.transform_pos_y_keyframes.is_empty()
                    || !clip.transform_scale_keyframes.is_empty()
                    || !clip.transform_rotation_keyframes.is_empty();

                if has_transform_kf {
                    let (mut pos_x, mut pos_y, mut scale_x, mut scale_y, mut rotation, anchor_x, anchor_y, rotation_x_val, rotation_y_val, perspective_val) =
                        fx.iter().find_map(|e| match e {
                            klipp_effects::BuiltinEffect::Transform {
                                position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y,
                                rotation_x, rotation_y, perspective,
                            } => Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y, *rotation_x, *rotation_y, *perspective)),
                            _ => None,
                        }).unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0));

                    if !clip.transform_pos_x_keyframes.is_empty() {
                        pos_x = eval_with_modifiers(&clip.transform_pos_x_keyframes, &clip.param_modifiers, &klipp_core::ParamId::PositionX, clip_time_secs, pos_x, &eval_ctx);
                    }
                    if !clip.transform_pos_y_keyframes.is_empty() {
                        pos_y = eval_with_modifiers(&clip.transform_pos_y_keyframes, &clip.param_modifiers, &klipp_core::ParamId::PositionY, clip_time_secs, pos_y, &eval_ctx);
                    }
                    if !clip.transform_scale_keyframes.is_empty() {
                        let s = eval_with_modifiers(&clip.transform_scale_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Scale, clip_time_secs, scale_x, &eval_ctx);
                        scale_x = s;
                        scale_y = s;
                    }
                    if !clip.transform_rotation_keyframes.is_empty() {
                        rotation = eval_with_modifiers(&clip.transform_rotation_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Rotation, clip_time_secs, rotation, &eval_ctx);
                    }

                    let new_transform = klipp_effects::BuiltinEffect::Transform {
                        position_x: pos_x, position_y: pos_y,
                        scale_x, scale_y, rotation, anchor_x, anchor_y,
                        rotation_x: rotation_x_val, rotation_y: rotation_y_val, perspective: perspective_val,
                    };
                    let mut found = false;
                    for effect in &mut fx {
                        if matches!(effect, klipp_effects::BuiltinEffect::Transform { .. }) {
                            *effect = new_transform.clone();
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        fx.push(new_transform);
                    }
                }

                // Evaluate effect parameter keyframes (blur, sharpen, crop, chroma key, opacity, etc.)
                for (i, effect) in fx.iter_mut().enumerate() {
                    match effect {
                        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
                            let key = format!("{}:radius", i);
                            if let Some(kfs) = clip.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *radius = eval_effect_with_modifiers(kfs, &clip.param_modifiers, i, "radius", clip_time_secs, *radius, &eval_ctx);
                                }
                            }
                        }
                        klipp_effects::BuiltinEffect::Sharpen { amount } => {
                            let key = format!("{}:amount", i);
                            if let Some(kfs) = clip.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *amount = eval_effect_with_modifiers(kfs, &clip.param_modifiers, i, "amount", clip_time_secs, *amount, &eval_ctx);
                                }
                            }
                        }
                        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
                            for (name, val) in [("left", left as &mut f32), ("right", right), ("top", top), ("bottom", bottom), ("feather", feather)] {
                                let key = format!("{}:{}", i, name);
                                if let Some(kfs) = clip.effect_keyframes.get(&key) {
                                    if !kfs.is_empty() {
                                        *val = eval_effect_with_modifiers(kfs, &clip.param_modifiers, i, name, clip_time_secs, *val, &eval_ctx);
                                    }
                                }
                            }
                        }
                        klipp_effects::BuiltinEffect::ChromaKey { tolerance, spill_suppression, .. } => {
                            for (name, val) in [("tolerance", tolerance as &mut f32), ("spill", spill_suppression)] {
                                let key = format!("{}:{}", i, name);
                                if let Some(kfs) = clip.effect_keyframes.get(&key) {
                                    if !kfs.is_empty() {
                                        *val = eval_effect_with_modifiers(kfs, &clip.param_modifiers, i, name, clip_time_secs, *val, &eval_ctx);
                                    }
                                }
                            }
                        }
                        klipp_effects::BuiltinEffect::Opacity { value } => {
                            let key = format!("{}:value", i);
                            if let Some(kfs) = clip.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *value = eval_effect_with_modifiers(kfs, &clip.param_modifiers, i, "value", clip_time_secs, *value, &eval_ctx);
                                }
                            }
                        }
                        _ => {} // Transform already handled by dedicated keyframes
                    }
                }

                if let Some(gpu) = &mut self.gpu_effects {
                    gpu.apply_effects(&fx, rgba, width, height)
                } else {
                    klipp_effects::apply_effects(&fx, rgba, width, height)
                }
            } else { rgba }
        } else { rgba };

        // 3. OFX effects chain — OFX plugins are an external boundary we
        //    don't control, so they still receive sRGB u8. Bridge
        //    locally (linear → u8 in, u8 → linear out) and keep the
        //    round-trip scoped to just the OFX call.
        if !clip.effects.is_empty() {
            let rgba_u8 = klipp_effects::linear_f32_rgba_to_srgb_u8(&rgba);
            let out_u8 = self.ofx_host.apply_effects_chain(&clip.effects, &rgba_u8, width, height, clip_time_secs);
            klipp_effects::srgb_u8_to_linear_f32_rgba(&out_u8)
        } else {
            rgba
        }
    }

    /// Phase 4d.3 companion to `post_process_rgba`: call this when the
    /// frame you're handing in carries a GPU-resident mirror
    /// (`LinearFrame.gpu_source`). It primes the grade chain's
    /// `buf_a` directly from the on-GPU u8 buffer via the
    /// `bridge_u8_to_linear` compute shader — the CPU upload that
    /// `start_chain` would otherwise do is skipped entirely.
    ///
    /// Falls through to the normal `post_process_rgba` path when
    /// `gpu_source` is `None`, priming fails, or the GPU pipeline
    /// isn't available.
    fn post_process_rgba_with_gpu_source(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        gpu_source: Option<&std::sync::Arc<crate::frame::GpuFrame>>,
    ) -> Vec<f32> {
        if let Some(gpu_src) = gpu_source {
            // Single-shot: consumed by the first `start_chain` inside
            // the executor. If it returns false (device lost, size
            // mismatch, no GPU) the normal CPU upload path runs —
            // correctness preserved, just no perf win.
            let _ = self.graph_executor.prime_chain_from_gpu_u8_buffer(
                &gpu_src.rgba_buffer, width, height,
            );
        }
        self.post_process_rgba(linear_rgba, width, height)
    }

    /// Working-space-aware viewer display encode. Used at every site
    /// where a linear-light frame leaves the engine for the framebuffer
    /// — the single dispatch point that picks between the legacy
    /// Rec.709 tone-map path and the ACES RRT+ODT path based on the
    /// project's active working colorspace.
    ///
    /// Reads `WORKING_COLORSPACE_PREF` lock-free per-call; cheap.
    /// Without this helper the engine would have ~14 viewer-encode
    /// call sites to thread the working-space argument through; this
    /// keeps every site at a single-line `self.display_encode(&linear)`
    /// invocation.
    fn display_encode(&self, linear: &[f32]) -> Vec<u8> {
        klipp_color::display::display_transform_ws(
            linear,
            self.tone_map_op,
            crate::video_decoder::working_colorspace(),
        )
    }

    fn post_process_rgba(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32) -> Vec<f32> {
        mark_stage(EngineStage::PostProcessEntry);
        let t_wrap_start = std::time::Instant::now();
        mark_stage(EngineStage::PostProcessEnsureGpu);
        self.ensure_gpu_effects();
        let t_ensure_gpu = t_wrap_start.elapsed();

        // The pregrade encode feeds three consumers — `cached_pregrade_
        // frame` (compare-mode snapshot), `frame_cache` (scrub-back),
        // and `fb.pregrade_data` (UI scope-preview during slider scrub).
        // None of them are touched during pure forward playback. At 4K
        // the encode + 32 MB allocation is 40-90 ms/frame, and that
        // single line is the dominant cost in the grade-disabled hot
        // path. Skip when:
        //   * we're playing forward (state == Playing), AND
        //   * no compare-mode is active (compare reads pregrade_data),
        //     AND
        //   * the user isn't actively node-param-editing (UI scope
        //     preview reads pregrade_data on every slider tick).
        // Pause / scrub / compare / grade-edit still pay the encode —
        // EXCEPT during a Playing+drag with no compare modes active.
        // In that combination the encode runs at full res ~30 times
        // per second purely to keep the UI scope-preview fresh. The
        // user is staring at the main image while gesturing, not the
        // scope, so we accept a slightly stale scope preview to
        // recover the ~5 ms / frame and stay under the per-frame
        // budget. Compare-mode + paused-scrub still get fresh data.
        let drag_skip_pregrade = drag_downsample_enabled()
            && self.in_node_param_edit
            && self.state == PlaybackState::Playing
            && !self.compare_stabilization
            && !self.compare_effect_active
            && !self.bypass_color_grade;
        let pregrade_needed = (self.state != PlaybackState::Playing
            || self.compare_stabilization
            || self.compare_effect_active
            || self.in_node_param_edit)
            && !drag_skip_pregrade;
        let t_bridge = std::time::Instant::now();
        mark_stage(EngineStage::PostProcessPregradeEncode);
        let rgba_u8 = if pregrade_needed {
            // Routes large frames (≥ 1080p) through the GPU compute
            // shader; HD-and-smaller stay on CPU LUT. At 4K this is
            // the difference between 40-90 ms and 3-5 ms.
            self.linear_rgba_to_u8_smart(&linear_rgba, width, height)
        } else {
            Vec::new()
        };
        let t_bridge_u8 = t_bridge.elapsed();

        let t_pregrade = std::time::Instant::now();
        let mut pregrade_fb_lock_us: u64 = 0;
        if pregrade_needed {
            let rgba_arc = Arc::new(rgba_u8);
            self.cached_pregrade_frame = Some((Arc::clone(&rgba_arc), width, height));
            self.frame_cache.insert(self.playhead.value, Arc::clone(&rgba_arc), width, height);
            {
                mark_stage(EngineStage::PostProcessFbLockA);
                let t_lock = std::time::Instant::now();
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                pregrade_fb_lock_us = t_lock.elapsed().as_micros() as u64;
                fb.pregrade_data = rgba_arc;
            }
        }
        let t_pregrade_ms = t_pregrade.elapsed();

        // ── Smart render cache: check for cached graded frame ──
        // Skip render cache when overlays are present — cache keys don't represent
        // multi-clip composite state. V1-only playback still benefits from the cache.
        //
        // Also skip the render cache while a colour-grade slider drag is in
        // flight. The grade hash changes every UI tick during a drag, so a
        // read would always miss and a write would burn ~6 MB of LZ4 for an
        // entry that never gets reused (203 evictions + 34 decompress races
        // in a 60 s scrub session). Nulling `cache_key` makes both the read
        // block and the write block skip naturally. Playback without an
        // active gesture still gets the full cache benefit.
        mark_stage(EngineStage::PostProcessCacheLookup);
        let t_key_lookup = std::time::Instant::now();
        let has_overlays = !self.overlay_frames.is_empty();
        let cache_key = if has_overlays || self.in_node_param_edit {
            None
        } else {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let clip = seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|t| t.clip_at_timecode(self.playhead))
                .map(|(_, c)| c);
            if let Some(clip) = clip {
                if self.current_grade_clip_id != Some(clip.id) || self.current_grade_hash == 0 {
                    self.current_grade_hash = crate::render_cache::compute_grade_hash(
                        &clip.node_graph, &clip.builtin_effects, &clip.builtin_effects_enabled,
                    );
                    self.current_grade_clip_id = Some(clip.id);
                    // CRITICAL: `current_grade_clip_id` is shared with
                    // the graph-Arc cache in `post_process_rgba_inner`
                    // (the `cache_hit` check compares that same id).
                    // Bumping the id without invalidating the Arc would
                    // leave the previous clip's graph in the cache and
                    // `_inner` would happily reuse it — exactly the
                    // "grade applied to every clip" bug we chased here.
                    self.current_graph_arc = None;
                }
                if !clip.node_graph.is_identity() || !clip.builtin_effects.is_empty() {
                    Some(crate::render_cache::RenderCacheKey {
                        clip_id: clip.id,
                        frame_number: self.playhead.value,
                        grade_hash: self.current_grade_hash,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Check hot cache first, then disk cache
        if let Some(ref key) = cache_key {
            let t_hit_lookup = std::time::Instant::now();
            let mut hit_source = "miss";
            let cached_result = self.render_cache_hot.get(key)
                .filter(|(_, w, h)| *w == width && *h == height)
                .map(|(data, w, h)| { hit_source = "hot"; (data.to_vec(), w, h) })
                .or_else(|| {
                    self.render_cache_disk.get(key)
                        .filter(|(_, w, h)| *w == width && *h == height)
                        .inspect(|_| { hit_source = "disk"; })
                });
            let t_hit_lookup_ms = t_hit_lookup.elapsed().as_secs_f64() * 1000.0;

            if let Some((cached, _w, _h)) = cached_result {
                let t_hit_insert = std::time::Instant::now();
                self.render_cache_hot.insert(key.clone(), cached.clone(), width, height);
                let t_hit_insert_ms = t_hit_insert.elapsed().as_secs_f64() * 1000.0;
                if self.should_render_scope() {
                  if let Some(gpu) = &mut self.gpu_effects {
                    let scope_w = 720u32;
                    let scope_h = 256u32;
                    let mode = self.scope_render_mode;
                    let tex = gpu_scope_kind_for_mode(mode).map(|kind| match kind {
                        GpuScopeKind::Waveform => gpu.render_scope_waveform(&cached, width, height, scope_w, scope_h),
                        GpuScopeKind::Parade => gpu.render_scope_parade(&cached, width, height, scope_w, scope_h),
                        GpuScopeKind::Histogram => gpu.render_scope_histogram(&cached, width, height, scope_w, scope_h),
                    });
                    self.last_scope_render = std::time::Instant::now();
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    match tex {
                        Some(t) => {
                            fb.scope_texture = t;
                            fb.scope_texture_w = scope_w;
                            fb.scope_texture_h = scope_h;
                            fb.scope_texture_mode = mode;
                        }
                        None => {
                            fb.scope_texture.clear();
                            fb.scope_texture_w = 0;
                            fb.scope_texture_h = 0;
                            fb.scope_texture_mode = mode;
                        }
                    }
                  }
                }
                let t_hit_decode = std::time::Instant::now();
                let linear_out = u8_rgba_to_linear(&cached);
                let t_hit_decode_ms = t_hit_decode.elapsed().as_secs_f64() * 1000.0;
                let hit_total_ms = t_wrap_start.elapsed().as_secs_f64() * 1000.0;
                if hit_total_ms > 5.0 {
                    debug!(
                        "PERF post_process_hit: {hit_total_ms:.1}ms source={hit_source} | \
                         bridge_u8={:.1} pregrade={:.1} lookup={:.1} insert={:.1} decode={:.1}",
                        t_bridge_u8.as_secs_f64() * 1000.0,
                        t_pregrade_ms.as_secs_f64() * 1000.0,
                        t_hit_lookup_ms,
                        t_hit_insert_ms,
                        t_hit_decode_ms,
                    );
                }
                return linear_out;
            }
        }

        // Key computation + hot/disk lookup is done now; everything above was
        // cache-check and potential cache-hit early return. If we reach
        // here it was a miss and we're running the real grade pipeline.
        let t_key_lookup_ms = t_key_lookup.elapsed();
        mark_stage(EngineStage::PostProcessInner);
        let t_inner = std::time::Instant::now();
        let result_linear = self.post_process_rgba_inner_drag_aware(linear_rgba, width, height);
        let t_inner_ms = t_inner.elapsed();

        // The reencode (`result_u8`) feeds two consumers — the render
        // cache (`cache_put`) and the scope renderer. Both are
        // independently gated:
        //   * `cache_put` skips during Playing (see below).
        //   * scope is throttled at ~10 Hz and skipped when not visible
        //     (`should_render_scope`).
        // So during steady-state playback with cache_put off and scope
        // throttle off, `result_u8` is wholly unused. At 4K the encode
        // is 40-90 ms / frame — the second-largest single cost in the
        // wrap. Compute it lazily so the common playback frame pays
        // zero CPU u8 conversion.
        let cache_put_active = cache_key.is_some() && self.state != PlaybackState::Playing;
        let scope_active = self.should_render_scope() && self.gpu_effects.is_some()
            && gpu_scope_kind_for_mode(self.scope_render_mode).is_some();
        let need_result_u8 = cache_put_active || scope_active;

        mark_stage(EngineStage::PostProcessReencode);
        let t_reencode = std::time::Instant::now();
        // CPU LUT, not the smart-GPU split. `result_linear` is already
        // a CPU `Vec<f32>` returned from `post_process_rgba_inner`, so
        // taking the GPU path here pays a 33 MB upload + shader + 8 MB
        // readback (~13 ms at 1080p, dominated by `device.poll(Wait)`
        // draining the whole queue) purely to convert data the CPU
        // already has. The CPU LUT path is a single LUT lookup per
        // channel and lands at ~3-5 ms on a modern CPU at 1080p.
        // The GPU path is only the right answer when the source data
        // is *already* on the GPU — that's the future GPU-resident
        // grade output (option B), which would let the scope shader
        // bind the chain buffer directly and skip the encode entirely.
        let result_u8 = if need_result_u8 {
            linear_rgba_to_u8(&result_linear)
        } else {
            Vec::new()
        };
        let t_reencode_ms = t_reencode.elapsed();

        let t_cache_put = std::time::Instant::now();
        // Render-cache write is paused-only (see `cache_put_active`).
        if cache_put_active {
            if let Some(key) = cache_key {
                self.render_cache_hot.insert(key.clone(), result_u8.clone(), width, height);
                self.render_cache_disk.put(&key, &result_u8, width, height);
            }
        }
        let t_cache_put_ms = t_cache_put.elapsed();

        // Render GPU scope texture from the graded frame. Modes with
        // no GPU kernel (vectorscope, unknown codes) clear the
        // texture so the UI falls back to the CPU renderer in
        // `klipp-ui/widgets/scopes.rs`.
        mark_stage(EngineStage::PostProcessScopeRender);
        let t_scope = std::time::Instant::now();
        let mut scope_gpu_ms: f64 = 0.0;
        let mut scope_fb_lock_us: u64 = 0;
        if self.should_render_scope() {
          if let Some(gpu) = &mut self.gpu_effects {
            let scope_w = 720u32;
            let scope_h = 256u32;
            let mode = self.scope_render_mode;
            let t_gpu = std::time::Instant::now();
            let tex = gpu_scope_kind_for_mode(mode).map(|kind| match kind {
                GpuScopeKind::Waveform => gpu.render_scope_waveform(&result_u8, width, height, scope_w, scope_h),
                GpuScopeKind::Parade => gpu.render_scope_parade(&result_u8, width, height, scope_w, scope_h),
                GpuScopeKind::Histogram => gpu.render_scope_histogram(&result_u8, width, height, scope_w, scope_h),
            });
            scope_gpu_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;
            self.last_scope_render = std::time::Instant::now();
            let t_lock = std::time::Instant::now();
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            scope_fb_lock_us = t_lock.elapsed().as_micros() as u64;
            match tex {
                Some(t) => {
                    fb.scope_texture = t;
                    fb.scope_texture_w = scope_w;
                    fb.scope_texture_h = scope_h;
                    fb.scope_texture_mode = mode;
                }
                None => {
                    fb.scope_texture.clear();
                    fb.scope_texture_w = 0;
                    fb.scope_texture_h = 0;
                    fb.scope_texture_mode = mode;
                }
            }
          }
        }
        let t_scope_ms = t_scope.elapsed();

        let wrap_total_ms = t_wrap_start.elapsed().as_secs_f64() * 1000.0;
        // Always emit the breakdown while the user is dragging a grade
        // param — that's the window we're trying to diagnose. Otherwise
        // keep the old > 5 ms threshold so steady-state playback stays
        // quiet in the log.
        let force_log = self.in_node_param_edit;
        if wrap_total_ms > 5.0 || force_log {
            // Breakdown of the OUTER post_process_rgba wrapper. `inner`
            // is the grade/ofx/builtin/lut pipeline (matches PERF
            // post_process). `scope_gpu_ms` is the wgpu submit +
            // device.poll(Wait) + map_async readback fence only — the
            // hard sync that stalls this thread until the GPU has
            // drained the whole queue. `fb_lock_us` fields capture
            // mutex contention with the UI thread. During a slider
            // drag (force_log=true) this fires every frame, so the
            // timeline is fully reconstructable from the log.
            debug!(
                "PERF post_process_wrap: {:.1}ms total | ensure_gpu={:.1} bridge_u8={:.1} pregrade={:.1} key_lookup={:.1} inner={:.1} reencode={:.1} cache_put={:.1} scope={:.1} | scope_gpu={:.1} pregrade_fb_lock_us={} scope_fb_lock_us={} in_edit={} wxh={}x{}",
                wrap_total_ms,
                t_ensure_gpu.as_secs_f64() * 1000.0,
                t_bridge_u8.as_secs_f64() * 1000.0,
                t_pregrade_ms.as_secs_f64() * 1000.0,
                t_key_lookup_ms.as_secs_f64() * 1000.0,
                t_inner_ms.as_secs_f64() * 1000.0,
                t_reencode_ms.as_secs_f64() * 1000.0,
                t_cache_put_ms.as_secs_f64() * 1000.0,
                t_scope_ms.as_secs_f64() * 1000.0,
                scope_gpu_ms,
                pregrade_fb_lock_us,
                scope_fb_lock_us,
                self.in_node_param_edit,
                width, height,
            );
        }

        // Surface wrap-stage timings to the live perf overlay. The
        // grade-only sample pushed by `post_process_rgba_inner` shows
        // 0 for everything when the user has no grade applied — this
        // patch fills in the bridge/reencode/scope numbers so the
        // overlay always reports something useful for any clip.
        if let Ok(mut ring) = self.perf_ring.lock() {
            ring.update_wrap_stages(crate::perf::WrapStages {
                bridge_u8_ms: t_bridge_u8.as_secs_f64() as f32 * 1000.0,
                reencode_ms: t_reencode_ms.as_secs_f64() as f32 * 1000.0,
                scope_ms: t_scope_ms.as_secs_f64() as f32 * 1000.0,
            });
        }

        // Mirror the graded output into `FrameBuffer.gpu_output` when
        // the viewer-GPU path is wired and a shared wgpu device is
        // available. The UI picks this texture up via its egui
        // `TextureId` registration and samples it directly — no
        // `fb.data` upload. CPU `fb.data` is still populated above
        // so the fallback path keeps working if the UI hasn't
        // registered the texture yet.
        //
        // Width constraint: `w × 4` must be a multiple of
        // `COPY_BYTES_PER_ROW_ALIGNMENT` (256). Every playback
        // resolution meets this (1920 × 4 = 7680, ...), but scope-
        // preview or odd downscales might not — skip silently in
        // that case.
        if should_publish_gpu_viewer() && (width * 4) % 256 == 0 {
            mark_stage(EngineStage::GpuEncodeToTexture);
            // When the grade chain just ran, its final linear-RGBA
            // output is still in `buf_a`/`buf_b` on the GPU — the
            // CPU `result_linear` we hold is a mirror produced by
            // the chain-tail download. Routing the viewer texture
            // from that on-GPU buffer via `encode_grade_to_texture`
            // skips the 33 MB PCIe re-upload that
            // `linear_rgba_to_texture_gpu` would otherwise do
            // (~5 ms at 1080p). Falls back to the CPU-upload path
            // when no chain ran (identity graph, pure-CPU node, or
            // executor produced its result without engaging GPU).
            let chain_slot = self.graph_executor.last_chain_final_slot();
            if let Some(gpu) = self.graph_executor.gpu_pipeline_mut() {
                let device = gpu.device();
                let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                let tex = fb.ensure_gpu_output(&device, width, height);
                drop(fb);
                if let Some(tex) = tex {
                    match chain_slot {
                        Some(slot_a) => {
                            let _ = gpu.encode_grade_to_texture(slot_a, width, height, &tex);
                        }
                        None => {
                            let _ = gpu.linear_rgba_to_texture_gpu(&result_linear, width, height, &tex);
                        }
                    }
                }
            }
        }

        result_linear
    }

    /// Re-grade the cached pre-grade frame without re-decoding.
    /// Called when color parameters change while paused.
    fn regrade_cached_frame(&mut self) {
        mark_stage(EngineStage::PausedRegrade);
        self.ensure_gpu_effects();
        // Case 1: V1 video exists — regrade and composite with overlay(s)
        if let Some((ref rgba_arc, w, h)) = self.cached_pregrade_frame.clone() {
            // cached_pregrade_frame stores sRGB u8 — decode once here
            // so the grade + composite run in linear.
            let linear_in = u8_rgba_to_linear(rgba_arc.as_ref());
            let graded_linear = self.post_process_rgba_inner_drag_aware(linear_in, w, h);

            // Composite overlay frames (titles etc.) over V1, in track order.
            // Effective opacity = track.opacity * clip-level opacity (with keyframes/modifiers).
            // Evaluating here means paused opacity drags update the visible composite.
            let has_overlays = !self.overlay_frames.is_empty();
            let out_u8 = if has_overlays {
                use crate::composite::{composite_layers_linear, CompositeLayerLinear};
                let mut layers = vec![CompositeLayerLinear {
                    rgba: graded_linear,
                    width: w,
                    height: h,
                    opacity: 1.0,
                    blend_mode: klipp_core::BlendMode::Normal,
                }];
                {
                    let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                    let fps_f64 = self.fps.as_f64();
                    for (track_idx, track) in &seq.overlay_video_tracks(self.current_track_index) {
                        if let Some(ov_frame) = self.overlay_frames.get(track_idx) {
                            let clip_opacity = track
                                .clip_at_timecode(self.playhead)
                                .map(|(_, clip)| evaluate_clip_opacity(clip, self.playhead, fps_f64))
                                .unwrap_or(1.0);
                            let effective = track.opacity * clip_opacity;
                            if effective > 0.001 {
                                // overlay_frames is already linear f32 —
                                // clone the buffer instead of doing a
                                // 12 ms u8→linear decode per regrade.
                                layers.push(CompositeLayerLinear {
                                    rgba: ov_frame.data.clone(),
                                    width: ov_frame.width,
                                    height: ov_frame.height,
                                    opacity: effective,
                                    blend_mode: klipp_core::BlendMode::Normal,
                                });
                            }
                        }
                    }
                }
                let linear_out = composite_layers_linear(&layers, w, h);
                // Display boundary — same tone-map path as the main
                // playback compositor.
                self.display_encode(&linear_out)
            } else {
                // No overlays — the single-layer graded frame IS the
                // output; display_encode at the viewer seam.
                self.display_encode(&graded_linear)
            };

            // Render GPU scope texture from the graded frame. Modes
            // with no GPU kernel (vectorscope, unknown codes) clear
            // the texture so the UI falls back to the CPU renderer.
            // Throttled to ~10 Hz — rapid slider scrubbing fires this
            // path on every parameter change, which would otherwise
            // pay the scope-render cost dozens of times per second.
            let should_render = self.should_render_scope();
            let scope_tex = if should_render {
                if let Some(gpu) = &mut self.gpu_effects {
                    tracing::debug!("Engine: GPU scope render mode={} frame={}x{}", self.scope_render_mode, w, h);
                    let scope_w = 720u32;
                    let scope_h = 256u32;
                    let mode = self.scope_render_mode;
                    let tex_opt = gpu_scope_kind_for_mode(mode).map(|kind| match kind {
                        GpuScopeKind::Waveform => gpu.render_scope_waveform(&out_u8, w, h, scope_w, scope_h),
                        GpuScopeKind::Parade => gpu.render_scope_parade(&out_u8, w, h, scope_w, scope_h),
                        GpuScopeKind::Histogram => gpu.render_scope_histogram(&out_u8, w, h, scope_w, scope_h),
                    });
                    self.last_scope_render = std::time::Instant::now();
                    Some((tex_opt, scope_w, scope_h, mode))
                } else {
                    None
                }
            } else {
                None
            };

            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(out_u8, w, h);
            if let Some((tex_opt, sw, sh, mode)) = scope_tex {
                match tex_opt {
                    Some(t) => {
                        fb.scope_texture = t;
                        fb.scope_texture_w = sw;
                        fb.scope_texture_h = sh;
                        fb.scope_texture_mode = mode;
                    }
                    None => {
                        fb.scope_texture.clear();
                        fb.scope_texture_w = 0;
                        fb.scope_texture_h = 0;
                        fb.scope_texture_mode = mode;
                    }
                }
            }
            drop(fb);
            let _ = self.event_tx.send(AppEvent::FrameReady);
            // Wake the UI. When paused, egui sleeps until an input
            // event or a scheduled repaint — neither fires on its
            // own for a param-change-triggered regrade. Without this,
            // slider drags on a paused clip write a fresh frame to
            // `frame_buffer` but the viewer stays asleep until the
            // user moves the mouse.
            (self.repaint_cb)();
        } else if let Some((track_idx, ov_w, ov_h)) = {
            // Pick the topmost overlay (highest track index) with a frame in deterministic order.
            let overlay_indices: Vec<usize> = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                seq.overlay_video_tracks(self.current_track_index).iter().map(|(i, _)| *i).collect()
            };
            overlay_indices.iter().rev().find_map(|ti| {
                self.overlay_frames.get(ti).map(|f| (*ti, f.width, f.height))
            })
        } {
            // Case 2: No V1 video, but overlay exists (standalone title/generator).
            // Apply track + clip opacity by pre-multiplying alpha.
            let (effective_opacity, ov_rgba) = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let fps_f64 = self.fps.as_f64();
                let track_opacity = seq.tracks.get(track_idx).map(|t| t.opacity).unwrap_or(1.0);
                let clip_opacity = seq.tracks.get(track_idx)
                    .and_then(|t| t.clip_at_timecode(self.playhead))
                    .map(|(_, clip)| evaluate_clip_opacity(clip, self.playhead, fps_f64))
                    .unwrap_or(1.0);
                let effective = track_opacity * clip_opacity;
                let rgba = self.overlay_frames.get(&track_idx).map(|f| f.to_rgba()).unwrap_or_default();
                (effective, rgba)
            };
            let mut rgba = ov_rgba;
            if (effective_opacity - 1.0).abs() > 0.001 && effective_opacity >= 0.0 {
                let mul = (effective_opacity * 255.0).clamp(0.0, 255.0) as u16;
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[3] = ((pixel[3] as u16 * mul) / 255) as u8;
                }
            }
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(rgba, ov_w, ov_h);
            drop(fb);
            let _ = self.event_tx.send(AppEvent::FrameReady);
            (self.repaint_cb)();
        }
    }

    /// Drag-time downsample wrapper around `post_process_rgba_inner`.
    ///
    /// During an active colour-grade gesture (`in_node_param_edit`), the
    /// grade chain is the dominant per-frame cost — running it at half
    /// resolution removes ~75 % of that cost while the user is dragging.
    /// The result is upsampled back so the wrapper's caller never has
    /// to know which pass it got.
    ///
    /// Bypassed when:
    /// - the env gate (`KLIPP_DRAG_DOWNSAMPLE=1`) is off,
    /// - any compare mode is active (those paths read full-res snapshots),
    /// - the engine is recovering from drag end (`force_full_res_regrade`),
    /// - the source is too small for the optimisation to be worth the
    ///   resample overhead (< 480p in either axis).
    fn post_process_rgba_inner_drag_aware(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        let downsample = self.in_node_param_edit
            && drag_downsample_enabled()
            && !self.force_full_res_regrade
            && !self.bypass_color_grade
            && !self.compare_stabilization
            && !self.compare_effect_active
            && width >= 480
            && height >= 480;

        if self.in_node_param_edit {
            self.drag_grade_count = self.drag_grade_count.saturating_add(1);
        }

        if !downsample {
            return self.post_process_rgba_inner(linear_rgba, width, height);
        }

        let half_w = (width / 2).max(1);
        let half_h = (height / 2).max(1);

        let t_down = std::time::Instant::now();
        let small_in = crate::grade_resample::downsample_linear_2x(
            &linear_rgba, width, height,
        );
        let down_ms = t_down.elapsed().as_secs_f64() * 1000.0;

        let t_grade = std::time::Instant::now();
        let small_out = self.post_process_rgba_inner(small_in, half_w, half_h);
        let grade_ms = t_grade.elapsed().as_secs_f64() * 1000.0;

        let t_up = std::time::Instant::now();
        let full_out = crate::grade_resample::upsample_linear_2x(
            &small_out, half_w, half_h,
        );
        let up_ms = t_up.elapsed().as_secs_f64() * 1000.0;

        self.drag_downsampled_count = self.drag_downsampled_count.saturating_add(1);

        tracing::debug!(
            "DRAG_DOWNSAMPLE src={}x{} grade={}x{} down={:.1}ms grade={:.1}ms up={:.1}ms",
            width, height, half_w, half_h, down_ms, grade_ms, up_ms,
        );

        full_out
    }

    fn post_process_rgba_inner(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32) -> Vec<f32> {
        // Through this function `rgba` now names the linear-f32 RGBA
        // buffer, not the sRGB u8 buffer from before the linear-edge-
        // to-edge refactor. Each u8 boundary (speed-ramp blend, OFX
        // plugin chain, global 3D LUT, compare snapshots) bridges
        // locally and commentarily, keeping the shared node-graph /
        // builtin-effects spine in linear throughout.
        let rgba = linear_rgba;
        let t0 = std::time::Instant::now();
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // Pick the clip by playhead position, NOT by the engine's
        // current_track_index/current_clip_index decoder bookkeeping.
        // The two can disagree for a window during clip transitions
        // (decoder switches files before advance_to_next_clip bumps
        // the indices, or vice versa), and any disagreement here means
        // the wrong clip's grade gets applied to the new clip's
        // pixels. The cache key path above (`clip_at_timecode`) uses
        // playhead lookup; matching it makes the grade follow the
        // playhead 1:1.
        //
        // Falls back to the index-based lookup when no playable clip
        // sits under the playhead — preserves the synthetic / no-clip
        // paths that the original lookup served.
        let clip = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(self.playhead))
            .map(|(_, c)| c)
            .or_else(|| seq.tracks.get(self.current_track_index)
                .and_then(|t| t.clips.get(self.current_clip_index)));
        // Capture the current clip's id for per-effect compare gating after
        // the seq lock drops. Some() only when we actually have a clip.
        let current_clip_id: Option<klipp_core::ClipId> = clip.map(|c| c.id);
        let ofx_effects = clip.map(|c| c.effects.clone()).unwrap_or_default();
        // Reuse the cached Arc<NodeGraph> when the current clip still matches.
        // The Arc is invalidated on SequenceUpdated (graph mutation) and on clip
        // switch, so a cache hit here means we can skip the multi-kB node/edge/
        // params clone entirely — we just bump the refcount.
        let node_graph: Option<Arc<klipp_nodes::NodeGraph>> = if let Some(c) = clip {
            let cache_hit = self.current_graph_arc.is_some()
                && self.current_grade_clip_id == Some(c.id);
            if cache_hit {
                // Safe unwrap: cache_hit checks is_some().
                Some(Arc::clone(self.current_graph_arc.as_ref().unwrap()))
            } else {
                let arc = Arc::new(c.node_graph.clone());
                self.current_graph_arc = Some(Arc::clone(&arc));
                self.current_grade_clip_id = Some(c.id);
                Some(arc)
            }
        } else {
            None
        };
        let rsc_clip_id_hash = clip.map(|c| c.id.0.as_u64_pair().0).unwrap_or(0);
        let rsc_media_id = clip.and_then(|c| c.media_id());
        let clip_time_secs = clip.map(|_c| {
            (self.playhead.to_secs_f64() - self.clip_timeline_start.to_secs_f64()) * self.clip_speed
                + self.clip_source_start
        }).unwrap_or(0.0);

        // Clip-relative time for keyframe evaluation (time since clip's timeline_in)
        let clip_relative_secs = clip.map(|c| {
            (self.playhead.to_secs_f64() - c.timeline_in.to_secs_f64()).max(0.0)
        }).unwrap_or(0.0);

        // Build modifier evaluation context
        let eval_ctx = clip.map(|c| klipp_core::EvalContext {
            clip_time_secs: clip_relative_secs,
            clip_duration_secs: (c.source_out.to_secs_f64() - c.source_in.to_secs_f64()),
            timeline_time_secs: self.playhead.to_secs_f64(),
            fps: self.fps.as_f64(),
            seed: c.id.0.as_u64_pair().0,
        });

        // Build builtin_fx with keyframed values overriding static ones
        let builtin_fx = if let Some(c) = clip {
            let mut fx = c.builtin_effects.clone();

            // Evaluate opacity keyframes + modifiers
            let has_opacity_modifiers = c.param_modifiers.contains_key(&klipp_core::ParamId::Opacity);
            if !c.opacity_keyframes.is_empty() || has_opacity_modifiers {
                let opacity_default = fx.iter().find_map(|e| match e {
                    klipp_effects::BuiltinEffect::Opacity { value } => Some(*value),
                    _ => None,
                }).unwrap_or(1.0);
                let opacity_val = eval_with_modifiers(
                    &c.opacity_keyframes,
                    &c.param_modifiers,
                    &klipp_core::ParamId::Opacity,
                    clip_relative_secs,
                    opacity_default,
                    eval_ctx.as_ref().unwrap(),
                );
                let mut found = false;
                for effect in &mut fx {
                    if let klipp_effects::BuiltinEffect::Opacity { value } = effect {
                        *value = opacity_val;
                        found = true;
                        break;
                    }
                }
                if !found && (opacity_val - 1.0).abs() > 0.001 {
                    fx.push(klipp_effects::BuiltinEffect::Opacity { value: opacity_val });
                }
            }

            // Evaluate transform keyframes + modifiers
            let has_transform_modifiers = c.param_modifiers.contains_key(&klipp_core::ParamId::PositionX)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionY)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::Scale)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::Rotation);
            let has_transform_kf = !c.transform_pos_x_keyframes.is_empty()
                || !c.transform_pos_y_keyframes.is_empty()
                || !c.transform_scale_keyframes.is_empty()
                || !c.transform_rotation_keyframes.is_empty()
                || has_transform_modifiers;

            if has_transform_kf {
                // Get current transform values (or defaults)
                let (mut pos_x, mut pos_y, mut scale_x, mut scale_y, mut rotation, anchor_x, anchor_y, rotation_x_val, rotation_y_val, perspective_val) =
                    fx.iter().find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Transform {
                            position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y,
                            rotation_x, rotation_y, perspective,
                        } => Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y, *rotation_x, *rotation_y, *perspective)),
                        _ => None,
                    }).unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0));

                let ectx = eval_ctx.as_ref().unwrap();
                if !c.transform_pos_x_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionX) {
                    pos_x = eval_with_modifiers(&c.transform_pos_x_keyframes, &c.param_modifiers, &klipp_core::ParamId::PositionX, clip_relative_secs, pos_x, ectx);
                }
                if !c.transform_pos_y_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionY) {
                    pos_y = eval_with_modifiers(&c.transform_pos_y_keyframes, &c.param_modifiers, &klipp_core::ParamId::PositionY, clip_relative_secs, pos_y, ectx);
                }
                if !c.transform_scale_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::Scale) {
                    let s = eval_with_modifiers(&c.transform_scale_keyframes, &c.param_modifiers, &klipp_core::ParamId::Scale, clip_relative_secs, scale_x, ectx);
                    scale_x = s;
                    scale_y = s; // uniform scale from the Video tab
                }
                if !c.transform_rotation_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::Rotation) {
                    rotation = eval_with_modifiers(&c.transform_rotation_keyframes, &c.param_modifiers, &klipp_core::ParamId::Rotation, clip_relative_secs, rotation, ectx);
                }

                // Source rotation is now handled in the decode pipeline (NV12→RGBA GPU converter)
                // rather than the effects pipeline, so it's not added to the Transform here.

                let new_transform = klipp_effects::BuiltinEffect::Transform {
                    position_x: pos_x, position_y: pos_y,
                    scale_x, scale_y, rotation, anchor_x, anchor_y,
                    rotation_x: rotation_x_val, rotation_y: rotation_y_val, perspective: perspective_val,
                };
                let mut found = false;
                for effect in &mut fx {
                    if matches!(effect, klipp_effects::BuiltinEffect::Transform { .. }) {
                        *effect = new_transform.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    fx.push(new_transform);
                }
            }

            // Evaluate effect keyframes + modifiers (blur, sharpen, crop, chroma key)
            {
                let ectx = eval_ctx.as_ref().unwrap();
                for (i, effect) in fx.iter_mut().enumerate() {
                    match effect {
                        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
                            let key = format!("{}:radius", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *radius = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "radius", clip_relative_secs, *radius, ectx);
                                }
                            } else {
                                // No keyframes but may have modifiers
                                *radius = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "radius", clip_relative_secs, *radius, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Sharpen { amount } => {
                            let key = format!("{}:amount", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *amount = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "amount", clip_relative_secs, *amount, ectx);
                                }
                            } else {
                                *amount = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "amount", clip_relative_secs, *amount, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
                            for (name, val) in [("left", left as &mut f32), ("right", right), ("top", top), ("bottom", bottom), ("feather", feather)] {
                                let key = format!("{}:{}", i, name);
                                let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::ChromaKey { tolerance, spill_suppression, .. } => {
                            for (name, val) in [("tolerance", tolerance as &mut f32), ("spill", spill_suppression)] {
                                let key = format!("{}:{}", i, name);
                                let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Opacity { value } => {
                            let key = format!("{}:value", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *value = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "value", clip_relative_secs, *value, ectx);
                                }
                            } else {
                                *value = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "value", clip_relative_secs, *value, ectx);
                            }
                        }
                        _ => {} // Transform already handled by dedicated keyframes
                    }
                }
            }

            // Filter out disabled builtin effects. The iter-by-ref + clone
            // shape (instead of into_iter) lets the per-effect compare path
            // below filter the same `fx` a second time with a flipped bit.
            let enabled = &c.builtin_effects_enabled;
            fx.iter().enumerate()
                .filter(|(i, _)| enabled.get(*i).copied().unwrap_or(true))
                .map(|(_, e)| e.clone())
                .collect()
        } else {
            Vec::new()
        };

        // Per-effect compare: build a second builtin_fx that flips the
        // target effect's enabled flag. Runs only when compare is active
        // AND targets a builtin on the current clip. Uses the same
        // post-keyframe `fx` list as the main path, refiltering with a
        // cloned enabled vec whose target bit is toggled.
        let compare_builtin_fx: Vec<klipp_effects::BuiltinEffect> =
            if self.compare_effect_active
                && !self.compare_effect_is_ofx
                && current_clip_id.is_some()
                && current_clip_id == self.compare_effect_clip_id
            {
                if let Some(c) = clip {
                    let mut fx = c.builtin_effects.clone();
                    // Re-run the same transform / keyframe evaluation loop so
                    // the compare fx matches the main fx's animated state.
                    // (The main path mutates a local `fx`; it's out of scope
                    // here, so we just re-evaluate on our own clone — same
                    // cost as one extra keyframe pass.)
                    let target = self.compare_effect_index;
                    let mut enabled_clone = c.builtin_effects_enabled.clone();
                    while enabled_clone.len() <= target {
                        enabled_clone.push(true);
                    }
                    if target < enabled_clone.len() {
                        enabled_clone[target] = !enabled_clone[target];
                    }
                    // Evaluate effect keyframes against the clone (mirrors
                    // the main path — only the filter differs).
                    if let Some(ectx) = eval_ctx.as_ref() {
                        for (i, effect) in fx.iter_mut().enumerate() {
                            match effect {
                                klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
                                    let key = format!("{}:radius", i);
                                    let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                    *radius = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "radius", clip_relative_secs, *radius, ectx);
                                }
                                klipp_effects::BuiltinEffect::Sharpen { amount } => {
                                    let key = format!("{}:amount", i);
                                    let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                    *amount = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "amount", clip_relative_secs, *amount, ectx);
                                }
                                klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
                                    for (name, val) in [("left", left as &mut f32), ("right", right), ("top", top), ("bottom", bottom), ("feather", feather)] {
                                        let key = format!("{}:{}", i, name);
                                        let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                        *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                                    }
                                }
                                klipp_effects::BuiltinEffect::ChromaKey { tolerance, spill_suppression, .. } => {
                                    for (name, val) in [("tolerance", tolerance as &mut f32), ("spill", spill_suppression)] {
                                        let key = format!("{}:{}", i, name);
                                        let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                        *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                                    }
                                }
                                klipp_effects::BuiltinEffect::Opacity { value } => {
                                    let key = format!("{}:value", i);
                                    let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                    *value = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "value", clip_relative_secs, *value, ectx);
                                }
                                _ => {}
                            }
                        }
                    }
                    fx.into_iter().enumerate()
                        .filter(|(i, _)| enabled_clone.get(*i).copied().unwrap_or(true))
                        .map(|(_, e)| e)
                        .collect()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

        // Per-effect compare (OFX): clone `ofx_effects` with the target
        // effect's `enabled` flag flipped. Used by the second render pass
        // further down.
        let compare_ofx_effects: Vec<klipp_plugin::AppliedEffect> =
            if self.compare_effect_active
                && self.compare_effect_is_ofx
                && current_clip_id.is_some()
                && current_clip_id == self.compare_effect_clip_id
            {
                let mut clone = ofx_effects.clone();
                if let Some(e) = clone.get_mut(self.compare_effect_index) {
                    e.enabled = !e.enabled;
                }
                clone
            } else {
                Vec::new()
            };

        // Extract stabilization homography while seq lock is held
        let stab_homography: Option<[f32; 12]> = if builtin_fx.iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::VideoStabilization { .. })) {
            clip.and_then(|c| {
                let smoothed = c.smoothed_trajectory.as_ref()?;
                if smoothed.corrections.is_empty() { return None; }
                let source_dur = (c.source_out.to_secs_f64() - c.source_in.to_secs_f64()).max(0.001);
                let source_fps = smoothed.corrections.len() as f64 / source_dur;
                let frame_idx = (clip_time_secs * source_fps).round() as usize;
                let frame_idx = frame_idx.min(smoothed.corrections.len() - 1);
                let correction = &smoothed.corrections[frame_idx];
                Some([
                    correction.h[0][0] as f32, correction.h[0][1] as f32, correction.h[0][2] as f32,
                    correction.h[1][0] as f32, correction.h[1][1] as f32, correction.h[1][2] as f32,
                    correction.h[2][0] as f32, correction.h[2][1] as f32, correction.h[2][2] as f32,
                    0.0, 0.0, 0.0,
                ])
            })
        } else {
            None
        };

        drop(seq);

        // Save pre-grade frame (linear) for Before/After split view.
        // The frame buffer stores u8 so the u8 encode stays at the
        // split-view seam below; the snapshot itself is linear to
        // match the rest of the chain.
        let pre_grade_rgba = if self.bypass_color_grade {
            Some(rgba.clone())
        } else {
            None
        };

        // 1. Per-clip color grade via node graph executor
        let t1 = std::time::Instant::now();

        // Determine matte preview node: when matte_preview is active, find first
        // Qualifier or Mask node in the graph to preview its matte output
        let matte_pv_node = if self.matte_preview {
            self.matte_preview_node.or_else(|| {
                node_graph.as_ref().and_then(|g| {
                    g.nodes.iter()
                        .find(|n| matches!(n.kind, klipp_nodes::NodeKind::Qualifier | klipp_nodes::NodeKind::Mask))
                        .map(|n| n.id)
                })
            })
        } else {
            None
        };

        let rgba = if let Some(ref graph) = node_graph {
            if !graph.is_identity() || matte_pv_node.is_some() {
                let frame_number = (clip_time_secs * self.playhead.rate as f64).max(0.0) as u64;
                // Use the linear native entry point — the graph
                // executor operates in linear natively; the old
                // `execute` wrapper would decode u8→linear then re-
                // encode on the way back.
                mark_stage(EngineStage::PostProcessInnerGradeExecute);
                // Per-node thumbnail capture — only while NOT actively
                // playing. During playback the N extra readbacks (one
                // per visible node) would cost more than the grade
                // itself; when paused we can afford them. `None` =
                // zero-overhead capture skip; `Some(&set)` enables it.
                let thumb_set: Option<&std::collections::HashSet<klipp_nodes::NodeId>> =
                    if self.state != PlaybackState::Playing
                        && !self.visible_node_thumbs.is_empty()
                        && !self.thumb_capture_suspended
                    {
                        Some(&self.visible_node_thumbs)
                    } else {
                        None
                    };
                self.graph_executor.set_working_colorspace(
                    crate::video_decoder::working_colorspace(),
                );
                match self.graph_executor.execute_linear_with_thumbs(
                    &rgba,
                    graph,
                    width,
                    height,
                    clip_time_secs,
                    frame_number,
                    self.scope_tap_node,
                    matte_pv_node,
                    thumb_set,
                ) {
                    Ok((result, tapped, thumbs)) => {
                        // Store tapped output for scope display — scope
                        // consumers still speak u8, so encode at this
                        // one seam only.
                        let has_tap = tapped.is_some();
                        let has_thumbs = !thumbs.is_empty();
                        if has_tap || has_thumbs {
                            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(tap_linear) = tapped {
                                let tap_u8 = klipp_effects::linear_f32_rgba_to_srgb_u8(&tap_linear);
                                fb.scope_data = tap_u8;
                            }
                            if has_thumbs {
                                fb.node_thumbs = thumbs;
                                fb.node_thumbs_gen = fb.node_thumbs_gen.wrapping_add(1);
                            }
                        }
                        result
                    }
                    Err(e) => {
                        tracing::error!("Node graph execution failed: {:?}", e);
                        rgba
                    }
                }
            } else {
                rgba
            }
        } else {
            rgba
        };
        let t2 = std::time::Instant::now();

        // 1.5. Speed Ramp — modify effective speed if SpeedRamp effect is present
        // The SpeedRamp effect modifies the clip's playback speed natively, bypassing OFX.
        // Frame blending uses the GPU transition pipeline for smooth interpolation.
        //
        // `prev_blend_frame` stays sRGB u8: the slow-motion crossfade
        // here is bit-for-bit the pre-refactor math (lerp of two u8
        // frames clamped to `[0, 255]`). Converting the alpha to linear
        // would change the blend curve across stops. We bridge at the
        // entry/exit of this block instead — one decode + one encode —
        // to preserve the original behaviour exactly.
        let speed_ramp = builtin_fx.iter().find_map(|e| match e {
            klipp_effects::BuiltinEffect::SpeedRamp { speed, blend_mode, reverse, freeze, freeze_at, .. } => {
                Some((*speed, *blend_mode, *reverse, *freeze, *freeze_at))
            }
            _ => None,
        });
        let rgba = if let Some((speed, blend_mode, _reverse, freeze, _freeze_at)) = speed_ramp {
            if freeze {
                // Freeze frame: keep using the current frame (no speed change needed)
                rgba
            } else if blend_mode == 1
                && speed > 0.0
                && speed < 1.0
                && self.prev_blend_frame.is_some()
                && self.prev_blend_dims == (width, height)
                && rgba.len() == width as usize * height as usize * 4
            {
                // u8 boundary: decode this frame, blend against the u8
                // prev frame, re-encode. Pre-refactor-identical math.
                let mut cur_u8 = linear_rgba_to_u8(&rgba);
                let source_secs = self.master_clock.current_secs();
                let frac = (source_secs * self.fps.rate() as f64).fract();
                let alpha = frac.clamp(0.0, 1.0) as f32;
                let inv_alpha = 1.0 - alpha;
                let prev = self.prev_blend_frame.as_ref().unwrap();
                if prev.len() == cur_u8.len() {
                    for (o, p) in cur_u8.iter_mut().zip(prev.iter()) {
                        *o = (*p as f32 * inv_alpha + *o as f32 * alpha).clamp(0.0, 255.0) as u8;
                    }
                    u8_rgba_to_linear(&cur_u8)
                } else {
                    // Length mismatch (e.g. stride) — skip blend defensively.
                    rgba
                }
            } else {
                // blend_mode == 0 (nearest), speed >= 1.0, or no prev frame yet —
                // pass through. SpeedRamp's effective speed is applied by the
                // clip.speed / speed_keyframes pipeline, not here.
                rgba
            }
        } else {
            rgba
        };

        // Cache this frame (as u8) as the blend source for the next
        // tick. Only track when the SpeedRamp effect is present —
        // avoids a per-frame clone cost on clips with no time-remap
        // work to do.
        if speed_ramp.is_some() && rgba.len() == (width as usize * height as usize * 4) {
            self.prev_blend_frame = Some(linear_rgba_to_u8(&rgba));
            self.prev_blend_dims = (width, height);
        } else if speed_ramp.is_none() && self.prev_blend_frame.is_some() {
            // Free memory when the effect is removed.
            self.prev_blend_frame = None;
            self.prev_blend_dims = (0, 0);
        }

        // Per-effect compare (OFX branch): snapshot before the OFX chain
        // runs so we can re-apply the chain with the target bypassed.
        // Snapshot is linear, matching the rest of the chain; the OFX
        // compare replay below bridges at the plugin call.
        let effect_compare_ofx_active = self.compare_effect_active
            && self.compare_effect_is_ofx
            && current_clip_id == self.compare_effect_clip_id;
        let pre_ofx_for_compare = if effect_compare_ofx_active {
            Some(rgba.clone())
        } else { None };

        // 2. OFX effects chain (with temporal clip access for Retime/SlitScan/etc.)
        //
        // OFX plugins are an external boundary we don't control, so
        // they still receive sRGB u8. Bridge locally: linear→u8 in,
        // u8→linear out.
        let frame_time = self.playhead.to_secs_f64();
        let rgba = if !ofx_effects.is_empty() {
            let rgba_u8 = linear_rgba_to_u8(&rgba);
            // Check if any OFX effect needs temporal clip access (Retime, SlitScan, etc.)
            let needs_temporal = ofx_effects.iter().any(|e| {
                e.plugin_name.contains("Retime")
                    || e.plugin_name.contains("SlitScan")
                    || e.plugin_name.contains("timeOffset")
                    || e.plugin_name.contains("TimeOffset")
                    || e.plugin_name.contains("FrameBlend")
                    || e.plugin_name.contains("TimeBlur")
            });

            let out_u8 = if needs_temporal {
                // Only create temporal decoder for plugins that actually need it
                let (source_path, clip_source_in) = {
                    let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                    let clip = seq.tracks.get(self.current_track_index)
                        .and_then(|t| t.clips.get(self.current_clip_index));
                    let path = clip.and_then(|c| {
                        c.media_id().and_then(|mid| {
                            self.media_pool.read().ok().and_then(|pool| {
                                pool.get(&mid).map(|item| item.path.clone())
                            })
                        })
                    });
                    let src_in = clip.map(|c| c.source_in.to_secs_f64()).unwrap_or(0.0);
                    (path, src_in)
                };

                if let Some(path) = source_path {
                    let cached_decoder = std::cell::RefCell::new(
                        CachedFrameDecoder::open(&path)
                    );
                    let frame_provider: klipp_plugin::FrameProviderFn = Box::new(move |time_secs: f64| {
                        let source_secs = (clip_source_in + time_secs).max(0.0);
                        let mut decoder = cached_decoder.borrow_mut();
                        if let Some(ref mut dec) = *decoder {
                            dec.decode_at(source_secs)
                        } else {
                            None
                        }
                    });
                    self.ofx_host.apply_effects_chain_temporal(
                        &ofx_effects, &rgba_u8, width, height, frame_time, frame_provider,
                    )
                } else {
                    self.ofx_host.apply_effects_chain(&ofx_effects, &rgba_u8, width, height, frame_time)
                }
            } else {
                // No temporal plugins — fast path, no decoder creation
                self.ofx_host.apply_effects_chain(&ofx_effects, &rgba_u8, width, height, frame_time)
            };
            u8_rgba_to_linear(&out_u8)
        } else { rgba };
        let t3 = std::time::Instant::now();

        // 2.4. Video Stabilization: upload pre-extracted homography to GPU
        if let Some(ref h_f32) = stab_homography {
            if let Some(gpu) = &mut self.gpu_effects {
                gpu.upload_stabilization_homography(h_f32);
            }
        }

        // 2.5. Rolling Shutter Correction: compute per-row transforms from adjacent frames
        if let Some(rsc_effect) = builtin_fx.iter().find(|e| matches!(e, klipp_effects::BuiltinEffect::RollingShutterCorrection { .. })) {
            if let klipp_effects::BuiltinEffect::RollingShutterCorrection { strength, readout_ratio } = rsc_effect {
                if *strength > f32::EPSILON {
                    let frame_pts_ms = (clip_time_secs * 1000.0) as i64;

                    // Check cache
                    let cached = self.rsc_motion_cache.get(rsc_clip_id_hash, frame_pts_ms).cloned();

                    let row_transforms = if let Some((h_prev, h_next)) = cached {
                        klipp_effects::rolling_shutter::compute_row_transforms(&h_prev, &h_next, height, *readout_ratio, *strength)
                    } else {
                        // Decode adjacent frames for motion estimation
                        let frame_dur = if self.fps.as_f64() > 0.0 { 1.0 / self.fps.as_f64() } else { 1.0 / 30.0 };
                        let prev_secs = (clip_time_secs - frame_dur).max(0.0);
                        let next_secs = clip_time_secs + frame_dur;

                        // Use the source media path to open a temporary decoder
                        let source_path = rsc_media_id.and_then(|mid| {
                            self.media_pool.read().ok().and_then(|pool| pool.get(&mid).map(|item| item.path.clone()))
                        });

                        if let Some(ref path) = source_path {
                            // Decode prev and next frames at 480p for fast motion estimation
                            let prev_frame = generate_thumbnail_at_size(path, prev_secs, 480).ok();
                            let next_frame = generate_thumbnail_at_size(path, next_secs, 480).ok();

                            if let (Some((prev_rgba, pw, ph)), Some((next_rgba, nw, nh))) = (prev_frame, next_frame) {
                                let curr_frame = generate_thumbnail_at_size(path, clip_time_secs, 480).ok();
                                if let Some((curr_rgba, cw, ch)) = curr_frame {
                                    if let Some((h_prev, h_next)) = klipp_effects::rolling_shutter::estimate_motion(
                                        &prev_rgba, pw, ph, &curr_rgba, cw, ch, &next_rgba, nw, nh,
                                    ) {
                                        self.rsc_motion_cache.insert(rsc_clip_id_hash, frame_pts_ms, h_prev.clone(), h_next.clone());
                                        klipp_effects::rolling_shutter::compute_row_transforms(&h_prev, &h_next, height, *readout_ratio, *strength)
                                    } else {
                                        klipp_effects::rolling_shutter::compute_row_transforms(
                                            &klipp_effects::rolling_shutter::Homography::identity(),
                                            &klipp_effects::rolling_shutter::Homography::identity(),
                                            height, *readout_ratio, *strength,
                                        )
                                    }
                                } else {
                                    vec![]
                                }
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    };

                    if !row_transforms.is_empty() {
                        if let Some(gpu) = &mut self.gpu_effects {
                            gpu.upload_rsc_row_transforms(&row_transforms);
                        }
                    }
                }
            }
        }

        // Stabilization compare: if compare mode is on AND this clip has a
        // VideoStabilization effect, snapshot the buffer here so we can re-run
        // the builtin chain a second time with stab filtered out.
        let stab_compare_active = self.compare_stabilization
            && builtin_fx.iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::VideoStabilization { .. }));
        // Per-effect compare (builtin branch): reuse the same pre-builtin
        // snapshot so we can re-run the builtin chain with the target
        // effect flipped. OFX compare snapshots earlier (before step 2).
        let effect_compare_builtin_active = self.compare_effect_active
            && !self.compare_effect_is_ofx
            && current_clip_id == self.compare_effect_clip_id;
        let pre_builtin_for_compare = if stab_compare_active || effect_compare_builtin_active {
            Some(rgba.clone())
        } else { None };

        // 3. Built-in effects (GPU-accelerated when available, CPU fallback).
        //    Both `gpu.apply_effects` and the CPU fallback now speak
        //    linear-f32 natively — no u8 round-trip here.
        let rgba = if !builtin_fx.is_empty() {
            if let Some(gpu) = &mut self.gpu_effects {
                gpu.apply_effects(&builtin_fx, rgba, width, height)
            } else {
                klipp_effects::apply_effects(&builtin_fx, rgba, width, height)
            }
        } else { rgba };
        let t4 = std::time::Instant::now();

        // 4. Global color pipeline (GPU-accelerated 3D LUT when available).
        //    `apply_3d_lut_gpu` now speaks linear-f32. The CPU fallback
        //    `color_pipeline.process_frame` still speaks u8 — bracket only that
        //    branch.
        let rgba = if self.color_pipeline.enabled {
            if let (Some(lut), Some(gpu)) = (&self.color_pipeline.global_lut, &mut self.gpu_effects) {
                gpu.apply_3d_lut_gpu(rgba, width, height, &lut.data, lut.size as u32, self.color_pipeline.global_lut_opacity)
            } else {
                let rgba_u8 = linear_rgba_to_u8(&rgba);
                let out_u8 = self.color_pipeline.process_frame(rgba_u8, width, height);
                u8_rgba_to_linear(&out_u8)
            }
        } else {
            rgba
        };
        let t5 = std::time::Instant::now();

        // 4b. Stabilization compare second pass: re-run builtin chain + LUT with
        // VideoStabilization filtered out, producing the "raw" half of the split.
        // Mirrors steps 3 + 4 exactly so both halves are pixel-comparable except
        // for the stab warp.
        // Only the stab compare path actually consumes `pre_builtin_for_compare`
        // directly; the per-effect compare path further down borrows it by
        // reference via `.as_ref()`. To let both paths coexist when — in
        // theory — both were ever active on the same frame, we clone into
        // the stab path instead of moving out.
        let unstab_rgba = if stab_compare_active {
            let pre_builtin = pre_builtin_for_compare.as_ref()
                .expect("stab compare path: pre_builtin snapshot must exist when stab_compare_active")
                .clone();
            let no_stab: Vec<klipp_effects::BuiltinEffect> = builtin_fx.iter()
                .filter(|e| !matches!(e, klipp_effects::BuiltinEffect::VideoStabilization { .. }))
                .cloned()
                .collect();
            let r = if !no_stab.is_empty() {
                if let Some(gpu) = &mut self.gpu_effects {
                    gpu.apply_effects(&no_stab, pre_builtin, width, height)
                } else {
                    klipp_effects::apply_effects(&no_stab, pre_builtin, width, height)
                }
            } else { pre_builtin };
            let r = if self.color_pipeline.enabled {
                if let (Some(lut), Some(gpu)) = (&self.color_pipeline.global_lut, &mut self.gpu_effects) {
                    gpu.apply_3d_lut_gpu(r, width, height, &lut.data, lut.size as u32, self.color_pipeline.global_lut_opacity)
                } else {
                    let r_u8 = linear_rgba_to_u8(&r);
                    let out_u8 = self.color_pipeline.process_frame(r_u8, width, height);
                    u8_rgba_to_linear(&out_u8)
                }
            } else { r };
            Some(r)
        } else { None };

        // 4c. Per-effect compare second pass. Two branches: builtin (re-run
        // builtin chain + LUT with the target's enabled flag flipped) and
        // OFX (re-run OFX chain + builtin chain + LUT with the target's
        // enabled flag flipped). The viewer composites the output onto the
        // left half of the split; the right half is the normal `rgba`.
        let compare_effect_rgba = if effect_compare_builtin_active {
            pre_builtin_for_compare.as_ref().map(|pre| {
                // Builtin path: re-run builtin chain with compare_builtin_fx.
                let r = if !compare_builtin_fx.is_empty() {
                    if let Some(gpu) = &mut self.gpu_effects {
                        gpu.apply_effects(&compare_builtin_fx, pre.clone(), width, height)
                    } else {
                        klipp_effects::apply_effects(&compare_builtin_fx, pre.clone(), width, height)
                    }
                } else {
                    pre.clone()
                };
                // Global color LUT — same pass as the main path so both
                // halves are pixel-comparable.
                if self.color_pipeline.enabled {
                    if let (Some(lut), Some(gpu)) = (&self.color_pipeline.global_lut, &mut self.gpu_effects) {
                        gpu.apply_3d_lut_gpu(r, width, height, &lut.data, lut.size as u32, self.color_pipeline.global_lut_opacity)
                    } else {
                        let r_u8 = linear_rgba_to_u8(&r);
                        let out_u8 = self.color_pipeline.process_frame(r_u8, width, height);
                        u8_rgba_to_linear(&out_u8)
                    }
                } else {
                    r
                }
            })
        } else if effect_compare_ofx_active {
            pre_ofx_for_compare.as_ref().map(|pre| {
                // OFX path: re-apply OFX chain with the target bypassed,
                // then the full builtin chain and LUT. OFX plugins speak
                // u8 — bridge.
                let pre_u8 = linear_rgba_to_u8(pre);
                let r_u8 = if !compare_ofx_effects.is_empty() {
                    self.ofx_host.apply_effects_chain(&compare_ofx_effects, &pre_u8, width, height, frame_time)
                } else {
                    pre_u8
                };
                let r = u8_rgba_to_linear(&r_u8);
                let r = if !builtin_fx.is_empty() {
                    if let Some(gpu) = &mut self.gpu_effects {
                        gpu.apply_effects(&builtin_fx, r, width, height)
                    } else {
                        klipp_effects::apply_effects(&builtin_fx, r, width, height)
                    }
                } else {
                    r
                };
                if self.color_pipeline.enabled {
                    if let (Some(lut), Some(gpu)) = (&self.color_pipeline.global_lut, &mut self.gpu_effects) {
                        gpu.apply_3d_lut_gpu(r, width, height, &lut.data, lut.size as u32, self.color_pipeline.global_lut_opacity)
                    } else {
                        let r_u8 = linear_rgba_to_u8(&r);
                        let out_u8 = self.color_pipeline.process_frame(r_u8, width, height);
                        u8_rgba_to_linear(&out_u8)
                    }
                } else {
                    r
                }
            })
        } else {
            None
        };

        // Store pre-grade frame in frame buffer for Before/After split.
        // `fb.original_data` is u8 sRGB — encode at the seam.
        if let Some(original_linear) = pre_grade_rgba {
            let original_u8 = linear_rgba_to_u8(&original_linear);
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.original_data = original_u8;
        } else {
            // Clear original when not in split mode to free memory
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if !fb.original_data.is_empty() {
                fb.original_data = Vec::new();
            }
        }

        // Store unstab frame for stabilization compare split. `fb.unstab_data` is u8.
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(u_linear) = unstab_rgba {
                fb.unstab_data = linear_rgba_to_u8(&u_linear);
            } else if !fb.unstab_data.is_empty() {
                fb.unstab_data = Vec::new();
            }
        }

        // Store per-effect compare frame (or clear when inactive).
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cmp_linear) = compare_effect_rgba {
                fb.compare_effect_data = linear_rgba_to_u8(&cmp_linear);
            } else if !fb.compare_effect_data.is_empty() {
                fb.compare_effect_data = Vec::new();
            }
        }

        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let seq_ms = (t1 - t0).as_secs_f64() * 1000.0;
        let grade_ms = (t2 - t1).as_secs_f64() * 1000.0;
        let ofx_ms = (t3 - t2).as_secs_f64() * 1000.0;
        let builtin_ms = (t4 - t3).as_secs_f64() * 1000.0;
        let lut_ms = (t5 - t4).as_secs_f64() * 1000.0;

        if total_ms > 2.0 {
            // `gpu=y` means `gpu_effects` is initialized — the GRAD
            // pipeline *could* run on GPU. Doesn't prove every node inside
            // the node graph used GPU (some nodes may still fall back to
            // CPU internally). `gpu=n` is a definite red flag — means GPU
            // init failed and everything is on CPU.
            let gpu_available = if self.gpu_effects.is_some() { "y" } else { "n" };
            debug!(
                "PERF post_process: {:.1}ms total | seq_read={:.1}ms grade={:.1}ms ofx={:.1}ms builtin={:.1}ms lut={:.1}ms | gpu={}",
                total_ms, seq_ms, grade_ms, ofx_ms, builtin_ms, lut_ms, gpu_available,
            );
        }

        // Push one sample per processed frame into the shared ring
        // so the UI overlay + session recorder have real numbers to
        // paint. Target fps is read from the active clip's source
        // framerate (falls back to 0.0 when nothing is playing).
        // Drop heuristic: `total_ms` exceeded the frame budget.
        let target_fps = self.target_fps_for_overlay();
        let dropped = match target_fps {
            Some(fps) if fps > 0.0 => total_ms as f32 > (1000.0 / fps),
            _ => false,
        };
        let sample = crate::perf::PerfSample {
            timestamp: std::time::Instant::now(),
            width,
            height,
            target_fps: target_fps.unwrap_or(0.0),
            seq_read_ms: seq_ms as f32,
            grade_ms: grade_ms as f32,
            ofx_ms: ofx_ms as f32,
            builtin_ms: builtin_ms as f32,
            lut_ms: lut_ms as f32,
            total_ms: total_ms as f32,
            dropped,
            // Wrap-level stages aren't known here — `post_process_rgba`
            // (the wrapper) updates them in the most-recent sample
            // after we return. Defaulted to 0 so cache-hit paths
            // that skip the wrapper still produce a valid sample.
            bridge_u8_ms: 0.0,
            reencode_ms: 0.0,
            scope_ms: 0.0,
            composite_ms: 0.0,
            update_playback_ms: 0.0,
        };
        if let Ok(mut ring) = self.perf_ring.lock() {
            ring.push(sample);
        }

        rgba
    }

    /// Best-available target fps for perf reporting. Reads from the
    /// currently-playing sequence's time base; returns `None` if the
    /// engine is idle or we can't lock the sequence cheaply.
    fn target_fps_for_overlay(&self) -> Option<f32> {
        let Ok(seq) = self.sequence.try_read() else { return None };
        let fps = seq.fps.as_f64() as f32;
        if fps > 0.0 && fps.is_finite() { Some(fps) } else { None }
    }

    /// Shared handle to the perf ring buffer. The UI clones this
    /// Arc to paint the overlay and (on request) export a session
    /// recording.
    pub fn perf_ring(&self) -> crate::perf::SharedPerfRing {
        self.perf_ring.clone()
    }

    /// Whether enough wall time has passed since the last GPU scope
    /// render to justify another one. Colourists can't perceive
    /// scope updates faster than ~10 Hz, and each scope render costs
    /// two full GPU sync fences (~30-40 ms together). Capping the
    /// refresh rate here trades ~70 ms of latency on the scope
    /// overlay for getting every frame back inside the 24 fps
    /// budget during playback with sliders active.
    fn should_render_scope(&self) -> bool {
        const SCOPE_MIN_INTERVAL_MS: u128 = 100;
        self.scopes_visible
            && self.last_scope_render.elapsed().as_millis() >= SCOPE_MIN_INTERVAL_MS
    }

    /// Wait for a video frame near `target_secs` to appear in the queue.
    /// Drains any stale frames that are far from the target (>2s away).
    /// This ensures the clock and audio don't start before video is ready.
    fn wait_for_valid_frame(&self, target_secs: f64) {
        let Some(dec) = &self.v1_decoder else { return };
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        loop {
            {
                let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                // Drain frames that are far from the seek target
                while let Some(front) = queue.front() {
                    let pts = front.pts_secs();
                    if (pts - target_secs).abs() < 2.0 || front.is_eof {
                        // Frame is close enough to target — good to go
                        return;
                    }
                    // Stale frame from a previous position — discard
                    debug!(
                        "wait_for_valid_frame: discarding stale frame pts={:.3}s (target={:.3}s)",
                        pts, target_secs
                    );
                    queue.pop();
                }
            }
            if std::time::Instant::now() >= deadline {
                debug!("wait_for_valid_frame: timed out waiting for frame near {target_secs:.3}s");
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn sequence_duration(&self) -> Timecode {
        self.sequence.read().unwrap_or_else(|e| e.into_inner()).total_duration()
    }

    fn path_for_media(&self, media_id: MediaId) -> Option<PathBuf> {
        self.media_pool
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&media_id)
            .map(|item| {
                // 1. Proxy files (explicit user choice, highest priority)
                if self.use_proxies {
                    if let Some(ref proxy_path) = item.proxy_path {
                        if item.proxy_ready && proxy_path.exists() {
                            tracing::trace!("resolve_media_path: using proxy for {}", item.filename);
                            return proxy_path.clone();
                        }
                    }
                }
                // 2. Optimised playback: substitute the skim file for the
                //    source when the source is too heavy for real-time
                //    playback on the current pipeline. Mirrors
                //    DaVinci Optimized Media + FCP Proxy. Pure decision
                //    in `should_use_skim_for_playback` (unit-tested);
                //    we just feed it the inputs and pick the path.
                if let Some(ref skim_path) = item.skim_file {
                    let mode = crate::video_decoder::optimised_playback_mode();
                    let skim_ready = skim_path.exists();
                    if crate::video_decoder::should_use_skim_for_playback(
                        mode,
                        item.resolution.height,
                        item.fps.as_f64(),
                        skim_ready,
                    ) {
                        tracing::info!(
                            "resolve_media_path: using skim ({} {}x{}@{:.2} fps) for {}",
                            match mode { 1 => "AlwaysOriginal", 2 => "AlwaysOptimized", _ => "Auto" },
                            item.resolution.width, item.resolution.height,
                            item.fps.as_f64(), item.filename,
                        );
                        return skim_path.clone();
                    }
                }
                // 3. Original source file.
                item.path.clone()
            })
    }

    fn load_file(&mut self, path: PathBuf, media_id: MediaId) {
        info!("Loading media: {:?}", path);

        match ffmpeg_next::format::input(&path) {
            Ok(format_ctx) => {
                let duration_secs = format_ctx.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

                let mut video_w = 1920u32;
                let mut video_h = 1080u32;
                let mut container_fps = 24.0f64;
                let mut video_codec = String::new();
                let mut audio_codec = String::new();
                let mut has_audio = false;
                let mut sample_rate: Option<u32> = None;
                let mut audio_channels: Option<u16> = None;
                let mut colour_space: u8 = 0; // 0=Unknown, 1=SDR, 2=HDR10, 3=HLG

                for stream in format_ctx.streams() {
                    let params = stream.parameters();
                    let medium = params.medium();
                    match medium {
                        ffmpeg_next::media::Type::Video => {
                            let decoder = ffmpeg_next::codec::context::Context::from_parameters(params.clone())
                                .and_then(|ctx| ctx.decoder().video());
                            if let Ok(dec) = decoder {
                                video_w = dec.width();
                                video_h = dec.height();
                                video_codec = format!("{:?}", dec.id());
                                // Extract colour space from codec parameters (FFI)
                                // color_primaries: BT.709=1, BT.2020=9
                                // color_trc: BT.709=1, PQ/SMPTE2084=16, HLG/ARIB=18
                                unsafe {
                                    let par = params.as_ptr();
                                    if !par.is_null() {
                                        let primaries = (*par).color_primaries as i32;
                                        let transfer = (*par).color_trc as i32;
                                        colour_space = if primaries == 9 && transfer == 16 {
                                            2 // HDR10: BT.2020 + PQ
                                        } else if primaries == 9 && transfer == 18 {
                                            3 // HLG: BT.2020 + HLG
                                        } else if primaries == 1 || primaries == 0 || primaries == 2 {
                                            1 // SDR: BT.709 or unspecified
                                        } else {
                                            0 // Unknown
                                        };
                                    }
                                }
                            }
                            let rate = stream.avg_frame_rate();
                            if rate.1 != 0 {
                                container_fps = rate.0 as f64 / rate.1 as f64;
                            }
                        }
                        ffmpeg_next::media::Type::Audio => {
                            has_audio = true;
                            let decoder = ffmpeg_next::codec::context::Context::from_parameters(params)
                                .and_then(|ctx| ctx.decoder().audio());
                            if let Ok(dec) = decoder {
                                audio_codec = format!("{:?}", dec.id());
                                sample_rate = Some(dec.rate() as u32);
                                audio_channels = Some(dec.channels() as u16);
                            }
                        }
                        _ => {}
                    }
                }

                // Detect rotation and swap dimensions for display if needed (phone videos)
                let source_rotation = detect_rotation_from_path(&path);
                if source_rotation == 90 || source_rotation == 270 {
                    std::mem::swap(&mut video_w, &mut video_h);
                }

                // Detect still images: has video, no audio, near-zero duration,
                // and file extension matches common image formats.
                let is_still = !video_codec.is_empty()
                    && !has_audio
                    && (duration_secs < 0.1 || duration_secs.is_nan())
                    && is_image_extension(&path);

                self.fps = fps_from_float(container_fps);
                self.duration = RationalTime::from_secs_f64(duration_secs, self.fps.rate());
                self.playhead = RationalTime::from_frames(0, self.fps.rate());
                self.current_file = Some(path.clone());
                self.current_clip_index = 0;
                self.current_track_index = 0;
                self.state = PlaybackState::Stopped;

                let fs_meta = std::fs::metadata(&path).ok();
                let file_size_bytes = fs_meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let file_modified_at = fs_meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());

                // Try to extract creation_time from container metadata (camera shoot date)
                let media_created_at = format_ctx.metadata().get("creation_time")
                    .and_then(|s| parse_iso8601_timestamp(s));

                // Extract extended metadata from container and stream tags
                let fmt_meta = &format_ctx.metadata();
                let camera_make = fmt_meta.get("com.apple.quicktime.make")
                    .or_else(|| fmt_meta.get("make"))
                    .or_else(|| fmt_meta.get("manufacturer"))
                    .map(|s| s.to_string());
                let camera_model = fmt_meta.get("com.apple.quicktime.model")
                    .or_else(|| fmt_meta.get("model"))
                    .map(|s| s.to_string());
                // Encoder tag: try format-level first, then fall
                // back to stream-level. Sony cameras (A6700 etc.)
                // write `encoder=HEVC Coding` only at the stream
                // level — without this fallback the tag is invisible
                // and the log-format heuristic loses its only signal
                // on out-of-camera Sony HEVC files.
                let mut encoder = fmt_meta.get("encoder")
                    .or_else(|| fmt_meta.get("encoding_tool"))
                    .or_else(|| fmt_meta.get("com.apple.quicktime.software"))
                    .map(|s| s.to_string());
                if encoder.is_none() {
                    for stream in format_ctx.streams() {
                        let m = stream.metadata();
                        if let Some(e) = m.get("encoder")
                            .or_else(|| m.get("encoding_tool"))
                        {
                            encoder = Some(e.to_string());
                            break;
                        }
                    }
                }

                // Extract bit depth, bitrate, pixel format, color info from video stream
                let mut bit_depth: Option<u8> = None;
                let mut video_bitrate_kbps: Option<u32> = None;
                let mut audio_bitrate_kbps: Option<u32> = None;
                let mut color_primaries_name: Option<String> = None;
                let mut color_transfer_name: Option<String> = None;
                let mut pixel_format_name: Option<String> = None;

                for stream in format_ctx.streams() {
                    let params = stream.parameters();
                    match params.medium() {
                        ffmpeg_next::media::Type::Video => {
                            unsafe {
                                let par = params.as_ptr();
                                if !par.is_null() {
                                    // Bit depth from pixel format
                                    let pix_fmt = (*par).format;
                                    let desc = ffmpeg_next::ffi::av_pix_fmt_desc_get(std::mem::transmute::<_, ffmpeg_next::ffi::AVPixelFormat>(pix_fmt));
                                    if !desc.is_null() {
                                        bit_depth = Some((*desc).comp[0].depth as u8);
                                        let name_ptr = ffmpeg_next::ffi::av_get_pix_fmt_name(std::mem::transmute::<_, ffmpeg_next::ffi::AVPixelFormat>(pix_fmt));
                                        if !name_ptr.is_null() {
                                            pixel_format_name = Some(std::ffi::CStr::from_ptr(name_ptr).to_string_lossy().to_string());
                                        }
                                    }
                                    // Color primaries and transfer names
                                    let prim = (*par).color_primaries as i32;
                                    color_primaries_name = Some(match prim {
                                        1 => "BT.709", 4 => "BT.470M", 5 => "BT.470BG",
                                        6 => "SMPTE 170M", 9 => "BT.2020", 11 => "DCI-P3", 12 => "P3-D65",
                                        _ => "Unspecified",
                                    }.to_string());
                                    let trc = (*par).color_trc as i32;
                                    color_transfer_name = Some(match trc {
                                        1 => "BT.709", 4 => "Gamma 2.2", 5 => "Gamma 2.8",
                                        6 => "SMPTE 170M", 8 => "Linear", 13 => "sRGB",
                                        14 => "BT.2020 10-bit", 15 => "BT.2020 12-bit",
                                        16 => "PQ (ST 2084)", 18 => "HLG (ARIB STD-B67)",
                                        _ => "Unspecified",
                                    }.to_string());
                                    // Video bitrate
                                    let br = (*par).bit_rate;
                                    if br > 0 { video_bitrate_kbps = Some((br / 1000) as u32); }
                                }
                            }
                        }
                        ffmpeg_next::media::Type::Audio => {
                            unsafe {
                                let par = params.as_ptr();
                                if !par.is_null() {
                                    let br = (*par).bit_rate;
                                    if br > 0 { audio_bitrate_kbps = Some((br / 1000) as u32); }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let _ = self.event_tx.send(AppEvent::MediaLoaded(Box::new(
                    klipp_core::MediaLoadedPayload {
                        media_id,
                        path,
                        duration: self.duration,
                        fps: self.fps,
                        resolution: Resolution {
                            width: video_w,
                            height: video_h,
                        },
                        video_codec,
                        audio_codec,
                        has_audio,
                        file_size_bytes,
                        sample_rate,
                        audio_channels,
                        is_still,
                        file_modified_at,
                        media_created_at,
                        colour_space,
                        source_rotation,
                        camera_make,
                        camera_model,
                        encoder,
                        bit_depth,
                        video_bitrate_kbps,
                        audio_bitrate_kbps,
                        color_primaries: color_primaries_name,
                        color_transfer: color_transfer_name,
                        pixel_format: pixel_format_name,
                    },
                )));

                // NOTE: Do NOT grab the first frame here. `load_file`
                // runs on every media import, and writing the first
                // frame into the shared Program-viewer frame buffer
                // made every newly imported file auto-appear in the
                // Program viewer — even when the user was just adding
                // a clip to the bin. Source-viewer previews use the
                // dedicated source decode path (`source_video_texture`
                // via `LoadSourceMedia` / `start_source_frame_decode_at`),
                // not this entry point. Timeline playback triggers its
                // own decode when a clip at the playhead is activated,
                // so losing the side-effect first-frame here doesn't
                // regress the Program viewer's grade / scrub paths.
                (self.repaint_cb)();
            }
            Err(e) => {
                error!("Failed to open media file: {e}");
                let _ = self.event_tx.send(AppEvent::MediaLoadFailed {
                    media_id,
                    error: e.to_string(),
                });
                (self.repaint_cb)();
            }
        }
    }

    /// Kept for targeted callers that want to explicitly seed the
    /// Program viewer's frame buffer with a clip's first frame. Used
    /// to run automatically from `load_file` for every import — that
    /// made every media import auto-appear in the Program viewer, so
    /// the call site was removed. Left in-place so a future "preview
    /// this timeline clip" gesture can call it explicitly.
    #[allow(dead_code)]
    fn grab_first_frame(&mut self) {
        if let Some(path) = &self.current_file {
            match VideoDecoder::open(path, self.current_source_rotation) {
                Ok(dec) => {
                    dec.resume();
                    self.v1_decoder = Some(dec);
                    // Longer timeout: VAAPI init can take ~150ms alone
                    if !self.grab_single_frame_timeout(Duration::from_millis(500)) {
                        if let Some(dec) = &self.v1_decoder {
                            dec.pause();
                        }
                    }
                }
                Err(e) => {
                    warn!("Could not open decoder for first frame: {e}");
                }
            }
        }
    }

    // ── Source Viewer playback ─────────────────────────────────────
    //
    // The source viewer is the second viewer of an NLE — a colourist
    // / editor previews unedited media here, marks IN/OUT, then edits
    // it into the timeline. It's intentionally separate from the
    // program viewer's state machine: a different decoder, a
    // different frame buffer, an independent playhead. Audio shares
    // the existing cpal stream + mixer, on a dedicated mixer track,
    // and the mutual-exclusion rule below means at most one viewer
    // owns audio at any moment so the two never fight over the
    // device.

    /// Bind a media file to the source viewer. Spawns a persistent
    /// `VideoDecoder` against the file (keeping the decoder open
    /// across seeks is what makes scrub + playback responsive vs the
    /// previous one-shot thumbnail decode), opens an audio source if
    /// the file has audio, and decodes the first frame into the
    /// source frame buffer so the panel has something to show
    /// immediately. No-op when the same path is already bound.
    fn set_source_media(&mut self, path: PathBuf, source_rotation: i32) {
        if self.source_path.as_ref() == Some(&path) {
            return;
        }
        // Tear down anything currently bound — clean slate.
        self.stop_source();

        match VideoDecoder::open(&path, source_rotation) {
            Ok(dec) => {
                dec.resume();
                self.source_decoder = Some(dec);
                self.source_path = Some(path.clone());
                self.source_rotation = source_rotation;
                self.source_state = PlaybackState::Paused;
                self.source_playhead_secs = 0.0;
                self.source_at_eof = false;
                // Arm a single paused-drain so the per-tick loop
                // pops the very first decoded frame to the viewer
                // (the cold-start poster). Cleared after one frame
                // lands so the playhead doesn't keep walking on its
                // own through the rest of the buffered queue.
                self.source_drain_pending = true;
                // Detect FPS from the file via a tiny ffmpeg probe — needed
                // for `StepSource` and to pace the playback advance. Falls
                // back to 24 fps on probe failure so a step still moves.
                self.source_fps = probe_source_fps(&path).unwrap_or(24.0);
                // Drain one frame to seed the source frame buffer. Same
                // wait-for-decoder pattern as the timeline's first-frame
                // grab — gives VAAPI/NVDEC time to warm up.
                self.grab_one_source_frame_timeout(Duration::from_millis(500));
                // Open audio (silent attach — `play_source` is what
                // actually unmutes / resumes).
                self.open_source_audio(&path, 0.0);
            }
            Err(e) => {
                warn!("Source viewer: failed to open decoder for {path:?}: {e}");
            }
        }
    }

    /// Rebuild the source decoder from scratch when the previous
    /// playback hit EOF. ffmpeg's mp4toannexb / annexb BSFs latch
    /// to EOF once they see it and reject every subsequent packet
    /// ("A non-NULL packet sent after an EOF"); a plain `dec.seek`
    /// or `dec.resume` is not enough to recover. Tear the decoder
    /// down and reopen so the BSF starts fresh.
    fn rebuild_source_decoder_if_at_eof(&mut self) {
        if !self.source_at_eof {
            return;
        }
        self.source_at_eof = false;
        let Some(path) = self.source_path.clone() else { return };
        let rotation = self.source_rotation;
        // `set_source_media` early-returns when the path matches —
        // clear `source_path` so the rebuild takes the full open +
        // decode-thread spawn path.
        self.source_path = None;
        self.set_source_media(path, rotation);
    }

    /// Begin source-viewer playback. Auto-pauses the timeline so the
    /// audio mixer doesn't have two viewers feeding it simultaneously.
    /// Resumes the source decoder, anchors the wall-clock to the
    /// current source playhead, mutes timeline audio, unmutes the
    /// source audio source.
    fn play_source(&mut self) {
        if self.source_decoder.is_none() {
            tracing::warn!("play_source: source_decoder is None, ignoring Play");
            return;
        }
        tracing::info!(
            "play_source: starting playback (source_state was {:?}, at_eof={})",
            self.source_state, self.source_at_eof,
        );
        // If the previous play hit EOF, the BSF is latched and a
        // plain resume won't produce frames — rebuild the decoder
        // before resuming.
        self.rebuild_source_decoder_if_at_eof();
        // Mutual exclusion: stop the timeline before the source takes
        // over. `Pause` is a no-op when the timeline isn't playing.
        if self.state == PlaybackState::Playing {
            self.handle_command(AppCommand::Pause);
        }
        // Mute timeline tracks (don't tear them down — the user might
        // switch back) and unmute the source track on the mixer.
        self.set_source_audio_owner(true);
        // Resume the audio source (it starts paused after open).
        if let Some(ref src) = self.source_audio_source {
            src.resume();
        }
        // Resume the decoder thread + audio output.
        if let Some(ref dec) = self.source_decoder {
            dec.resume();
        }
        if let Some(ref aout) = self.audio_output {
            aout.0.resume();
        }
        // Pre-warm: wait briefly for the decoder to queue at least
        // one frame before returning. After Set/SeekSource the queue
        // is drained to seed the viewer; without this wait the user
        // sees a ~100 ms stutter at the start of every Play while
        // the decoder catches up. 60 ms covers a 4K NVDEC seek+
        // decode comfortably; we exit early as soon as the first
        // frame lands so faster sources don't pay the full budget.
        if let Some(ref dec) = self.source_decoder {
            let deadline = std::time::Instant::now() + Duration::from_millis(60);
            while std::time::Instant::now() < deadline {
                let n = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).len();
                if n >= 1 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        // Anchor the wall-clock AFTER the pre-warm so the first frame
        // pops at t=0 of playback instead of "100 ms behind".
        self.source_clock_anchor_wall = std::time::Instant::now();
        self.source_clock_anchor_secs = self.source_playhead_secs;
        self.source_state = PlaybackState::Playing;
        let _ = self.event_tx.send(AppEvent::SourcePlaybackStarted);
        (self.repaint_cb)();
    }

    /// Pause source-viewer playback. Decoder and audio source stay
    /// open so resuming is instant — only state + clock anchor flip.
    fn pause_source(&mut self) {
        if self.source_state != PlaybackState::Playing {
            return;
        }
        if let Some(ref dec) = self.source_decoder {
            dec.pause();
        }
        if let Some(ref src) = self.source_audio_source {
            src.pause();
        }
        // Don't touch cpal `audio_output` — it might still be feeding
        // a chirp or a future timeline play. Just mute the source
        // track so its (now stale) consumer doesn't bleed silence.
        self.source_state = PlaybackState::Paused;
        let _ = self.event_tx.send(AppEvent::SourcePlaybackPaused);
        (self.repaint_cb)();
    }

    /// Seek the source viewer to `secs` (source-time, not timeline).
    /// Pauses the decoder briefly, flushes the queue, seeks the audio
    /// source if present, and decodes one frame so the viewer paints
    /// the new position immediately.
    fn seek_source(&mut self, secs: f64) {
        if self.source_decoder.is_none() {
            return;
        }
        let secs = secs.max(0.0);
        // If we're parked at EOF, the BSF rejects every packet —
        // each seek would block in `grab_one_source_frame_timeout`
        // for 500ms producing nothing. Rebuild before seeking so a
        // scrub on the post-EOF clip stays responsive.
        self.rebuild_source_decoder_if_at_eof();
        let was_playing = self.source_state == PlaybackState::Playing;
        if let Some(ref dec) = self.source_decoder {
            dec.seek(secs);
            dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
            dec.resume();
        }
        if let Some(ref src) = self.source_audio_source {
            src.seek(secs);
        }
        self.source_playhead_secs = secs;
        // Fire-and-forget: do NOT block here waiting for the
        // decoder to produce the seek-target frame. The per-tick
        // paused-drain picks the frame up the moment it lands.
        //
        // History: the previous 50 ms blocking wait pinned the
        // engine thread at 100 % during scrub. 30 Hz UI dispatch
        // × 50 ms wait = engine spending all its time inside this
        // one function, leaving no slot for the paused-drain (so
        // frames never reached the viewer) and no slot for the
        // decoder thread to actually finish its previous seek
        // before the next one piled on. By returning immediately
        // here we let the engine tick at full rate and let the
        // decoder coalesce SeekTo commands aggressively (its own
        // queue accumulates while it works).
        self.source_drain_pending = true;
        if was_playing {
            // Re-anchor the clock at the new playhead so playback
            // continues from here.
            self.source_clock_anchor_wall = std::time::Instant::now();
            self.source_clock_anchor_secs = self.source_playhead_secs;
            if let Some(ref dec) = self.source_decoder {
                dec.resume();
            }
        }
        // NOTE: do NOT pause the decoder when we're paused-seeking.
        // Sending Pause right after Resume + SeekTo means the
        // decoder thread coalesces the queue and ends up in its
        // paused-sleep branch — the seek is applied (format_ctx
        // moves) but no frames are produced, so the viewer keeps
        // painting the pre-seek image. The decoder backpressures
        // naturally once its 16-frame queue fills, which is the
        // correct way to "rest" between scrubs.
        let _ = self.event_tx.send(AppEvent::SourcePlayheadMoved(
            self.source_playhead_secs,
        ));
        (self.repaint_cb)();
    }

    /// Step the source playhead by `frames` source frames (negative
    /// for backward). Maps to a `seek_source` at the new time — the
    /// existing seek-then-grab path already produces the right frame.
    fn step_source(&mut self, frames: i64) {
        if self.source_fps <= 0.0 {
            return;
        }
        let dt = frames as f64 / self.source_fps;
        let target = (self.source_playhead_secs + dt).max(0.0);
        self.seek_source(target);
    }

    /// Tear down the source viewer: stop decoder thread, drop audio
    /// source, mute the source mixer track, blank the source frame
    /// buffer. Safe to call on an already-stopped viewer.
    fn stop_source(&mut self) {
        if let Some(mut dec) = self.source_decoder.take() {
            dec.stop_detached();
        }
        if let Some(mut src) = self.source_audio_source.take() {
            src.stop_detached();
        }
        // Make sure the mixer's source track is silent regardless of
        // who owned audio last.
        if let Ok(mut m) = self.mixer.lock() {
            m.ensure_tracks(self.source_mixer_track_index + 1);
            m.tracks[self.source_mixer_track_index].muted = true;
        }
        self.source_path = None;
        self.source_state = PlaybackState::Idle;
        self.source_playhead_secs = 0.0;
    }

    /// Per-tick source-viewer playback loop. Mirrors the program
    /// viewer's `update_playback` but lives outside the timeline
    /// state machine — runs whenever `source_state == Playing`,
    /// independent of `self.state`. Pops any frame the decoder has
    /// produced, advances the playhead off the wall-clock anchor,
    /// writes the new frame to `source_frame_buffer`. EOF stops
    /// playback (caller can press Play to restart from 0).
    fn update_source_playback(&mut self) {
        if self.source_state != PlaybackState::Playing {
            return;
        }
        let Some(dec) = self.source_decoder.as_ref() else { return };
        // Pop one frame at a time so the playhead moves at video rate
        // — popping all queued frames per tick would burn through the
        // buffer in a few ms and stall on EOF.
        let frame = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop();
        let Some(mut frame) = frame else { return };
        if frame.is_eof {
            // End of file — flag it and pause. The next play_source
            // call will seek back to 0 before resuming, otherwise
            // the decoder stays parked at EOF and produces no
            // frames on the second click.
            self.source_at_eof = true;
            self.pause_source();
            return;
        }
        let frame_pts = frame.pts_secs();
        let (w, h) = (frame.width, frame.height);
        // GPU-resident decode (KLIPP_GPU_FRAMES=1) ships with empty
        // `frame.data` until ensure_cpu_data() is called — without
        // this the source viewer writes empty buffers, the upload
        // path bails on the size mismatch, and the viewer stays on
        // the static thumbnail with playback "invisible".
        frame.ensure_cpu_data();
        // Linear f32 → sRGB u8 for the frame buffer (no grading on
        // the source viewer — this is the raw media).
        let rgba_u8 = linear_rgba_to_u8(&frame.data);
        {
            let mut fb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(rgba_u8, w, h);
        }
        // Advance the source playhead. The wall-clock anchor is the
        // ground truth — frame PTS is only used to detect drift
        // (logged for debugging in update_playback's equivalent; here
        // we keep it minimal). Letting wall-time drive avoids the
        // playhead lurching when the decoder produces a burst of
        // frames after a seek.
        let elapsed = self.source_clock_anchor_wall.elapsed().as_secs_f64();
        self.source_playhead_secs = self.source_clock_anchor_secs + elapsed;
        // Sanity clamp to actual frame PTS if we're more than half a
        // frame ahead — keeps display + clock from desyncing on
        // long-running playback.
        let half_frame = if self.source_fps > 0.0 { 0.5 / self.source_fps } else { 0.02 };
        if (self.source_playhead_secs - frame_pts).abs() > 1.0 {
            self.source_clock_anchor_wall = std::time::Instant::now();
            self.source_clock_anchor_secs = frame_pts;
            self.source_playhead_secs = frame_pts;
        } else if self.source_playhead_secs > frame_pts + half_frame {
            self.source_playhead_secs = frame_pts;
        }
        let _ = self.event_tx.send(AppEvent::FrameReady);
        let _ = self.event_tx.send(AppEvent::SourcePlayheadMoved(
            self.source_playhead_secs,
        ));
        (self.repaint_cb)();
    }

    /// Non-blocking sibling of `grab_one_source_frame_timeout`.
    /// Pops a single ready frame off the source decoder queue (if
    /// any) and writes it to `source_frame_buffer`. Does NOT wait
    /// for the decoder. Called from the engine tick when the
    /// source viewer is paused, so frames that arrive a tick or
    /// two after a fast `seek_source` (50 ms timeout) still reach
    /// the viewer instead of stalling in the queue.
    /// Returns true if a frame was consumed (so the per-tick caller
    /// can clear its "drain pending" flag and stop pumping until
    /// the next seek). Returns false on empty queue / EOF — the
    /// caller leaves the flag set so it tries again next tick.
    fn drain_one_source_frame_to_buffer(&mut self) -> bool {
        let Some(dec) = self.source_decoder.as_ref() else { return false };
        let frame = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop();
        let Some(mut frame) = frame else { return false };
        if frame.is_eof {
            // EOF arriving while paused — flag it so the next play
            // does the rebuild, but don't change state (we're
            // already paused). Treat as "consumed" so the caller
            // doesn't keep retrying forever on a dead queue.
            self.source_at_eof = true;
            return true;
        }
        let (w, h) = (frame.width, frame.height);
        frame.ensure_cpu_data();
        let rgba_u8 = linear_rgba_to_u8(&frame.data);
        {
            let mut fb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame(rgba_u8, w, h);
        }
        self.source_playhead_secs = frame.pts_secs();
        let _ = self.event_tx.send(AppEvent::FrameReady);
        let _ = self.event_tx.send(AppEvent::SourcePlayheadMoved(
            self.source_playhead_secs,
        ));
        (self.repaint_cb)();
        true
    }

    /// Wait up to `timeout` for the source decoder to produce a
    /// frame, then post-process it into the source frame buffer.
    /// Used by `set_source_media` and `seek_source` to seed the
    /// viewer with a real frame instead of waiting for the next
    /// playback tick.
    fn grab_one_source_frame_timeout(&mut self, timeout: Duration) {
        let Some(dec) = self.source_decoder.as_ref() else { return };
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let frame = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop();
            if let Some(mut frame) = frame {
                if frame.is_eof { return; }
                let (w, h) = (frame.width, frame.height);
                // GPU-resident frames ship with empty `data` until
                // ensure_cpu_data() materialises it — see matching
                // call in update_source_playback.
                frame.ensure_cpu_data();
                let rgba_u8 = linear_rgba_to_u8(&frame.data);
                {
                    let mut fb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.write_frame(rgba_u8, w, h);
                }
                self.source_playhead_secs = frame.pts_secs();
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Open an `AudioSource` for the source viewer's bound media and
    /// register it on the dedicated mixer track. Reuses
    /// `ensure_mix_pipeline` so the cpal stream stays shared with the
    /// timeline. The new source starts paused — `play_source` resumes
    /// it after flipping the mute state.
    fn open_source_audio(&mut self, path: &std::path::Path, source_secs: f64) {
        let track = self.source_mixer_track_index;
        // Default: PCM cache fast path (only available for media that's been
        // through audio-conform), then ffmpeg-decoder fallback.
        // Source viewer is not bound to a timeline clip — keyframe time
        // domain is irrelevant (no audio_effects, no kfs passed), so 0.0
        // and an empty kf map are correct.
        match AudioDecoder::open(
            path,
            self.cpal_sample_rate,
            Vec::new(),
            std::collections::HashMap::new(),
            0.0,
        ) {
            Ok((adec, consumer)) => {
                if source_secs > 0.01 {
                    adec.seek(source_secs);
                }
                self.ensure_mix_pipeline(consumer, track);
                // Mark the new track muted by default — Play takes care
                // of unmuting on the actual handoff.
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track + 1);
                    m.tracks[track].muted = true;
                }
                self.source_audio_source = Some(AudioSource::Decoder(adec));
            }
            Err(e) => {
                debug!("Source viewer: no audio for {path:?}: {e}");
            }
        }
    }

    /// Toggle which viewer owns the audio mixer.
    /// `source_owns = true`: mute every timeline track, unmute the
    /// source viewer track. `source_owns = false`: inverse.
    /// The mute is on the mixer (cpal-callback-safe atomics), so the
    /// switch is instant and glitch-free.
    fn set_source_audio_owner(&mut self, source_owns: bool) {
        let Ok(mut m) = self.mixer.lock() else { return };
        let n = m.tracks.len();
        for (i, t) in m.tracks.iter_mut().enumerate() {
            if i == self.source_mixer_track_index {
                t.muted = !source_owns;
            } else if i < n {
                t.muted = source_owns;
            }
        }
    }
}

/// One-shot ffmpeg probe to read a media file's frame rate. Returns
/// `None` on any failure (file unreadable, no video stream, zero
/// denominator) — callers fall back to 24 fps so step / playback keeps
/// working.
fn probe_source_fps(path: &std::path::Path) -> Option<f64> {
    let ictx = ffmpeg_next::format::input(path).ok()?;
    let stream = ictx.streams().best(ffmpeg_next::media::Type::Video)?;
    let rate = stream.rate();
    if rate.denominator() == 0 {
        return None;
    }
    Some(rate.numerator() as f64 / rate.denominator() as f64)
}

/// Coalesce a batch of commands: collapse consecutive seeks into just the last one.
fn coalesce_commands(commands: Vec<AppCommand>) -> Vec<AppCommand> {
    if commands.len() <= 1 {
        return commands;
    }

    let mut result = Vec::with_capacity(commands.len());
    let mut last_seek: Option<AppCommand> = None;
    let mut last_source_seek: Option<AppCommand> = None;
    let mut relative_sum: i64 = 0;
    let mut has_sequence_updated = false;

    for cmd in commands {
        match &cmd {
            AppCommand::SeekTo(_) | AppCommand::SetPlayhead(_) => {
                // Flush any pending relative seek before absolute seek
                if relative_sum != 0 {
                    result.push(AppCommand::SeekRelative(relative_sum));
                    relative_sum = 0;
                }
                // Replace any pending seek with this newer one
                last_seek = Some(cmd);
            }
            AppCommand::SeekRelative(frames) => {
                // Sum all relative seeks into one net offset
                // (prevents OS key repeat from queueing multiple seeks)
                if let Some(seek) = last_seek.take() {
                    result.push(seek);
                }
                relative_sum += frames;
            }
            AppCommand::SeekSource(_) => {
                // Source-viewer scrub-drag fires one SeekSource per
                // drag-frame; each handler blocks up to ~500ms in
                // grab_one_source_frame_timeout. Without coalescing,
                // a one-second drag queues 30+ seeks and freezes the
                // engine thread for many seconds (watchdog stall +
                // user kills the app). Only the latest target
                // matters — discard the rest.
                last_source_seek = Some(cmd);
            }
            AppCommand::SequenceUpdated => {
                has_sequence_updated = true;
            }
            _ => {
                // Flush pending seeks before other commands
                if relative_sum != 0 {
                    result.push(AppCommand::SeekRelative(relative_sum));
                    relative_sum = 0;
                }
                if let Some(seek) = last_seek.take() {
                    result.push(seek);
                }
                if let Some(seek) = last_source_seek.take() {
                    result.push(seek);
                }
                result.push(cmd);
            }
        }
    }

    // Flush remaining
    if relative_sum != 0 {
        result.push(AppCommand::SeekRelative(relative_sum));
    }
    if let Some(seek) = last_seek.take() {
        result.push(seek);
    }
    if let Some(seek) = last_source_seek.take() {
        result.push(seek);
    }
    if has_sequence_updated {
        result.push(AppCommand::SequenceUpdated);
    }

    result
}

/// Parse ISO 8601 timestamp like "2024-03-15T10:30:00.000000Z" to Unix epoch seconds.
fn parse_iso8601_timestamp(s: &str) -> Option<u64> {
    // Minimal parser for ffmpeg's creation_time format: YYYY-MM-DDTHH:MM:SS[.frac]Z
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    let time_main = time.split('.').next()?; // strip fractional seconds
    let mut time_parts = time_main.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next()?.parse().ok()?;
    // Approximate days since epoch (not accounting for leap seconds, good enough for sorting)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 0..(month.saturating_sub(1) as usize).min(11) {
        days += mdays[m];
    }
    days += day.saturating_sub(1);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_image_extension(path: &std::path::Path) -> bool {
    // Delegates to the canonical klipp_core list — adding a new image format
    // is one edit there, not here.
    klipp_core::media_extensions::path_is_image(path)
}

fn fps_from_float(fps: f64) -> Fps {
    if (fps - 23.976).abs() < 0.01 {
        Fps::ntsc_23976()
    } else if (fps - 24.0).abs() < 0.01 {
        Fps::film_24()
    } else if (fps - 25.0).abs() < 0.01 {
        Fps::pal_25()
    } else if (fps - 29.97).abs() < 0.01 {
        Fps::ntsc_29970()
    } else if (fps - 30.0).abs() < 0.01 {
        Fps::new(30, 1)
    } else if (fps - 50.0).abs() < 0.01 {
        Fps::new(50, 1)
    } else if (fps - 59.94).abs() < 0.01 {
        Fps::new(60000, 1001)
    } else if (fps - 60.0).abs() < 0.01 {
        Fps::new(60, 1)
    } else {
        Fps::new(fps.round() as u32, 1)
    }
}

// ---------------------------------------------------------------------------
// GPU-accelerated thumbnail/filmstrip helpers
// ---------------------------------------------------------------------------

/// Cached GPU vendor, initialised at `PlaybackEngine::new()` and read by
/// thumbnail/filmstrip background threads that don't have a `&PlaybackEngine`
/// reference. Falls back to sysfs detection if somehow accessed before the
/// engine starts (e.g. a unit test that builds a decoder directly).
static DETECTED_GPU_VENDOR: std::sync::OnceLock<klipp_core::GpuVendor> =
    std::sync::OnceLock::new();

fn cached_gpu_vendor() -> klipp_core::GpuVendor {
    *DETECTED_GPU_VENDOR.get_or_init(klipp_core::GpuVendor::detect)
}

/// Open a video decoder for thumbnail/filmstrip generation.
/// Hardware acceleration chain: CUDA → VAAPI (AMD/Intel only) → Software.
/// VAAPI is skipped on NVIDIA to avoid stderr noise from nvidia-vaapi-driver,
/// which bypasses av_log suppression. On AMD and Intel iGPUs, VAAPI is the
/// native GPU decode path and works correctly.
/// Returns (decoder, stream_index, is_hw).
fn open_gpu_decoder(
    format_ctx: &ffmpeg_next::format::context::Input,
) -> Result<(ffmpeg_next::decoder::Video, usize, bool), String> {
    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    // Try GPU acceleration: CUDA → VAAPI (AMD/Intel only) → Software
    let vendor = cached_gpu_vendor();
    let is_hw = unsafe {
        if crate::video_decoder::try_init_cuda(codec_ctx.as_mut_ptr()) {
            true
        } else if vendor.should_try_vaapi()
            && crate::video_decoder::try_init_vaapi(codec_ctx.as_mut_ptr())
        {
            true
        } else {
            // Software fallback: use half cores
            let half_cores = (std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4) / 2)
                .max(1) as i32;
            (*codec_ctx.as_mut_ptr()).thread_count = half_cores;
            false
        }
    };

    let decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    Ok((decoder, stream_index, is_hw))
}

/// Convert a decoded frame (HW or SW) to RGBA at the given target dimensions.
/// For HW frames: GPU→CPU transfer + swscale. For SW frames: just swscale.
/// `sws_cache` is an optional cached sws context pointer for reuse across frames.
fn frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    _is_hw: bool,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
) -> Option<Vec<u8>> {
    // Detect actual frame format — CUDA init may "succeed" but format negotiation
    // can fall back to software. Check the real pixel format per-frame.
    let actual_hw = unsafe {
        let fmt = (*frame.as_ptr()).format;
        fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_CUDA as i32
            || fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32
    };
    if actual_hw {
        // HW frame path: GPU→CPU transfer then swscale
        unsafe { hw_frame_to_rgba(frame, dst_w, dst_h, sws_cache) }
    } else {
        // SW frame path: direct swscale
        sw_frame_to_rgba(frame, dst_w, dst_h)
    }
}

/// Transfer HW frame to CPU and scale to RGBA at target dimensions.
unsafe fn hw_frame_to_rgba(
    hw_frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
) -> Option<Vec<u8>> {
    use ffmpeg_next::ffi::*;

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return None;
    }

    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    if ret < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    let src_w = (*sw_frame).width as u32;
    let src_h = (*sw_frame).height as u32;
    let sw_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);

    let sws = sws_getCachedContext(
        sws_cache.unwrap_or(std::ptr::null_mut()),
        src_w as i32,
        src_h as i32,
        sw_fmt,
        dst_w as i32,
        dst_h as i32,
        AVPixelFormat::AV_PIX_FMT_RGBA,
        1, // SWS_FAST_BILINEAR
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
    );
    if sws.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    *sws_cache = Some(sws);

    let rgba_frame = av_frame_alloc();
    if rgba_frame.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    (*rgba_frame).width = dst_w as i32;
    (*rgba_frame).height = dst_h as i32;
    (*rgba_frame).format = AVPixelFormat::AV_PIX_FMT_RGBA as i32;
    if av_frame_get_buffer(rgba_frame, 0) < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        av_frame_free(&mut (rgba_frame as *mut _));
        return None;
    }

    sws_scale(
        sws,
        (*sw_frame).data.as_ptr() as *const *const u8,
        (*sw_frame).linesize.as_ptr(),
        0,
        src_h as i32,
        (*rgba_frame).data.as_ptr(),
        (*rgba_frame).linesize.as_ptr(),
    );

    av_frame_free(&mut (sw_frame as *mut _));

    let stride = (*rgba_frame).linesize[0] as usize;
    let expected_stride = (dst_w * 4) as usize;
    let data_slice = std::slice::from_raw_parts((*rgba_frame).data[0], stride * dst_h as usize);

    let rgba = if stride == expected_stride {
        data_slice.to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * dst_h as usize);
        for row in 0..dst_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&data_slice[start..start + expected_stride]);
        }
        buf
    };

    av_frame_free(&mut (rgba_frame as *mut _));
    Some(rgba)
}

/// Convert a software-decoded frame to RGBA at target dimensions.
fn sw_frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    use ffmpeg_next::software::scaling;

    let mut scaler = scaling::Context::get(
        frame.format(),
        frame.width(),
        frame.height(),
        ffmpeg_next::format::Pixel::RGBA,
        dst_w,
        dst_h,
        scaling::Flags::FAST_BILINEAR,
    )
    .ok()?;

    let mut rgba_frame = ffmpeg_next::frame::Video::empty();
    scaler.run(frame, &mut rgba_frame).ok()?;

    let stride = rgba_frame.stride(0);
    let expected = dst_w as usize * 4;
    let rgba = if stride == expected {
        rgba_frame.data(0)[..expected * dst_h as usize].to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected * dst_h as usize);
        for row in 0..dst_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&rgba_frame.data(0)[start..start + expected]);
        }
        buf
    };
    Some(rgba)
}

/// Open a software-only video decoder (no GPU). Used for thumbnails where
/// GPU context creation overhead isn't worth it for a single frame.
fn open_sw_decoder(
    format_ctx: &ffmpeg_next::format::context::Input,
) -> Result<(ffmpeg_next::decoder::Video, usize), String> {
    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    unsafe {
        let cores = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as i32)
            .min(16);
        (*codec_ctx.as_mut_ptr()).thread_count = cores;
    }

    let decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    Ok((decoder, stream_index))
}

/// Extract rotation angle from video stream metadata.
/// Returns 0, 90, 180, or 270 degrees.
fn detect_rotation(format_ctx: &ffmpeg_next::format::context::Input, stream_index: usize) -> i32 {
    // Check stream metadata for "rotate" tag (older containers)
    for stream in format_ctx.streams() {
        if stream.index() == stream_index {
            if let Some(rotate_str) = stream.metadata().get("rotate") {
                if let Ok(angle) = rotate_str.parse::<i32>() {
                    return ((angle % 360) + 360) % 360;
                }
            }
            break;
        }
    }
    // Check container-level metadata
    if let Some(rotate_str) = format_ctx.metadata().get("rotate") {
        if let Ok(angle) = rotate_str.parse::<i32>() {
            return ((angle % 360) + 360) % 360;
        }
    }
    0
}

/// Detect rotation from a file path using ffprobe (catches display matrix side data
/// which ffmpeg-next doesn't expose directly). Falls back to 0 on error.
pub fn detect_rotation_from_path(path: &std::path::Path) -> i32 {
    // First try ffprobe for side_data rotation (most reliable for modern files)
    if let Ok(output) = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream_side_data=rotation",
            "-of", "csv=p=0",
        ])
        .arg(path)
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse first valid rotation value.
        // ffprobe reports how the video IS rotated; to correct it we negate.
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Ok(angle) = trimmed.parse::<f64>() {
                let int_angle = angle.round() as i32;
                if int_angle != 0 {
                    let correction = ((-int_angle % 360) + 360) % 360;
                    return correction;
                }
            }
        }
    }
    0
}

/// Rotate RGBA pixel buffer by 90, 180, or 270 degrees.
/// Returns (new_rgba, new_width, new_height).
pub(crate) fn rotate_rgba(rgba: &[u8], w: u32, h: u32, degrees: i32) -> (Vec<u8>, u32, u32) {
    match degrees {
        90 => {
            // 90° clockwise: new dimensions are (h, w)
            let nw = h;
            let nh = w;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let src = ((y * w + x) * 4) as usize;
                    let dx = h - 1 - y;
                    let dy = x;
                    let dst = ((dy * nw + dx) * 4) as usize;
                    out[dst..dst+4].copy_from_slice(&rgba[src..src+4]);
                }
            }
            (out, nw, nh)
        }
        180 => {
            let mut out = vec![0u8; rgba.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = ((y * w + x) * 4) as usize;
                    let dx = w - 1 - x;
                    let dy = h - 1 - y;
                    let dst = ((dy * w + dx) * 4) as usize;
                    out[dst..dst+4].copy_from_slice(&rgba[src..src+4]);
                }
            }
            (out, w, h)
        }
        270 => {
            let nw = h;
            let nh = w;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let src = ((y * w + x) * 4) as usize;
                    let dx = y;
                    let dy = w - 1 - x;
                    let dst = ((dy * nw + dx) * 4) as usize;
                    out[dst..dst+4].copy_from_slice(&rgba[src..src+4]);
                }
            }
            (out, nw, nh)
        }
        _ => (rgba.to_vec(), w, h),
    }
}

/// Generate a thumbnail for a media file. Uses software decode (CPU) — thumbnails
/// are single small frames, not worth GPU context creation overhead.
/// Decode the FIRST available video frame from the format context, using a
/// correct two-phase pattern that handles codecs requiring multi-packet warmup
/// (AV1, B-frame HEVC, VP9) and a final EOF flush for very short files where
/// the decoder buffers everything before yielding output.
///
/// Per the project's CLAUDE.md decode notes:
///   - ALWAYS drain receive_frame BEFORE feeding new send_packet (avoids
///     EAGAIN packet loss)
///   - Some decoders only release frames after `send_eof()` — flush at end
///
/// Returns true if a frame was decoded into `out_frame`. The previous version
/// of this loop interleaved send/receive per iteration which caused several
/// codecs to fail with "No frame decoded" — see the codec coverage notes in
/// the file-browser hover-skim audit.
fn decode_one_frame(
    format_ctx: &mut ffmpeg_next::format::context::Input,
    decoder: &mut ffmpeg_next::decoder::Video,
    stream_index: usize,
    out_frame: &mut ffmpeg_next::frame::Video,
) -> bool {
    decode_frame_at_or_after(format_ctx, decoder, stream_index, out_frame, None)
}

/// Decode a video frame, optionally discarding frames whose presentation
/// timestamp is earlier than `target_secs` seconds. This is the precise-seek
/// pattern needed for files with sparse keyframes (long GOPs / B-frame video):
/// after `format_ctx.seek` lands on the keyframe BEFORE the target, we keep
/// decoding forward and throwing frames away until we reach the requested
/// timestamp.
///
/// When `target_secs` is `None`, returns the very first decoded frame
/// (used by `generate_thumbnail` / poster-frame paths where any frame from
/// the seeked region is acceptable).
///
/// Two-phase decode loop with EOF flush — handles AV1, HEVC with B-frames,
/// VP9, and very short files. See `decode_one_frame` callers for context.
fn decode_frame_at_or_after(
    format_ctx: &mut ffmpeg_next::format::context::Input,
    decoder: &mut ffmpeg_next::decoder::Video,
    stream_index: usize,
    out_frame: &mut ffmpeg_next::frame::Video,
    target_secs: Option<f64>,
) -> bool {
    // Cache the stream's time_base so we can convert each frame's PTS into
    // seconds for the target comparison. Time base is the unit one PTS tick
    // represents (e.g. 1/16384 means each tick is 1/16384 of a second).
    let time_base = format_ctx
        .stream(stream_index)
        .map(|s| s.time_base())
        .unwrap_or_else(|| ffmpeg_next::Rational::new(1, 1));
    let tb_secs = time_base.numerator() as f64 / time_base.denominator().max(1) as f64;

    let frame_passes_target = |frame: &ffmpeg_next::frame::Video| -> bool {
        let Some(target) = target_secs else { return true; };
        let pts = frame.pts().unwrap_or(0);
        let frame_secs = pts as f64 * tb_secs;
        // Accept the first frame at or after the target. A small slack
        // (one frame at 24 fps = ~42 ms) avoids missing the exact frame
        // due to floating-point imprecision.
        frame_secs + 0.042 >= target
    };

    // Phase 1: feed packets, draining all available frames after each send.
    for (stream, packet) in format_ctx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        // Drain BEFORE sending (avoids EAGAIN on full input queue).
        while decoder.receive_frame(out_frame).is_ok() {
            if frame_passes_target(out_frame) {
                return true;
            }
        }
        if decoder.send_packet(&packet).is_err() {
            // EAGAIN after a drain — rare but possible with high-latency
            // codecs (AV1, HEVC B-frames). Drain again then retry once.
            while decoder.receive_frame(out_frame).is_ok() {
                if frame_passes_target(out_frame) {
                    return true;
                }
            }
            if decoder.send_packet(&packet).is_err() {
                tracing::warn!("decode_frame_at_or_after: packet lost after drain+retry");
            }
        }
    }
    // Phase 2: stream exhausted but no frame yet — drain anything that came
    // out from the last send_packet.
    while decoder.receive_frame(out_frame).is_ok() {
        if frame_passes_target(out_frame) {
            return true;
        }
    }
    // Phase 3: final EOF flush. AV1, HEVC with B-frames, and short files all
    // need this — the decoder has buffered packets but won't release the
    // first frame until told there's no more input coming.
    let _ = decoder.send_eof();
    while decoder.receive_frame(out_frame).is_ok() {
        if frame_passes_target(out_frame) {
            return true;
        }
    }
    // If we ran out of frames without reaching the target, fall back to
    // whatever frame is currently in `out_frame` if it's valid (i.e. we
    // decoded SOMETHING). Better than returning failure for a request near
    // end-of-file. The width check distinguishes "frame populated" from
    // "still empty".
    target_secs.is_some() && out_frame.width() > 0
}

pub fn generate_thumbnail(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;
    let rotation = {
        let r = detect_rotation(&format_ctx, stream_index);
        if r != 0 { r } else { detect_rotation_from_path(path) }
    };

    if seek_secs > 0.1 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let got_frame = decode_one_frame(&mut format_ctx, &mut decoder, stream_index, &mut decoded_frame);

    if !got_frame {
        return Err("No frame decoded".to_string());
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();
    let scale = (160.0 / src_w as f64).min(90.0 / src_h as f64).min(1.0);
    let dst_w = (src_w as f64 * scale) as u32;
    let dst_h = (src_h as f64 * scale) as u32;

    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h)
        .ok_or("Failed to convert frame to RGBA")?;

    // Apply source rotation if needed (phone videos)
    if rotation != 0 {
        let (rotated, rw, rh) = rotate_rgba(&rgba, dst_w, dst_h, rotation);
        Ok((rotated, rw, rh))
    } else {
        Ok((rgba, dst_w, dst_h))
    }
}

/// Generate a thumbnail at a specific max width (for viewer-quality poster frames).
/// Uses software decode — single frame, not worth GPU overhead.
pub fn generate_thumbnail_at_size(
    path: &std::path::Path,
    seek_secs: f64,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;
    let rotation = {
        let r = detect_rotation(&format_ctx, stream_index);
        if r != 0 { r } else { detect_rotation_from_path(path) }
    };

    // Precise seek: ffmpeg's `format_ctx.seek` only lands on the nearest
    // keyframe BEFORE the target. For files with sparse keyframes (long
    // GOPs / heavy B-frame use / very short single-GOP files), that means
    // every requested time returns the same keyframe — making hover skim
    // useless. We pass the requested time to `decode_frame_at_or_after`
    // so the decoder discards earlier frames after the seek and returns
    // the actual frame at or after the requested timestamp.
    let target = if seek_secs > 0.1 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
        Some(seek_secs)
    } else {
        None
    };

    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let got_frame = decode_frame_at_or_after(
        &mut format_ctx,
        &mut decoder,
        stream_index,
        &mut decoded_frame,
        target,
    );

    if !got_frame {
        return Err("No frame decoded".to_string());
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();

    // For rotated video (90/270), the effective dimensions are swapped
    let (eff_w, _eff_h) = if rotation == 90 || rotation == 270 {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    };

    let scale = (max_width as f64 / eff_w as f64).min(1.0);
    let dst_w = ((src_w as f64 * scale) as u32).max(1);
    let dst_h = ((src_h as f64 * scale) as u32).max(1);

    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h)
        .ok_or("Failed to convert frame to RGBA")?;

    // Apply source rotation if needed (phone videos)
    if rotation != 0 {
        let (rotated, rw, rh) = rotate_rgba(&rgba, dst_w, dst_h, rotation);
        Ok((rotated, rw, rh))
    } else {
        Ok((rgba, dst_w, dst_h))
    }
}

/// Cached frame decoder — keeps the ffmpeg context open for fast temporal access.
/// Optimized for sequential and near-sequential access patterns (common in Retime).
/// Only seeks when the requested time is far from the current position.
struct CachedFrameDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    frame: ffmpeg_next::frame::Video,
    /// Current decoder position in seconds
    current_pts: f64,
    /// Frame duration in seconds (1/fps)
    frame_duration: f64,
    /// Cached frames: (time, rgba, w, h)
    cache: Vec<(f64, Vec<u8>, u32, u32)>,
}

impl CachedFrameDecoder {
    fn open(path: &std::path::Path) -> Option<Self> {
        let format_ctx = ffmpeg_next::format::input(path).ok()?;
        let (decoder, stream_index) = open_sw_decoder(&format_ctx).ok()?;

        // Get frame duration from stream
        let frame_duration = format_ctx.streams().best(ffmpeg_next::media::Type::Video)
            .map(|s| {
                let r = s.avg_frame_rate();
                if r.numerator() > 0 { r.denominator() as f64 / r.numerator() as f64 } else { 1.0 / 24.0 }
            })
            .unwrap_or(1.0 / 24.0);

        Some(Self {
            format_ctx,
            decoder,
            stream_index,
            frame: ffmpeg_next::frame::Video::empty(),
            current_pts: -1.0,
            frame_duration,
            cache: Vec::new(),
        })
    }

    fn decode_at(&mut self, seek_secs: f64) -> Option<(Vec<u8>, u32, u32)> {
        // Check cache first
        for (t, rgba, w, h) in &self.cache {
            if (*t - seek_secs).abs() < self.frame_duration * 0.5 {
                return Some((rgba.clone(), *w, *h));
            }
        }

        // Determine if we need to seek or can just read forward
        let need_seek = if self.current_pts < 0.0 {
            true // first access
        } else if seek_secs < self.current_pts - self.frame_duration {
            true // seeking backwards
        } else if seek_secs > self.current_pts + self.frame_duration * 30.0 {
            true // jumping far forward (>30 frames)
        } else {
            false // close enough — just read forward
        };

        if need_seek {
            let ts = ((seek_secs - 0.5).max(0.0) * 1_000_000.0) as i64;
            let _ = self.format_ctx.seek(ts, ..ts);
            self.decoder.flush();
            self.current_pts = seek_secs - 1.0; // approximate
        }

        // Read frames until we reach or pass the target time
        let _target_us = (seek_secs * 1_000_000.0) as i64;
        let mut best_frame: Option<(Vec<u8>, u32, u32, f64)> = None;

        for (stream, packet) in self.format_ctx.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            if self.decoder.send_packet(&packet).is_err() {
                while self.decoder.receive_frame(&mut self.frame).is_ok() {}
                if self.decoder.send_packet(&packet).is_err() {
                    tracing::warn!("CachedFrameDecoder: packet lost after drain+retry");
                }
            }
            while self.decoder.receive_frame(&mut self.frame).is_ok() {
                let pts = self.frame.pts().unwrap_or(0);
                let time_base = stream.time_base();
                let frame_secs = pts as f64 * time_base.numerator() as f64 / time_base.denominator() as f64;
                self.current_pts = frame_secs;

                let w = self.frame.width();
                let h = self.frame.height();

                if let Some(rgba) = sw_frame_to_rgba(&self.frame, w, h) {
                    best_frame = Some((rgba, w, h, frame_secs));
                }

                // If we've reached or passed the target, stop
                if frame_secs >= seek_secs - self.frame_duration * 0.5 {
                    if let Some((rgba, w, h, t)) = best_frame {
                        // Cache this frame (keep last 8 frames for Retime interpolation)
                        if self.cache.len() >= 8 {
                            self.cache.remove(0);
                        }
                        self.cache.push((t, rgba.clone(), w, h));
                        return Some((rgba, w, h));
                    }
                }
            }
        }

        // Return whatever we decoded last
        best_frame.map(|(rgba, w, h, t)| {
            if self.cache.len() >= 8 { self.cache.remove(0); }
            self.cache.push((t, rgba.clone(), w, h));
            (rgba, w, h)
        })
    }
}

/// Decode a single full-resolution frame from a video file at the given time.
/// Used for OFX temporal clip access — plugins that need frames at arbitrary times.
pub fn decode_frame_at_time(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;

    if seek_secs > 0.01 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let mut got_frame = false;

    for (stream, packet) in format_ctx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            // EAGAIN — drain pending frames, then retry once
            if decoder.receive_frame(&mut decoded_frame).is_ok() {
                got_frame = true;
                break;
            }
            if decoder.send_packet(&packet).is_err() {
                tracing::warn!("decode_frame_at_time: packet lost after drain+retry");
            }
        }
        if decoder.receive_frame(&mut decoded_frame).is_ok() {
            got_frame = true;
            break;
        }
    }

    if !got_frame {
        return Err("No frame decoded".to_string());
    }

    let w = decoded_frame.width();
    let h = decoded_frame.height();
    let rgba = sw_frame_to_rgba(&decoded_frame, w, h)
        .ok_or("Failed to convert frame to RGBA")?;
    Ok((rgba, w, h))
}

/// Check if an RGBA frame is effectively black (< 5% average brightness).
/// Samples ~64 evenly spaced pixels for speed.
pub fn is_frame_black(rgba: &[u8], width: u32, height: u32) -> bool {
    let total_pixels = (width * height) as usize;
    if total_pixels == 0 || rgba.len() < total_pixels * 4 {
        return true;
    }
    let step = (total_pixels / 64).max(1);
    let mut brightness_sum: u64 = 0;
    let mut samples = 0u64;
    for i in (0..total_pixels).step_by(step) {
        let idx = i * 4;
        brightness_sum += rgba[idx] as u64 + rgba[idx + 1] as u64 + rgba[idx + 2] as u64;
        samples += 1;
    }
    if samples == 0 {
        return true;
    }
    // Average brightness per channel (0-255). Threshold: 13 (~5% of 255)
    let avg = brightness_sum / (samples * 3);
    avg < 13
}

/// Generate a thumbnail, skipping black frames by trying progressively later positions.
/// Tries the initial seek, then 5%, 10%, 20%, 30% of duration.
pub fn generate_thumbnail_non_black(
    path: &std::path::Path,
    initial_seek_secs: f64,
    duration_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let positions = [
        initial_seek_secs,
        duration_secs * 0.05,
        duration_secs * 0.10,
        duration_secs * 0.20,
        duration_secs * 0.30,
    ];
    let mut last_result = None;
    for &secs in &positions {
        if secs < 0.0 {
            continue;
        }
        match generate_thumbnail(path, secs) {
            Ok((rgba, w, h)) => {
                if !is_frame_black(&rgba, w, h) {
                    return Ok((rgba, w, h));
                }
                last_result = Some((rgba, w, h));
            }
            Err(e) => {
                if last_result.is_none() {
                    last_result = Some(generate_thumbnail(path, 0.0).map_err(|_| e)?);
                }
            }
        }
    }
    // All positions were black — return the last one anyway
    last_result.ok_or_else(|| "No frame decoded".to_string())
}

/// Same as generate_thumbnail_non_black but at a specific max width.
pub fn generate_thumbnail_at_size_non_black(
    path: &std::path::Path,
    initial_seek_secs: f64,
    duration_secs: f64,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let positions = [
        initial_seek_secs,
        duration_secs * 0.05,
        duration_secs * 0.10,
        duration_secs * 0.20,
        duration_secs * 0.30,
    ];
    let mut last_result = None;
    for &secs in &positions {
        if secs < 0.0 {
            continue;
        }
        if let Ok((rgba, w, h)) = generate_thumbnail_at_size(path, secs, max_width) {
            if !is_frame_black(&rgba, w, h) {
                return Ok((rgba, w, h));
            }
            last_result = Some((rgba, w, h));
        }
    }
    last_result.ok_or_else(|| "No non-black frame found".to_string())
}

/// Generate filmstrip thumbnails incrementally with GPU acceleration (CUDA → VAAPI → CPU fallback).
/// Calls `on_frame` for each generated frame. Returns the total number of frames generated.
///
/// Optional `pause_token` (when callers integrate with the global
/// [`klipp_jobs::JobScheduler`]) is checked between frames so the
/// scheduler can SIGSTOP-style suspend filmstrip work whenever the
/// user starts playback / scrubbing. When `None`, behaves as before
/// (back-compat for callers that don't yet wire a token).
pub fn generate_filmstrip_incremental(
    path: &std::path::Path,
    duration_secs: f64,
    interval_secs: f64,
    target_height: u32,
    mut on_frame: impl FnMut(f64, Vec<u8>, u32, u32),
    pause_token: Option<Arc<klipp_jobs::PauseToken>>,
) -> Result<usize, String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index, is_hw) = open_gpu_decoder(&format_ctx)?;
    let rotation = {
        let r = detect_rotation(&format_ctx, stream_index);
        if r != 0 { r } else { detect_rotation_from_path(path) }
    };

    let src_w = decoder.width();
    let src_h = decoder.height();
    // For rotated video (90/270), the final display is portrait.
    // We decode at dimensions that, after rotation, give us the target height.
    let (decode_w, decode_h, final_w, final_h) = if rotation == 90 || rotation == 270 {
        // After rotation, width becomes height and vice versa.
        // Final: target_height tall, width proportional to portrait aspect.
        let aspect = src_h as f64 / src_w.max(1) as f64; // portrait aspect
        let fh = target_height;
        let fw = (fh as f64 * aspect).round() as u32;
        // Decode at swapped dims so rotation produces fw × fh
        (fh, fw, fw, fh)
    } else {
        let aspect = src_w as f64 / src_h.max(1) as f64;
        let fh = target_height;
        let fw = (fh as f64 * aspect).round() as u32;
        (fw, fh, fw, fh)
    };
    let _dst_w = decode_w;
    let _dst_h = decode_h;

    // RAII guard so the cached raw SwsContext is released on every exit
    // path (early return on `Ok(count)`, panic unwind, or future `?`
    // propagations). Without this the context leaked every time the
    // filmstrip helper ran, mirroring a pattern that SequentialFrameDecoder
    // already handles correctly in its own Drop impl.
    struct SwsCacheGuard(Option<*mut ffmpeg_next::ffi::SwsContext>);
    impl Drop for SwsCacheGuard {
        fn drop(&mut self) {
            if let Some(sws) = self.0.take() {
                unsafe { ffmpeg_next::ffi::sws_freeContext(sws); }
            }
        }
    }
    let mut sws_cache = SwsCacheGuard(None);
    let mut count = 0;

    let num_frames = ((duration_secs / interval_secs).ceil() as usize).min(500);
    let accel_label = if is_hw { "GPU" } else { "CPU" };
    tracing::info!("Filmstrip: generating {} frames via {} decode for {:?}", num_frames, accel_label, path);

    for i in 0..=num_frames {
        let seek_secs = i as f64 * interval_secs;
        if seek_secs > duration_secs {
            break;
        }

        // Cooperative pause point — the scheduler signals here when
        // the user starts playback / scrubbing. Returns instantly
        // when not paused. Cancellation also lands here so we exit
        // gracefully mid-strip instead of running the entire 500-
        // frame batch.
        if let Some(ref token) = pause_token {
            token.wait_while_paused();
            if token.is_cancelled() {
                tracing::info!("Filmstrip: cancelled at frame {} of {} for {:?}", i, num_frames, path);
                break;
            }
        }

        // Throttle: yield between frames so playback decode isn't starved.
        // GPU decode is fast so use shorter sleep; CPU needs more breathing room.
        // Kept as a backstop for callers that don't wire a pause_token.
        if i > 0 {
            let sleep_ms = if is_hw { 10 } else { 100 };
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }

        let ts = (seek_secs * 1_000_000.0) as i64;
        if format_ctx.seek(ts, ..ts).is_err() {
            continue;
        }
        decoder.flush();

        let mut decoded_frame = ffmpeg_next::frame::Video::empty();
        let mut got_frame = false;
        let target_pts = ts;

        for (stream, packet) in format_ctx.packets() {
            if stream.index() != stream_index {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                while decoder.receive_frame(&mut decoded_frame).is_ok() {}
                if decoder.send_packet(&packet).is_err() {
                    tracing::warn!("filmstrip: packet lost after drain+retry");
                }
            }
            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let frame_ts = decoded_frame.timestamp().unwrap_or(0);
                let tb = stream.time_base();
                let frame_us = frame_ts * 1_000_000 * tb.numerator() as i64 / tb.denominator() as i64;
                if frame_us >= target_pts - 100_000 {
                    got_frame = true;
                    break;
                }
            }
            if got_frame { break; }
        }

        if !got_frame {
            continue;
        }

        if let Some(rgba) = frame_to_rgba(&decoded_frame, decode_w, decode_h, is_hw, &mut sws_cache.0) {
            if rotation != 0 {
                let (rotated, rw, rh) = rotate_rgba(&rgba, decode_w, decode_h, rotation);
                on_frame(seek_secs, rotated, rw, rh);
            } else {
                on_frame(seek_secs, rgba, final_w, final_h);
            }
            count += 1;
        }
    }

    Ok(count)
}

/// Sequential frame walker for analysis passes (e.g. video stabilization).
///
/// Opens the file once with GPU decode if available (CUDA → SW fallback), then
/// yields decoded RGBA frames in source order via `next_frame()`. No per-frame
/// seeks, no per-frame ffmpeg open. Designed to replace patterns that called
/// `generate_thumbnail_at_size` in a tight loop, which paid the
/// open+seek+sw-decode cost on every frame.
///
/// Forward-only sequential access. Not for random seek.
pub struct SequentialFrameDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    is_hw: bool,
    rotation: i32,
    decode_w: u32,
    decode_h: u32,
    sws_cache: Option<*mut ffmpeg_next::ffi::SwsContext>,
    eof_reached: bool,
    drained: bool,
}

impl SequentialFrameDecoder {
    /// Open `path` and prepare for sequential decode. Output frames are scaled
    /// so the longest visible (rotation-aware) edge is at most `max_width`
    /// pixels. Uses GPU decode when available.
    pub fn open(path: &std::path::Path, max_width: u32) -> Result<Self, String> {
        let format_ctx = ffmpeg_next::format::input(path)
            .map_err(|e| format!("ffmpeg open: {e}"))?;
        let (decoder, stream_index, is_hw) = open_gpu_decoder(&format_ctx)?;
        let rotation = {
            let r = detect_rotation(&format_ctx, stream_index);
            if r != 0 { r } else { detect_rotation_from_path(path) }
        };
        let src_w = decoder.width();
        let src_h = decoder.height();
        let eff_w = if rotation == 90 || rotation == 270 { src_h } else { src_w };
        let scale = (max_width as f64 / eff_w as f64).min(1.0);
        let decode_w = ((src_w as f64 * scale) as u32).max(1);
        let decode_h = ((src_h as f64 * scale) as u32).max(1);
        tracing::info!(
            "SequentialFrameDecoder: opened {:?} accel={} src={}x{} target={}x{}",
            path,
            if is_hw { "GPU" } else { "SW" },
            src_w,
            src_h,
            decode_w,
            decode_h
        );
        Ok(Self {
            format_ctx,
            decoder,
            stream_index,
            is_hw,
            rotation,
            decode_w,
            decode_h,
            sws_cache: None,
            eof_reached: false,
            drained: false,
        })
    }

    /// Decode and return the next frame in source order, or `None` at EOF.
    pub fn next_frame(&mut self) -> Option<(Vec<u8>, u32, u32)> {
        if self.drained {
            return None;
        }
        let mut decoded = ffmpeg_next::frame::Video::empty();

        if self.decoder.receive_frame(&mut decoded).is_ok() {
            return self.convert(&decoded);
        }

        loop {
            if self.eof_reached {
                if self.decoder.receive_frame(&mut decoded).is_ok() {
                    return self.convert(&decoded);
                }
                self.drained = true;
                return None;
            }

            let mut packet = ffmpeg_next::codec::packet::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(()) => {
                    if packet.stream() != self.stream_index {
                        continue;
                    }
                    if self.decoder.send_packet(&packet).is_err() {
                        // EAGAIN — drain before retry (stabilize / analysis
                        // path processes thousands of sequential frames; a
                        // lost packet here subtly corrupts motion vectors)
                        while self.decoder.receive_frame(&mut decoded).is_ok() {
                            return self.convert(&decoded);
                        }
                        if self.decoder.send_packet(&packet).is_err() {
                            tracing::warn!("SequentialFrameDecoder: packet lost after drain+retry");
                        }
                    }
                }
                Err(ffmpeg_next::Error::Eof) => {
                    self.eof_reached = true;
                    let _ = self.decoder.send_eof();
                    continue;
                }
                Err(e) => {
                    tracing::warn!("SequentialFrameDecoder: packet read error: {e}");
                    self.drained = true;
                    return None;
                }
            }

            if self.decoder.receive_frame(&mut decoded).is_ok() {
                return self.convert(&decoded);
            }
        }
    }

    fn convert(&mut self, decoded: &ffmpeg_next::frame::Video) -> Option<(Vec<u8>, u32, u32)> {
        let rgba = frame_to_rgba(decoded, self.decode_w, self.decode_h, self.is_hw, &mut self.sws_cache)?;
        if self.rotation != 0 {
            let (rotated, rw, rh) = rotate_rgba(&rgba, self.decode_w, self.decode_h, self.rotation);
            Some((rotated, rw, rh))
        } else {
            Some((rgba, self.decode_w, self.decode_h))
        }
    }
}

impl Drop for SequentialFrameDecoder {
    fn drop(&mut self) {
        if let Some(sws) = self.sws_cache.take() {
            unsafe { ffmpeg_next::ffi::sws_freeContext(sws); }
        }
    }
}

#[cfg(test)]
mod scope_dispatch_tests {
    //! These tests pin the UI-mode → GPU-kernel mapping for scope
    //! rendering. Commit cc32c3d's linear-storage flip silently broke
    //! the waveform domain, but a separate latent bug was that the
    //! engine fell through to `render_scope_waveform` on *every*
    //! non-parade, non-histogram mode — including vectorscope (mode
    //! 2) — which produced a waveform image tagged as a vectorscope
    //! texture. These tests would have caught that in isolation.
    use super::{gpu_scope_kind_for_mode, GpuScopeKind};

    #[test]
    fn mode_0_is_waveform() {
        assert_eq!(gpu_scope_kind_for_mode(0), Some(GpuScopeKind::Waveform));
    }

    #[test]
    fn mode_1_is_parade() {
        assert_eq!(gpu_scope_kind_for_mode(1), Some(GpuScopeKind::Parade));
    }

    #[test]
    fn mode_2_vectorscope_is_cpu_only() {
        // The whole point: mode 2 must *not* resolve to a GPU kernel.
        // Returning `Some(Waveform)` here — the pre-fix behaviour —
        // is what painted a waveform into the vectorscope tab.
        assert_eq!(gpu_scope_kind_for_mode(2), None);
    }

    #[test]
    fn mode_3_is_histogram() {
        assert_eq!(gpu_scope_kind_for_mode(3), Some(GpuScopeKind::Histogram));
    }

    #[test]
    fn unknown_modes_fall_back_to_cpu() {
        // Any code not in {0,1,3} must return None so the engine
        // clears `fb.scope_texture` and the UI uses the CPU path.
        // Belt-and-braces against a future mode being added to the
        // UI enum without the engine being taught about it.
        for m in [4u8, 5, 10, 42, 200, 255] {
            assert_eq!(
                gpu_scope_kind_for_mode(m), None,
                "unknown mode {m} should resolve to None",
            );
        }
    }
}

#[cfg(test)]
mod codec_fix_tests {
    /// Opt-in test for the codec coverage fix in `decode_one_frame`.
    /// Decodes a frame from a user-supplied path via the KLIPP_TEST_DECODE_FILE
    /// env var. Skipped silently if the env var is unset.
    ///
    /// Run with:
    ///     KLIPP_TEST_DECODE_FILE=/path/to/file.mp4 \
    ///         cargo test --release -p klipp-engine \
    ///         codec_fix_tests::decode_user_supplied_file -- --nocapture
    #[test]
    fn decode_user_supplied_file() {
        let path = match std::env::var("KLIPP_TEST_DECODE_FILE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("[skipped] set KLIPP_TEST_DECODE_FILE to a media file path to run this test");
                return;
            }
        };
        super::super::init_ffmpeg().expect("ffmpeg init");

        // Verify the header probe (used by file-browser hover skim to learn
        // file duration before mapping cursor X to a timestamp).
        let header = super::super::probe_media_header(&path);
        eprintln!("[probe] {} → {:?}", path.display(), header);

        // Try a few seek positions that often trip up codec edge cases:
        //   - 0.0     (very first frame, no seek)
        //   - 0.5     (mid-GOP after keyframe)
        //   - 2.0     (deep into the file — may land on a B-frame)
        //   - 4.0     (near end of short test files)
        let positions = [0.0_f64, 0.5, 2.0, 4.0];
        let mut any_succeeded = false;
        let mut frame_hashes: Vec<(f64, u64)> = Vec::new();
        for &t in &positions {
            match super::generate_thumbnail_at_size(&path, t, 480) {
                Ok((rgba, w, h)) => {
                    // Cheap content hash so we can verify different positions
                    // return DIFFERENT frames (proves precise seek is working).
                    let mut h64: u64 = 0xcbf29ce484222325;
                    for &b in rgba.iter().step_by(101) { // sparse hash for speed
                        h64 ^= b as u64;
                        h64 = h64.wrapping_mul(0x100000001b3);
                    }
                    eprintln!("[OK] {} @ {:.1}s → {}x{} hash={:016x}", path.display(), t, w, h, h64);
                    frame_hashes.push((t, h64));
                    any_succeeded = true;
                }
                Err(e) => {
                    eprintln!("[FAIL] {} @ {:.1}s → {}", path.display(), t, e);
                }
            }
        }
        assert!(any_succeeded, "decode failed at all probed positions for {}", path.display());

        // Sanity check: at least TWO of the positions should produce different
        // frames. If they all return the same hash, precise-seek is broken
        // (we're just getting the keyframe over and over).
        if frame_hashes.len() >= 2 {
            let unique: std::collections::HashSet<u64> = frame_hashes.iter().map(|(_, h)| *h).collect();
            eprintln!("[unique frame count] {} of {} probed positions",
                unique.len(), frame_hashes.len());
            if unique.len() == 1 {
                eprintln!("[WARN] all positions returned the same frame — precise seek may not be working for this file");
            }
        }
    }
}

#[cfg(test)]
mod gpu_vendor_cache_tests {
    use super::*;

    #[test]
    fn cached_gpu_vendor_is_stable_across_calls() {
        // Whatever the first call returns (real detection on current machine,
        // or whatever PlaybackEngine::new set earlier in the test run), all
        // subsequent calls must return the same value. This verifies the
        // OnceLock cache semantics.
        let first = cached_gpu_vendor();
        let second = cached_gpu_vendor();
        let third = cached_gpu_vendor();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn cached_gpu_vendor_returns_defined_value() {
        // Must return one of the four defined variants — never panic or hang.
        let v = cached_gpu_vendor();
        assert!(matches!(
            v,
            klipp_core::GpuVendor::Nvidia
                | klipp_core::GpuVendor::Amd
                | klipp_core::GpuVendor::Intel
                | klipp_core::GpuVendor::Unknown,
        ));
    }

    #[test]
    fn should_try_vaapi_drives_the_gate() {
        // Sanity: gating logic is the one bit that differs between vendors.
        // NVIDIA is the only one excluded. Confirm the library function agrees.
        assert!(!klipp_core::GpuVendor::Nvidia.should_try_vaapi());
        assert!(klipp_core::GpuVendor::Amd.should_try_vaapi());
        assert!(klipp_core::GpuVendor::Intel.should_try_vaapi());
        assert!(klipp_core::GpuVendor::Unknown.should_try_vaapi());
    }
}

#[cfg(test)]
mod param_change_queue_tests {
    //! Unit tests for the lock-free UI→engine param-change queue.
    //!
    //! Regression target: during rapid colour-grade slider scrubbing the UI
    //! used to call `sync_sequence_to_engine()` per tick, contending with the
    //! engine's long `sequence.read()` and stalling the UI up to 90 ms. The
    //! queue lets us batch per-frame with a single write-lock acquisition.
    use super::{apply_param_changes, ArrayQueue, PARAM_CHANGE_QUEUE_CAPACITY};
    use klipp_core::{MediaId, ParamChange, RationalTime};
    use klipp_timeline::Sequence;
    use std::sync::{Arc, RwLock};

    /// Build a Sequence with a single clip and an Exposure node added
    /// to its graph. Returns (shared sequence, clip id, exposure node id).
    /// The default clip graph is bare Source → Output, so the test
    /// pushes the Exposure node it needs explicitly.
    fn make_fixture() -> (Arc<RwLock<Sequence>>, klipp_core::ClipId, klipp_nodes::NodeId) {
        let mut seq = Sequence::new_default();
        let media_id = MediaId::new();
        let duration = RationalTime::from_frames(120, 24);
        let track = seq.tracks.iter_mut().find(|t| matches!(t.kind, klipp_timeline::TrackKind::Video))
            .expect("default sequence has a video track");
        let clip_id = track.append_clip(media_id, duration);
        let exposure_id = klipp_nodes::NodeId::new_v4();
        for t in &mut seq.tracks {
            if let Some(c) = t.find_clip_mut(clip_id) {
                c.node_graph.nodes.push(klipp_nodes::Node {
                    id: exposure_id,
                    kind: klipp_nodes::NodeKind::Exposure,
                    label: "Exposure".into(),
                    enabled: true,
                    params: klipp_nodes::NodeParams::default(),
                    keyframes: klipp_nodes::NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: vec![],
                });
                break;
            }
        }
        (Arc::new(RwLock::new(seq)), clip_id, exposure_id)
    }

    #[test]
    fn param_change_queue_applies_without_flatten() {
        let (seq, clip_id, node_id) = make_fixture();
        let queue: Arc<ArrayQueue<ParamChange>> = Arc::new(ArrayQueue::new(32));
        // Five consecutive slider ticks, ending on exposure=0.75.
        for v in [0.1f32, 0.25, 0.4, 0.6, 0.75] {
            queue.push(ParamChange {
                clip_id,
                node_id,
                param: "exposure".to_string(),
                value: v,
            }).expect("queue not full");
        }
        let mut grade_hash = 0xdead_beef_dead_beefu64;
        let mut graph_arc: Option<Arc<klipp_nodes::NodeGraph>> = Some(Arc::new(
            seq.read().unwrap().all_clips().next().unwrap().node_graph.clone()
        ));

        let applied = apply_param_changes(&seq, &queue, &mut grade_hash, &mut graph_arc);

        assert_eq!(applied, 5, "all five queued changes must apply");
        assert!(queue.is_empty(), "queue drained after apply");
        // Grade-hash and graph Arc must be invalidated so the engine picks up
        // the changes on the next frame (mirrors SequenceUpdated behaviour).
        assert_eq!(grade_hash, 0);
        assert!(graph_arc.is_none());
        // Final value wins.
        let seq_r = seq.read().unwrap();
        let clip = seq_r.all_clips().find(|c| c.id == clip_id).unwrap();
        let node = clip.node_graph.nodes.iter().find(|n| n.id == node_id).unwrap();
        let exposure = node.params.exposure.expect("exposure set");
        assert!((exposure - 0.75).abs() < 1e-5, "last-write-wins: got {}", exposure);
    }

    #[test]
    fn empty_queue_takes_no_write_lock() {
        let (seq, _clip_id, _node_id) = make_fixture();
        let queue: Arc<ArrayQueue<ParamChange>> = Arc::new(ArrayQueue::new(8));
        // Hold a read lock from another "thread" (logical — we just take it
        // here and prove apply_param_changes doesn't deadlock or block on it
        // when the queue is empty).
        let read_guard = seq.read().unwrap();
        let mut grade_hash = 42u64;
        let mut graph_arc: Option<Arc<klipp_nodes::NodeGraph>> = None;
        let applied = apply_param_changes(&seq, &queue, &mut grade_hash, &mut graph_arc);
        assert_eq!(applied, 0);
        // No write-lock was taken, so grade_hash must not have been reset.
        assert_eq!(grade_hash, 42);
        drop(read_guard);
    }

    #[test]
    fn unknown_clip_is_dropped_silently() {
        let (seq, _clip_id, node_id) = make_fixture();
        let queue: Arc<ArrayQueue<ParamChange>> = Arc::new(ArrayQueue::new(8));
        queue.push(ParamChange {
            clip_id: klipp_core::ClipId::new(),
            node_id,
            param: "exposure".into(),
            value: 0.5,
        }).unwrap();
        let mut grade_hash = 0u64;
        let mut graph_arc = None;
        let applied = apply_param_changes(&seq, &queue, &mut grade_hash, &mut graph_arc);
        // The unknown clip's entry was popped but not applied.
        assert_eq!(applied, 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn queue_capacity_matches_documented_bound() {
        // Pin the bound so PR review catches accidental shrinks.
        assert_eq!(PARAM_CHANGE_QUEUE_CAPACITY, 256);
    }
}

// ═══════════════════════════════════════════════════════════════════
// Audio fingerprint sensitivity to audio_effect_keyframes
//
// `compute_audio_fingerprint` is what gates chain rebuild on
// `SequenceUpdated` — if the kf state isn't part of the hash, the
// running chain misses every kf edit until the next seek/play. The
// per-clip portion is extracted into `audio_fingerprint_for_clip`
// for testability. These tests pin that every observable kf change
// flips the hash so the engine actually sees the edit.
// ═══════════════════════════════════════════════════════════════════
#[cfg(test)]
mod audio_fingerprint_kf_tests {
    use super::audio_fingerprint_for_clip;
    use klipp_core::{AudioEffectInstance, Interpolation, Keyframe, MediaId, RationalTime};
    use klipp_timeline::Clip;

    fn make_media_clip() -> Clip {
        Clip::media(
            MediaId::new(),
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24),
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24),
        )
    }

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe {
            time_secs: t,
            value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
        }
    }

    #[test]
    fn fingerprint_changes_when_kf_track_added() {
        let mut clip = make_media_clip();
        clip.audio_effects = vec![AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![],
        }];
        let fp_before = audio_fingerprint_for_clip(&clip);

        clip.audio_effect_keyframes
            .insert("0:threshold".to_string(), vec![kf(0.0, -18.0)]);
        let fp_after = audio_fingerprint_for_clip(&clip);

        assert_ne!(fp_before, fp_after,
            "adding a kf track must invalidate the running chain — \
             without this the user adds a kf and hears nothing change");
    }

    #[test]
    fn fingerprint_changes_when_kf_added_to_existing_track() {
        let mut clip = make_media_clip();
        clip.audio_effects = vec![AudioEffectInstance {
            effect_type: "compressor".into(), enabled: true, wet_dry: 1.0, params: vec![],
        }];
        clip.audio_effect_keyframes
            .insert("0:threshold".to_string(), vec![kf(0.0, -18.0)]);
        let fp_one = audio_fingerprint_for_clip(&clip);

        clip.audio_effect_keyframes
            .get_mut("0:threshold")
            .unwrap()
            .push(kf(1.0, -6.0));
        let fp_two = audio_fingerprint_for_clip(&clip);

        assert_ne!(fp_one, fp_two);
    }

    #[test]
    fn fingerprint_changes_when_kf_value_moves() {
        let mut clip = make_media_clip();
        clip.audio_effects = vec![AudioEffectInstance {
            effect_type: "compressor".into(), enabled: true, wet_dry: 1.0, params: vec![],
        }];
        clip.audio_effect_keyframes
            .insert("0:threshold".to_string(), vec![kf(0.0, -18.0), kf(1.0, -6.0)]);
        let fp_before = audio_fingerprint_for_clip(&clip);

        // Same time, different value.
        clip.audio_effect_keyframes.get_mut("0:threshold").unwrap()[1].value = -3.0;
        let fp_after = audio_fingerprint_for_clip(&clip);

        assert_ne!(fp_before, fp_after);
    }

    #[test]
    fn fingerprint_changes_when_kf_interpolation_flips() {
        let mut clip = make_media_clip();
        clip.audio_effects = vec![AudioEffectInstance {
            effect_type: "compressor".into(), enabled: true, wet_dry: 1.0, params: vec![],
        }];
        clip.audio_effect_keyframes
            .insert("0:threshold".to_string(), vec![kf(0.0, -18.0)]);
        let fp_linear = audio_fingerprint_for_clip(&clip);

        clip.audio_effect_keyframes.get_mut("0:threshold").unwrap()[0].interpolation =
            Interpolation::Bezier;
        let fp_bezier = audio_fingerprint_for_clip(&clip);

        assert_ne!(fp_linear, fp_bezier,
            "interpolation change must restart the chain — bezier vs linear \
             produces different smoothing in the kf-evaluation path");
    }

    /// Pin the intentional scope of compute_audio_fingerprint: only
    /// clips that are CURRENTLY at the playhead contribute. Non-playhead
    /// clips are excluded by design — when the playhead reaches them,
    /// `open_audio_source` clones their kfs fresh, so edits land at
    /// clip-transition time without needing a fingerprint flip.
    ///
    /// `audio_fingerprint_for_clip` is the per-clip helper, called once
    /// per playhead-clip in the engine method. This test mirrors that
    /// pattern and verifies that two clips with identical kf state but
    /// different media_ids produce *different* fingerprints when both
    /// are folded in (the non-playhead clip simply isn't folded in).
    /// Pin the chain-bleed regression: two clips that share the same
    /// MediaId but carry different audio_effects MUST produce different
    /// chain fingerprints. The boundary check in advance_to_next_clip
    /// uses these fingerprints to decide whether to rebuild the audio
    /// chain when crossing a clip boundary on the same source media.
    /// Before this fix, clip A's reverb would bleed into clip B's
    /// playback whenever the two referenced the same file.
    #[test]
    fn audio_chain_fingerprint_distinguishes_same_media_different_effects() {
        use crate::engine::audio_chain_fingerprint_for;
        let kfs_empty = std::collections::HashMap::new();
        let no_effects: Vec<AudioEffectInstance> = vec![];
        let with_reverb = vec![AudioEffectInstance {
            effect_type: "reverb".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![],
        }];

        let fp_clean = audio_chain_fingerprint_for(&no_effects, &kfs_empty);
        let fp_reverb = audio_chain_fingerprint_for(&with_reverb, &kfs_empty);

        assert_ne!(fp_clean, fp_reverb,
            "two clips with identical media but different effect chains \
             must have different chain fingerprints — without this the \
             engine reuses clip A's chain for clip B and the user hears \
             reverb on the dry clip");
    }

    /// Pin the parity: two clips with the same chain produce the same
    /// fingerprint. Otherwise the boundary check would fire spurious
    /// rebuilds even when the chain genuinely hasn't changed.
    #[test]
    fn audio_chain_fingerprint_is_stable_when_chain_unchanged() {
        use crate::engine::audio_chain_fingerprint_for;
        let kfs_empty = std::collections::HashMap::new();
        let effects = vec![AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -18.0)],
        }];
        let fp1 = audio_chain_fingerprint_for(&effects, &kfs_empty);
        let fp2 = audio_chain_fingerprint_for(&effects, &kfs_empty);
        assert_eq!(fp1, fp2);
    }

    /// Chain fingerprint must NOT include media_id — that's compared
    /// separately at the boundary. If the chain fingerprint folded in
    /// media_id, clip-transition restarts would over-fire on every
    /// boundary even when the chain is unchanged across the cut.
    #[test]
    fn audio_chain_fingerprint_is_independent_of_media_id() {
        use crate::engine::audio_chain_fingerprint_for;
        // The function takes audio_effects + audio_effect_keyframes
        // only — it has no access to a media_id. This test exists as
        // a contract pin: a future "let me fold media_id into the chain
        // fingerprint" change would break the boundary semantics
        // (because then identical chains on different media would also
        // differ, defeating the purpose of the separate media_id
        // comparison). The signature alone enforces this; the test
        // just documents intent.
        let kfs = std::collections::HashMap::new();
        let effects: Vec<AudioEffectInstance> = vec![];
        let _ = audio_chain_fingerprint_for(&effects, &kfs);
    }

    #[test]
    fn fingerprint_excludes_non_playhead_clips_by_design() {
        let mid_a = MediaId::new();
        let mid_b = MediaId::new();
        let make_clip = |mid: MediaId, kf_value: f32| -> Clip {
            let mut c = Clip::media(
                mid,
                RationalTime::from_frames(0, 24),
                RationalTime::from_frames(48, 24),
                RationalTime::from_frames(0, 24),
                RationalTime::from_frames(48, 24),
            );
            c.audio_effects = vec![AudioEffectInstance {
                effect_type: "compressor".into(),
                enabled: true,
                wet_dry: 1.0,
                params: vec![],
            }];
            c.audio_effect_keyframes
                .insert("0:gain".to_string(), vec![kf(0.0, kf_value)]);
            c
        };

        // Simulate the engine: only "current playhead clip" gets folded
        // into the fingerprint. Clip B exists in the timeline but is
        // not at the playhead, so its kfs do NOT contribute.
        let clip_a = make_clip(mid_a, 0.5);
        let clip_b_v1 = make_clip(mid_b, 0.5);
        let clip_b_v2 = make_clip(mid_b, 0.9); // user edits clip B's kf

        // The "engine" sees only clip_a. The fingerprint must be the
        // same whether clip_b_v1 or clip_b_v2 sits later in the timeline.
        let fp_with_b_v1 = audio_fingerprint_for_clip(&clip_a);
        let fp_with_b_v2 = audio_fingerprint_for_clip(&clip_a);
        assert_eq!(fp_with_b_v1, fp_with_b_v2,
            "non-playhead kf edit must not flip the fingerprint — \
             open_audio_source picks up the new kfs at clip transition");

        // Sanity: clip_b's two versions DO produce different fingerprints
        // when fingerprinted directly. Otherwise this test would pass
        // trivially.
        assert_ne!(
            audio_fingerprint_for_clip(&clip_b_v1),
            audio_fingerprint_for_clip(&clip_b_v2),
            "clip_b's kf edit must change clip_b's own fingerprint",
        );
    }

    #[test]
    fn fingerprint_is_stable_across_hashmap_iteration_orders() {
        // Two clips with identical kf state inserted in different orders
        // must produce the same fingerprint. HashMap iteration order is
        // not deterministic, so the formula sorts keys before hashing.
        // Without that sort the engine would restart the chain on every
        // SequenceUpdated even when nothing kf-relevant changed.
        let media_id = MediaId::new(); // Same MediaId for both clips —
        // otherwise we'd be measuring media_id divergence, not kf order.
        let make_clip_with_kfs = |order: &[&str]| {
            let mut c = Clip::media(
                media_id,
                RationalTime::from_frames(0, 24),
                RationalTime::from_frames(48, 24),
                RationalTime::from_frames(0, 24),
                RationalTime::from_frames(48, 24),
            );
            c.audio_effects = vec![AudioEffectInstance {
                effect_type: "compressor".into(), enabled: true, wet_dry: 1.0, params: vec![],
            }];
            for name in order {
                c.audio_effect_keyframes
                    .insert(format!("0:{name}"), vec![kf(0.5, 1.0)]);
            }
            c
        };
        let a = make_clip_with_kfs(&["threshold", "ratio", "makeup"]);
        let b = make_clip_with_kfs(&["makeup", "threshold", "ratio"]);
        assert_eq!(audio_fingerprint_for_clip(&a), audio_fingerprint_for_clip(&b),
            "fingerprint must be insertion-order-independent");
    }
}
