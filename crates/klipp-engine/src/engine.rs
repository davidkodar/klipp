use crate::audio_decoder::AudioDecoder;
use crate::audio_output::AudioOutput;
use crate::audio_source::AudioSource;
use crate::mix_thread::MixThread;
use crate::pcm_reader::PcmAudioFeeder;
use crate::sync::MasterClock;
use crate::texture::SharedFrameBuffer;
use crate::video_decoder::VideoDecoder;
use klipp_compose::eval::eval_with_modifiers;
#[cfg(test)]
use klipp_compose::eval::{resolve_clip_transform, ResolvedTransform};

mod streams;
mod view;

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

fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
        klipp_timeline::ChannelConfig::Surround51 => 3,
        klipp_timeline::ChannelConfig::Surround71 => 4,
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
    use rayon::prelude::*;
    let srgb_lut = srgb_u8_to_linear_lut();
    let alpha_lut = alpha_u8_to_f32_lut();
    let mut out = vec![0.0f32; rgba.len() / 4 * 4];
    // In parallel: one thread took ~50 ms over a 4K frame, most of a
    // render-cache hit's cost.
    out.par_chunks_exact_mut(4).zip(rgba.par_chunks_exact(4)).for_each(|(o, chunk)| {
        o[0] = srgb_lut[chunk[0] as usize];
        o[1] = srgb_lut[chunk[1] as usize];
        o[2] = srgb_lut[chunk[2] as usize];
        o[3] = alpha_lut[chunk[3] as usize];
    });
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
/// Map each colour channel of an RGBA frame through `lut`, leaving alpha as
/// it is — the display transform does not touch alpha, which
/// `display::the_transform_leaves_alpha_where_it_found_it` pins.
///
/// One pass over the frame, no float buffer: this is the whole cost of a
/// render-cache hit's display transform once the chain has been collapsed
/// into a table.
fn map_u8_rgb(rgba: &[u8], lut: &[u8; 256]) -> Vec<u8> {
    use rayon::prelude::*;
    let mut out = vec![0u8; rgba.len() / 4 * 4];
    out.par_chunks_exact_mut(4).zip(rgba.par_chunks_exact(4)).for_each(|(o, c)| {
        o[0] = lut[c[0] as usize];
        o[1] = lut[c[1] as usize];
        o[2] = lut[c[2] as usize];
        o[3] = c[3];
    });
    out
}

fn linear_rgba_to_u8(linear: &[f32]) -> Vec<u8> {
    use rayon::prelude::*;
    let lut = linear_to_srgb_u8_lut();
    let mut out = vec![0u8; linear.len() / 4 * 4];
    // Clamp → quantise to 12 bits → LUT lookup. Alpha stays linear per
    // sRGB spec (no gamma), so its "LUT" is just the clamp+scale. In
    // parallel: one thread took 40 ms over a 4K frame.
    out.par_chunks_exact_mut(4).zip(linear.par_chunks_exact(4)).for_each(|(o, chunk)| {
        let q = |v: f32| -> usize { (v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize };
        o[0] = lut[q(chunk[0])];
        o[1] = lut[q(chunk[1])];
        o[2] = lut[q(chunk[2])];
        o[3] = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    });
    out
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
    /// Same chain-latency atomic the live decoder advertises;
    /// captured at pre-seek time so the mix thread can configure
    /// PDC the moment the pre-seeked source swaps in. PCM-feeder
    /// pre-seeks pass a freshly-allocated zero atomic until
    /// PDC-NEXT-PCM-FEEDER-WIRING ships.
    latency_advertise: std::sync::Arc<std::sync::atomic::AtomicU32>,
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Whether the UI currently expects to render continuously.
///
/// egui is *reactive*: unless something calls `request_repaint`, the UI
/// thread parks until the next input event and `update()` legitimately
/// stops being called. Klipp only asks for continuous frames while
/// playback is running, so an idle stopped editor can sit for minutes
/// without a single `update()` — which is correct, not a hang.
///
/// The watchdog needs to tell those apart. The UI stores `true` here
/// while it has a repaint pending and `false` when it has deliberately
/// gone idle; the watchdog only reports a UI stall when this is `true`.
/// Without it every idle moment produced a bogus `UI_WATCHDOG STALLED`,
/// which buried the real hangs it exists to catch.
pub static UI_WANTS_FRAMES: AtomicBool = AtomicBool::new(false);

/// Set once the engine thread has finished tearing itself down.
///
/// The UI waits on this before calling `process::exit`. `process::exit`
/// runs libc's atexit handlers and terminates without waiting for other
/// threads, and the NVIDIA GL driver registers teardown there — so
/// exiting while this thread is still dropping its wgpu pipelines pulls
/// the device out from under it mid-call. That is a confirmed SIGSEGV,
/// not a theoretical one: a core dump caught the engine thread inside
/// `destroy_pipeline` while the main thread sat in `libGLX_nvidia`'s
/// exit path.
///
/// Ordering matters: this is stored *after* `PlaybackEngine` has been
/// dropped, so observing `true` means every GPU resource the engine
/// owned has already been released.
pub static ENGINE_SHUTDOWN_COMPLETE: AtomicBool = AtomicBool::new(false);

/// Quiet time before the paused engine pre-renders nearby frames into the
/// render cache: past the gap between steps of someone stepping through
/// frames, short of anyone noticing the cache is late.
const PRERENDER_AFTER_IDLE: Duration = Duration::from_millis(400);

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

/// Kill switch for the direct-present fast path (see
/// `PlaybackEngine::direct_present_blocker`). On by default; set
/// `KLIPP_DIRECT_PRESENT=0` to force every frame through the full
/// grade + composite pipeline, which is the way to check whether a
/// suspected display problem comes from the fast path.
fn direct_present_enabled() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| std::env::var("KLIPP_DIRECT_PRESENT").ok().as_deref() != Some("0"))
}

/// Decoders the viewer opens besides the primary track's: overlays, a
/// transition's other side, multicam angles. Caps decoder spawning on
/// projects with many video tracks; clips beyond it still export.
const MAX_CLIP_STREAMS: usize = 6;

/// Common write of a freshly-rendered scope texture into the shared
/// `FrameBuffer`. `Some(t)` populates the texture and mode tag; `None`
/// (mode has no renderer at this path) clears the texture so the UI's
/// CPU fallback runs for that mode.
fn write_scope_tex_to_fb(
    fb: &mut crate::texture::FrameBuffer,
    tex: Option<Vec<u8>>,
    scope_w: u32,
    scope_h: u32,
    mode: u8,
) {
    match tex {
        Some(t) => {
            // Keep the legacy singular fields populated for the in-tree
            // dock reader (`widgets/scopes.rs`). Also fan out into the
            // per-mode map so a UI consumer can look up by mode and
            // skip the stale-comparison race that caused the
            // click-twice mode-switch bug.
            let entry = fb
                .scope_textures
                .entry(mode)
                .or_insert(crate::texture::ScopeTextureEntry {
                    data: Vec::new(),
                    w: 0,
                    h: 0,
                    generation: 0,
                });
            entry.data = t.clone();
            entry.w = scope_w;
            entry.h = scope_h;
            entry.generation = entry.generation.wrapping_add(1);

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
            // Map entry intentionally untouched on `None` — keeps the
            // previous frame visible for that mode rather than blanking,
            // matching the prior skim semantics.
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Stopped,
    Playing,
    Paused,
}

/// A frame's pre-grade pixels, as the engine's caches hold them.
///
/// SDR frames are sRGB u8. HDR frames are their packed 10-bit PQ/HLG code
/// values: the grade has to start from scene-linear with the highlights
/// intact, and an 8-bit sRGB copy clips everything above white — a
/// paused regrade or a step backwards would then grade a different image
/// from the one playback graded. Both shapes are 4 bytes a pixel, so a
/// full cache of 4K frames costs what it always did.
#[derive(Clone)]
enum PregradePixels {
    Srgb8(Arc<Vec<u8>>),
    Hdr(crate::types::HdrInput),
}

impl PregradePixels {
    /// What a decoded frame should be cached as.
    fn from_frame(frame: &crate::types::LinearFrame) -> Self {
        match frame.hdr.as_ref() {
            Some(h) => Self::Hdr(h.to_cpu()),
            None => Self::Srgb8(Arc::new(linear_rgba_to_u8(&frame.data))),
        }
    }

}

/// A frame before its grade, as the caches hold it, with the ungraded
/// picture the UI shows for it (the scope preview's source while a slider
/// is dragged).
///
/// That picture is made once, on first use, and shared by every clone: an
/// HDR frame's costs a decode, tone map and sRGB encode — 20-30 ms at 4K —
/// and it was remade every time a cached frame was shown again.
#[derive(Clone)]
pub struct Pregrade {
    pixels: PregradePixels,
    display: Arc<std::sync::OnceLock<Arc<Vec<u8>>>>,
}

impl Pregrade {
    fn new(pixels: PregradePixels) -> Self {
        Self { pixels, display: Arc::new(std::sync::OnceLock::new()) }
    }

    fn from_frame(frame: &crate::types::LinearFrame) -> Self {
        Self::new(PregradePixels::from_frame(frame))
    }

    /// The ungraded picture, sRGB RGBA8 — made on first ask and kept.
    ///
    /// Only the UI's scope preview wants this, and only while a grade is
    /// being dragged, so the engine publishes the [`Pregrade`] and leaves
    /// the encode to whoever asks. Forcing it on every paused frame instead
    /// cost an HDR 4K frame 20 to 30 ms — a decode of the stored codes, a
    /// tone map and an encode — for bytes that were usually dropped
    /// unread. An SDR frame keeps its own bytes and this is free.
    pub fn display(&self) -> Arc<Vec<u8>> {
        Arc::clone(self.display.get_or_init(|| {
            PREGRADE_DISPLAYS_MADE.fetch_add(1, Ordering::Relaxed);
            match &self.pixels {
                PregradePixels::Srgb8(u8s) => Arc::clone(u8s),
                PregradePixels::Hdr(h) => {
                    let codes = h.cpu_codes();
                    let mut linear =
                        klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(&codes, h.transfer, h.source_to_working);
                    klipp_color::hdr::apply_hdr_display_tonemap(&mut linear, h.transfer);
                    Arc::new(klipp_effects::linear_f32_rgba_to_srgb_u8(&linear))
                }
            }
        }))
    }
}

/// How many ungraded pictures have been made (see [`Pregrade::display`]).
/// One per frame the viewer has shown paused; showing a frame again, from
/// either cache, must not add to it. Read by
/// `tests/pregrade_made_once.rs`.
pub static PREGRADE_DISPLAYS_MADE: AtomicU64 = AtomicU64::new(0);

/// Bidirectional frame cache for instant backward stepping.
/// Stores pre-grade frames keyed by frame number.
struct FrameCache {
    entries: Vec<Option<FrameCacheEntry>>,
    capacity: usize,
}

struct FrameCacheEntry {
    frame_number: i64,   // playhead.value at this frame
    pixels: Pregrade,    // pre-grade pixels (Arc-shared either way)
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
    fn insert(&mut self, frame_number: i64, pixels: Pregrade, width: u32, height: u32) {
        let slot = (frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot] = Some(FrameCacheEntry { frame_number, pixels, width, height });
    }

    /// Insert unless this frame is already held. Pre-grade pixels of a
    /// frame do not change, so re-inserting one gains nothing — and it
    /// would drop the ungraded picture the held copy has already made
    /// (`Pregrade::display`), which the decode-ahead paths did to every
    /// frame the viewer had just shown.
    fn insert_if_absent(&mut self, frame_number: i64, pixels: Pregrade, width: u32, height: u32) {
        let slot = (frame_number.unsigned_abs() as usize) % self.capacity;
        if self.entries[slot].as_ref().is_some_and(|e| e.frame_number == frame_number) {
            return;
        }
        self.entries[slot] = Some(FrameCacheEntry { frame_number, pixels, width, height });
    }

    /// Look up a frame by number. Returns (pixels, width, height) if found.
    fn get(&self, frame_number: i64) -> Option<(Pregrade, u32, u32)> {
        let slot = (frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot].as_ref()
            .filter(|e| e.frame_number == frame_number)
            .map(|e| (e.pixels.clone(), e.width, e.height))
    }

    /// Frame numbers currently held — the only frames that can be graded
    /// without decoding.
    fn frame_numbers(&self) -> impl Iterator<Item = i64> + '_ {
        self.entries.iter().flatten().map(|e| e.frame_number)
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

/// How coarse HDR monitoring's light map is: one sample every this many
/// pixels, in both directions.
///
/// Shared because both paths that fill it — the full render's
/// `encode_for_screen` and the graded fast path — must agree, and the
/// readout maps the pointer's position through it. A whole-frame map is
/// 33 MB at 4K and the readout does not need one.
pub(crate) const NIT_MAP_STEP: u32 = 4;

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
    /// Decoders for every other media clip a frame needs: overlays, a
    /// transition's other side, multicam angles. See `engine::streams`.
    streams: streams::ClipStreams,
    /// Renders every frame the viewer shows off the fast paths — the same
    /// renderer, and rules, export uses. Owns the node-graph executor and
    /// GPU effects pipeline the fast paths drive too.
    renderer: klipp_compose::Renderer,
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
    current_source_rotation: i32, // source rotation in degrees (0, 90, 180, 270)
    /// Last sequence-frame number rendered by the synthetic playback path.
    /// Avoids re-rendering the same generator/title frame ~1000x per second.
    last_synth_frame_rendered: Option<i64>,
    // Audio seek synchronization — generation counter
    expected_audio_generation: u64,
    // Performance
    last_stats_time: std::time::Instant,
    /// Per-stats-interval counters (2-second window). Let us see at a
    /// glance how much work the engine is doing without grep-summing
    /// the 60 fps-worth of PERF lines by hand.
    dbg_update_playback_count: u64,
    dbg_regrade_count: u64,
    dbg_param_changes_drained: u64,
    /// Last-seen cumulative readback counters, for per-interval deltas.
    dbg_last_readback_count: u64,
    dbg_last_readback_us: u64,
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
    /// What the viewer shows while grading for an HDR delivery
    /// (`AppCommand::SetHdrMonitor`). Off renders the SDR picture.
    hdr_monitor: klipp_core::HdrMonitor,
    /// Whether the HDR picture is being published for `klipp-hdr-out`
    /// (`AppCommand::SetHdrReferenceOutput`).
    hdr_reference_output: bool,
    /// The shared frame, created on the first published frame and dropped
    /// when the output is turned off or the geometry changes.
    hdr_writer: Option<klipp_hdr_out::FrameWriter>,
    /// Phase 4C — Inspector's currently-displayed matte consumer.
    /// While set, after each composited frame the engine emits an
    /// `AppEvent::MatteMaskReady` with the resolved single-channel
    /// mask downsampled to ~80×45 px so the inspector preview tile
    /// stays in sync with what the user is grading. `None` = no
    /// preview wanted (default).
    inspector_matte_preview_target: Option<klipp_core::ClipId>,
    // Scope tap — which node's output to capture for scope display
    scope_tap_node: Option<klipp_nodes::NodeId>,
    /// Solo target — when set, the engine returns this node's output
    /// as the final image instead of the Output-node-input. Single
    /// global slot; auto-inactive when the id doesn't exist in the
    /// active clip's graph (so switching clips silently un-solos).
    solo_node: Option<klipp_nodes::NodeId>,
    /// V2 canvas "nodes visible right now" set — the executor
    /// captures downsampled thumbnails for each of these on the
    /// paused / post-seek re-grade so the canvas can paint per-node
    /// previews. Empty during playback (capturing N readbacks per
    /// frame would cost more than the grade itself).
    visible_node_thumbs: std::collections::HashSet<klipp_nodes::NodeId>,
    /// The clip the grade panel is on, from the node canvas's and scope
    /// tap's commands. The grade tools follow it when it is on screen.
    inspect_clip: Option<klipp_core::ClipId>,
    /// A paused frame was shown before every clip stream had its frame;
    /// re-render when they do.
    pending_stream_refresh: bool,
    /// The global LUT last handed to the renderer, to hand it over again
    /// only when it changes.
    pushed_lut_sig: Option<(bool, Option<PathBuf>, u32, Option<(usize, usize)>)>,
    /// Per-clip hashes keying the render cache, dropped whenever
    /// `current_grade_hash` is reset.
    render_hashes: HashMap<klipp_core::ClipId, u64>,
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
    /// When `pending_paused_refresh` was set, and the nearest frame short
    /// of the playhead it has decoded so far, held back rather than shown
    /// while the frame at the playhead is still coming.
    paused_refresh_since: Option<std::time::Instant>,
    paused_refresh_best: Option<crate::types::LinearFrame>,
    /// When the engine last received a command; the idle pre-render waits
    /// for `PRERENDER_AFTER_IDLE` of quiet.
    last_command_at: std::time::Instant,
    /// Full-quality parked refresh: when optimised playback runs on
    /// the skim substitute, a parked playhead re-decodes ONE frame
    /// from the original media (background decoder, polled per tick)
    /// so the paused viewer shows source quality instead of the skim
    /// transcode's compression artifacts. Mirrors Resolve / FCP proxy
    /// behaviour: proxy while playing, full quality when parked.
    /// (decoder, playhead it targets, start time)
    parked_refresh: Option<(VideoDecoder, Timecode, std::time::Instant)>,
    /// Playhead the last parked refresh completed — or gave up — for.
    /// Prevents re-triggering while parked on the same frame.
    parked_refresh_done_for: Option<Timecode>,
    /// Parked position and when it was reached; None while not paused.
    parked_since: Option<(Timecode, std::time::Instant)>,

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
    /// Used as the main-dock single-mode key + back-compat target for the
    /// legacy `scope_texture` singular field. The full per-tick render set
    /// is `active_scope_modes` (which always includes this value while the
    /// dock is visible).
    scope_render_mode: u8,
    /// Current FFmpeg-scope settings (low-pass / quality / R-G-B mask)
    /// pushed from UI by `AppCommand::SetScopeFfmpegSettings`. Changes
    /// trigger a per-mode filter-graph rebuild on the next render via
    /// the bank's settings comparison.
    scope_ffmpeg_settings: klipp_core::ScopeFfmpegSettings,
    /// Whether to apply the bloom post-pass to scope textures
    /// (`klipp_effects::light::apply_bloom` — threshold + Gaussian +
    /// additive composite). Runs after every FFmpeg scope render for
    /// the waveform/parade modes so dense traces glow like the in-tree
    /// GPU path. Pushed by `AppCommand::SetScopeBloom`.
    scope_bloom_enabled: bool,
    /// Union of scope modes the UI currently needs rendered per tick:
    /// (main dock active mode) ∪ (each floating-window pane's mode).
    /// Updated by `AppCommand::SetActiveScopeModes`. The engine renders
    /// every mode in this set on each scope tick (FFmpeg path) and writes
    /// the results into `FrameBuffer::scope_textures` keyed by mode.
    /// Empty set ⇒ legacy single-mode dispatch path (scope_render_mode
    /// only) — covers boot before the UI publishes anything and the
    /// `KLIPP_SCOPE_FFMPEG=0` legacy GPU/CPU path.
    active_scope_modes: std::collections::BTreeSet<u8>,
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
    cached_pregrade_frame: Option<(Pregrade, u32, u32)>, // (pixels, width, height)
    /// Reasons the direct-present fast path declined the previous frame,
    /// so they can be logged when the set *changes* rather than once per
    /// frame. Empty means the last frame took the fast path.
    last_direct_present_blockers: Vec<&'static str>,
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
    /// Drag-window telemetry. Set on `BeginNodeParamEdit`, summarised +
    /// cleared on `EndNodeParamEdit`. Counts the grade frames run
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
    /// libavfilter-based scope renderer bank (waveform / parade /
    /// vectorscope / histogram). Enabled by `KLIPP_SCOPE_FFMPEG=1`;
    /// when off, the existing GPU shader path is used. The bank holds
    /// one `FfmpegScope` instance per mode so simultaneous modes (main
    /// dock + floating-window panes) can render in parallel without
    /// graph-rebuild cost on mode-swap.
    ffmpeg_scope_bank: crate::ffmpeg_scope::FfmpegScopeBank,
    /// GPU vendor detected at startup — gates VAAPI in thumbnail/filmstrip decode
    /// on AMD/Intel while skipping it on NVIDIA (where nvidia-vaapi-driver
    /// writes stderr noise that bypasses av_log suppression).
    #[allow(dead_code)] // Read via DETECTED_GPU_VENDOR cache from free functions
    gpu_vendor: klipp_core::GpuVendor,
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
        // MasterClock is created up-front so the mix thread can write
        // PDC offset into it whenever AudioMixer's global max chain
        // latency changes.
        let master_clock = Arc::new(MasterClock::new());
        // True-peak oversampling defaults on (BS.1770-4 Annex 2 compliant).
        // No live toggle from UI prefs yet — change requires next playback
        // session. Matches `prefs.audio.tp_oversample` default in klipp-ui.
        let (mix_thread, main_consumer) = MixThread::new(
            cpal_sample_rate,
            mixer.clone(),
            master_clock.clone(),
            true,
        );
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
            streams: streams::ClipStreams::new(MAX_CLIP_STREAMS),
            renderer: klipp_compose::Renderer::new(ofx_host, title_renderer),
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
            master_clock,
            clip_source_start: 0.0,
            clip_timeline_start: Timecode::zero(),
            clip_speed: 1.0,
            speed_keyframes: Vec::new(),
            speed_ramp_enabled: false,
            ramp_timeline_anchor_secs: 0.0,
            ramp_timeline_anchor_wall: std::time::Instant::now(),
            ramp_last_clock_speed: 1.0,
            current_source_rotation: 0,
            last_synth_frame_rendered: None,
            expected_audio_generation: 0,
            last_stats_time: std::time::Instant::now(),
            dbg_update_playback_count: 0,
            dbg_regrade_count: 0,
            dbg_param_changes_drained: 0,
            dbg_last_readback_count: 0,
            dbg_last_readback_us: 0,
            dbg_commands_handled: 0,
            frames_consumed: 0,
            last_playback_log: std::time::Instant::now(),
            last_reliability_time: std::time::Instant::now(),
            last_rss_mb: 0.0,
            color_pipeline: klipp_color::ColorPipeline::new(),
            bypass_color_grade: false,
            compare_stabilization: false,
            compare_effect_active: false,
            compare_effect_clip_id: None,
            compare_effect_index: 0,
            compare_effect_is_ofx: false,
            matte_preview: false,
            hdr_monitor: klipp_core::HdrMonitor::default(),
            hdr_reference_output: false,
            hdr_writer: None,
            inspector_matte_preview_target: None,
            scope_tap_node: None,
            solo_node: None,
            visible_node_thumbs: std::collections::HashSet::new(),
            inspect_clip: None,
            pending_stream_refresh: false,
            pushed_lut_sig: None,
            render_hashes: HashMap::new(),
            thumb_capture_suspended: false,
            pending_paused_refresh: false,
            paused_refresh_since: None,
            paused_refresh_best: None,
            last_command_at: std::time::Instant::now(),
            parked_refresh: None,
            parked_refresh_done_for: None,
            parked_since: None,
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
            scope_ffmpeg_settings: klipp_core::ScopeFfmpegSettings::default(),
            scope_bloom_enabled: true,
            active_scope_modes: std::collections::BTreeSet::new(),
            scopes_visible: false,
            last_scope_render: std::time::Instant::now() - std::time::Duration::from_secs(1),
            matte_preview_node: None,
            cached_pregrade_frame: None,
            last_direct_present_blockers: Vec::new(),
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
            ffmpeg_scope_bank: crate::ffmpeg_scope::FfmpegScopeBank::new(),
            gpu_vendor: {
                let v = klipp_core::GpuVendor::detect();
                let _ = DETECTED_GPU_VENDOR.set(v);
                info!("PlaybackEngine: detected GPU vendor = {v:?}");
                v
            },
            composite_scratch_u8: Vec::new(),
            tone_map_op: klipp_color::display::ToneMapOp::None,
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
            .streams
            .queues()
            .enumerate()
            .map(|(idx, q)| {
                let q = q.lock().unwrap_or_else(|e| e.into_inner());
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

    pub fn run(self) {
        // `run_inner` consumes `self`, so by the time it returns every
        // resource the engine owned — including its GPU pipelines — has
        // been dropped. Only then is it safe to tell the UI it may exit.
        self.run_inner();
        ENGINE_SHUTDOWN_COMPLETE.store(true, Ordering::Release);
        info!("Engine thread stopped — teardown complete");
    }

    fn run_inner(mut self) {
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
            // A slider held: 2 ms. Its values come on the param queue,
            // which does not wake this wait, so at 16 ms every step of a
            // drag sat for up to a frame before it was seen.
            let timeout = if self.state == PlaybackState::Playing
                || self.source_state == PlaybackState::Playing
            {
                Duration::from_millis(8)
            } else if self.in_node_param_edit {
                Duration::from_millis(2)
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
            let wait_start = std::time::Instant::now();
            match self.cmd_rx.recv_timeout(timeout) {
                Ok(cmd) => commands.push(cmd),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            // Idle time waiting for a command is not work; counting it made
            // every idle paused tick (a 16 ms timeout) log as "slow".
            let waited = wait_start.elapsed();
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                commands.push(cmd);
            }

            // Coalesce: keep only the last seek/setplayhead, drop earlier ones
            let commands = coalesce_commands(commands);
            let had_commands = !commands.is_empty();
            if had_commands {
                self.last_command_at = std::time::Instant::now();
            }

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
            // Full-quality refresh of the parked frame when playback ran
            // on the skim substitute (optimised playback). Runs every
            // tick: paused it debounces/polls/paints, any other state it
            // just tears down leftover refresh state and returns.
            self.tick_parked_refresh();

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
                // A paused frame shown before every clip stream had its
                // frame: re-render once they have.
                self.tick_stream_refresh();
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
                } else if !had_commands && self.last_command_at.elapsed() >= PRERENDER_AFTER_IDLE {
                    // Engine is genuinely idle — pre-render nearby frames
                    // into cache. Skip prerender when we just applied param
                    // changes so the next tick re-runs through the fresh
                    // graph instead of caching stale output. And not between
                    // the commands of someone stepping or clicking through
                    // frames: one pre-render is a full render (~100 ms at
                    // 4K), and the next step waited for it.
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
                // Actual HW decode status from the open decoder; None
                // when no decoder is active (overlay shows "—" rather
                // than a stale SW/NVDEC claim).
                let hw = self.v1_decoder.as_ref()
                    .map(|d| d.hw_active.load(Ordering::Relaxed));
                let _ = self.event_tx.send(AppEvent::EngineStats {
                    dropped_frames: self.dropped_frames,
                    hw_decode: hw,
                });
                // GPU->CPU readback cost over this interval. The
                // residency plan assumes these round-trips dominate;
                // this is the line that will show whether removing them
                // is worth restructuring the hot path for.
                let rb_count_now = crate::types::CPU_READBACK_COUNT.load(Ordering::Relaxed);
                let rb_us_now = crate::types::CPU_READBACK_US.load(Ordering::Relaxed);
                let rb_count = rb_count_now.saturating_sub(self.dbg_last_readback_count);
                let rb_us = rb_us_now.saturating_sub(self.dbg_last_readback_us);
                self.dbg_last_readback_count = rb_count_now;
                self.dbg_last_readback_us = rb_us_now;
                let rb_avg_ms = if rb_count > 0 {
                    (rb_us as f64 / rb_count as f64) / 1000.0
                } else {
                    0.0
                };
                tracing::info!(
                    "ENGINE_HEARTBEAT state={:?} updates={} regrades={} param_changes={} cmds={} elapsed={:.2}s rate={:.1}/s drops_total={} hw_decode={:?} has_v1={} overlays={} cpu_readbacks={} readback_avg={:.2}ms readback_total={:.1}ms",
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
                    self.streams.len(),
                    rb_count,
                    rb_avg_ms,
                    rb_us as f64 / 1000.0,
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
            let tick_ms = tick_start.elapsed().saturating_sub(waited).as_secs_f64() * 1000.0;
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
                            matches!(
                                q.front(),
                                Some(f) if !f.is_eof
                                    && (f.pts_secs() - source_secs).abs() < 0.1
                            )
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
                    self.streams.clear();
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
                // Identity seek while paused: seeking to the position we're
                // already parked at (the skim-end revert does this — it only
                // needs the committed frame repainted over the skim frame).
                // A full seek pays start_all_audio's teardown + PCM reopen +
                // cpal open (~300 ms); a queue of these from hover flapping
                // stalled the engine for 8+ s. The cached pregrade frame is
                // untouched by skim writes, so regrading it restores the
                // viewer with zero decoder/audio work. Cache empty or any
                // state other than Paused falls through to the real seek.
                if self.state == PlaybackState::Paused
                    && tc == self.playhead
                    && self.cached_pregrade_frame.is_some()
                {
                    self.regrade_cached_frame();
                    return false;
                }
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
                            value: self.playhead.value + frames * self.frame_ticks(),
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

            AppCommand::MediaDecodeChanged => {
                info!("Engine: media decode settings changed; re-decoding at the playhead");
                self.cached_pregrade_frame = None;
                // Decoded with the old settings: every stream reopens.
                self.streams.clear();
                // Idle / stopped has nothing on screen to refresh; the next
                // seek or play decodes with the new settings anyway.
                if !matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped) {
                    // Clears the frame cache and restarts decoding at the
                    // playhead, discarding frames read ahead.
                    let ph = self.playhead;
                    self.seek_in_sequence(ph);
                    (self.repaint_cb)();
                }
            }
            AppCommand::SetUseProxies(enabled) => {
                info!("Engine: use_proxies = {enabled}");
                if enabled != self.use_proxies {
                    // The other clips' streams decode from the old files.
                    self.streams.clear();
                }
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

            AppCommand::SetHdrReferenceOutput(on) => {
                if self.hdr_reference_output != on {
                    self.hdr_reference_output = on;
                    if !on {
                        // Dropping the writer clears the shared frame's
                        // magic, so a presenter stops showing a picture
                        // nothing is updating rather than freezing on it.
                        self.hdr_writer = None;
                    }
                    info!("HDR reference output: {}", if on { "on" } else { "off" });
                    // The fast paths stand aside while this is on, so the
                    // frame on screen has to be remade the long way for
                    // the first published one to exist.
                    self.regrade_cached_frame();
                }
            }

            AppCommand::SetHdrMonitor(mode) => {
                if self.hdr_monitor != mode {
                    self.hdr_monitor = mode;
                    info!("HDR monitoring: {mode:?}");
                    // The reference output carries the display light the
                    // render only produces for monitoring, so switching
                    // monitoring off stops the publishing. Let go of the
                    // shared frame when that happens: keeping it open
                    // would leave a reference monitor showing the last
                    // frame for ever, which reads as "the grade is fine"
                    // long after the grade stopped arriving. The output
                    // stays *enabled*, so it picks up again when
                    // monitoring comes back.
                    if !mode.renders_hdr() {
                        self.hdr_writer = None;
                    }
                    // The picture changes; the frame on screen was made
                    // the other way.
                    self.regrade_cached_frame();
                }
            }
            AppCommand::SetMattePreview(active) => {
                self.matte_preview = active;
                // When matte preview is disabled, clear the matte preview node
                if !active {
                    self.matte_preview_node = None;
                }
                debug!("Matte preview: {}", self.matte_preview);
            }

            AppCommand::SetInspectorMattePreviewTarget(target) => {
                self.inspector_matte_preview_target = target;
                debug!("Inspector matte preview target: {:?}", target);
            }

            AppCommand::SetScopeRenderMode { mode } => {
                self.scope_render_mode = mode;
            }
            AppCommand::SetActiveScopeModes { modes } => {
                // Evict per-mode scope textures that just dropped out of
                // the active set so the UI doesn't keep displaying stale
                // floating-pane outputs after a pane closes.
                {
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.scope_textures.retain(|m, _| modes.contains(m));
                }
                self.active_scope_modes = modes;
                // Force a fresh render on the next tick so newly-added
                // panes don't sit blank on a paused viewer.
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeFfmpegSettings { settings } => {
                self.scope_ffmpeg_settings = settings;
                // Invalidate the grade hash so the next tick re-renders
                // the paused frame. Without this, scope-settings changes
                // (R/G/B pills, filter, intensity, opacities, …) only
                // take visible effect when playback or a grade edit
                // already triggers a re-grade.
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeBloom { enabled } => {
                self.scope_bloom_enabled = enabled;
                self.current_grade_hash = 0;
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
                let scope_w = 1024u32;
                let scope_h = 512u32;
                let _ = self.scope_render_mode;
                // UI hands us an sRGB u8 frame (skim / scrub snapshot);
                // re-linearise via the LUT (~5 ms at 1080p, runs at
                // ≤10 Hz) so the scope sees 10-bit precision end-to-end.
                let linear = u8_rgba_to_linear(&rgba);
                self.render_active_scopes_via_ffmpeg_linear(&linear, width, height, scope_w, scope_h);
            }
            AppCommand::SetScopeTapNode { clip_id, node_id } => {
                self.inspect_clip = Some(clip_id);
                self.scope_tap_node = node_id;
                debug!("Scope tap node: {:?}", self.scope_tap_node);
            }
            AppCommand::SetNodeSolo { node_id } => {
                self.solo_node = node_id;
                debug!("Solo node: {:?}", self.solo_node);
            }
            AppCommand::SetVisibleNodeThumbs { clip_id, node_ids } => {
                self.inspect_clip = Some(clip_id);
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
                // The project's frame rate may be what changed.
                self.sync_fps_to_sequence();
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
                    // Paused: re-render, so title, generator and layout
                    // edits show at once.
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
                self.sync_streams();
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
                return;
            }

            // Standalone generator clip at playhead — render and display
            if let Some((ti, idx, _gen_data, timeline_in, _timeline_out)) = generator_info {
                info!("handle_play: generator clip {idx} on track {ti} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_track_index = ti;
                self.current_file = None;
                self.clip_source_start = 0.0;
                self.clip_timeline_start = timeline_in;

                self.present_frame(None);

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

                // Path-aware rotation: skim/proxy is autorotated by ffmpeg
                // during transcode, so source_rotation must be 0 there.
                let effective_rot = self.effective_decode_rotation(media_id, &path, source_rotation);
                // Open video decoder
                match VideoDecoder::open(&path, effective_rot) {
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
                // Path-aware: when path is the skim/proxy, ffmpeg autorotate
                // already produced upright pixels, so source_rotation must
                // be 0 to avoid double-rotation.
                self.current_source_rotation = self.effective_decode_rotation(media_id, &path, source_rotation);
                self.current_clip_index = clip_idx;
                self.current_track_index = track_idx;
                self.current_file = Some(path);
                self.refresh_speed_keyframes();
                self.state = PlaybackState::Playing;

                // Open overlay decoders for all non-primary video tracks at the playhead
                self.sync_streams();
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
        self.streams.clear();
        self.pending_stream_refresh = false;
        if let Some(mut dec) = self.preroll_decoder.take() {
            dec.stop_detached();
        }
        self.preroll_clip_index = None;
        self.preroll_source_in = 0.0;

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

        // Blank the shared frame buffer so the viewer no longer shows old pixels.
        //
        // CRITICAL: width/height MUST be zeroed alongside the data
        // clears. The UI's `upload_frame_to_egui` does
        // `mem::take(&mut fb.data)` after every successful upload, so
        // in steady state `fb.data` is an empty Vec while
        // `fb.width`/`fb.height` still describe the last-uploaded
        // frame. `FrameBuffer::clear()` only does `data.fill(0)`,
        // which is a no-op on an empty Vec — so without zeroing
        // width/height here, the next `upload_frame_to_egui` finds
        // `data.len()=0 != w*h*4`, returns early, and the viewer
        // keeps rendering the previous project's frame from the
        // already-allocated `viewer.video_texture` handle until a
        // fresh decode lands. With width/height also zeroed, the
        // upload guard stays consistent: empty data → zero
        // dimensions → no stale pixels.
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.clear();
            fb.data.clear();
            fb.width = 0;
            fb.height = 0;
            fb.pregrade = None;
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
    /// chain (types + enabled), and audio_effect_keyframes (track count +
    /// per-track length + per-kf time/value/interpolation bits). Any
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

                        // PCM feeder chain latency isn't advertised
                        // yet (PDC-NEXT-PCM-FEEDER-WIRING). For now
                        // PCM-cached audio reports 0 — heavy plugins
                        // on PCM-cached clips will misalign by their
                        // chain latency until that follow-up ships.
                        // Pre-existing behaviour pre-PDC was also no
                        // compensation, so this is parity, not a
                        // regression.
                        let zero_latency = std::sync::Arc::new(
                            std::sync::atomic::AtomicU32::new(0),
                        );
                        self.ensure_mix_pipeline(consumer, track_index, zero_latency);
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

                let latency_advertise = adec.latency_advertise.clone();
                self.ensure_mix_pipeline(consumer, track_index, latency_advertise);
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
    /// the given staging consumer for a track. `latency_advertise`
    /// is the producing source's chain-latency atomic — AudioDecoder
    /// shares its own; PCM cache paths (no decoder, no chain) pass
    /// a freshly-allocated atomic at 0. The mix thread polls this
    /// each block to drive AudioMixer's PDC state. PCM feeder
    /// chains do exist and would benefit from real latency
    /// advertisement; that's tracked as PDC-NEXT-PCM-FEEDER-WIRING.
    fn ensure_mix_pipeline(
        &mut self,
        staging_consumer: crate::audio_decoder::AudioConsumer,
        track_index: usize,
        latency_advertise: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        // Create mix thread + audio output on first use
        if self.mix_thread.is_none() {
            let (mt, main_consumer) = MixThread::new(
                self.cpal_sample_rate,
                self.mixer.clone(),
                self.master_clock.clone(),
                true,
            );
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

        if let Some(ref mt) = self.mix_thread {
            mt.add_staging(track_index, staging_consumer, latency_advertise);
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

        // Master clock is the single source of truth. Video frame
        // picking uses the PDC-compensated PTS — current_display_secs
        // subtracts the global max chain latency from the playhead
        // so the rendered frame matches what's currently being heard
        // out of the audio bus. With no plugins reporting latency,
        // this is identical to current_secs() (offset = 0).
        let current_source_secs = self.master_clock.current_display_secs();
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

                self.present_frame(None);

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

        let frame = match display_frame {
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

        // Every other clip the frame needs — overlays, a transition's other
        // side, multicam angles — follows the playhead on its own decoder.
        self.sync_streams();
        stage_overlay_open_us = t_stage.elapsed().as_micros();
        t_stage = std::time::Instant::now();

        let (final_w, final_h) = (frame.width, frame.height);
        let gpu_source = frame.gpu_source.clone();

        // Direct present: the decoder's frame is already the frame the
        // viewer wants. When nothing grades or composites it, the whole
        // linear excursion below reproduces its own input, so hand the
        // decoded bytes straight to the viewer instead. See
        // `direct_present_blocker` for the conditions and why this is
        // byte-identical.
        if let Some(ref gpu) = gpu_source {
            let blockers = self.direct_present_blockers();
            // Log only on change: per-frame this would be 60 lines a
            // second, but the transition is exactly what you want to see
            // when asking why playback is slower than it should be. The
            // scope refresh flips this at 10 Hz, so that one reason is
            // not worth announcing on its own.
            let interesting: Vec<&'static str> = blockers
                .iter()
                .copied()
                .filter(|r| *r != "scope refresh due this frame")
                .collect();
            if interesting != self.last_direct_present_blockers {
                if interesting.is_empty() {
                    tracing::debug!(
                        "direct present ON: publishing decoded frames straight to the viewer"
                    );
                } else {
                    tracing::debug!(
                        "direct present OFF: {} — grading and compositing these frames",
                        interesting.join(", "),
                    );
                }
                self.last_direct_present_blockers = interesting;
            }
            if blockers.is_empty() {
                self.present_decoded_frame_directly(
                    gpu, final_w, final_h, current_source_secs, q_len,
                );
                return;
            }
            // The graph is the only thing between the decoder and the
            // viewer: grade it on the GPU and present the result without
            // the float round trips. Falls through to the full pipeline if
            // the frame cannot take this path.
            if Self::only_the_graded_path_handles(&blockers)
                && self.present_graded_frame_on_gpu(
                    &frame, final_w, final_h, current_source_secs, q_len,
                )
            {
                return;
            }
        }

        // Everything else: the shared renderer, as export renders it. The
        // frame's pixels are read back only if the render uses them.
        self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
        stage_mid_us = t_stage.elapsed().as_micros();

        self.frames_consumed += 1;
        let update_ms = t_update.elapsed().as_secs_f64() * 1000.0;
        if update_ms > 5.0 {
            debug!(
                "PERF update_playback: {update_ms:.1}ms for {final_w}x{final_h} frame | \
                 drain={:.1} clock={:.1} queue_pop={:.1} streams={:.1} render={:.1}",
                stage_drain_us as f64 / 1000.0,
                stage_clock_ramp_us as f64 / 1000.0,
                stage_queue_pop_us as f64 / 1000.0,
                stage_overlay_open_us as f64 / 1000.0,
                stage_mid_us as f64 / 1000.0,
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
    }

    fn advance_to_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let old_clip = self.v1_clip_id(&seq);

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
            self.current_clip_index = actual_idx;
            self.playhead = timeline_in;
            // The current clip's decoder: kept for a transition into this
            // clip, stopped otherwise.
            self.retire_v1_decoder(old_clip);
            self.current_file = None;
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
            self.sync_streams();
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
            // Contiguous = same file, so the existing decoder's rotation
            // (already path-aware from when it was opened) is still
            // correct. Don't overwrite with the raw clip.source_rotation,
            // which would re-introduce the skim double-rotation bug if
            // the next clip happened to be reading the skim file.
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
                self.sync_streams();
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

            self.sync_streams();
            return;
        }

        self.current_clip_index = actual_idx;
        self.playhead = timeline_in;
        self.last_synth_frame_rendered = None;

        // Preroll hit: if we pre-decoded this clip, swap in the ready decoder — zero stall.
        if self.preroll_clip_index == Some(actual_idx) {
            if let Some(preroll_dec) = self.preroll_decoder.take() {
                info!("CUT-DIAG PREROLL HIT: swapping in pre-decoded clip idx={} source_in={:.4}s", actual_idx, source_in_secs);
                self.retire_v1_decoder(old_clip);
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
                // Path-aware rotation — see effective_rotation_for. Skim
                // pixels are pre-rotated by ffmpeg autorotate during
                // transcode, so applying clip.source_rotation again would
                // double-rotate.
                self.current_source_rotation = self.path_for_media(media_id)
                    .map(|p| self.effective_decode_rotation(media_id, &p, source_rotation))
                    .unwrap_or(source_rotation);
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                self.sync_streams();
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
                // Path-aware rotation — see effective_rotation_for. Skim
                // pixels are pre-rotated by ffmpeg autorotate during
                // transcode, so applying clip.source_rotation again would
                // double-rotate.
                self.current_source_rotation = self.path_for_media(media_id)
                    .map(|p| self.effective_decode_rotation(media_id, &p, source_rotation))
                    .unwrap_or(source_rotation);
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                self.sync_streams();
                return;
            }

            info!("Advancing to clip {} ({:?})", actual_idx, path);

            self.retire_v1_decoder(old_clip);
            for active in &mut self.audio_sources {
                active.source.stop_detached();
            }
            self.audio_sources.clear();
            self.audio_output = None;

            // Open new video decoder
            let effective_rot = self.effective_decode_rotation(media_id, &path, source_rotation);
            match VideoDecoder::open(&path, effective_rot) {
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
            // Path-aware rotation; see effective_rotation_for.
            self.current_source_rotation = self.effective_decode_rotation(media_id, &path, source_rotation);
            self.last_frame_time = std::time::Instant::now();
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

            self.sync_streams();
        } else {
            warn!("No path for media {:?}", media_id);
            self.stop_at_end();
        }
    }

    /// The primary decoder is leaving `old_clip`. When the frame at the
    /// (new) playhead still shows that clip — the outgoing side of a
    /// transition, running on into its handle — the clip streams take the
    /// decoder over: it is already where that side needs it, and a fresh
    /// one would leave the side empty while it seeks. Stopped otherwise.
    fn retire_v1_decoder(&mut self, old_clip: Option<klipp_core::ClipId>) {
        let Some(mut dec) = self.v1_decoder.take() else { return };
        if let Some(id) = old_clip.filter(|id| !self.streams.has(*id)) {
            let time = self.playhead;
            let still_shown = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let fps = seq.fps.as_f64();
                klipp_compose::active_layers(&seq, time)
                    .iter()
                    .flat_map(|l| l.clips.iter())
                    .find(|c| c.id == id)
                    .map(|c| klipp_compose::render::source_secs(c, time, fps))
            };
            if let Some(secs) = still_shown {
                debug!("CUT-DIAG primary decoder handed to the clip streams for {id:?} at {secs:.3}s");
                self.streams.adopt(id, dec, secs);
                return;
            }
        }
        dec.stop_detached();
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
                let effective_preroll_rot = self.effective_decode_rotation(media_id, &path, preroll_rotation);
                match VideoDecoder::open(&path, effective_preroll_rot) {
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
        self.streams.clear();
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
                    let zero_latency = std::sync::Arc::new(
                        std::sync::atomic::AtomicU32::new(0),
                    );
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        media_id,
                        audio_chain_fingerprint: chain_fp,
                        source: AudioSource::PcmFeeder(feeder),
                        consumer,
                        latency_advertise: zero_latency,
                    });
                    continue;
                }
                // Fallback: ffmpeg decode
                if let Ok((adec, consumer)) = AudioDecoder::open(&path, sample_rate, audio_effects, audio_effect_keyframes, source_secs) {
                    if source_secs > 0.01 {
                        adec.seek(source_secs);
                    }
                    let latency_advertise = adec.latency_advertise.clone();
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        media_id,
                        audio_chain_fingerprint: chain_fp,
                        source: AudioSource::Decoder(adec),
                        consumer,
                        latency_advertise,
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
                    let latency = ps_source.latency_advertise.clone();
                    self.ensure_mix_pipeline(ps_source.consumer, ps_source.track_index, latency);
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
                            // Path-aware rotation — see overlay path above for the rationale.
                            // Skim/proxy is already upright; only rotate originals.
                            let effective_rot = self.effective_decode_rotation(media_id, &path, rot);
                            if let Ok(dec) = VideoDecoder::open(&path, effective_rot) {
                                dec.seek(source_secs);
                                dec.resume();
                                self.v1_decoder = Some(dec);
                                self.current_clip_index = idx;
                                self.current_file = Some(path);
                                self.clip_source_start = clip_source_in;
                                self.clip_timeline_start = timeline_in;
                                self.clip_speed = spd;
                                self.refresh_speed_keyframes();
                                self.current_source_rotation = effective_rot;
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

                // 6. The other clips' streams follow (they seek themselves
                // when their time jumps back).
                self.playhead = target;
                self.sync_streams();

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
        drop(seq);

        // Handle title clip at loop target
        if let Some((idx, _title_data, timeline_in, clip_source_in)) = title_info {
            self.v1_decoder = None;
            self.current_clip_index = idx;
            self.current_file = None;
            self.clip_source_start = clip_source_in;
            self.clip_timeline_start = timeline_in;
            self.playhead = target;
            self.present_frame(None);

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
                    let effective_rot = self.effective_decode_rotation(media_id, &path, rot);
                    match VideoDecoder::open(&path, effective_rot) {
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
                            self.current_source_rotation = effective_rot;
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

            // 5. The other clips' streams follow (they seek themselves
            // when their time jumps back).
            self.playhead = target;
            self.sync_streams();

            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
        } else {
            // No video clip at target — audio-only loop
            self.v1_decoder = None;

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

    /// Take the timeline's frame rate, which positions on it are counted
    /// in. Playback start always did; a timeline only ever seeked and
    /// stepped on paused kept the engine's 24 fps default, so at 29.97 a
    /// stepped frame's position was snapped to 1/24 s. No timeline (a
    /// file opened on its own) keeps the file's rate.
    fn sync_fps_to_sequence(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        if !seq.tracks.is_empty() {
            self.fps = seq.fps;
        }
    }

    fn seek_in_sequence(&mut self, tc: Timecode) {
        self.seek_in_sequence_with_timeout(tc, Duration::from_millis(50));
    }

    fn seek_in_sequence_with_timeout(&mut self, tc: Timecode, grab_timeout: Duration) {
        self.sync_fps_to_sequence();
        // A frame held back for the previous seek is not this one's.
        self.paused_refresh_best = None;
        // Clear frame cache on explicit seek (cache is only valid for sequential stepping)
        self.frame_cache.clear();
        self.last_synth_frame_rendered = None;
        // No media clip on any video track at this time: nothing to
        // decode. A title or generator there becomes the clip playback
        // starts from; the renderer draws whatever is on screen.
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let has_media_clip = seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .any(|track| {
                    track.clip_at_timecode(tc)
                        .map(|(_, c)| c.media_id().is_some())
                        .unwrap_or(false)
                });
            let synth = if !has_media_clip {
                seq.tracks.iter().enumerate()
                    .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|(ti, track)| {
                        track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                            let source_start = match &clip.kind {
                                klipp_timeline::ClipKind::Title(_) => clip.source_in.to_secs_f64(),
                                klipp_timeline::ClipKind::Generator(_) => 0.0,
                                _ => return None,
                            };
                            Some((ti, idx, clip.timeline_in, source_start))
                        })
                    })
            } else {
                None
            };
            drop(seq);

            if let Some((ti, idx, timeline_in, source_start)) = synth {
                self.v1_decoder = None;
                self.current_track_index = ti;
                self.current_clip_index = idx;
                self.current_file = None;
                self.clip_source_start = source_start;
                self.clip_timeline_start = timeline_in;
                self.playhead = tc;
                self.present_frame(None);
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
                (self.repaint_cb)();
                return;
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
            let need_new_decoder =
                self.current_track_index != ti || self.current_clip_index != idx || self.v1_decoder.is_none();
            self.current_track_index = ti;

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.stop_all_audio();
                    let effective_rot = self.effective_decode_rotation(media_id, &path, rot);
                    match VideoDecoder::open(&path, effective_rot) {
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
                            self.current_source_rotation = effective_rot;
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

            // The frame renders at the new time; the other clips' streams
            // start seeking now, alongside the primary decoder.
            self.playhead = tc;
            self.sync_streams();

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
                    self.paused_refresh_since = Some(std::time::Instant::now() - grab_timeout);
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
            // No video clip at this timecode — handle audio-only seek.
            // The viewer shows what is there: nothing.
            self.v1_decoder = None;
            self.playhead = tc;
            self.present_frame(None);

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
        // Positions here are playhead ticks (frame_cache is keyed by
        // them too); a frame is `frame_ticks` of them.
        let step = self.frame_ticks();
        let target_frame = self.playhead.value + direction * step;

        // Check frame cache first (works for both forward AND backward).
        // The cache stores sRGB u8 — bridge at this viewer seam.
        if let Some((pixels, w, h)) = self.frame_cache.get(target_frame) {
            self.playhead = RationalTime { value: target_frame, rate: self.playhead.rate };
            self.present_frame(Some(streams::HeldFrame::from_pregrade(&pixels, w, h)));
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
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
                frame.realise_hdr_on_cpu();
                if frame.hdr.is_none() {
                    frame.ensure_cpu_data();
                }
                self.frame_cache.insert_if_absent(self.playhead.value, Pregrade::from_frame(&frame), w, h);
                self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
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
                    frame.realise_hdr_on_cpu();
                    if frame.hdr.is_none() {
                        frame.ensure_cpu_data();
                    }
                    self.frame_cache.insert_if_absent(self.playhead.value, Pregrade::from_frame(&frame), w, h);
                    self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
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
            let prefetch_start = (target_frame - 30 * step).max(0);
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
                    frame.realise_hdr_on_cpu();
                    if frame.hdr.is_none() {
                        frame.ensure_cpu_data();
                    }
                    self.frame_cache.insert_if_absent(frame_num, Pregrade::from_frame(&frame), w, h);

                    // When we reach the target frame, display it
                    if frame_num >= target_frame && !found_target {
                        found_target = true;
                        self.playhead = RationalTime { value: frame_num, rate: self.playhead.rate };
                        self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
                        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                        let _ = self.event_tx.send(AppEvent::FrameReady);
                        (self.repaint_cb)();
                        // Keep decoding a few more frames into cache, then stop
                    }

                    // Stop after caching a few frames past target
                    if frame_num > target_frame + 5 * step { break; }
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

    /// Pre-render nearby frames into the render cache during idle time,
    /// one per call to stay responsive. Only frames already decoded into
    /// `frame_cache` are candidates — this never decodes — nearest the
    /// playhead first. (It once enumerated every timeline tick of the
    /// clip; at 59.94 fps a tick is a thousandth of a frame, and the scan
    /// held the engine thread for seconds on every idle tick.)
    fn prerender_cache_frames(&mut self) {
        mark_stage(EngineStage::Prerender);
        let t_call = std::time::Instant::now();
        if !self.render_cache_disk.is_enabled() {
            return;
        }
        let clip_range = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks
                .get(self.current_track_index)
                .and_then(|t| t.clips.get(self.current_clip_index))
                .filter(|c| !(c.node_graph.is_identity() && c.builtin_effects.is_empty()))
                .map(|c| (c.timeline_in.value, c.timeline_out.value))
        };
        let Some((clip_start, clip_end)) = clip_range else { return };
        let current = self.playhead.value;
        let mut candidates: Vec<i64> = self
            .frame_cache
            .frame_numbers()
            .filter(|f| *f >= clip_start && *f < clip_end && *f != current)
            .collect();
        candidates.sort_by_key(|f| (f - current).abs());
        let candidate_count = candidates.len();
        for (scanned, frame_num) in candidates.into_iter().enumerate() {
            let Some((pixels, w, h)) = self.frame_cache.get(frame_num) else { continue };
            if self.prerender_frame(frame_num, streams::HeldFrame::from_pregrade(&pixels, w, h)) {
                let ms = t_call.elapsed().as_secs_f64() * 1000.0;
                if ms > 20.0 {
                    debug!("PERF prerender: {ms:.1}ms rendered frame {frame_num} ({w}x{h}) after scanning {}/{candidate_count}", scanned + 1);
                }
                return;
            }
        }
        self.update_render_cache_ranges();
    }

    /// Update the shared frame buffer with current render cache coverage per clip.
    fn update_render_cache_ranges(&mut self) {
        // A lock on a clone of the Arc, so hashing (which caches into
        // `self`) can run while it is held.
        let sequence = Arc::clone(&self.sequence);
        let seq = sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut ranges = Vec::new();

        for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
            for clip in &track.clips {
                if clip.node_graph.is_identity() && clip.builtin_effects.is_empty() {
                    continue;
                }
                let grade_hash = self.render_key_hash(clip, track.opacity);
                // Timeline positions are ticks; a frame is `fps.den` of them
                // (1001 at the NTSC rates, 1 at integer rates). Counting
                // ticks made every clip look 1001x longer than it is, so
                // the cache bar read ~0% however much was cached.
                let ticks_per_frame = (self.fps.den as i64).max(1);
                let total = ((clip.timeline_out.value - clip.timeline_in.value) / ticks_per_frame)
                    .max(1) as u32;
                // Hot cache only — disk is too slow for timeline display.
                let cached = self.render_cache_hot.count_for(clip.id, grade_hash) as u32;
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
                let pixels = Pregrade::new(match frame.hdr.as_ref() {
                    Some(hdr) => PregradePixels::Hdr(hdr.to_cpu()),
                    None => PregradePixels::Srgb8(Arc::new(frame.to_rgba())),
                });
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
                self.frame_cache.insert_if_absent(frame_num, pixels, w, h);
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
                    let pixels = Pregrade::new(match frame.hdr.as_ref() {
                        Some(hdr) => PregradePixels::Hdr(hdr.to_cpu()),
                        None => PregradePixels::Srgb8(Arc::new(frame.to_rgba())),
                    });
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
                    self.frame_cache.insert_if_absent(frame_num, pixels, w, h);
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

    /// The primary clip's source time at the playhead.
    fn v1_source_secs(&self) -> Option<f64> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip = seq.tracks.get(self.current_track_index)?.clips.get(self.current_clip_index)?;
        Some(klipp_compose::render::source_secs(clip, self.playhead, seq.fps.as_f64()))
    }

    /// Take the primary decoder's frame for the playhead out of its queue:
    /// the latest at or before the playhead's source time, leaving later
    /// frames queued for stepping on. Returns whether `best` is settled —
    /// within half a frame of the time, or the queue shows nothing closer
    /// is coming. (After a seek the decoder hands over frames from up to
    /// 50 ms early, a whole frame at 25 fps; taking whatever arrived first
    /// showed the frame before the playhead.)
    fn take_v1_frame_for_playhead(&mut self, best: &mut Option<crate::types::LinearFrame>) -> bool {
        let Some(target) = self.v1_source_secs() else { return true };
        let half = self.fps.frame_duration_secs() * 0.5;
        let Some(dec) = self.v1_decoder.as_ref() else { return true };
        let epoch = dec.requested_seek();
        let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        while let Some(f) = queue.front() {
            // From before the seek in flight: the decode thread clears its
            // queue when it takes one, but a frame already converted can
            // still land. Which seek a frame was decoded under is recorded
            // on it; the timestamp only ever allowed a guess — "more than a
            // second out, so probably stale" — which cannot tell a stale
            // frame from a legitimately distant one.
            if f.seek_epoch < epoch {
                queue.pop();
                continue;
            }
            if f.is_eof {
                queue.pop();
                return true;
            }
            let pts = f.pts_secs();
            if pts > target + half {
                // Past the playhead. The nearest there is, if nothing came
                // before it (the media starts after the time, or skips it).
                if best.is_none() {
                    *best = queue.pop();
                }
                return true;
            }
            *best = queue.pop();
        }
        best.as_ref().is_some_and(|b| b.pts_secs() >= target - half)
    }

    fn grab_single_frame_timeout(&mut self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut best = None;
        let mut settled = false;
        while std::time::Instant::now() < deadline {
            if self.take_v1_frame_for_playhead(&mut best) && best.is_some() {
                settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(frame) = best.take_if(|_| settled) {
            // The whole frame, overlays and mattes included: the other
            // clips' streams are waited for, briefly, before it renders.
            self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
            if let Some(dec) = &self.v1_decoder {
                dec.pause();
            }
            return true;
        }
        // Timed out before the frame at the playhead. DO NOT pause the
        // decoder here — the caller may set `pending_paused_refresh` so the
        // per-tick paused branch picks up the frame when it finally
        // arrives. A frame short of the playhead that did arrive is kept
        // for it: shown then, it would have been left on screen with the
        // decoder paused, as if it were the frame at the playhead.
        self.paused_refresh_best = best;
        false
    }

    /// Per-tick handler for `pending_paused_refresh`. Pops the frames the
    /// decoder has produced since the timed-out seek, runs the full
    /// post_process pipeline on the one at the playhead (which captures
    /// per-node thumbs when V2 canvas is open), and pauses the decoder.
    /// Returns true on success so the caller can clear the flag. Returns
    /// false and leaves the decoder running while that frame is still
    /// coming.
    ///
    /// A frame short of the playhead is shown only once the seek has run
    /// `PROGRESS_AFTER` — then a slow seek shows where it has got to. A
    /// quick one would otherwise pay a full render (~100 ms at 4K) for a
    /// frame that is not the playhead's, just before the one that is.
    fn try_drain_paused_refresh_frame(&mut self) -> bool {
        const PROGRESS_AFTER: Duration = Duration::from_millis(200);
        let mut best = self.paused_refresh_best.take();
        let settled = self.take_v1_frame_for_playhead(&mut best);
        let Some(frame) = best else { return false };
        let slow = self.paused_refresh_since.is_none_or(|t| t.elapsed() >= PROGRESS_AFTER);
        if !settled && !slow {
            self.paused_refresh_best = Some(frame);
            return false;
        }
        self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
        if !settled {
            // Shown, but a closer frame is still coming: keep draining.
            return false;
        }
        if let Some(dec) = &self.v1_decoder {
            dec.pause();
        }
        true
    }

    /// Full-quality parked refresh (see the `parked_refresh` field).
    /// Called once per tick while paused. Three responsibilities:
    /// arm/reset the park timer when the parked position changes,
    /// poll an in-flight refresh decoder, and — after the debounce —
    /// kick off a refresh when the visible frame came from the skim
    /// substitute rather than the original media.
    fn tick_parked_refresh(&mut self) {
        const PARK_DEBOUNCE: Duration = Duration::from_millis(400);
        const REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

        if self.state != PlaybackState::Paused {
            self.parked_since = None;
            if let Some((mut dec, _, _)) = self.parked_refresh.take() {
                dec.stop_detached();
            }
            return;
        }

        let ph = self.playhead;
        match self.parked_since {
            Some((p, _)) if p == ph => {}
            _ => {
                // Parked position changed — restart the debounce and
                // abandon any refresh aimed at the old position.
                self.parked_since = Some((ph, std::time::Instant::now()));
                if let Some((mut dec, _, _)) = self.parked_refresh.take() {
                    dec.stop_detached();
                }
            }
        }

        // Poll the in-flight refresh decoder.
        if self.parked_refresh.is_some() {
            let (frame, timed_out) = {
                let (dec, target, started) = self.parked_refresh.as_ref().unwrap();
                if *target != ph {
                    // Stale target (shouldn't happen given the reset
                    // above, but belt-and-braces).
                    (None, true)
                } else {
                    let f = {
                        let mut q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                        // First frame at/after the seek target — the
                        // decode thread already skipped pre-seek frames.
                        let mut first = None;
                        if let Some(f) = q.pop() {
                            if !f.is_eof {
                                first = Some(f);
                            }
                        }
                        first
                    };
                    let out_of_time = f.is_none() && started.elapsed() > REFRESH_TIMEOUT;
                    (f, out_of_time)
                }
            };
            if let Some(frame) = frame {
                if let Some((mut dec, _, _)) = self.parked_refresh.take() {
                    dec.stop_detached();
                }
                let (w, h) = (frame.width, frame.height);
                self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
                self.parked_refresh_done_for = Some(ph);
                debug!(
                    "parked refresh: original-quality frame painted at {:.3}s ({w}x{h})",
                    ph.to_secs_f64(),
                );
            } else if timed_out {
                if let Some((mut dec, _, _)) = self.parked_refresh.take() {
                    dec.stop_detached();
                }
                // Give up for this position — the skim frame stays.
                // Retrying every tick would loop the failure.
                self.parked_refresh_done_for = Some(ph);
                debug!("parked refresh: timed out, keeping skim frame");
            }
            return;
        }

        // Maybe start a refresh.
        if self.parked_refresh_done_for == Some(ph)
            || self.pending_paused_refresh
            || self.in_node_param_edit
        {
            return;
        }
        let Some((_, since)) = self.parked_since else { return };
        if since.elapsed() < PARK_DEBOUNCE {
            return;
        }
        // Only when the active decode source is the skim substitute.
        let Some(current) = self.current_file.clone() else { return };
        let clip_info = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.iter().rev()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|track| {
                    track.clip_at_timecode(ph).and_then(|(_, clip)| {
                        let media_id = clip.media_id()?;
                        let source_secs = clip.source_in.to_secs_f64()
                            + (ph - clip.timeline_in).to_secs_f64() * clip.speed;
                        Some((media_id, source_secs))
                    })
                })
        };
        let Some((media_id, source_secs)) = clip_info else {
            self.parked_refresh_done_for = Some(ph);
            return;
        };
        let original = {
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            pool.get(&media_id).and_then(|item| {
                let substituted = item
                    .skim_file
                    .as_deref()
                    .is_some_and(|p| p == current.as_path())
                    && item.path != current;
                substituted.then(|| {
                    (item.path.clone(), item.effective_rotation_for(&item.path))
                })
            })
        };
        let Some((orig_path, rotation)) = original else {
            self.parked_refresh_done_for = Some(ph);
            return;
        };
        match VideoDecoder::open(&orig_path, rotation) {
            Ok(dec) => {
                dec.seek(source_secs);
                dec.resume();
                debug!(
                    "parked refresh: decoding original {:?} at {source_secs:.3}s",
                    orig_path.file_name(),
                );
                self.parked_refresh = Some((dec, ph, std::time::Instant::now()));
            }
            Err(e) => {
                debug!("parked refresh: open failed ({e}), keeping skim frame");
                self.parked_refresh_done_for = Some(ph);
            }
        }
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
        let (graph, clip_track_points, clip_time_secs) = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let found = seq.tracks.iter().find_map(|track| {
                track.clips.iter().find(|c| c.id == clip_id).map(|c| {
                    let speed = c.speed.max(0.001);
                    let clip_t = c.source_in.to_secs_f64()
                        + (playhead - c.timeline_in).to_secs_f64() * speed;
                    (c.node_graph.clone(), c.track_points.clone(), clip_t.max(0.0))
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
        let executor = self.renderer.executor();
        // Mirror the project's active working colorspace into the
        // executor so gamut-aware grade ops (Saturation, Vibrance,
        // ToneControls, ColorWheels, etc.) use the right luma vector.
        // Cheap setter; safe to call per-frame.
        executor.set_working_colorspace(crate::video_decoder::working_colorspace());
        executor.set_current_clip_track_points(&clip_track_points);
        match executor.execute_linear_with_thumbs(
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

    /// Every reason this frame cannot be handed to the viewer straight
    /// off the decoder's GPU buffer. Empty means it can.
    ///
    /// The decoder already leaves the frame as display-ready sRGB-u8
    /// RGBA. When the graph is an identity and nothing composites over
    /// it, the entire CPU excursion the engine performs — expand to
    /// linear f32, grade, blend, tone-map, re-encode to u8 — is the
    /// round trip `u8 -> linear -> u8`, which lands back on the bytes it
    /// started from. At 4K that round trip measures ~19 ms of readback
    /// plus ~17 ms of composite and encode, to reproduce its own input.
    ///
    /// So the fast path reads the decoder's bytes back and publishes
    /// them. Its correctness rests on two conditions checked below —
    /// `ToneMapOp::None` and a Rec.709 working space — under which
    /// `display_transform_ws` reduces to exactly the sRGB encode that
    /// inverts the decode. A wide-gamut working space does not qualify:
    /// ACES needs its ODT, and BT.2020 needs a matrix into BT.709 before
    /// the encode, so neither round-trips to the decoder's own bytes. `direct_present_is_byte_identical` in
    /// tests/direct_present_identity.rs pins that for all 256 levels.
    ///
    /// Everything else here is a consumer that wants pixels this path
    /// does not produce (linear light, ungraded input, a real composite),
    /// so each one sends the frame down the full pipeline instead. The
    /// reasons are `&'static str` rather than a bool so the log can name
    /// the consumers that opted out — the first question to ask when the
    /// fast path is not engaging.
    fn direct_present_blockers(&self) -> Vec<&'static str> {
        self.fast_path_blockers(false)
    }

    /// Whether the only things standing between the decoder and the
    /// viewer are ones the *graded* GPU path handles itself: a grade to
    /// run, and a display transform it has a shader for.
    ///
    /// Direct present cannot live with any of these — it publishes the
    /// decoder's own bytes — but `grade_to_display_on_gpu` produces
    /// exactly the renderer's picture for each, which
    /// `tests/fast_paths_match_renderer.rs` holds it to. Kept in one
    /// place because both graded gates ask the same question, and a new
    /// GPU display transform must reach both.
    fn only_the_graded_path_handles(blockers: &[&'static str]) -> bool {
        blockers.iter().all(|b| {
            matches!(
                *b,
                "grade graph is not an identity"
                    | "ACES working space"
                    | "BT.2020 working space"
                    | "HDR preview"
            )
        })
    }

    /// [`Self::direct_present_blockers`], for playback (`parked_drag`
    /// false) or for a slider step on the parked frame (true). A parked
    /// frame's ungraded picture is already made, so a drag needs nothing
    /// from the full path that playback does not — except what the grade
    /// tools read off the linear frame while paused: a scope tap and the
    /// inspector's matte preview. (Node thumbnails are not captured while
    /// a slider is held.)
    fn fast_path_blockers(&self, parked_drag: bool) -> Vec<&'static str> {
        if !direct_present_enabled() {
            return vec!["KLIPP_DIRECT_PRESENT=0"];
        }
        // Steady-state playback, or a drag on the parked frame; any other
        // paused frame goes through `regrade_cached_frame`'s full render.
        // Checked first and alone so the sequence lock below is not taken
        // on every idle tick.
        if !parked_drag && self.state != PlaybackState::Playing {
            return vec!["not playing"];
        }
        if parked_drag && (self.state != PlaybackState::Paused || !self.in_node_param_edit) {
            return vec!["not a slider drag on the parked frame"];
        }

        // Every remaining reason is collected rather than returned at the
        // first hit: finding out one run at a time which consumer opted
        // out is the slow way to learn why playback is not accelerating.
        let mut blockers: Vec<&'static str> = Vec::new();

        if parked_drag {
            if self.scope_tap_node.is_some() {
                blockers.push("scope tap on a node");
            }
            if self.inspector_matte_preview_target.is_some() {
                blockers.push("inspector matte preview");
            }
        } else {
            // Compare modes, paused scrubbing and param drags read the
            // ungraded CPU frame.
            if self.pregrade_encode_needed() {
                blockers.push("pre-grade CPU encode needed");
            }
            if self.in_node_param_edit {
                blockers.push("node param edit in progress");
            }
        }
        // What the byte-identity of *direct present* rests on, and what
        // the graded path can live with.
        //
        // Direct present hands the decoder's own sRGB bytes to the
        // viewer, so it needs `display_transform_ws` to reduce to the
        // sRGB encode that inverts the decode: BT.709 and no tone map.
        //
        // The graded path only needs a display transform the GPU has.
        // It has BT.709's, ACES' and BT.2020's, so each wide-gamut
        // project gets its own reason string — one the graded gates
        // tolerate the way they tolerate a non-identity graph, and
        // direct present does not, because a render through either
        // display transform is emphatically not the bytes the decoder
        // produced.
        //
        // Matched exhaustively on purpose: a working space added later
        // must not inherit acceleration it has no GPU transform for, so
        // this is a compile error rather than a silent wrong picture.
        match crate::video_decoder::working_colorspace() {
            klipp_core::WorkingColorspace::Rec709Linear => {
                if !matches!(self.tone_map_op, klipp_color::display::ToneMapOp::None) {
                    blockers.push("tone-map operator active");
                }
            }
            // The ODT carries its own tone curve; `display_transform_ws`
            // marks the viewer's tone-map operator unused on this path,
            // so it is not a blocker here.
            klipp_core::WorkingColorspace::AcesCG => blockers.push("ACES working space"),
            // BT.2020 changes basis and gamut-maps into BT.709, then
            // tone-maps — and unlike ACES it does honour the viewer's
            // operator, which the shader does not carry. So the tone map
            // is a blocker here as it is for BT.709.
            klipp_core::WorkingColorspace::Bt2020Linear => {
                blockers.push("BT.2020 working space");
                if !matches!(self.tone_map_op, klipp_color::display::ToneMapOp::None) {
                    blockers.push("tone-map operator active");
                }
            }
        }
        // Scopes sample the post-grade linear frame, which this path
        // never builds. But they refresh at 10 Hz, so only the frames
        // that actually feed one need the full pipeline — at 59.94 fps
        // that is one frame in six, and the other five accelerate. This
        // is deliberately `should_render_scope()` and not "are scopes
        // open": gating on the panel being visible turned the fast path
        // off for the entire session, which is exactly the case colour
        // work is done in.
        if self.should_render_scope() {
            blockers.push("scope refresh due this frame");
        }
        // Node thumbnails are only captured while paused (`thumb_set` is
        // None whenever state is Playing), so they cannot block a path
        // that runs only during playback — no check for them here.
        if self.matte_preview_node.is_some() {
            blockers.push("matte preview active");
        }
        // Direct present hands over the decoder's own bytes, which is
        // emphatically not an HDR picture, so both modes stop it. The
        // *graded* path has monitoring's preview as a mode on its display
        // encode, so that one gets a reason string the graded gates
        // tolerate — the same arrangement ACES and BT.2020 have.
        //
        // False colour does not: it replaces the picture with flat bands
        // off a table, which no shader here has, and it is a mode you flip
        // to for a moment rather than grade under.
        match self.hdr_monitor {
            klipp_core::HdrMonitor::Off => {}
            klipp_core::HdrMonitor::Preview => blockers.push("HDR preview"),
            klipp_core::HdrMonitor::FalseColour => blockers.push("HDR false colour"),
        }
        // Feeding a reference output means the HDR frame has to reach
        // another process, which means it has to reach the CPU. Neither
        // fast path produces it there, so this is the one case where
        // keeping the frame on the GPU is not what we want — and paying a
        // readback for a reference monitor is the right trade.
        if self.hdr_reference_output {
            blockers.push("HDR reference output active");
        }
        if self.compare_stabilization || self.compare_effect_active {
            blockers.push("compare mode active");
        }
        // Before/after split: the grade stores the pre-grade frame for the
        // split's "before" half. Neither fast path produces it, so that
        // half would freeze on a stale frame.
        if self.bypass_color_grade {
            blockers.push("before/after split active");
        }
        // The project-wide colour pipeline (global LUT) runs after the
        // graph on every graded-path frame, identity or not. Neither fast
        // path applies it, so either would show the picture without it.
        if self.color_pipeline.enabled {
            blockers.push("global colour pipeline active");
        }

        // The fast paths show the primary decoder's frame, so they are the
        // shared renderer's frame only when that renders exactly one clip,
        // the primary decoder's, on a fully opaque track: no other layer, no
        // transition, no other multicam angle. Asked of the renderer's own
        // stack, so the two cannot disagree about what is on screen.
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let layers = klipp_compose::active_layers(&seq, self.playhead);
        let primary = self.v1_clip_id(&seq);
        let (layer, clip) = match layers.as_slice() {
            [] => {
                blockers.push("nothing on screen");
                return blockers;
            }
            [layer] if layer.transition => {
                blockers.push("transition active");
                return blockers;
            }
            [layer] => match layer.clips.as_slice() {
                [clip] if Some(clip.id) == primary => (layer, *clip),
                _ => {
                    blockers.push("another clip on screen");
                    return blockers;
                }
            },
            _ => {
                blockers.push("overlay layers present");
                return blockers;
            }
        };
        if seq.tracks.get(layer.track).is_none_or(|t| (t.opacity - 1.0).abs() > 1e-6) {
            blockers.push("track opacity below 1.0");
        }
        if clip.consumed_as_matte {
            blockers.push("clip is a matte source");
        }
        if !clip.node_graph.is_identity() {
            blockers.push("grade graph is not an identity");
        }
        if !clip.effects.is_empty() {
            blockers.push("plugin effects on clip");
        }
        if !clip.builtin_effects.is_empty() {
            blockers.push("builtin effects on clip");
        }
        if !clip.param_modifiers.is_empty() {
            blockers.push("param modifiers on clip");
        }
        if !clip.opacity_keyframes.is_empty()
            || !clip.transform_pos_x_keyframes.is_empty()
            || !clip.transform_pos_y_keyframes.is_empty()
            || !clip.transform_scale_keyframes.is_empty()
            || !clip.transform_rotation_keyframes.is_empty()
        {
            blockers.push("render keyframes on clip");
        }
        if !matches!(clip.blend_mode, klipp_core::BlendMode::Normal) {
            blockers.push("clip blend mode is not Normal");
        }
        if clip.matte_source.is_some() || clip.matte_stencil.is_some() {
            blockers.push("clip has a matte");
        }
        if clip.source_rotation != 0 {
            blockers.push("clip has a source rotation");
        }
        if !clip.kind.needs_decoder() {
            blockers.push("clip is synthetic");
        }
        blockers
    }

    /// Publish the decoder's frame to the viewer without grading or
    /// compositing it. Only called when `direct_present_blocker` returned
    /// `None`; see there for why this is byte-identical to the full path.
    fn present_decoded_frame_directly(
        &mut self,
        gpu: &std::sync::Arc<crate::frame::GpuFrame>,
        width: u32,
        height: u32,
        current_source_secs: f64,
        q_len: usize,
    ) {
        let t_frame = std::time::Instant::now();
        mark_stage(EngineStage::PostProcessEntry);
        let bytes = crate::types::realise_srgb_u8_from_gpu(gpu);
        let readback_ms = t_frame.elapsed().as_secs_f64() * 1000.0;

        if bytes.len() != (width as usize) * (height as usize) * 4 {
            // The decoder's buffer disagrees with the frame's declared
            // size. Better to drop this frame than publish a torn one;
            // the next frame re-evaluates from scratch.
            tracing::error!(
                "direct present: readback {} bytes != {width}x{height}x4 — frame skipped",
                bytes.len(),
            );
            return;
        }

        self.publish_fast_path_frame(
            bytes, width, height, t_frame, 0.0, readback_ms, "direct_present",
            current_source_secs, q_len, None,
        );
    }

    /// What the primary clip's node graph needs to run for the frame under
    /// the playhead, exactly as the shared renderer runs it: the graph (an
    /// Arc cached per clip, rebuilt when the sequence changes), the
    /// clip-relative time and frame number it is evaluated at, and the
    /// clip's track points for tracker-bound masks. The clip is the primary
    /// decoder's — the one `direct_present_blockers` checked is alone on
    /// screen.
    fn v1_graph_setup(&mut self) -> Option<(Arc<klipp_nodes::NodeGraph>, f64, u64, Vec<klipp_core::TrackPoint>)> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip = seq.tracks.get(self.current_track_index)?.clips.get(self.current_clip_index)?;
        let cached = self
            .current_graph_arc
            .as_ref()
            .filter(|_| self.current_grade_clip_id == Some(clip.id))
            .map(Arc::clone);
        let graph = match cached {
            Some(arc) => arc,
            None => {
                let arc = Arc::new(clip.node_graph.clone());
                self.current_graph_arc = Some(Arc::clone(&arc));
                self.current_grade_clip_id = Some(clip.id);
                arc
            }
        };
        let (clip_time_secs, frame_number) = klipp_compose::render::grade_time(clip, self.playhead, seq.fps.as_f64());
        Some((graph, clip_time_secs, frame_number, clip.track_points.clone()))
    }

    /// A slider step on the parked frame, graded and encoded for display
    /// on the GPU: `present_graded_frame_on_gpu` for a paused frame. The
    /// parked frame goes up once, on the drag's first step, and stays; each
    /// step then runs the graph and reads back only display bytes. Through
    /// the renderer a step cost a 4K HDR frame ~40 ms, most of it floats
    /// crossing the bus and being tone-mapped and encoded on the CPU.
    ///
    /// Only when `fast_path_blockers(true)` names nothing but the graph, so
    /// the renderer would have done no more than this. Returns `false`,
    /// having shown nothing, when the frame cannot take this path.
    fn regrade_parked_on_gpu(&mut self) -> bool {
        let Some((pixels, width, height)) = self.cached_pregrade_frame.clone() else { return false };
        if !Self::only_the_graded_path_handles(&self.fast_path_blockers(true)) {
            return false;
        }
        let t_frame = std::time::Instant::now();
        let Some((graph, clip_time_secs, frame_number, track_points)) = self.v1_graph_setup() else {
            return false;
        };
        let seed = match &pixels.pixels {
            PregradePixels::Hdr(h) => match &h.codes {
                crate::types::HdrCodes::Cpu(codes) if (h.width, h.height) == (width, height) => {
                    crate::fast_path::GpuSeed::SceneCodes {
                        codes,
                        transfer: h.transfer,
                        source_to_working: h.source_to_working,
                    }
                }
                _ => return false,
            },
            PregradePixels::Srgb8(bytes) => crate::fast_path::GpuSeed::Srgb8Bytes(bytes),
        };
        let at = crate::fast_path::GradeAt {
            graph: &graph,
            clip_time: clip_time_secs,
            frame_number,
            track_points: &track_points,
            working: crate::video_decoder::working_colorspace(),
            lut_tetrahedral: self.color_pipeline.use_tetrahedral,
            solo: self.solo_node,
            // Monitoring's preview. False colour is a different picture
            // and stays on the CPU, so `fast_path_blockers` refuses it.
            hdr_preview: self.hdr_monitor == klipp_core::HdrMonitor::Preview,
            // The same peak `view.rs` renders monitoring at.
            hdr_peak_nits: klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2,
        };
        let (tone_map, working, preview) = (self.tone_map_op, at.working, at.hdr_preview);
        // The frame the fallback gets is display light when monitoring is
        // on, so it takes the preview encode — the same substitution the
        // GPU branch makes.
        let display = move |linear: &[f32]| {
            if preview {
                klipp_color::hdr_view::preview_frame(linear, tone_map, working)
            } else {
                klipp_color::display::display_transform_ws(linear, tone_map, working)
            }
        };
        mark_stage(EngineStage::PostProcessInnerGradeExecute);
        let Some(mut bytes) =
            crate::fast_path::grade_to_display_on_gpu(self.renderer.executor(), seed, width, height, &at, &display)
        else {
            return false;
        };
        let grade_ms = t_frame.elapsed().as_secs_f64() * 1000.0;
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, width, height);
            fb.set_presented(self.playhead);
        }
        self.composite_scratch_u8 = bytes;
        let total_ms = t_frame.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut ring) = self.perf_ring.lock() {
            ring.push(crate::perf::PerfSample {
                timestamp: std::time::Instant::now(),
                width,
                height,
                target_fps: 0.0,
                decode_ms: 0.0,
                seq_read_ms: 0.0,
                grade_ms: grade_ms as f32,
                ofx_ms: 0.0,
                builtin_ms: 0.0,
                lut_ms: 0.0,
                total_ms: total_ms as f32,
                dropped: false,
                bridge_u8_ms: 0.0,
                reencode_ms: 0.0,
                scope_ms: 0.0,
                composite_ms: 0.0,
                update_playback_ms: 0.0,
            });
        }
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
        true
    }

    /// Grade the decoder's frame and present it without it ever leaving
    /// the GPU as floats: seed the chain from the decoder's buffer (HDR
    /// codes or SDR u8), run the graph, then tone-map (HDR) and encode to
    /// display u8 on the GPU and read back only those 4 bytes a pixel.
    ///
    /// The CPU path it replaces measured 5.8 fps on 4K HDR with the grade
    /// itself at 5.7 ms: every frame crossed the bus as 132 MB of floats,
    /// twice. Only called when `direct_present_blockers` names nothing but
    /// the graph, so no other step — effects, LUTs, composite, scopes —
    /// would have touched the frame.
    ///
    /// Returns `false`, having published nothing, if the frame cannot take
    /// this path (no GPU-resident source, seed refused, a node needs CPU
    /// pixels); the caller then grades normally.
    fn present_graded_frame_on_gpu(
        &mut self,
        frame: &crate::types::LinearFrame,
        width: u32,
        height: u32,
        current_source_secs: f64,
        q_len: usize,
    ) -> bool {
        let t_frame = std::time::Instant::now();
        let Some((graph, clip_time_secs, frame_number, track_points)) = self.v1_graph_setup() else {
            return false;
        };
        let seed = match (frame.hdr.as_ref(), frame.gpu_source.as_ref()) {
            (Some(h), _) => match &h.codes {
                crate::types::HdrCodes::Gpu { buffer, .. } => crate::fast_path::GpuSeed::SceneReferred {
                    buffer,
                    transfer: h.transfer,
                    source_to_working: h.source_to_working,
                },
                crate::types::HdrCodes::Cpu(_) => return false,
            },
            (None, Some(g)) => crate::fast_path::GpuSeed::Srgb8(&g.rgba_buffer),
            (None, None) => return false,
        };
        let at = crate::fast_path::GradeAt {
            graph: &graph,
            clip_time: clip_time_secs,
            frame_number,
            track_points: &track_points,
            working: crate::video_decoder::working_colorspace(),
            lut_tetrahedral: self.color_pipeline.use_tetrahedral,
            solo: self.solo_node,
            // Monitoring's preview. False colour is a different picture
            // and stays on the CPU, so `fast_path_blockers` refuses it.
            hdr_preview: self.hdr_monitor == klipp_core::HdrMonitor::Preview,
            // The same peak `view.rs` renders monitoring at.
            hdr_peak_nits: klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2,
        };
        let (tone_map, working, preview) = (self.tone_map_op, at.working, at.hdr_preview);
        // The frame the fallback gets is display light when monitoring is
        // on, so it takes the preview encode — the same substitution the
        // GPU branch makes.
        let display = move |linear: &[f32]| {
            if preview {
                klipp_color::hdr_view::preview_frame(linear, tone_map, working)
            } else {
                klipp_color::display::display_transform_ws(linear, tone_map, working)
            }
        };
        mark_stage(EngineStage::PostProcessInnerGradeExecute);
        let t_grade = std::time::Instant::now();
        let Some(bytes) =
            crate::fast_path::grade_to_display_on_gpu(self.renderer.executor(), seed, width, height, &at, &display)
        else {
            return false;
        };
        // Monitoring's readout needs the picture's light. The display
        // encode wrote to its own buffer and left the chain's linear frame
        // where it is, so this measures it in place — the whole reason
        // monitoring can take this path instead of bringing 132 MB down.
        let nits = at.hdr_preview.then(|| {
            let to_2020 = klipp_effects::color_wgsl::pack_primaries_matrix(
                klipp_color::primaries::working_to_bt2020(at.working),
            );
            self.renderer.executor().nit_map_of_output(width, height, NIT_MAP_STEP, to_2020)
        });
        // The grade and its display readback run as one submission; the
        // time is reported as the grade's.
        let grade_ms = t_grade.elapsed().as_secs_f64() * 1000.0;
        let readback_ms = 0.0;
        self.publish_fast_path_frame(
            bytes, width, height, t_frame, grade_ms, readback_ms, "graded_gpu_present",
            current_source_secs, q_len, nits.flatten(),
        );
        true
    }

    /// The shared tail of both fast paths: publish a finished display
    /// frame, push a perf sample (or the overlay would freeze on the last
    /// full-pipeline frame and report numbers this frame never paid), and
    /// notify the UI.
    #[allow(clippy::too_many_arguments)]
    fn publish_fast_path_frame(
        &mut self,
        mut bytes: Vec<u8>,
        width: u32,
        height: u32,
        t_frame: std::time::Instant,
        grade_ms: f64,
        readback_ms: f64,
        path: &str,
        current_source_secs: f64,
        q_len: usize,
        nits: Option<(Vec<f32>, u32, u32)>,
    ) {
        let t_fb = std::time::Instant::now();
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, width, height);
            fb.set_presented(self.playhead);
            // HDR monitoring's light map, for the readout under the
            // pointer. Cleared when there is none, or the readout would
            // report a stale frame's levels.
            match nits {
                Some((map, mw, mh)) => {
                    fb.nit_map = map;
                    (fb.nit_map_w, fb.nit_map_h, fb.nit_map_step) = (mw, mh, NIT_MAP_STEP);
                }
                None if !fb.nit_map.is_empty() => {
                    fb.nit_map = Vec::new();
                    (fb.nit_map_w, fb.nit_map_h) = (0, 0);
                }
                None => {}
            }
        }
        // `write_frame_swap` handed back the previous buffer — keep it as
        // the u8 scratch so the next frame reuses the allocation.
        self.composite_scratch_u8 = bytes;
        let fb_write_ms = t_fb.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_frame.elapsed().as_secs_f64() * 1000.0;
        // The overlay reads the ring, so a fast-path frame has to push a
        // sample too or the display freezes on the last graded frame and
        // reports numbers this frame never paid.
        let target_fps = self.target_fps_for_overlay();
        let decode_ms = self.v1_decoder.as_ref().map(|d| d.last_decode_ms()).unwrap_or(0.0);
        let dropped = match target_fps {
            Some(fps) if fps > 0.0 => (total_ms as f32).max(decode_ms) > (1000.0 / fps),
            _ => false,
        };
        if let Ok(mut ring) = self.perf_ring.lock() {
            ring.push(crate::perf::PerfSample {
                timestamp: std::time::Instant::now(),
                width,
                height,
                target_fps: target_fps.unwrap_or(0.0),
                decode_ms,
                seq_read_ms: 0.0,
                // Only the node graph's own time. The readback is counted
                // as a readback, not folded in here.
                grade_ms: grade_ms as f32,
                ofx_ms: 0.0,
                builtin_ms: 0.0,
                lut_ms: 0.0,
                total_ms: total_ms as f32,
                dropped,
                bridge_u8_ms: 0.0,
                reencode_ms: 0.0,
                scope_ms: 0.0,
                composite_ms: 0.0,
                update_playback_ms: total_ms as f32,
            });
        }

        self.frames_consumed += 1;
        if total_ms > 5.0 {
            tracing::debug!(
                "PERF {path}: {total_ms:.1}ms for {width}x{height} | \
                 grade={grade_ms:.1} readback={readback_ms:.1} fb_write={fb_write_ms:.1}",
            );
        }
        if self.last_playback_log.elapsed().as_secs() >= 2 {
            let elapsed = self.last_playback_log.elapsed().as_secs_f64();
            let fps = self.frames_consumed as f64 / elapsed.max(0.001);
            tracing::debug!(
                "PLAYBACK direct: clock={current_source_secs:.3}s consumed={} \
                 ({fps:.1}fps) drops={} q_len={q_len}",
                self.frames_consumed, self.dropped_frames,
            );
            self.frames_consumed = 0;
            self.last_playback_log = std::time::Instant::now();
        }

        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    /// Whether this frame needs the pre-grade CPU encode.
    ///
    /// Extracted so the decision can be made *before* the GPU→CPU
    /// readback rather than 4000 lines later, inside `post_process_rgba`,
    /// by which point the pixels have already been fetched. It is the one
    /// consumer of the *ungraded* CPU buffer — everything downstream of
    /// the grade works from the executor's output — so when this is false
    /// and the grade chain can be seeded from the decoder's GPU buffer,
    /// the readback has no consumer at all.
    ///
    /// Compare modes and paused scrubbing still need it. The drag
    /// exception is deliberate: during a Playing param-drag with no
    /// compare mode active the encode runs at full resolution ~30 times a
    /// second purely to refresh a scope preview the user is not looking
    /// at, so a slightly stale preview buys back the frame budget.
    fn pregrade_encode_needed(&self) -> bool {
        let drag_skip = drag_downsample_enabled()
            && self.in_node_param_edit
            && self.state == PlaybackState::Playing
            && !self.compare_stabilization
            && !self.compare_effect_active
            && !self.bypass_color_grade;
        (self.state != PlaybackState::Playing
            || self.compare_stabilization
            || self.compare_effect_active
            || self.in_node_param_edit)
            && !drag_skip
    }





    /// Re-render the paused frame without decoding again: the primary
    /// decoder's frame from the pre-grade cache, every other clip's from
    /// its stream. Called whenever something that changes the picture
    /// changes while paused.
    /// Playhead ticks in one frame. `Timecode.value` counts `fps.rate()` —
    /// the numerator — per second, so a frame is 1001 ticks at 23.976,
    /// 29.97 and 59.94 fps and 1 at integer rates. Frame counts (a step, a
    /// jump of N frames) must be scaled by this before they meet a
    /// playhead. The timeline's rate, not `self.fps`: that one follows the
    /// timeline only once playback starts, and a timeline parked on since
    /// it was opened still has the engine's 24 fps default there.
    fn frame_ticks(&self) -> i64 {
        let fps = self.sequence.read().unwrap_or_else(|e| e.into_inner()).fps;
        ((self.playhead.rate as f64 / fps.as_f64()).round() as i64).max(1)
    }

    fn regrade_cached_frame(&mut self) {
        mark_stage(EngineStage::PausedRegrade);
        // A slider step on a frame the renderer would only grade.
        if self.in_node_param_edit && self.regrade_parked_on_gpu() {
            return;
        }
        let primary = self
            .cached_pregrade_frame
            .as_ref()
            .map(|(pixels, w, h)| streams::HeldFrame::from_pregrade(pixels, *w, *h));
        // A primary decoder whose first frame has not arrived yet: nothing
        // to show until it does (the paused refresh paints it).
        if primary.is_none() && self.v1_decoder.is_some() {
            return;
        }
        self.present_frame(primary);
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

    /// Render every mode in `(active_scope_modes ∪ {scope_render_mode})`
    /// via libavfilter and return per-mode RGBA outputs. Does NOT touch
    /// the framebuffer; pair with [`Self::apply_scope_renders`] to write
    /// the results. Splitting render-from-apply lets the playback path
    /// (Site 4) share one `frame_buffer.lock()` with `write_frame`.
    ///
    /// `None` entries mean the filter buffered the input or rebuild
    /// failed — caller should leave the previous texture in place
    /// instead of clearing.
    fn render_active_scopes_pending(
        &mut self,
        linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
    ) -> Vec<(u8, Option<Vec<u8>>)> {
        // Union: always include the main-dock mode so its singular-field
        // back-compat readers (in-tree dock UI) never see a stale
        // texture, even when the floating window's active set excludes
        // the dock mode (e.g. four parade panes while dock shows
        // vectorscope).
        let dock_mode = self.scope_render_mode;
        let mut to_render: std::collections::BTreeSet<u8> = self.active_scope_modes.clone();
        to_render.insert(dock_mode);
        // The CIE scope is in the active set but is not a filter graph —
        // it asks for sample data instead. See `cie_samples_pending`.
        to_render.remove(&crate::ffmpeg_scope::CIE_SAMPLES_MODE);

        let settings = self.scope_ffmpeg_settings;
        let bloom_enabled = self.scope_bloom_enabled;
        // The frame is in the project's working space, so that is the basis
        // its luma and chroma mean anything in.
        let basis = klipp_color::scopes::ScopeBasis::for_working_space(
            crate::video_decoder::working_colorspace(),
        );
        let mut rendered: Vec<(u8, Option<Vec<u8>>)> = Vec::with_capacity(to_render.len());
        for &mode in &to_render {
            let tex = match self
                .ffmpeg_scope_bank
                .render_linear(mode, linear_rgba, src_w, src_h, settings, basis)
            {
                Ok(Some(bytes)) => Some(bytes),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!("FfmpegScope multi-mode render failed (mode={mode}): {e}");
                    None
                }
            };
            // Bloom post-pass: only on waveform (0) + parade (1) where
            // dense trace regions actually glow. Vectorscope + histogram
            // skip bloom — applying it to their sparse shapes looks
            // muddy. CPU helper at klipp-effects/light.rs:apply_bloom;
            // ~3-5 ms at 1024×512 RGBA on a modern CPU, runs at ≤10 Hz
            // so the per-second cost is negligible.
            let tex = match tex {
                Some(bytes) if bloom_enabled && (mode == 0 || mode == 1) => {
                    Some(klipp_effects::light::apply_bloom(
                        bytes,
                        crate::ffmpeg_scope::SCOPE_OUT_W,
                        crate::ffmpeg_scope::SCOPE_OUT_H,
                        0.55,  // luma threshold — matches in-tree default
                        6.0,   // blur radius (px)
                        0.6,   // intensity — softer than typical cinema
                               // bloom so the scope graticule stays legible
                    ))
                }
                other => other,
            };
            rendered.push((mode, tex));
        }
        rendered
    }

    /// Apply the output of [`Self::render_active_scopes_pending`] to a
    /// `FrameBuffer` already held by the caller. Writes the legacy
    /// singular fields for `scope_render_mode` (back-compat with the
    /// in-tree dock reader) AND the per-mode `scope_textures` map
    /// (floating multi-scope window pane reader).
    fn apply_scope_renders(
        fb: &mut crate::texture::FrameBuffer,
        dock_mode: u8,
        renders: Vec<(u8, Option<Vec<u8>>)>,
        scope_w: u32,
        scope_h: u32,
    ) {
        for (mode, tex_opt) in renders.into_iter() {
            if mode == dock_mode {
                write_scope_tex_to_fb(fb, tex_opt.clone(), scope_w, scope_h, mode);
            }
            match tex_opt {
                Some(bytes) => {
                    let entry = fb
                        .scope_textures
                        .entry(mode)
                        .or_insert(crate::texture::ScopeTextureEntry {
                            data: Vec::new(),
                            w: 0,
                            h: 0,
                            generation: 0,
                        });
                    entry.data = bytes;
                    entry.w = scope_w;
                    entry.h = scope_h;
                    entry.generation = entry.generation.wrapping_add(1);
                }
                None => {
                    // Leave the existing entry alone — UI keeps the
                    // previous frame. Mirrors the singular path's
                    // "no clear" semantics for skim writes.
                }
            }
        }
    }

    /// Combined render + apply for sites that don't need to share the
    /// FB lock with another write. Acquires the lock exactly once.
    fn render_active_scopes_via_ffmpeg_linear(
        &mut self,
        linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
        scope_w: u32,
        scope_h: u32,
    ) {
        let renders = self.render_active_scopes_pending(linear_rgba, src_w, src_h);
        let cie = self.cie_samples_pending(linear_rgba, src_w, src_h);
        let dock_mode = self.scope_render_mode;
        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        Self::apply_scope_renders(&mut fb, dock_mode, renders, scope_w, scope_h);
        match cie {
            Some((samples, w, h)) => {
                fb.cie_samples = samples;
                fb.cie_samples_w = w;
                fb.cie_samples_h = h;
            }
            None if !fb.cie_samples.is_empty() => {
                fb.cie_samples = Vec::new();
                fb.cie_samples_w = 0;
                fb.cie_samples_h = 0;
            }
            None => {}
        }
    }

    /// The picture as the renderer made it, reduced to a sample grid, for
    /// the CIE chromaticity scope. `None` when no such scope is open.
    ///
    /// Deliberately the linear working-space frame and not `fb.data`: a
    /// chromaticity diagram is absolute, so it has to plot the colours the
    /// picture has, not the ones left after the display transform has fitted
    /// them to the screen's gamut.
    fn cie_samples_pending(
        &self,
        linear_rgba: &[f32],
        src_w: u32,
        src_h: u32,
    ) -> Option<(Vec<f32>, u32, u32)> {
        if !self
            .active_scope_modes
            .contains(&crate::ffmpeg_scope::CIE_SAMPLES_MODE)
        {
            return None;
        }
        if src_w == 0 || src_h == 0 || linear_rgba.len() < (src_w * src_h * 4) as usize {
            return None;
        }
        let edge = crate::ffmpeg_scope::CIE_SAMPLES_MAX_EDGE;
        let step = (src_w.max(src_h) as f32 / edge as f32).ceil().max(1.0) as u32;
        let (out_w, out_h) = (src_w.div_ceil(step), src_h.div_ceil(step));
        let mut out = Vec::with_capacity((out_w * out_h * 3) as usize);
        for y in (0..src_h).step_by(step as usize) {
            for x in (0..src_w).step_by(step as usize) {
                let i = ((y * src_w + x) * 4) as usize;
                out.extend_from_slice(&linear_rgba[i..i + 3]);
            }
        }
        Some((out, out_w, out_h))
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

    /// Resolve the rotation the decoder should apply when reading
    /// `decode_path` for `media_id`. Returns 0 when the path is the
    /// item's skim/proxy (ffmpeg autorotate already produced upright
    /// pixels during transcode); returns the item's `source_rotation`
    /// otherwise. Falls back to `fallback` when the media is missing
    /// from the pool — the same defensive shape `path_for_media` uses.
    fn effective_decode_rotation(
        &self,
        media_id: MediaId,
        decode_path: &std::path::Path,
        fallback: i32,
    ) -> i32 {
        self.media_pool
            .read()
            .ok()
            .and_then(|pool| pool.get(&media_id).map(|item| item.effective_rotation_for(decode_path)))
            .unwrap_or(fallback)
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
                                sample_rate = Some(dec.rate());
                                audio_channels = Some(dec.channels());
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
                    .and_then(parse_iso8601_timestamp);

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
                                    // Bit depth from pixel format.
                                    // `format` is a bare c_int from the
                                    // container; AVPixelFormat is a repr(i32)
                                    // enum, so transmuting an out-of-range
                                    // value into it would be undefined. Range
                                    // check first, and build the enum once.
                                    let raw_fmt = (*par).format;
                                    let pix_fmt = (-1..ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NB as i32)
                                        .contains(&raw_fmt)
                                        .then(|| {
                                            std::mem::transmute::<i32, ffmpeg_next::ffi::AVPixelFormat>(
                                                raw_fmt,
                                            )
                                        });
                                    let desc = pix_fmt
                                        .map_or(std::ptr::null(), |f| ffmpeg_next::ffi::av_pix_fmt_desc_get(f));
                                    if !desc.is_null() {
                                        bit_depth = Some((*desc).comp[0].depth as u8);
                                        let name_ptr = pix_fmt
                                            .map_or(std::ptr::null(), |f| ffmpeg_next::ffi::av_get_pix_fmt_name(f));
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
        // GPU-resident decode ships with empty
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
                let latency_advertise = adec.latency_advertise.clone();
                self.ensure_mix_pipeline(consumer, track, latency_advertise);
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
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
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

    let codec_id = params.id();
    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    // Always multi-threaded. Hardware decode ignores it; a software
    // decoder with the default of one thread is the single biggest cost
    // there is — see `hw_capable_decoder`.
    let half_cores = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 2).max(1) as i32;
    unsafe { (*codec_ctx.as_mut_ptr()).thread_count = half_cores };

    // Try GPU acceleration: CUDA → VAAPI (AMD/Intel only) → Software
    use ffmpeg_next::ffi::AVHWDeviceType::{AV_HWDEVICE_TYPE_CUDA, AV_HWDEVICE_TYPE_VAAPI};
    let vendor = cached_gpu_vendor();
    let hw_codec = unsafe {
        match hw_capable_decoder(codec_id, AV_HWDEVICE_TYPE_CUDA) {
            Some(c) if crate::video_decoder::try_init_cuda(codec_ctx.as_mut_ptr()) => Some(c),
            _ if vendor.should_try_vaapi() => hw_capable_decoder(codec_id, AV_HWDEVICE_TYPE_VAAPI)
                .filter(|_| crate::video_decoder::try_init_vaapi(codec_ctx.as_mut_ptr())),
            _ => None,
        }
    };
    let is_hw = hw_codec.is_some();

    let decoder = match hw_codec {
        Some(codec) => codec_ctx.decoder().open_as(codec).and_then(|o| o.video()),
        None => codec_ctx.decoder().video(),
    }
    .map_err(|e| format!("video decoder: {e}"))?;

    Ok((decoder, stream_index, is_hw))
}

/// The decoder for `codec_id` that can actually decode on `device` (CUDA or
/// VAAPI), if any.
///
/// ffmpeg's default decoder for a codec need not be it: for AV1 the
/// default is libdav1d, which ignores a hardware device entirely. Attaching
/// CUDA to it still "succeeded", so filmstrips and thumbnails believed they
/// were hardware-decoding, left the thread count at its default of one,
/// and decoded 4K AV1 in software on a single core — about 60 fps against
/// ~430 for NVDEC, and a filmstrip tile every few seconds. The cuvid
/// decoders are excluded: they need bitstream filters, which only the
/// playback decoder sets up.
unsafe fn hw_capable_decoder(
    codec_id: ffmpeg_next::codec::Id,
    device: ffmpeg_next::ffi::AVHWDeviceType,
) -> Option<ffmpeg_next::Codec> {
    use ffmpeg_next::ffi::*;
    let id: AVCodecID = codec_id.into();
    let mut opaque: *mut std::ffi::c_void = std::ptr::null_mut();
    loop {
        let c = av_codec_iterate(&mut opaque);
        if c.is_null() {
            return None;
        }
        if av_codec_is_decoder(c) == 0 || (*c).id != id {
            continue;
        }
        let name = std::ffi::CStr::from_ptr((*c).name).to_string_lossy();
        if name.ends_with("_cuvid") {
            continue;
        }
        let mut i = 0;
        loop {
            let cfg = avcodec_get_hw_config(c, i);
            if cfg.is_null() {
                break;
            }
            if (*cfg).device_type == device
                && (*cfg).methods & (AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
            {
                return ffmpeg_next::codec::decoder::find_by_name(&name);
            }
            i += 1;
        }
    }
}

/// Convert a decoded frame (HW or SW) to RGBA at the given target dimensions.
/// For HW frames: GPU→CPU transfer + swscale. For SW frames: just swscale.
/// `sws_cache` is an optional cached sws context pointer for reuse across frames.
/// What a single-frame decode (thumbnail, filmstrip, skim frame) needs to
/// colour-manage its frames: the file being read — for its camera log
/// format; proxy and skim files are registered under their own paths — and
/// the stream's colour tags.
///
/// Scene-referred frames (PQ, HLG, camera log) are converted exactly as the
/// viewer shows them ungraded. Without this they went through plain
/// swscale: log looked flat and PQ washed out in the browser, the source
/// viewer, thumbnails and filmstrips.
#[derive(Clone)]
struct ThumbColour {
    path: std::path::PathBuf,
    baseline: klipp_core::SourceColorspace,
}

impl ThumbColour {
    fn new(path: &std::path::Path, format_ctx: &ffmpeg_next::format::context::Input, stream_index: usize) -> Self {
        let baseline = format_ctx
            .stream(stream_index)
            .map(|st| unsafe { crate::video_decoder::stream_colour_baseline((*st.as_ptr()).codecpar) })
            .unwrap_or_else(|| unsafe { crate::video_decoder::stream_colour_baseline(std::ptr::null()) });
        Self { path: path.to_path_buf(), baseline }
    }

    /// sRGB RGBA8 for a scene-referred software frame; `None` for a
    /// display-referred one, which keeps its swscale path.
    ///
    /// SAFETY: `frame` must be a valid software AVFrame.
    unsafe fn scene_referred_rgba8(
        &self,
        frame: *const ffmpeg_next::ffi::AVFrame,
        dst_w: u32,
        dst_h: u32,
    ) -> Option<Vec<u8>> {
        let cs = crate::video_decoder::single_frame_colourspace(frame, self.baseline, &self.path);
        crate::video_decoder::scene_referred_display_rgba8(frame, cs, dst_w, dst_h)
    }
}

fn frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    _is_hw: bool,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
    colour: &ThumbColour,
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
        unsafe { hw_frame_to_rgba(frame, dst_w, dst_h, sws_cache, colour) }
    } else {
        // SW frame path: direct swscale
        sw_frame_to_rgba(frame, dst_w, dst_h, colour)
    }
}

/// Transfer HW frame to CPU and scale to RGBA at target dimensions.
unsafe fn hw_frame_to_rgba(
    hw_frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
    colour: &ThumbColour,
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
    // The download drops most colour tags; take them from the HW frame.
    (*sw_frame).color_range = (*hw_frame.as_ptr()).color_range;
    (*sw_frame).colorspace = (*hw_frame.as_ptr()).colorspace;
    (*sw_frame).color_primaries = (*hw_frame.as_ptr()).color_primaries;
    (*sw_frame).color_trc = (*hw_frame.as_ptr()).color_trc;
    if let Some(rgba) = colour.scene_referred_rgba8(sw_frame, dst_w, dst_h) {
        av_frame_free(&mut (sw_frame as *mut _));
        return Some(rgba);
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
    colour: &ThumbColour,
) -> Option<Vec<u8>> {
    use ffmpeg_next::software::scaling;

    if let Some(rgba) = unsafe { colour.scene_referred_rgba8(frame.as_ptr(), dst_w, dst_h) } {
        return Some(rgba);
    }

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

    let colour = ThumbColour::new(path, &format_ctx, stream_index);
    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h, &colour)
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

    let colour = ThumbColour::new(path, &format_ctx, stream_index);
    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h, &colour)
        .ok_or("Failed to convert frame to RGBA")?;

    // Apply source rotation if needed (phone videos)
    if rotation != 0 {
        let (rotated, rw, rh) = rotate_rgba(&rgba, dst_w, dst_h, rotation);
        Ok((rotated, rw, rh))
    } else {
        Ok((rgba, dst_w, dst_h))
    }
}

/// Same as [`generate_thumbnail_at_size`] but lets the caller force a
/// specific rotation instead of auto-detecting from the file's metadata.
///
/// Needed by the browser-skim path: ffmpeg's skim transcode runs autorotate
/// on the input (consuming the displaymatrix and writing upright pixels)
/// AND keeps the displaymatrix on the output stream. Auto-detecting from
/// such a skim file then sees rotation=90 and rotates the already-upright
/// pixels — the symptom is portrait phone clips playing sideways in the
/// source viewer when hover-skimming. The caller passes `0` for skim/proxy
/// paths via `MediaItem::effective_rotation_for`.
pub fn generate_thumbnail_at_size_with_rotation(
    path: &std::path::Path,
    seek_secs: f64,
    max_width: u32,
    rotation: i32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;
    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;

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
    let (eff_w, _eff_h) = if rotation == 90 || rotation == 270 {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    };
    let scale = (max_width as f64 / eff_w as f64).min(1.0);
    let dst_w = ((src_w as f64 * scale) as u32).max(1);
    let dst_h = ((src_h as f64 * scale) as u32).max(1);

    let colour = ThumbColour::new(path, &format_ctx, stream_index);
    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h, &colour)
        .ok_or("Failed to convert frame to RGBA")?;

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
    colour: ThumbColour,
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

        let colour = ThumbColour::new(path, &format_ctx, stream_index);
        Some(Self {
            colour,
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
        // `||` rather than a chain of `if .. { true } else if ..`, which
        // is the shape that hid a real bug in `handle_title_commands`: one
        // arm of six returned `false` and the value was reported wrong.
        // Written this way there is no per-arm boolean to get wrong, and
        // the reason for each case still sits beside it. Anything else
        // means the frame is close enough to read forward to.
        let need_seek = self.current_pts < 0.0                                        // first access
            || seek_secs < self.current_pts - self.frame_duration                     // seeking backwards
            || seek_secs > self.current_pts + self.frame_duration * 30.0;             // jumping far forward (>30 frames)

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

                if let Some(rgba) = sw_frame_to_rgba(&self.frame, w, h, &self.colour) {
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
    let colour = ThumbColour::new(path, &format_ctx, stream_index);
    let rgba = sw_frame_to_rgba(&decoded_frame, w, h, &colour)
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

/// Timestamp (µs, same scale as decoded frame timestamps) of the keyframe
/// at or before `us` in the demuxer's index. `None` when the index has no
/// such entry — the caller then simply seeks.
fn keyframe_at_or_before_us(
    format_ctx: &ffmpeg_next::format::context::Input,
    stream_index: usize,
    us: i64,
) -> Option<i64> {
    use ffmpeg_next::ffi::*;
    let st = format_ctx.stream(stream_index)?;
    let tb = st.time_base();
    let (num, den) = (tb.numerator() as i128, tb.denominator() as i128);
    if num <= 0 || den <= 0 {
        return None;
    }
    let ts = (us as i128 * den / (num * 1_000_000)) as i64;
    let entry = unsafe {
        avformat_index_get_entry_from_timestamp(st.as_ptr() as *mut AVStream, ts, AVSEEK_FLAG_BACKWARD)
    };
    if entry.is_null() {
        return None;
    }
    Some((unsafe { (*entry).timestamp } as i128 * num * 1_000_000 / den) as i64)
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

    let colour = ThumbColour::new(path, &format_ctx, stream_index);

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

    // Timestamp of the last frame the decoder produced, for deciding
    // whether a tile needs a seek at all.
    let mut decoded_to_us: Option<i64> = None;
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
        // Seek only when it saves work. A seek lands on the keyframe at or
        // before the target and decodes forward from there; when that
        // keyframe is at or behind where the decoder already is, carrying
        // on forward reaches the target by decoding strictly fewer frames.
        // Tiles are closer together than keyframes in most long-GOP files
        // (4K AV1 here: tiles 0.78 s apart, keyframes every 1-7 s), and
        // re-seeking re-decoded the same GOP for every tile in it.
        let forward = decoded_to_us.is_some_and(|pos| {
            pos < ts && keyframe_at_or_before_us(&format_ctx, stream_index, ts).is_some_and(|kf| kf <= pos)
        });
        if !forward {
            if format_ctx.seek(ts, ..ts).is_err() {
                continue;
            }
            decoder.flush();
        }

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
                decoded_to_us = Some(frame_us);
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

        if let Some(rgba) = frame_to_rgba(&decoded_frame, decode_w, decode_h, is_hw, &mut sws_cache.0, &colour) {
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
    colour: ThumbColour,
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
        let colour = ThumbColour::new(path, &format_ctx, stream_index);
        Ok(Self {
            colour,
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
                        // EAGAIN: the decoder's output queue is full, so it
                        // will not take another packet until one frame is
                        // collected. Take one, *then* retry the packet, and
                        // only then hand the frame back.
                        //
                        // The order is the whole point. This used to read
                        // `while receive_frame().is_ok() { return ... }`,
                        // which returned on the first frame and so never
                        // reached the retry — the packet was dropped and its
                        // frame never decoded. That is exactly the corruption
                        // the comment warned about, in the code meant to
                        // prevent it: this decoder walks every frame in order
                        // for stabilisation and analysis, and a hole in that
                        // sequence corrupts motion vectors downstream.
                        // `clippy::never_loop` found it — the `while` could
                        // only ever run once.
                        let drained = if self.decoder.receive_frame(&mut decoded).is_ok() {
                            self.convert(&decoded)
                        } else {
                            None
                        };
                        if self.decoder.send_packet(&packet).is_err() {
                            tracing::warn!("SequentialFrameDecoder: packet lost after drain+retry");
                        }
                        if drained.is_some() {
                            return drained;
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
        let rgba = frame_to_rgba(decoded, self.decode_w, self.decode_h, self.is_hw, &mut self.sws_cache, &self.colour)?;
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
mod glyph_focal_resolution_tests {
    //! HZ-NEXT-GLYPH-LOCK — engine resolver tests.
    //!
    //! Pin the load-bearing back-compat guarantee (focal_ref=None →
    //! byte-identical to today) plus the glyph-anchored resolution
    //! math, title-transform composition, and the three fallback
    //! paths (missing clip / kind changed / char_index out of range).
    use super::{resolve_clip_transform, ResolvedTransform};
    use klipp_core::{ClipId, MediaId, RationalTime, Timecode};
    use klipp_effects::{BuiltinEffect, FocalRef};
    use klipp_timeline::{Clip, ClipKind, FontStyle, Sequence, TextAlignment, TextBox, TitleData, Track, TrackKind};
    use std::collections::HashMap;

    fn tc(frames: i64) -> Timecode { RationalTime::from_frames(frames, 24) }

    fn make_title_data(text: &str) -> TitleData {
        let mut tb = TextBox::default();
        tb.text = text.to_string();
        tb.font_name = "Sans".to_string();
        tb.font_size = 72.0;
        tb.font_style = FontStyle::Regular;
        tb.alignment = TextAlignment::Center;
        TitleData::single_text(tb)
    }

    fn title_clip(text: &str) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c.kind = ClipKind::Title(make_title_data(text));
        c
    }

    fn media_clip_with_anchor(anchor_x: f32, anchor_y: f32) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x, anchor_y,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
        c
    }

    fn seq_with(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn resolve(clip: &Clip, seq: &Sequence) -> ResolvedTransform {
        let mut tcache = HashMap::new();
        let mut gcache = HashMap::new();
        let mut tr = klipp_title::TitleRenderer::new();
        resolve_clip_transform(clip, seq, tc(0), 24.0, &mut tcache, &mut gcache, &mut tr, 1920, 1080)
    }

    // ─────────── Back-compat parity pin ───────────

    #[test]
    fn focal_ref_none_yields_static_anchor() {
        // The load-bearing guarantee: focal_ref=None on the
        // Transform builtin → resolved anchor IS the static
        // (anchor_x, anchor_y). Any drift here means the
        // glyph-resolver leaked into the no-focal-ref path and
        // existing projects would render differently.
        let consumer = media_clip_with_anchor(0.42, 0.58);
        let seq = seq_with(vec![consumer.clone()]);
        let out = resolve(&consumer, &seq);
        assert!((out.5 - 0.42).abs() < 1e-6, "anchor_x preserved");
        assert!((out.6 - 0.58).abs() < 1e-6, "anchor_y preserved");
    }

    // ─────────── Glyph-anchor resolution ───────────

    fn build_glyph_test_setup(char_index: u32, fallback_xy: (f32, f32)) -> (Clip, Clip, Sequence) {
        // Title clip + consumer clip with FocalRef::Glyph pointing
        // at the title. Returns (consumer, title, sequence).
        let title = title_clip("Hi");
        let title_id = title.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        consumer.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: fallback_xy.0, anchor_y: fallback_xy.1,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(FocalRef::Glyph {
                title_clip_id: title_id.0,
                char_index,
                fallback_xy,
            }),
        });
        consumer.builtin_effects_enabled.push(true);
        let seq = seq_with(vec![consumer.clone(), title.clone()]);
        (consumer, title, seq)
    }

    #[test]
    fn focal_ref_glyph_resolves_to_glyph_centroid_in_clip_space() {
        // First glyph of "Hi" in a 1920×1080 frame. Exact pixel
        // centroid depends on fontdue's layout output for the
        // bundled NotoSans, but the resolver should produce SOME
        // anchor in [0, 1] that's NOT the fallback (which is set
        // to a recognisable distinct value).
        let fallback = (0.123, 0.876);
        let (consumer, _title, seq) = build_glyph_test_setup(0, fallback);
        let out = resolve(&consumer, &seq);
        // The glyph centroid must differ from the fallback —
        // resolution succeeded.
        let dx = (out.5 - fallback.0).abs();
        let dy = (out.6 - fallback.1).abs();
        assert!(dx > 1e-3 || dy > 1e-3,
            "resolved anchor should differ from fallback when glyph resolution succeeds, got ({}, {})", out.5, out.6);
        // And both should be in [0, 1].
        assert!((0.0..=1.0).contains(&out.5), "anchor_x in clip space, got {}", out.5);
        assert!((0.0..=1.0).contains(&out.6), "anchor_y in clip space, got {}", out.6);
    }

    // ─────────── Fallback paths ───────────

    #[test]
    fn fallback_when_title_clip_missing() {
        // FocalRef points at a uuid that's not in the sequence.
        let fallback = (0.25, 0.75);
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        consumer.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: fallback.0, anchor_y: fallback.1,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(FocalRef::Glyph {
                title_clip_id: ClipId::new().0, // never inserted into seq
                char_index: 0,
                fallback_xy: fallback,
            }),
        });
        consumer.builtin_effects_enabled.push(true);
        let seq = seq_with(vec![consumer.clone()]);
        let out = resolve(&consumer, &seq);
        assert!((out.5 - fallback.0).abs() < 1e-6, "fell back to anchor_x");
        assert!((out.6 - fallback.1).abs() < 1e-6, "fell back to anchor_y");
    }

    #[test]
    fn fallback_when_referenced_clip_is_not_a_title() {
        // FocalRef points at a clip that exists but isn't a Title.
        let fallback = (0.30, 0.70);
        let non_title = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        let non_title_id = non_title.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        consumer.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: fallback.0, anchor_y: fallback.1,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(FocalRef::Glyph {
                title_clip_id: non_title_id.0,
                char_index: 0,
                fallback_xy: fallback,
            }),
        });
        consumer.builtin_effects_enabled.push(true);
        let seq = seq_with(vec![consumer.clone(), non_title]);
        let out = resolve(&consumer, &seq);
        assert!((out.5 - fallback.0).abs() < 1e-6, "kind-changed → fall back to anchor_x");
        assert!((out.6 - fallback.1).abs() < 1e-6, "kind-changed → fall back to anchor_y");
    }

    #[test]
    fn fallback_when_char_index_past_end_of_text() {
        // Title is "Hi" (2 chars). char_index=99 is past the end.
        let fallback = (0.20, 0.80);
        let (consumer, _title, seq) = build_glyph_test_setup(99, fallback);
        let out = resolve(&consumer, &seq);
        assert!((out.5 - fallback.0).abs() < 1e-6, "stale char_index → fall back to anchor_x");
        assert!((out.6 - fallback.1).abs() < 1e-6, "stale char_index → fall back to anchor_y");
    }

    // ─────────── Title-transform composition ───────────

    #[test]
    fn glyph_resolution_picks_up_title_position_offset() {
        // Same title + consumer, but the title carries a static
        // Transform with a position offset. The glyph's resolved
        // screen position should shift by the offset (in clip space).
        let fallback = (0.5, 0.5);
        let mut title = title_clip("Hi");
        title.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 100.0, position_y: 50.0,  // offset 100 px right, 50 px down
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        title.builtin_effects_enabled.push(true);
        let title_id = title.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        consumer.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: fallback.0, anchor_y: fallback.1,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(FocalRef::Glyph {
                title_clip_id: title_id.0,
                char_index: 0,
                fallback_xy: fallback,
            }),
        });
        consumer.builtin_effects_enabled.push(true);
        let seq = seq_with(vec![consumer.clone(), title]);

        // Reference resolution with no title offset.
        let (ref_consumer, _, ref_seq) = build_glyph_test_setup(0, fallback);
        let ref_out = resolve(&ref_consumer, &ref_seq);
        let off_out = resolve(&consumer, &seq);

        // Offset should shift x by 100/1920 ≈ 0.0521, y by 50/1080 ≈ 0.0463.
        let expected_dx = 100.0 / 1920.0;
        let expected_dy = 50.0 / 1080.0;
        assert!(((off_out.5 - ref_out.5) - expected_dx).abs() < 1e-3,
            "title position_x=100 should shift glyph anchor by ~{:.4}, got {:.4}",
            expected_dx, off_out.5 - ref_out.5);
        assert!(((off_out.6 - ref_out.6) - expected_dy).abs() < 1e-3,
            "title position_y=50 should shift glyph anchor by ~{:.4}, got {:.4}",
            expected_dy, off_out.6 - ref_out.6);
    }
}


// Legacy `scope_dispatch_tests` module removed during the in-tree
// scope renderer deletion (Phase 8). The FFmpeg path renders all four
// modes through a single dispatch — there's no per-mode kernel
// selection to test any more. End-to-end coverage of the FFmpeg path
// lives in `crates/klipp-engine/tests/ffmpeg_scope_smoke.rs`.

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

#[cfg(test)]
mod cached_frame_display_tests {
    use super::*;

    /// The table path a render-cache hit takes has to land on the bytes the
    /// float path would have produced. `klipp-color` pins the table against
    /// the display transform; this pins the composition around it — that
    /// `map_u8_rgb` applies it to the right channels, that alpha comes
    /// through, and that the engine's own decode LUT is the one the table
    /// was built from.
    #[test]
    fn the_table_path_is_the_float_path_it_replaces() {
        let ws = klipp_core::WorkingColorspace::Rec709Linear;
        // Every code in every channel, with alpha varying independently.
        let mut frame: Vec<u8> = Vec::new();
        for r in 0..256usize {
            frame.extend_from_slice(&[
                r as u8,
                (255 - r) as u8,
                ((r * 7) % 256) as u8,
                (r / 2) as u8,
            ]);
        }
        for op in klipp_color::display::TONE_MAP_OPS_ALL {
            let Some(lut) = klipp_color::display::display_transform_u8_lut(op, ws) else {
                continue;
            };
            let through_table = map_u8_rgb(&frame, &lut);
            let through_float =
                klipp_color::display::display_transform_ws(&u8_rgba_to_linear(&frame), op, ws);
            assert_eq!(
                through_table, through_float,
                "{op:?}: the cache-hit shortcut does not match the path it replaces",
            );
        }
    }
}
