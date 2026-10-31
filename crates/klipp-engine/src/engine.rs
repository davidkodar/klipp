use crate::audio_decoder::AudioDecoder;
use klipp_timeline::GradeAccess;
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

mod delivery;
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) mod inject;
mod render_job;
mod seek_lead;
mod reverse_gop;
mod skim_render;
pub use skim_render::{SKIMS_COMPOSITED, SKIMS_DECLINED, SKIM_SOURCES_OFF_GPU};
mod streams;
mod view;

/// Wrapper to make AudioOutput Send. The cpal::Stream inside is !Send
/// due to platform internals, but we only ever use it on the engine thread.
struct SendAudioOutput(#[allow(dead_code)] AudioOutput);
unsafe impl Send for SendAudioOutput {}

fn fade_curve_to_u32(c: klipp_timeline::FadeCurve) -> u32 {
    match c {
        // Kept exhaustive rather than delegating, so a new shape is a
        // compile error here too and not a silent 0.
        klipp_timeline::FadeCurve::Linear => 0,
        klipp_timeline::FadeCurve::Exponential => 1,
        klipp_timeline::FadeCurve::Logarithmic => 2,
        klipp_timeline::FadeCurve::SCurve => 3,
        klipp_timeline::FadeCurve::ConstantPower => 4,
    }
}

fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
        klipp_timeline::ChannelConfig::Surround51 => 3,
        klipp_timeline::ChannelConfig::Surround71 => 4,
        klipp_timeline::ChannelConfig::LeftOnly => 5,
        klipp_timeline::ChannelConfig::RightOnly => 6,
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
    klipp_effects::residency::record("display.cached_lut", "Cached display pixels transformed on CPU", klipp_effects::residency::Work::Fallback);
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

/// A rendered (graded, composited, generated) frame as the render cache
/// stores it: sRGB-encoded as it stands, rounded with the viewer's dither
/// (#2072). A cache hit in a Rec.709 project with no tone map shows these
/// bytes through an identity table, so they have to be the bytes the
/// viewer's encode gave the frame when it was drawn — dithered the same
/// way — or the hit would show the frame's gradients banded where the
/// render showed them smooth. The decoder's frames go through
/// [`linear_rgba_to_u8`] instead: they were 8-bit to begin with, and the
/// dither leaves a value already on a code alone.
fn rendered_frame_to_cache_u8(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_dithered_to(
        linear,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    )
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
    /// [`audio_settings_fingerprint_for`] of the settings the running
    /// chain has, so a change is handed to it once.
    settings_fingerprint: u64,
    source: AudioSource,
    /// The volume, gain, pan and channel config of the clip it plays,
    /// applied to this source's samples alone in the mix thread.
    mix: std::sync::Arc<crate::mix_thread::SourceMix>,
    /// Where its clip starts, for a source opened ahead of it to start
    /// on its own sample (#1588); `None` for one opened under its clip.
    starts_at: Option<Timecode>,
    /// Whether its effects can ring on past its clip's end (#2780): a
    /// PCM feeder's, with a tail or keyframes that could give one.
    may_ring: bool,
    /// The clip it last played, its out point, and where its sound ends
    /// on the timeline in seconds (past a crossfade's handle): a source
    /// retired there rings on; one whose clip was edited stops.
    clip: Option<(klipp_core::ClipId, Timecode, f64)>,
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

/// How long the playhead must rest before a paused frame drawn from the
/// skim substitute is redrawn from the original media: a seek or a
/// scrub's release moves it again within this. A pause from playback
/// rests where it stops, so it doesn't wait (#2136).
const PARK_DEBOUNCE: Duration = Duration::from_millis(400);

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

/// Whether the GPU stack may write the viewer's texture and skip the
/// readback: the UI's own wgpu device is the shared one (see
/// `GpuContext::install_shared`), and `KLIPP_GPU_VIEWER=0` has not
/// closed the hatch. On two devices the UI could not sample a texture
/// the engine wrote, so the frame comes back as bytes as before.
fn should_publish_gpu_viewer() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        if std::env::var("KLIPP_GPU_VIEWER").ok().as_deref() == Some("0") {
            tracing::info!("KLIPP_GPU_VIEWER=0: the viewer takes display bytes");
            return false;
        }
        let shared = klipp_effects::gpu_context::GpuContext::shared_is_ui_device();
        if !shared {
            tracing::info!("GPU viewer off: the engine and the UI are on separate wgpu devices");
        }
        shared
    })
}

/// Kill switch for the direct-present fast path (see
/// `PlaybackEngine::direct_present_blocker`). On by default; set
/// `KLIPP_DIRECT_PRESENT=0` to force every frame through the full
/// grade + composite pipeline, which is the way to check whether a
/// suspected display problem comes from the fast path.
/// What the pre-grade frame cache depends on: which media plays at each
/// playhead position, and how it is read. A grade is not in it — the
/// cache holds frames before the grade — so grading leaves the cache
/// standing.
/// What the video tracks hold at `tc`: which clips, of which file, at
/// which point of it. An edit that leaves this alone need not decode.
fn under_playhead_fingerprint(seq: &klipp_timeline::Sequence, tc: Timecode) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        if track.kind != klipp_timeline::TrackKind::Video {
            continue;
        }
        let Some((ci, clip)) = track.clip_at_timecode(tc) else { continue };
        (ti, ci, track.visible).hash(&mut h);
        clip.id.hash(&mut h);
        clip.media_id().hash(&mut h);
        clip.enabled.hash(&mut h);
        (clip.timeline_in.value, clip.timeline_in.rate).hash(&mut h);
        (clip.source_in.value, clip.source_in.rate).hash(&mut h);
        clip.speed.to_bits().hash(&mut h);
        hash_speed_keyframes(&clip.speed_keyframes, &mut h);
        hash_multicam(clip, &mut h);
        clip.source_rotation.hash(&mut h);
        std::mem::discriminant(&clip.kind).hash(&mut h);
    }
    h.finish()
}

/// A speed ramp's shape: every keyframe's time, value, interpolation,
/// tangents and handle kind. A drag
/// on one keyframe changes which source frame each playhead shows
/// without changing how many keyframes there are (#423).
fn hash_speed_keyframes(keyframes: &[klipp_core::Keyframe], h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    keyframes.len().hash(h);
    for k in keyframes {
        k.time_secs.to_bits().hash(h);
        k.value.to_bits().hash(h);
        std::mem::discriminant(&k.interpolation).hash(h);
        if let klipp_core::Interpolation::Eased(ease) = k.interpolation {
            ease.hash(h);
        }
        (k.tangent_out.0.to_bits(), k.tangent_out.1.to_bits()).hash(h);
        (k.tangent_in.0.to_bits(), k.tangent_in.1.to_bits()).hash(h);
        // Only a kind other than Broken, so a ramp's hash is as it was
        // before keys had kinds.
        if !k.handles.is_broken() {
            k.handles.hash(h);
        }
    }
}

/// A clip's place in a multicam group and, on the primary, its cut list:
/// which angle's file the frame under the playhead comes from. A switch
/// changes the cut list alone, and a parked frame fingerprinted without it
/// stayed the old camera's until something else made the engine look.
fn hash_multicam(clip: &klipp_timeline::Clip, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    clip.multicam_group.hash(h);
    clip.multicam_angle.hash(h);
    clip.multicam_cuts.len().hash(h);
    for (at, angle) in &clip.multicam_cuts {
        (at.to_bits(), angle).hash(h);
    }
}

/// Where a decoded frame of `clip`, at source time `pts_secs`, sits on
/// the timeline: the inverse of `klipp_compose::render::source_secs`,
/// through the clip's speed curve with the compositor's integration step.
/// Dividing by the base speed put a frame of a ramped clip at the wrong
/// playhead, and cached it there (#423).
fn timeline_for_source(clip: &klipp_timeline::Clip, pts_secs: f64, fps: f64, rate: u32) -> RationalTime {
    clip.timeline_in + RationalTime::from_secs_f64(clip.elapsed_for_source_secs(pts_secs, fps), rate)
}

fn timeline_layout_fingerprint(seq: &klipp_timeline::Sequence) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seq.fps.num.hash(&mut h);
    seq.fps.den.hash(&mut h);
    for (ti, track) in seq.tracks.iter().enumerate() {
        if track.kind != klipp_timeline::TrackKind::Video {
            continue;
        }
        ti.hash(&mut h);
        track.visible.hash(&mut h);
        for clip in &track.clips {
            clip.id.hash(&mut h);
            clip.media_id().hash(&mut h);
            clip.enabled.hash(&mut h);
            (clip.timeline_in.value, clip.timeline_in.rate).hash(&mut h);
            (clip.timeline_out.value, clip.timeline_out.rate).hash(&mut h);
            (clip.source_in.value, clip.source_in.rate).hash(&mut h);
            clip.speed.to_bits().hash(&mut h);
            hash_speed_keyframes(&clip.speed_keyframes, &mut h);
            hash_multicam(clip, &mut h);
            clip.source_rotation.hash(&mut h);
            std::mem::discriminant(&clip.kind).hash(&mut h);
        }
    }
    h.finish()
}

/// Which use of the direct-present fast path is being gated; see
/// [`PlaybackEngine::fast_path_blockers_for`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FastPath {
    /// Steady-state playback: decoded frames straight to the viewer.
    Playing,
    /// A slider step on the parked frame, graded on the GPU.
    ParkedDrag,
    /// A paused present — a seek, a scrub step, a skim — of one clip alone:
    /// shown as its decoded bytes when the renderer would only pass it
    /// through, graded on the GPU from its pre-grade when only the graph
    /// stands between.
    PausedPresent,
}

fn direct_present_enabled() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| std::env::var("KLIPP_DIRECT_PRESENT").ok().as_deref() != Some("0"))
}

/// How long before a shot's end the next shot on its track is opened and
/// seeked, so the cut finds its first frame decoded.
const PREROLL_LEAD_SECS: f64 = 1.5;

/// Common write of a freshly-rendered scope texture into the shared
/// Display bytes for the scopes: the codes on screen, and the luminance
/// weights of the primaries they are in, for their Y′.
pub(crate) struct DisplayScopeFrame<'a> {
    pub rgba: &'a [u8],
    pub luma: [f32; 3],
}

impl<'a> DisplayScopeFrame<'a> {
    /// sRGB / Rec.709 bytes: a skim or scrub frame, a decoder's own
    /// picture, an ACES ODT's output.
    pub fn rec709(rgba: &'a [u8]) -> Self {
        Self { rgba, luma: klipp_color::primaries::luminance_weights(klipp_core::WorkingColorspace::Rec709Linear) }
    }

    /// Bytes in the working space's primaries, as the GPU grade encodes
    /// them for the screen.
    pub fn working(rgba: &'a [u8]) -> Self {
        Self { rgba, luma: klipp_color::primaries::luminance_weights(crate::video_decoder::working_colorspace()) }
    }
}

/// The longest the scopes wait between passes, however cheap or costly.
/// The longest the scopes may go unrefreshed, and so the point at which
/// the one-third rule in [`Engine::should_render_scope`] stops being
/// honoured.
///
/// This was 100 ms, which quietly inverted the pacing it was meant to
/// provide. The interval is twice the last pass's cost — 33% duty — but
/// capped here, so the cap binds as soon as a pass costs more than
/// 50 ms and the duty climbs instead: a 200 ms pass ran every 300 ms
/// (66%), and the 1 s passes seen while skimming ran every 1.1 s (90%).
/// The engine then had 10% of itself left, the UI painted at 1 fps and
/// input arrived 376 events to a frame, while audio — on its own
/// thread — carried on, which is exactly what a frozen GUI with working
/// skim audio looks like.
///
/// Two seconds lets the backoff actually back off across every cost the
/// scopes realistically reach, at the price of scopes that are a couple
/// of seconds stale when a pass is that expensive. Stale scopes on a
/// responsive app beat live scopes on a frozen one.
const SCOPE_MAX_INTERVAL: Duration = Duration::from_secs(2);

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

/// A decoded frame as the pre-grade cache holds it, its picture read
/// back first where it is on the GPU only. Cached as it came, a GPU frame
/// was an empty picture, and a seek onto it later showed nothing (#2803
/// stopped the backward steps filling those frames first).
fn cached_pregrade(frame: &mut crate::types::LinearFrame) -> Pregrade {
    frame.realise_hdr_on_cpu();
    if frame.hdr.is_none() {
        frame.ensure_cpu_data();
    }
    Pregrade::from_frame(frame)
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
    /// The decoded frame's presentation time, when known: what a held
    /// output of the frame is keyed by (#1863).
    pts: Option<f64>,
}

impl Pregrade {
    fn new(pixels: PregradePixels) -> Self {
        Self { pixels, display: Arc::new(std::sync::OnceLock::new()), pts: None }
    }

    fn from_frame(frame: &crate::types::LinearFrame) -> Self {
        Self::new(PregradePixels::from_frame(frame)).with_pts(Some(frame.pts_secs()))
    }

    /// The same, of the frame at `pts`.
    fn with_pts(mut self, pts: Option<f64>) -> Self {
        self.pts = pts;
        self
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
                    let mut linear = codes.to_scene_linear_rgba(h.transfer, h.source_to_working);
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

/// The pre-grade frames the viewer has shown or read ahead, keyed by
/// playhead position: a step or a seek back onto one is shown without
/// decoding. Bounded by an entry count and by bytes, oldest out first —
/// a 4K frame is 33 MB, and the old fixed ring of 120 slots could hold
/// 4 GB of them.
///
/// Kept across seeks. It used to be cleared on every seek, so scrubbing
/// back over frames just shown decoded them all again; only stepping
/// gained from it.
struct FrameCache {
    entries: HashMap<i64, FrameCacheEntry>,
    /// Insertion order, oldest first, for eviction.
    order: std::collections::VecDeque<i64>,
    capacity: usize,
    budget_bytes: usize,
    bytes: usize,
}

struct FrameCacheEntry {
    pixels: Pregrade,    // pre-grade pixels (Arc-shared either way)
    /// The clip the primary decoder was reading. Inside a transition the
    /// same playhead has two: playing through a cut keeps the outgoing
    /// clip on the primary decoder, running into its handle, while a seek
    /// there opens the incoming one. A frame of one served as the other
    /// put the wrong side of a dissolve on screen (#411).
    clip: Option<klipp_core::ClipId>,
    width: u32,
    height: u32,
    bytes: usize,
}


/// A pending eyedropper read: the inspected render taps `tap`, the
/// output of the node feeding the asked node, and reads it at `(nx, ny)`.
/// `clip` and `node` are the asker's names for the node, given back with
/// the answer.
#[derive(Debug, Clone)]
pub(crate) struct NodeSample {
    pub(crate) clip: klipp_core::ClipId,
    pub(crate) node: klipp_nodes::NodeId,
    pub(crate) tap: klipp_compose::inspect::InspectAt,
    pub(crate) nx: f32,
    pub(crate) ny: f32,
}

/// Whether `clip` draws unmoved at `at`: its own transform the identity,
/// no link to another's, no tilt or corner pin and no sequence camera
/// moving it, so a point of the frame is the same point of its picture.
fn sits_unmoved(clip: &klipp_timeline::Clip, seq: &klipp_timeline::Sequence, at: Timecode) -> bool {
    let ctx = klipp_compose::eval::eval_context(clip, at, seq.fps.as_f64());
    let (px, py, sx, sy, rot, _, _) = klipp_compose::eval::evaluate_clip_own_transform(clip, ctx.clip_time_secs, &ctx);
    let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
    near(px, 0.0) && near(py, 0.0) && near(sx, 1.0) && near(sy, 1.0) && near(rot, 0.0)
        && clip.transform_link.is_none()
        && !clip.has_nonidentity_placement_entries()
        && !clip.has_tilt_keyframes()
        && (seq.camera.is_still() || clip.depth <= 0.0)
}


/// A gap between shots up to this many frames long is cut across rather
/// than played: a frame's slip in an edit, not a pause (#546).
const GAP_AS_CUT_FRAMES: f64 = 1.5;

/// The enabled title clips starting within `ahead` seconds of `now`
/// (or just started), on a title track or among the shots of a video
/// track (#2668): their resting render is made ahead of their cut.
fn titles_starting(seq: &klipp_timeline::Sequence, now: f64, ahead: f64) -> Vec<(&klipp_timeline::Clip, &klipp_timeline::TitleData)> {
    use klipp_timeline::TrackKind;
    seq.tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Text | TrackKind::Video))
        .flat_map(|t| &t.clips)
        .filter_map(|clip| {
            let klipp_timeline::ClipKind::Title(td) = &clip.kind else { return None };
            let start = clip.timeline_in.to_secs_f64();
            (clip.enabled && start >= now - 0.1 && start <= now + ahead).then_some((clip, td))
        })
        .collect()
}

/// How the shot after a gap was taken over: from its preroll, as a cut
/// is, or by a seek. For tests and the perf log.
static GAP_HAND_OVERS_WARM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GAP_HAND_OVERS_COLD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Gaps across which the playing decoder went on to the shot after
/// them, its file going on forward from where the decoder was (#2376).
static DECODERS_HANDED_ON: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many gaps the playing decoder was handed on across since the
/// process began: a strobe's pieces, one shot of a file cut with
/// frames left out.
pub fn decoders_handed_on() -> u64 {
    DECODERS_HANDED_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// The longest gap the playing decoder is handed on across, in frames:
/// it decodes through the gap's frames, which are dropped as the next
/// shot starts.
const HAND_ON_GAP_FRAMES: f64 = 12.0;

/// How far on in the same file a cut may land and still be decoded on
/// by the decoder playing the shot before it, rather than another one
/// opened and sought for it (#2397). Decoding on costs a frame per frame
/// skipped, at the cut; a seek costs every frame from the keyframe
/// before, which a preroll pays ahead of time.
pub(crate) const DECODE_ON_CUT_SECS: f64 = 0.5;

/// Whether the shot `next` goes on from `cur` in the same file, at most
/// [`DECODE_ON_CUT_SECS`] further on than where `cur` ends: a rippled
/// strobe's pieces, a jump cut with frames taken out (#2397). Its cut is
/// then the playing decoder's, decoding on, and nothing is opened ahead
/// for it. Across a gap, only the shot going on exactly from where `cur`
/// stopped, as far as the gap is long, whose decoder is handed on
/// through the gap (#2376). Never across a transition on `track`, which
/// shows both sides at once.
pub(crate) fn decodes_on(track: &klipp_timeline::Track, cur: &klipp_timeline::Clip, next: &klipp_timeline::Clip, frame_secs: f64) -> bool {
    let plain = |c: &klipp_timeline::Clip| !c.is_reversed() && c.speed_keyframes.is_empty();
    if cur.media_id().is_none() || cur.media_id() != next.media_id() || !plain(cur) || !plain(next) {
        return false;
    }
    if track.transitions.iter().any(|t| t.clip_a_id == cur.id || t.clip_b_id == next.id) {
        return false;
    }
    if (cur.speed - next.speed).abs() > 1e-6 || cur.source_rotation != next.source_rotation {
        return false;
    }
    let gap = next.timeline_in.to_secs_f64() - cur.timeline_out.to_secs_f64();
    let on = next.source_in.to_secs_f64() - cur.source_out.to_secs_f64() - gap.max(0.0) * cur.speed;
    if gap > frame_secs * GAP_AS_CUT_FRAMES {
        gap <= frame_secs * HAND_ON_GAP_FRAMES && on.abs() < 0.5 * frame_secs
    } else {
        gap > -0.5 * frame_secs && on > -1e-3 && on <= DECODE_ON_CUT_SECS
    }
}

#[cfg(test)]
mod titles_starting_tests {
    use super::titles_starting;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

    fn title(from: f64, to: f64) -> Clip {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 60);
        Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Title".into(), ..Default::default() })), ..Clip::gap(tc(from), tc(to)) }
    }

    /// #2668: a title cut into a video track's shots is made ahead of its
    /// cut as one on a title track is; a disabled one, one already well
    /// under way or one further off than the look-ahead is not.
    #[test]
    fn titles_on_title_and_video_tracks_are_made_ahead() {
        let mut seq = Sequence::new_default();
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips = vec![title(2.0, 4.0)];
        let mut video = Track::new("V1", TrackKind::Video);
        let mut off = title(3.5, 5.0);
        off.enabled = false;
        video.clips = vec![title(0.0, 1.0), title(1.5, 2.0), off, title(9.0, 10.0)];
        seq.tracks = vec![text, video];
        let starts: Vec<f64> = titles_starting(&seq, 1.0, 2.0).iter().map(|(c, _)| c.timeline_in.to_secs_f64()).collect();
        assert_eq!(starts, vec![2.0, 1.5]);
    }
}

#[cfg(test)]
mod decodes_on_tests {
    use super::*;
    use klipp_timeline::{Clip, ClipKind, Track, TrackKind, Transition};

    const FRAME: f64 = 1.0 / 25.0;

    fn tc(f: i64) -> Timecode {
        RationalTime { value: f, rate: 25 }
    }

    /// A shot of `media` at frames `at`..`at + len` of the timeline, from
    /// frame `from` of its file.
    fn shot(media: MediaId, at: i64, len: i64, from: i64) -> Clip {
        Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(from), source_out: tc(from + len), ..Clip::gap(tc(at), tc(at + len)) }
    }

    /// A shot going on a little further in the same file, where the one
    /// before ends, is decoded on into; one too far on, back, or of
    /// another file is not (#2397).
    #[test]
    fn a_cut_a_little_further_in_one_file_is_decoded_on() {
        let (a, b) = (MediaId::new(), MediaId::new());
        let track = Track::new("V1", TrackKind::Video);
        let cur = shot(a, 0, 4, 0);
        assert!(decodes_on(&track, &cur, &shot(a, 4, 4, 4), FRAME), "straight on");
        assert!(decodes_on(&track, &cur, &shot(a, 4, 4, 8), FRAME), "four frames left out");
        assert!(decodes_on(&track, &cur, &shot(a, 4, 4, 16), FRAME), "twelve frames left out, within half a second");
        assert!(!decodes_on(&track, &cur, &shot(a, 4, 4, 20), FRAME), "past half a second on");
        assert!(!decodes_on(&track, &cur, &shot(a, 4, 4, 2), FRAME), "back in the file");
        assert!(!decodes_on(&track, &cur, &shot(b, 4, 4, 8), FRAME), "another file");
        let mut faster = shot(a, 4, 4, 8);
        faster.speed = 2.0;
        assert!(!decodes_on(&track, &cur, &faster, FRAME), "another speed");
        let mut turned = shot(a, 4, 4, 8);
        turned.source_rotation = 90;
        assert!(!decodes_on(&track, &cur, &turned, FRAME), "another rotation");
    }

    /// Across a gap only a shot going on exactly from where the one before
    /// stopped, its decoder handed on through the gap (#2376).
    #[test]
    fn across_a_gap_only_a_shot_going_straight_on_is_decoded_on() {
        let a = MediaId::new();
        let track = Track::new("V1", TrackKind::Video);
        let cur = shot(a, 0, 4, 0);
        assert!(decodes_on(&track, &cur, &shot(a, 8, 4, 8), FRAME), "the gap's frames left out");
        assert!(!decodes_on(&track, &cur, &shot(a, 8, 4, 12), FRAME), "more left out than the gap is long");
        assert!(!decodes_on(&track, &cur, &shot(a, 20, 4, 20), FRAME), "a gap longer than a hand-on");
    }

    /// A transition shows both sides at once: each needs its decoder.
    #[test]
    fn a_cut_with_a_transition_is_not_decoded_on() {
        let a = MediaId::new();
        let mut track = Track::new("V1", TrackKind::Video);
        let (cur, next) = (shot(a, 0, 25, 0), shot(a, 25, 25, 30));
        assert!(decodes_on(&track, &cur, &next, FRAME));
        track.transitions.push(Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(10),
            clip_a_id: cur.id,
            clip_b_id: next.id,
            user_params: Default::default(),
            alignment: Default::default(),
        });
        assert!(!decodes_on(&track, &cur, &next, FRAME));
    }
}

/// (warm, cold) hand-overs at the end of a gap since the process began.
pub fn gap_hand_overs() -> (u64, u64) {
    (GAP_HAND_OVERS_WARM.load(std::sync::atomic::Ordering::Relaxed), GAP_HAND_OVERS_COLD.load(std::sync::atomic::Ordering::Relaxed))
}

/// Cuts to a shot on the primary track taken from its preroll, and ones
/// that opened its file at the cut; clip streams opened while playing for
/// a clip already on screen rather than before it came up (#2097). For
/// tests and the perf log.
static CUTS_FROM_PREROLL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CUTS_OPENED_COLD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STREAM_TAKE_OVERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static STREAMS_OPENED_ON_SCREEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Clip streams opened, on screen or ahead.
pub(crate) static STREAMS_OPENED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Clip streams a shot took over from the one before it in the same
/// file (#2397).
pub(crate) static STREAMS_HANDED_ON: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Layers on screen opened with a shorter queue of frames, memory being
/// short (#2128).
pub(crate) static STREAMS_SHORTENED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Layers on screen opened while their drive was already giving all the
/// viewer's streams may read from it (#2128).
pub(crate) static STREAMS_DISK_SHORT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// (from the preroll, opened cold) cuts to the next shot on the primary
/// track since the process began.
pub fn cut_openings() -> (u64, u64) {
    (CUTS_FROM_PREROLL.load(std::sync::atomic::Ordering::Relaxed), CUTS_OPENED_COLD.load(std::sync::atomic::Ordering::Relaxed))
}

/// Shots the primary decoder took over from their clip streams, rather
/// than opening their files again, since the process began.
pub fn stream_take_overs() -> u64 {
    STREAM_TAKE_OVERS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Clip streams opened while playing for a clip already on screen, since
/// the process began.
pub fn streams_opened_on_screen() -> u64 {
    STREAMS_OPENED_ON_SCREEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Different frames reversed clips' sources have handed out since the
/// process began (#2798): a frame shown again, as at a slow reversed
/// speed, isn't counted, nor are frames from the CPU reader.
pub(crate) static REVERSED_FRAMES_GIVEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// See `REVERSED_FRAMES_GIVEN`.
pub fn reversed_frames_given() -> u64 {
    REVERSED_FRAMES_GIVEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// The frames the viewer's source handed out, while kept (#2863): each
/// clip asked for, the source time asked, and the presentation time of
/// the frame given. Off until [`keep_frames_given`] turns it on: what a
/// test reads to tell a side of a transition that held one frame from one
/// that played.
static FRAMES_GIVEN: std::sync::Mutex<Option<Vec<(klipp_core::ClipId, f64, Option<f64>)>>> = std::sync::Mutex::new(None);
/// Whether [`FRAMES_GIVEN`] keeps them: read without the lock, each frame.
static KEEPING_FRAMES_GIVEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start keeping the frames the viewer's source hands out, from none.
pub fn keep_frames_given() {
    *FRAMES_GIVEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
    KEEPING_FRAMES_GIVEN.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The frames kept since [`keep_frames_given`], and stop keeping them.
pub fn frames_given() -> Vec<(klipp_core::ClipId, f64, Option<f64>)> {
    KEEPING_FRAMES_GIVEN.store(false, std::sync::atomic::Ordering::Relaxed);
    FRAMES_GIVEN.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default()
}

pub(crate) fn note_frame_given(clip: klipp_core::ClipId, asked: f64, pts: Option<f64>) {
    if !KEEPING_FRAMES_GIVEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(kept) = FRAMES_GIVEN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        kept.push((clip, asked, pts));
    }
}

/// Clip streams opened, on screen or ahead, since the process began.
pub fn streams_opened() -> u64 {
    STREAMS_OPENED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Clip streams a shot took over from the one before it in the same
/// file, since the process began (#2397).
pub fn streams_handed_on() -> u64 {
    STREAMS_HANDED_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// Layers on screen opened with fewer frames queued than the preference,
/// memory being short, since the process began (#2128).
pub fn streams_shortened() -> u64 {
    STREAMS_SHORTENED.load(std::sync::atomic::Ordering::Relaxed)
}

/// How many layers on screen opened while their drive was short.
pub fn streams_disk_short() -> u64 {
    STREAMS_DISK_SHORT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Title layers laid over a fast path's bytes from a CPU raster (#2351).
pub(crate) static CPU_TITLE_OVERLAYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many title layers the fast paths laid over their bytes from a CPU
/// raster since the process began: none while the GPU stack may
/// composite, which draws them as an export does (#2351).
pub fn cpu_title_overlays() -> u64 {
    CPU_TITLE_OVERLAYS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `clip`'s grade reads a matte sequence: an External Matte bound
/// to a tracked subject, or to a sequence's folder by path. The renderer
/// finds the first's folder from the clip's media and holds both to the
/// media's frames at its ends; the lean graded present can do neither, so
/// such a clip goes to the stack (#1881).
fn reads_matte_sequence(clip: &klipp_timeline::Clip) -> bool {
    clip.grade().nodes.iter().any(|n| {
        n.kind == klipp_nodes::NodeKind::ExternalMatte
            && n.enabled
            && (n.params.matte_subject.is_some() || n.params.matte_image_path.as_deref().is_some_and(klipp_nodes::matte_sequence::is_sequence))
    })
}

#[cfg(test)]
mod matte_sequence_route_tests {
    use super::reads_matte_sequence;
    use klipp_timeline::GradeAccess;

    /// #1881: a grade reading a matte sequence, bound to a subject or to
    /// its folder by path, keeps off the lean graded present; one reading
    /// a still image file, or none, doesn't.
    #[test]
    fn a_matte_sequence_keeps_a_clip_off_the_lean_present() {
        use klipp_nodes::{graph::{Edge, Node, NodeId}, matte_sequence, NodeKind, NodeParams};
        let dir = std::env::temp_dir().join(format!("klipp-route-seq-{}-{}", std::process::id(), NodeId::new_v4()));
        let m = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 2, height: 2, rotation: 0, model: "t".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        matte_sequence::create(&dir, &m).unwrap();
        let still = dir.with_extension("png");
        let with = |params: NodeParams| {
            let mut c = klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::RationalTime::from_frames(0, 24), klipp_core::RationalTime::from_frames(24, 24), klipp_core::RationalTime::from_frames(0, 24), klipp_core::RationalTime::from_frames(24, 24));
            c.edit_grade(|g| {
                let corr = g.ensure_corrector();
                let matte = NodeId::new_v4();
                g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Matte".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
                g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
            });
            c
        };
        assert!(reads_matte_sequence(&with(NodeParams { matte_image_path: Some(dir.clone()), ..Default::default() })), "a sequence by path");
        assert!(reads_matte_sequence(&with(NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() })), "a subject");
        assert!(!reads_matte_sequence(&with(NodeParams { matte_image_path: Some(still), ..Default::default() })), "a still image");
        assert!(!reads_matte_sequence(&with(NodeParams::default())), "nothing chosen");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod frame_cache_tests {
    use super::{FrameCache, Pregrade, PregradePixels};

    fn px(v: u8) -> Pregrade {
        Pregrade::new(PregradePixels::Srgb8(std::sync::Arc::new(vec![v; 16])))
    }

    /// A frame never read back from the GPU is an empty picture: not kept,
    /// so a seek onto it decodes it rather than showing nothing.
    #[test]
    fn an_empty_picture_is_not_cached() {
        let a = Some(klipp_core::ClipId::new());
        let mut cache = FrameCache::new(8);
        cache.insert_if_absent(5, a, Pregrade::new(PregradePixels::Srgb8(std::sync::Arc::new(Vec::new()))), 2, 2);
        assert!(cache.get(5, a).is_none(), "an empty picture was cached");
        cache.insert_if_absent(5, a, px(1), 2, 2);
        assert!(cache.get(5, a).is_some(), "the frame's own picture is");
    }

    /// #411: inside a dissolve one playhead has two primary clips — the
    /// outgoing one played into its handle, the incoming one a seek
    /// opens. A frame cached for one is not served as the other.
    #[test]
    fn a_frame_is_served_only_as_the_clip_it_was_read_from() {
        let (a, b) = (Some(klipp_core::ClipId::new()), Some(klipp_core::ClipId::new()));
        let mut cache = FrameCache::new(8);
        cache.insert_if_absent(99, a, px(1), 2, 2);
        assert!(cache.get(99, a).is_some());
        assert!(cache.get(99, b).is_none(), "the outgoing clip's frame served as the incoming one's");
        // The incoming clip's own frame replaces it, and is then served.
        cache.insert_if_absent(99, b, px(2), 2, 2);
        assert!(cache.get(99, b).is_some());
        assert!(cache.get(99, a).is_none());
    }
}

/// How much CPU memory the pre-grade cache may hold.
const FRAME_CACHE_BUDGET_BYTES: usize = 768 * 1024 * 1024;

fn pregrade_bytes(p: &Pregrade) -> usize {
    match &p.pixels {
        PregradePixels::Srgb8(b) => b.len(),
        PregradePixels::Hdr(h) => match &h.codes {
            crate::types::HdrCodes::Cpu(c) => c.byte_len(),
            crate::types::HdrCodes::Gpu { .. } => 0,
        },
    }
}

impl FrameCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
            budget_bytes: FRAME_CACHE_BUDGET_BYTES,
            bytes: 0,
        }
    }

    /// Insert a frame into the cache, replacing what the key held.
    /// `clip` is the clip the primary decoder read it from.
    /// A picture that isn't the frame's size (an sRGB one empty, from a
    /// frame never read back) is not kept: shown, it would be no picture.
    fn insert(&mut self, frame_number: i64, clip: Option<klipp_core::ClipId>, pixels: Pregrade, width: u32, height: u32) {
        if matches!(&pixels.pixels, PregradePixels::Srgb8(b) if b.len() != width as usize * height as usize * 4) {
            return;
        }
        self.remove(frame_number);
        let bytes = pregrade_bytes(&pixels);
        self.entries.insert(frame_number, FrameCacheEntry { pixels, clip, width, height, bytes });
        self.order.push_back(frame_number);
        self.bytes += bytes;
        while (self.entries.len() > self.capacity || self.bytes > self.budget_bytes) && self.order.len() > 1 {
            if let Some(oldest) = self.order.front().copied() {
                self.remove(oldest);
            }
        }
    }

    fn remove(&mut self, frame_number: i64) {
        if let Some(e) = self.entries.remove(&frame_number) {
            self.bytes -= e.bytes;
            self.order.retain(|f| *f != frame_number);
        }
    }

    /// Insert unless this frame is already held. Pre-grade pixels of a
    /// frame do not change, so re-inserting one gains nothing — and it
    /// would drop the ungraded picture the held copy has already made
    /// (`Pregrade::display`), which the decode-ahead paths did to every
    /// frame the viewer had just shown.
    fn insert_if_absent(&mut self, frame_number: i64, clip: Option<klipp_core::ClipId>, pixels: Pregrade, width: u32, height: u32) {
        if self.entries.get(&frame_number).is_some_and(|e| e.clip == clip) {
            return;
        }
        self.insert(frame_number, clip, pixels, width, height);
    }

    /// Look up the frame at `frame_number` read from `clip`, the clip it
    /// will be shown as. Returns (pixels, width, height) if held.
    fn get(&self, frame_number: i64, clip: Option<klipp_core::ClipId>) -> Option<(Pregrade, u32, u32)> {
        self.entries.get(&frame_number).filter(|e| e.clip == clip).map(|e| (e.pixels.clone(), e.width, e.height))
    }

    /// Frame numbers currently held — the only frames that can be graded
    /// without decoding.
    fn frame_numbers(&self) -> impl Iterator<Item = i64> + '_ {
        self.entries.keys().copied()
    }

    /// Clear the entire cache: the timeline changed under it, or the
    /// media decodes differently now.
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
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
    sequence: &Arc<klipp_timeline::SharedSequence>,
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
    let timeline_id = seq.timeline_grade_clip_id();
    while let Some(change) = queue.pop() {
        // A drag on the timeline grade's node: its graph lives on the
        // sequence, not on a clip.
        if change.clip_id == timeline_id {
            // The first drag after the toggle can arrive before any command
            // has settled a graph into the engine's copy: it starts one.
            if seq.timeline_grade_slot().edit_or_insert(|graph| apply_override_recursive(graph, change.node_id, &change.param, change.value)) {
                applied += 1;
            }
            continue;
        }
        // A drag on a grade group's pre-clip or post-clip node (#237). The
        // engine's copy runs each member with its group's grade baked in.
        if let Some((group, stage)) = seq.grade_group_stage_of(change.clip_id) {
            // The stage's node runs in each member's grade under the id the
            // bake gave it, its own unless the member's grade (or the
            // other stage) held that id already (M3 step 16a): the drag
            // goes to that node, never to another of the same id.
            let mut wrapping = None;
            if let Some(g) = seq.grade_groups.iter_mut().find(|g| g.id == group) {
                g.stage_slot(stage).edit(|graph| apply_override_recursive(graph, change.node_id, &change.param, change.value));
                wrapping = Some(g.clone());
            }
            for clip in seq.all_clips_mut().filter(|c| c.grade_group == Some(group)) {
                let target = wrapping.as_ref().and_then(|g| g.wrap_mapped(clip.own_grade()).1.get(&(stage, change.node_id)).copied()).unwrap_or(change.node_id);
                if clip.edit_grade(|graph| apply_override_recursive(graph, target, &change.param, change.value)) {
                    applied += 1;
                }
            }
            continue;
        }
        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == change.clip_id) {
            // A member of a grade group keeps its own grade beside the one
            // it runs; both take the drag (#237).
            clip.edit_own_grade(|own| apply_override_recursive(own, change.node_id, &change.param, change.value));
            if clip.edit_grade(|graph| apply_override_recursive(graph, change.node_id, &change.param, change.value)) {
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

/// Ownership-tag for the shared `Arc<klipp_timeline::SharedSequence>`. Every
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
    /// The tracks whose volume or pan the last sync took from their
    /// automation (#2306), by sequence index: one no longer automated gets
    /// its controls as set back.
    automated_tracks: std::sync::Mutex<Vec<bool>>,
    /// Which buses were last said to be held at the cap (#2861).
    buses_held_sent: std::sync::Mutex<Vec<bool>>,
    /// The track controls the user is moving or a pass is recording
    /// (#2306), by sequence index: played at this value, not from their
    /// keys, so what is heard is what is written.
    automation_override: std::sync::Mutex<std::collections::HashMap<(usize, klipp_core::automation::TrackParam), f32>>,
    fps: Fps,
    playhead: Timecode,
    sequence: Arc<klipp_timeline::SharedSequence>,
    media_pool: Arc<RwLock<MediaPool>>,
    repaint_cb: Box<dyn Fn() + Send>,
    last_frame_time: std::time::Instant,
    dropped_frames: u64,
    // Video decoders
    v1_decoder: Option<VideoDecoder>,
    /// The window of frames stepped back through while paused (#2798),
    /// for the clip it was opened on: decoded forward once, on the GPU
    /// where there is one, each step back a frame from it.
    step_back: Option<(klipp_core::ClipId, reverse_gop::ReverseGop)>,
    /// Decoders for every other media clip a frame needs: overlays, a
    /// transition's other side, multicam angles. See `engine::streams`.
    streams: streams::ClipStreams,
    /// How far ahead each upcoming clip's stream is opened, from its
    /// file's keyframes (#2108).
    seek_leads: seek_lead::SeekLeads,
    /// Neighbour frames a temporal node read beside the parked frame
    /// (#1863), kept between its renders.
    neighbour_hold: std::sync::Arc<std::sync::Mutex<view::NeighbourHold>>,
    /// What the renderer's held outputs were last held for: the state and
    /// the primary clip (#1863). A change empties them.
    held_scope: Option<(PlaybackState, Option<klipp_core::ClipId>)>,
    /// Whether [`Self::neighbour_hold`] is used (`set_neighbour_hold`).
    hold_neighbours: bool,
    /// The primary frame the last render drew, with its clip: the next
    /// frame's neighbour before for a temporal denoise on the GPU (#1988).
    previous_primary: Option<(klipp_core::ClipId, streams::HeldFrame)>,
    /// Renders every frame the viewer shows off the fast paths — the same
    /// renderer, and rules, export uses. Owns the node-graph executor and
    /// GPU effects pipeline the fast paths drive too.
    renderer: klipp_compose::Renderer,
    /// Title clips (and the frame size) whose resting render was asked
    /// for ahead of their start; cleared when the layout changes.
    titles_warmed: std::collections::HashSet<(klipp_core::ClipId, u32, u32)>,
    titles_warm_checked: std::time::Instant,
    /// Until when each presented or missing frame is logged after a cut.
    cut_diag_until: Option<std::time::Instant>,
    /// The outgoing clip's frame at the cut, taken from the primary
    /// decoder when it crossed the clip's out point: the frame a
    /// transition's outgoing side shows there, handed to the clip stream
    /// that takes the decoder over (#1494). With the clip it is of: taken
    /// only for that clip, at that time, so a frame a cut left behind never
    /// reaches a later cut's stream.
    boundary_frame: Option<(klipp_core::ClipId, crate::types::LinearFrame)>,
    // Audio — one source per active track, mixed through mix thread
    audio_sources: Vec<ActiveAudioSource>,
    /// Sources whose clips have ended, their effects ringing on (#2780):
    /// beside the sources that play now, at their clips' last settings,
    /// until their tails have run.
    audio_tails: Vec<ActiveAudioSource>,
    audio_output: Option<SendAudioOutput>,
    mix_thread: Option<MixThread>,
    /// Restarts made since the person last sought or played (#3181): of
    /// the decoder after a panic, and of the mix thread after it ended.
    /// After [`PANIC_RESTARTS`] each is left as it is, said once: a panic
    /// that comes back every time would otherwise be met every tick.
    decoder_restarts: u32,
    mix_restarts: u32,
    /// The decoder was left off for panicking too often: a loop's wrap or
    /// an edit's refresh does not open it again, nor say so again, until
    /// the person seeks or plays.
    decoder_given_up: bool,
    /// The clip those two count for: another clip's decoder that panics
    /// has tries of its own, and is said of its own.
    decoder_panic_clip: Option<klipp_core::ClipId>,
    mixer: std::sync::Arc<std::sync::Mutex<klipp_audio::AudioMixer>>,
    lufs_readings: Option<std::sync::Arc<std::sync::Mutex<klipp_audio::LufsReadings>>>,
    lufs_reset: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Playback heard at the export's delivered loudness (#1565).
    delivery: delivery::Delivery,
    spectrum_buf: Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>>,
    spectrum_sample_rate: u32,
    cpal_sample_rate: u32,
    /// Fingerprint of audio-relevant state. Prevents redundant audio teardown+rebuild
    /// when SequenceUpdated fires for non-audio changes (title edits, color changes).
    last_audio_fingerprint: u64,
    /// The clips, one per track, `last_audio_fingerprint` was taken over.
    /// Other clips under the playhead mean playback or a seek moved on,
    /// not that the sound was edited: see [`Self::audio_edited_at`].
    last_audio_clips: Vec<Option<klipp_core::ClipId>>,
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
    /// The primary clip's last frame while its file has ended and the clip
    /// runs on (#978) — a still image's one frame, a short file's last —
    /// and the sequence frame it was last shown at. Everything else on
    /// screen keeps moving over it: it is presented again each new
    /// sequence frame, where it used to be shown once and the picture held
    /// until the clip's end (#2097).
    held_tail: Option<(klipp_core::ClipId, streams::HeldFrame, i64)>,
    /// No picture clip is under the playhead but the timeline goes on
    /// (titles on a text track, music, a gap before the next shot): the
    /// playhead follows the master clock, in timeline seconds, and the
    /// renderer draws whatever is there. Set only where that is so, by a
    /// seek or by the last shot ending; cleared by every seek and every
    /// clip change, so no other path sees it (#542).
    clock_only: bool,
    /// Clock-only through a gap between two shots of the picture track:
    /// (the shot before, the shot after), by index. The shot after is
    /// prerolled during the gap and taken over as a cut is, not with a
    /// seek (#546). Cleared with `clock_only`'s other ways out.
    gap_hand_over: Option<(usize, usize)>,
    /// The size the GPU stack's empty frame was drawn ahead at (#2386).
    empty_stack_warm: Option<(u32, u32)>,
    /// Whether it is drawn ahead at all; off only in a test.
    prewarm_stack: bool,
    /// The last playhead said while playing, and whether playing forwards
    /// (#1806): what a report during playback is never behind, the way it
    /// plays (a floor forwards, a ceiling backwards). Cleared by a pause, a
    /// stop, a seek, a loop or a change of speed.
    report_floor: std::cell::Cell<Option<(RationalTime, bool)>>,
    // Audio seek synchronization — generation counter
    expected_audio_generation: u64,
    // Performance
    last_stats_time: std::time::Instant,
    /// When an ENGINE_HEARTBEAT with nothing in it was last logged
    /// (#2988): a quiet engine logs one every 30 s, not every 2 s.
    last_quiet_heartbeat: std::time::Instant,
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
    // effect picked in the inspector, identified by clip + its picture node
    // (M3 step 4: a reorder or a removal before it doesn't move it).
    // When active on the current clip, the engine renders the chain twice
    // (once normal, once with the target effect's enabled flag flipped to
    // false) and writes the bypassed version to FrameBuffer.compare_effect_data.
    // Any node of the clip's picture by its path, not only an effect: the
    // A/B wipe of a node's input against its output (#2758).
    pub compare_effect_active: bool,
    pub compare_effect_clip_id: Option<klipp_core::ClipId>,
    pub compare_effect_node: Option<klipp_graph_format::path::NodePath>,
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
    /// A clip the next frames render alone, for a colour measurement of
    /// it (#719); `None` for the picture as composited.
    measure_only_clip: Option<klipp_core::ClipId>,
    /// An eyedropper read of a node's input, waiting on the render
    /// `sample_node_input` asked for. See `NodeSample`.
    node_sample: Option<NodeSample>,
    /// Since when a pick has waited for its frame (a seek still
    /// decoding): refused when it has waited too long.
    node_sample_since: Option<std::time::Instant>,
    /// Solo target — when set, the engine returns this node's output
    /// as the final image instead of the Output-node-input. Single
    /// global slot; auto-inactive when the id doesn't exist in the
    /// active clip's graph (so switching clips silently un-solos).
    /// The soloed node's output, by path (M3 step 16a).
    solo: Option<klipp_compose::InspectAt>,
    /// What the viewer shows of the picture (#2758): the picture, or one
    /// channel of it in grey (`AppCommand::SetViewChannel`).
    view_channel: klipp_core::ViewChannel,
    /// `solo` in the grade the fast paths run (`v1_graph_setup`).
    fast_solo: Option<klipp_nodes::NodeId>,
    /// V2 canvas "nodes visible right now" set — the executor
    /// captures downsampled thumbnails for each of these on the
    /// paused / post-seek re-grade so the canvas can paint per-node
    /// previews. Empty during playback (capturing N readbacks per
    /// frame would cost more than the grade itself).
    visible_node_thumbs: std::collections::HashSet<klipp_compose::InspectAt>,
    thumbnail_request: u64,
    thumbnail_priority: Vec<klipp_compose::InspectAt>,
    /// The clip the grade panel is on, from the node canvas's and scope
    /// tap's commands. The grade tools follow it when it is on screen.
    inspect_clip: Option<klipp_core::ClipId>,
    /// A paused frame was shown before every clip stream had its frame;
    /// re-render when they do.
    pending_stream_refresh: bool,
    /// A paused seek's audio, owed: the timecode to bring the sources to,
    /// the source time for the scrub chirp, and since when. Opening the
    /// sources for another clip takes half a second, and the picture does
    /// not wait for it; see [`Self::reposition_audio_or_defer`].
    pending_audio_reposition: Option<(Timecode, f64, std::time::Instant)>,
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
    /// The file the source viewer's sound comes from: the original, even
    /// while its picture comes from the skim transcode, which has none.
    source_audio_path: Option<PathBuf>,
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
    /// The main dock's scope mode (0=waveform, 1=parade, 2=vectorscope,
    /// 3=histogram), always drawn along with `active_scope_modes`.
    scope_render_mode: u8,
    /// The scope settings (quality, channels, envelope, vectorscope
    /// window, histogram layout) pushed by `AppCommand::SetScopeSettings`.
    scope_settings: klipp_core::ScopeSettings,
    /// Whether the GPU scopes draw their glow. Pushed by
    /// `AppCommand::SetScopeBloom`.
    scope_bloom_enabled: bool,
    /// Whether the waveform and parade measure a low-passed copy of the
    /// picture. Pushed by `AppCommand::SetScopeLowPass`.
    scope_low_pass_enabled: bool,
    /// The viewing LUT the scopes measure after (#466); none, before it.
    scope_lut: Option<klipp_core::ScopeLut>,
    /// Union of scope modes the UI currently needs rendered per tick:
    /// (main dock active mode) ∪ (each floating-window pane's mode).
    /// Updated by `AppCommand::SetActiveScopeModes`. The engine draws
    /// every mode in this set on each scope tick and writes the results
    /// into `FrameBuffer::scope_textures` keyed by mode. Empty before the
    /// UI publishes anything: then only `scope_render_mode` is drawn.
    active_scope_modes: std::collections::BTreeSet<u8>,
    /// The physical pixel size the UI shows each scope mode at, pushed by
    /// `AppCommand::SetScopeSizes`. The GPU scopes are drawn at it.
    scope_sizes: std::collections::BTreeMap<u8, (u32, u32)>,
    /// Whether the scope panel is actually visible in the UI.
    /// Drives `should_render_scope()` together with the time-based
    /// throttle. Default `false` so a fresh engine doesn't burn the
    /// per-frame scope budget on data nobody is looking at — UI
    /// flips it on via `AppCommand::SetScopesVisible(true)` when
    /// the scopes panel opens.
    scopes_visible: bool,
    /// When the scopes were last drawn, and what that pass cost. Together
    /// they pace the scopes: see [`Self::should_render_scope`].
    last_scope_render: std::time::Instant,
    last_scope_cost: Duration,
    /// A frame reached the screen, or a scope's settings, size or set
    /// changed, without the scopes being drawn: they still show what came
    /// before. Paid on the next frame whose scopes are due, and while
    /// paused, when nothing else would render, by rendering the parked
    /// frame again once they are.
    scope_render_owed: bool,
    // Matte preview node — which Qualifier/Mask node's matte to show
    matte_preview_node: Option<klipp_nodes::NodeId>,
    // Cached pre-grade frame for instant parameter updates when paused.
    // Stored after decode, before color grading. Re-graded when sequence changes.
    cached_pregrade_frame: Option<(Pregrade, u32, u32)>, // (pixels, width, height)
    /// The frame `cached_pregrade_frame` was made from, while it is
    /// that one: a step of a slider on the parked frame whose primary is
    /// this frame (its HDR codes on the GPU, the pre-grade's on the CPU)
    /// has its pre-grade already (#2352).
    pregrade_source: Option<streams::HeldFrame>,
    /// The playhead `cached_pregrade_frame` was captured at. The frame is
    /// only the parked frame while the playhead is still there; after a
    /// seek it is a frame of somewhere else, and regrading it would show
    /// the old picture under the new position's grade.
    cached_pregrade_at: Option<Timecode>,
    /// Only the latest decoded primary picture, kept in its original
    /// storage (including GPU buffers). The time and source layout guard
    /// reuse across pause and grade edits; seeks discard it.
    retained_primary: Option<(Timecode, u64, streams::HeldFrame)>,
    /// A frame short of the playhead is being presented — a slow seek's
    /// progress — and must not be cached as the playhead's frame.
    presenting_progress: bool,
    /// The composite path the last presented frame took, for the log.
    last_composite_path: Option<&'static str>,
    /// What the pre-grade cache was filled under (see
    /// [`timeline_layout_fingerprint`]); a different layout empties it.
    layout_fingerprint: u64,
    /// What was under the playhead when the layout was last looked at
    /// (see [`under_playhead_fingerprint`]); an edit that changes it is
    /// shown by seeking there again.
    under_playhead_fingerprint: (Timecode, u64),
    /// A seek made to show an edit, not to go anywhere: it has no scrub
    /// chirp.
    quiet_seek: bool,
    /// The transfer the SDR viewer encodes for — the monitor it assumes.
    sdr_transfer: klipp_core::SdrTransfer,
    /// Reasons the direct-present fast path declined the previous frame,
    /// so they can be logged when the set *changes* rather than once per
    /// frame. Empty means the last frame took the fast path.
    last_direct_present_blockers: Vec<&'static str>,
    /// How long the last paused seek waited for the decoder's frame,
    /// apart from rendering it. For the seek timing log.
    last_grab_wait: Duration,
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
    /// The viewer frame's phases so far, for its perf sample (#1850).
    frame_phases: crate::perf::FramePhases,
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
    /// Render In to Out under way (#924).
    render_job: Option<render_job::RenderJob>,
    /// When the render status strip was last worked out (#924): idle,
    /// it is refreshed once a second so edits show.
    render_status_at: std::time::Instant,
    // Audio scrubbing: when false, audio is muted while the user drags the
    // playhead. Mirrors `prefs.playback.scrub_audio` from the UI side.
    scrub_audio_enabled: bool,
    // True while BeginScrub…EndScrub brackets a playhead drag.
    is_scrubbing: bool,
    /// Skim-file decoders for skim pictures (#2945), closed when the skim
    /// ends.
    skim_pool: skim_render::SkimPool,
    /// The media playback can crossfade, refreshed once per tick: see
    /// [`fadeable_media`].
    fadeable_media: FadeableMedia,
    // Predictive pre-seek — background thread pre-opens audio decoders before loop point
    pre_seek_result: Arc<std::sync::Mutex<Option<LoopPreSeekResult>>>,
    pre_seek_generation: u64,
    pre_seek_started: bool,
    // Clip look-ahead: pre-decoder for the next clip, opened ~1s before the cut point.
    // When the cut arrives, this decoder is swapped in as v1_decoder — zero stall.
    preroll_decoder: Option<VideoDecoder>,
    /// The next clip's decoder while a worker thread opens and seeks it:
    /// done on the engine thread, that took 35–43 ms of a tick at every
    /// preroll (#572). It arrives in `preroll_decoder`.
    preroll_pending: Option<std::sync::mpsc::Receiver<Option<VideoDecoder>>>,
    /// The next clip whose preroll failed to open. The look-ahead asks
    /// on every frame of the last 1.5 s, and a file that will not open
    /// was tried again each time, a worker thread a frame (#576). Kept
    /// until the preroll is cleared: the cut opens the clip itself.
    preroll_failed: Option<usize>,
    preroll_clip_index: Option<usize>,
    /// The track and media the preroll was opened for, with its index
    /// (#2138): an index alone is a clip of whichever track is the picture
    /// track at the cut, and a shot prerolled on one track was swapped in
    /// for the clip of the same index on another, its frame shown there.
    preroll_for: Option<(usize, MediaId)>,
    preroll_source_in: f64,
}

/// One track's audio at a time: track index, media, decode path, the
/// source second the playhead is at, and the clip's audio chain.
type TrackAudioRow = (
    usize,
    MediaId,
    PathBuf,
    f64,
    Vec<klipp_core::AudioEffectInstance>,
    std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
);

/// Every audio source that should sound at `tc`: per track, the clip
/// under it — an enabled clip on media with audio, and not the video
/// half of a linked pair whose audio half sits on an audio track — and,
/// inside an audio crossfade, the other clip of it too, playing from its
/// handle. Each row is keyed by [`source_key`]: the track, and the lane
/// the clip plays in, so a crossfade's two clips are two sources.
/// `pcm_cache` swaps in an item's conformed PCM file where one exists.
/// `fadeable` is the media playback can crossfade, see [`fadeable_media`].
fn audio_rows(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, fadeable: &FadeableMedia, tc: Timecode, use_proxies: bool, pcm_cache: bool) -> Vec<TrackAudioRow> {
    let mut rows = Vec::new();
    let fades = can_fade(fadeable);
    for (track_idx, track) in seq.tracks.iter().enumerate() {
        for sounding in klipp_timeline::crossfade::sounding_at_where(track, tc, &fades) {
            let clip = sounding.clip;
            // A multicam angle is heard where the group's sound rule
            // says, the rule the render gates the same clip by.
            // A ramped clip is silent (#638): its sound has no one speed to
            // be read at. Export leaves it out alike.
            if !clip.enabled || !clip.plays_its_sound() || !seq.multicam_sounds_at(clip, tc) {
                continue;
            }
            let Some(media_id) = clip.media_id() else { continue };
            let Some(item) = pool.get(&media_id) else { continue };
            if !item.has_audio {
                continue;
            }
            // A retimed clip's sound is read at its speed from the PCM
            // cache (#642); the live decoder reads at 1x only, and would
            // play it out of step. Until the cache is made, it is silent.
            if clip.is_varispeed() && !item.audio_cache.as_ref().is_some_and(|c| c.pcm_path.exists()) {
                continue;
            }
            // Skip audio for video-track clips that have a linked audio-track partner
            // (the audio track clip will produce audio instead)
            if track.kind == klipp_timeline::TrackKind::Video {
                if let Some(lid) = clip.linked_clip_id {
                    let partner_on_audio = seq.tracks.iter().any(|t| {
                        t.kind == klipp_timeline::TrackKind::Audio && t.clips.iter().any(|c| c.id == lid)
                    });
                    if partner_on_audio {
                        continue;
                    }
                }
            }
            // Before an incoming clip's in point this reaches back into
            // its head, past an outgoing clip's out point into its tail.
            let source_secs = klipp_compose::render::source_secs(clip, tc, seq.fps.as_f64());
            let decode_path = if use_proxies && item.proxy_ready {
                item.proxy_path.as_ref().filter(|p| p.exists()).cloned().unwrap_or_else(|| item.path.clone())
            } else {
                item.path.clone()
            };
            let path = if pcm_cache {
                item.audio_cache.as_ref().map(|c| c.pcm_path.clone()).filter(|p| p.exists()).unwrap_or(decode_path)
            } else {
                decode_path
            };
            rows.push((
                source_key(track_idx, sounding.lane),
                media_id,
                path,
                source_secs,
                clip.audio_effects.clone(),
                clip.audio_effect_keyframes.clone(),
            ));
        }
    }
    rows
}

/// How far ahead of where the mix is a clip's sound is opened, to start
/// on its own sample (#1588): past an engine tick, and the decoder's
/// first read.
const OPEN_AHEAD_SECS: f64 = 0.15;

/// The mixed frame a clip starting at `start` seconds is heard at, when
/// the newest mixed frame, `mixed`, is heard at `heard` seconds: `None`
/// for one already started, which opens under its clip as before.
pub(crate) fn start_frame_for(start: f64, heard: f64, mixed: u64, sample_rate: u32) -> Option<u64> {
    (start > heard).then(|| mixed + ((start - heard) * sample_rate as f64).round() as u64)
}

/// How far ahead of the output the mix thread works: the samples in the
/// main ring (interleaved stereo) at the device rate, in seconds.
fn mix_lead_secs(buffered_samples: usize, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    buffered_samples as f64 / (2.0 * sample_rate as f64)
}

/// A track's two sound lanes as source keys: lane 0 is the track index
/// itself, lane 1 a key far above any track. The mix thread folds both
/// back onto the track, so the track's volume, pan, mute, solo and
/// meters are one.
pub(crate) const LANE_STRIDE: usize = 1 << 16;

pub(crate) fn source_key(track_index: usize, lane: u8) -> usize {
    track_index + lane as usize * LANE_STRIDE
}

/// The track a source key belongs to.
pub(crate) fn key_track(key: usize) -> usize {
    key % LANE_STRIDE
}

/// Whether the clip after `clip` on `track`, from its out point, carries
/// the sound of a source playing `media` through the chain `chain_fp` on:
/// the same sound, played on by the one source.
fn carried_on(track: &klipp_timeline::Track, clip: &klipp_timeline::Clip, media: MediaId, chain_fp: u64) -> bool {
    track.clips.iter().any(|c| {
        c.id != clip.id
            && c.timeline_in == clip.timeline_out
            && c.media_id() == Some(media)
            && audio_chain_fingerprint_for(&c.audio_effects, &c.audio_effect_keyframes) == chain_fp
    })
}

/// The media whose sound can crossfade in playback: see [`fadeable_media`].
type FadeableMedia = std::collections::HashSet<MediaId>;

/// The media whose sound can crossfade in playback. Only the PCM
/// feeder applies an envelope (`AudioSource::set_fade` is a no-op on
/// the decoder), so a clip whose media has no usable PCM cache — none
/// yet, or its file gone, the same test `open_audio_source` makes
/// before it falls through to the decoder — is joined by a plain cut
/// instead: scheduling a crossfade it could not fade summed both clips
/// at full level and then stopped the outgoing one dead. One
/// file-system check per conformed item, made once per engine tick
/// (`PlaybackEngine::refresh_fadeable_media`) rather than under the
/// sequence lock each time a lane is asked, which the UI waits on.
fn fadeable_media(pool: &klipp_media::MediaPool) -> FadeableMedia {
    pool.all()
        .filter(|item| item.audio_cache.as_ref().is_some_and(|c| c.pcm_path.exists()))
        .map(|item| item.id)
        .collect()
}

/// Whether a clip's sound can crossfade in playback, see [`fadeable_media`].
fn can_fade(fadeable: &FadeableMedia) -> impl Fn(&klipp_timeline::Clip) -> bool + '_ {
    move |clip| clip.media_id().is_some_and(|id| fadeable.contains(&id))
}

/// The clip a source keyed `key` plays at `tc`, with its crossfades:
/// the ones playback can fade, see [`fadeable_media`].
fn sounding_for_key<'a>(seq: &'a klipp_timeline::Sequence, fadeable: &FadeableMedia, key: usize, tc: Timecode) -> Option<klipp_timeline::crossfade::Sounding<'a>> {
    let track = seq.tracks.get(key_track(key))?;
    let lane = (key / LANE_STRIDE) as u8;
    klipp_timeline::crossfade::sounding_at_where(track, tc, &can_fade(fadeable)).into_iter().find(|s| s.lane == lane)
}

/// Restarts of what a panic ended, since the person last sought or played
/// (#3181).
pub(crate) const PANIC_RESTARTS: u32 = 3;

impl PlaybackEngine {
    /// Every audio source that should sound at `tc`, keyed by track and
    /// lane: see [`audio_rows`].
    fn audio_rows_at(&self, tc: Timecode) -> Vec<TrackAudioRow> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        audio_rows(&seq, &pool, &self.fadeable_media, tc, self.use_proxies, false)
    }

    /// Titles about to start get their resting render made ahead on the
    /// warm-up thread, so the frame they start on places a sprite that
    /// is already there. Rastered on this thread at the cut, 4K of text
    /// cost 60–130 ms: a hitch at every cut into a title.
    fn warm_upcoming_titles(&mut self) {
        const AHEAD_SECS: f64 = 2.0;
        if self.state != PlaybackState::Playing || self.titles_warm_checked.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.titles_warm_checked = std::time::Instant::now();
        let now = self.playhead.to_secs_f64();
        let mut wants = Vec::new();
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            for (clip, td) in titles_starting(&seq, now, AHEAD_SECS) {
                // The frame size the title is placed at: the frame
                // the video under it is shown in, else the sequence's
                // within the playback resolution (`playback_canvas`).
                // That frame is sized from the picture as decoded —
                // at most the playback resolution — not as the file
                // holds it: warmed at the file's size, a 4K portrait
                // clip's title was made for a frame it is never
                // shown in.
                let decoded = seq
                    .tracks
                    .iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|t| {
                        let (_, c) = t.clip_at_timecode(clip.timeline_in)?;
                        let item = pool.get(&c.media_id()?)?;
                        // The item's size is already upright.
                        Some(crate::video_decoder::playback_dimensions(
                            item.resolution.width,
                            item.resolution.height,
                            0,
                        ))
                    });
                let (w, h) = view::playback_canvas(decoded, (seq.resolution.width, seq.resolution.height));
                // Warmed for playback, so at Playback Quality's size.
                let (w, h) = view::reduced_for_quality((w, h), crate::video_decoder::playback_quality_divisor());
                if w == 0 || h == 0 {
                    continue;
                }
                if self.titles_warmed.insert((clip.id, w, h)) {
                    wants.push((td.clone(), w, h));
                }
            }
        }
        self.renderer.warm_titles(wants);
    }

    /// While playing, the audio sources follow the audio clips under the
    /// playhead: a clip that begins between video cuts (a sound effect
    /// on a cut, a bed that starts late) gets its source the tick it
    /// begins, and a source whose clip has ended, or whose track now
    /// holds a different clip, is retired on its own. Nothing else is
    /// touched, so the music never restarts for a sound effect coming or
    /// going. Before this, sources were made only when playback started
    /// and at a video cut, so a late effect was silent and an ended one
    /// restarted every source at the next cut — a gap in the music.
    /// Hand each running source's chain the current settings of the clip
    /// it plays, where they changed (a slider, a preset, wet/dry).
    fn push_audio_effect_settings(&mut self) {
        let rows = self.audio_rows_at(self.playhead);
        for active in &mut self.audio_sources {
            let Some((_, media_id, _, _, effects, _)) = rows.iter().find(|r| r.0 == active.track_index) else { continue };
            if *media_id != active.media_id {
                continue;
            }
            let fp = audio_settings_fingerprint_for(effects);
            if fp != active.settings_fingerprint {
                debug!("audio: new effect settings for track {}", active.track_index);
                active.source.update_effect_settings(effects.clone());
                active.settings_fingerprint = fp;
            }
        }
    }

    /// Refresh the media playback can crossfade, see [`fadeable_media`]:
    /// once per tick, before anything in it asks. Skipped on an idle
    /// tick that has no command to handle, when nothing asks; the pool
    /// is taken as it is free, since a tick waiting on the UI's import
    /// of a hundred files would be a tick lost for nothing.
    fn refresh_fadeable_media(&mut self, had_commands: bool) {
        if !had_commands && matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped) {
            return;
        }
        let pool = match self.media_pool.try_read() {
            Ok(p) => p,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        self.fadeable_media = fadeable_media(&pool);
    }

    /// Each source's clip as it plays at `tc` ([`ActiveAudioSource::clip`]),
    /// and the test of whether one no longer wanted has reached its
    /// clip's end, unedited, and can ring on (#2780).
    fn note_source_clips(&mut self, tc: Timecode) -> impl Fn(&ActiveAudioSource) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        for a in self.audio_sources.iter_mut().filter(|a| a.may_ring) {
            if let Some(s) = sounding_for_key(&seq, &self.fadeable_media, a.track_index, tc) {
                if s.clip.media_id() == Some(a.media_id) && audio_chain_fingerprint_for(&s.clip.audio_effects, &s.clip.audio_effect_keyframes) == a.audio_chain_fingerprint {
                    a.clip = Some((s.clip.id, s.clip.timeline_out, s.clip.timeline_out.to_secs_f64() + s.extent.tail_secs));
                }
            }
        }
        // A clip as it was: still on its track, ending where it did.
        let unedited: std::collections::HashSet<(usize, klipp_core::ClipId, Timecode)> = self
            .audio_sources
            .iter()
            .filter_map(|a| {
                let (id, out, _) = a.clip?;
                seq.tracks.get(key_track(a.track_index))?.clips.iter().any(|c| c.id == id && c.timeline_out == out).then_some((a.track_index, id, out))
            })
            .collect();
        let heard = tc.to_secs_f64();
        move |a: &ActiveAudioSource| {
            a.may_ring && a.clip.is_some_and(|(id, out, ends)| heard >= ends - 0.05 && unedited.contains(&(a.track_index, id, out)))
        }
    }

    /// Stop the ringing sources whose tails have run, or that have rung
    /// as long as any can, at `heard` seconds on the timeline.
    fn end_rung_out_tails(&mut self, heard: f64) {
        let mut done = Vec::new();
        self.audio_tails.retain_mut(|t| {
            let ends = t.clip.map_or(0.0, |(_, _, ends)| ends);
            let tail = t.source.ring().and_then(|r| r.tail_secs()).unwrap_or(klipp_audio::dsp::MAX_TAIL_SECS as f64);
            let rung = heard > ends + tail + 0.1;
            if rung {
                t.source.stop_detached();
                done.push(t.mix.clone());
            }
            !rung
        });
        for mix in &done {
            if let Some(mt) = &self.mix_thread {
                mt.remove_staging_of(mix);
            }
        }
    }

    /// The clips whose effects still ring at `tc`, playback starting there
    /// (a play, a seek, a play after a pause): each one's sound opened as a
    /// tail, put back twice its tail before the clip's end, its fades on
    /// and its effects run through to `tc` without being heard, so it
    /// plays on from there as the export has it (#2793). Started inside a
    /// tail, playback was silent where the export rang.
    fn open_ringing_tails(&mut self, tc: Timecode) {
        let heard = tc.to_secs_f64();
        struct Tail {
            key: usize,
            media: MediaId,
            path: std::path::PathBuf,
            from_source: f64,
            discard: usize,
            effects: Vec<klipp_core::AudioEffectInstance>,
            keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
            sound: ClipSound,
            source_in: f64,
            duration: f64,
            clip: (klipp_core::ClipId, Timecode, f64),
        }
        let tails: Vec<Tail> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            let fps = seq.fps.as_f64();
            let mut tails = Vec::new();
            for (ti, track) in seq.tracks.iter().enumerate() {
                for clip in &track.clips {
                    let out = clip.timeline_out.to_secs_f64();
                    if out > heard || heard >= out + klipp_audio::dsp::MAX_TAIL_SECS as f64 {
                        continue;
                    }
                    // A clip playback would ring: its own sound at 1x, from a
                    // conformed file, not handed on to a crossfade or to the
                    // next clip of the same sound.
                    if !clip.enabled || !clip.plays_its_sound() || clip.is_varispeed() || klipp_timeline::crossfade::extent(track, clip).tail_secs > 0.0 {
                        continue;
                    }
                    let Some(media) = clip.media_id() else { continue };
                    let Some(path) = pool.get(&media).filter(|i| i.has_audio).and_then(|i| i.audio_cache.as_ref()).map(|c| c.pcm_path.clone()).filter(|p| p.exists()) else { continue };
                    if track.kind == klipp_timeline::TrackKind::Video
                        && clip.linked_clip_id.is_some_and(|l| seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio && t.clips.iter().any(|c| c.id == l)))
                    {
                        continue;
                    }
                    let (effects, keyframes) = (clip.audio_effects.clone(), clip.audio_effect_keyframes.clone());
                    if !klipp_audio::dsp::AudioEffectChain::may_ring(&effects, !keyframes.is_empty(), 48_000)
                        || carried_on(track, clip, media, audio_chain_fingerprint_for(&effects, &keyframes))
                    {
                        continue;
                    }
                    let chain = klipp_audio::dsp::AudioEffectChain::from_instances(&effects, 48_000);
                    let tail = (chain.tail_frames(48_000) + chain.total_latency_samples(48_000) as usize) as f64 / 48_000.0;
                    if heard >= out + tail {
                        continue;
                    }
                    // Twice the tail back: what came before has fallen to
                    // under -160 dB by the playhead.
                    let from = clip.timeline_in.to_secs_f64().max(out - 2.0 * tail);
                    tails.push(Tail {
                        key: ti,
                        media,
                        path,
                        from_source: klipp_compose::render::source_secs(clip, RationalTime::from_secs_f64(from, 48_000), fps),
                        discard: ((heard - from) * 48_000.0).round() as usize,
                        effects,
                        keyframes,
                        sound: clip_sound_at(&seq, clip, out, fps),
                        source_in: clip.source_in.to_secs_f64(),
                        duration: clip.duration().to_secs_f64(),
                        clip: (clip.id, clip.timeline_out, out),
                    });
                }
            }
            tails
        };
        for t in tails {
            let before = self.audio_sources.len();
            self.open_audio_source(t.key, t.media, &t.path, t.from_source, t.effects, t.keyframes);
            if self.audio_sources.len() == before {
                continue;
            }
            let Some(mut a) = self.audio_sources.pop() else { continue };
            if a.source.ring().is_none() {
                a.source.stop_detached();
                if let Some(mt) = &self.mix_thread {
                    mt.remove_staging_of(&a.mix);
                }
                continue;
            }
            let s = &t.sound;
            a.source.set_fade(t.source_in, t.duration, s.fade_in as f64, s.fade_out as f64, fade_curve_to_u32(s.fade_in_curve), fade_curve_to_u32(s.fade_out_curve), s.gain);
            if let Some(ring) = a.source.ring() {
                ring.set_stops(true);
            }
            a.mix.set(s.volume * s.gain, s.pan, channel_config_to_u32(s.channel_config));
            a.source.discard(t.discard);
            a.clip = Some(t.clip);
            // Its pre-roll runs now, while the rest are made ready.
            a.source.resume();
            debug!("audio tail on track {} opened {:.3}s into its ring", t.key, heard - t.clip.2);
            self.audio_tails.push(a);
        }
    }

    /// Stop every ringing source at once: a seek or a pause.
    fn drop_audio_tails(&mut self) {
        for mut t in std::mem::take(&mut self.audio_tails) {
            t.source.stop_detached();
            if let Some(mt) = &self.mix_thread {
                mt.remove_staging_of(&t.mix);
            }
        }
    }

    fn reconcile_audio_sources(&mut self) {
        if self.state != PlaybackState::Playing {
            return;
        }
        // A settings change can arrive before the sequence snapshot it was
        // made on, so the SequenceUpdated push may still read the old
        // values; checking each tick while playing picks the new ones up.
        self.push_audio_effect_settings();
        // Sources are swapped where the mix thread is working, not at the
        // playhead: a source opened at the playhead was heard a mix lead
        // (~200 ms) late, the old clip's sound running on into the new
        // shot. Swapped at the heard position, a cut's new sound is
        // already mixing when the picture gets there.
        let tc = self.audio_heard_at();
        let rows = self.audio_rows_at(tc);
        // A clip about to start is opened ahead, from its own first
        // sample, and starts on it in the mix: opened the tick after its
        // start it began a tick late, its head cut (#1588). At a cut on its
        // track the outgoing sound plays on beside it until that sample,
        // and ends there (#1632).
        let ahead = self.audio_rows_starting_soon(&rows);
        // A source is wanted while a clip of its media and chain sounds
        // on its track now or is about to: a track has two across a cut.
        let identity = |(t, m, _, _, fx, kfs): &TrackAudioRow| (*t, *m, audio_chain_fingerprint_for(fx, kfs));
        let wanted: std::collections::HashSet<(usize, MediaId, u64)> = rows.iter().chain(ahead.iter().map(|(row, _)| row)).map(identity).collect();
        let heard = tc.to_secs_f64();
        let ended = self.note_source_clips(tc);
        let mut retired = Vec::new();
        let mut ringing = Vec::new();
        for a in std::mem::take(&mut self.audio_sources) {
            if wanted.contains(&(a.track_index, a.media_id, a.audio_chain_fingerprint)) {
                self.audio_sources.push(a);
            } else if ended(&a) {
                // Its clip has ended: its effects ring on beside the
                // next clip's sound, at its last settings (#2780).
                debug!("audio source on track {} rings on past its clip's end at {heard:.3}s", a.track_index);
                ringing.push(a);
            } else {
                let mut a = a;
                a.source.stop_detached();
                retired.push((a.track_index, a.mix.clone()));
            }
        }
        self.audio_tails.extend(ringing);
        self.end_rung_out_tails(heard);
        for (t, mix) in &retired {
            if let Some(mt) = &self.mix_thread {
                mt.remove_staging_of(mix);
            }
            debug!("audio source on track {t} retired: no clip of its media under {:.3}s", tc.to_secs_f64());
        }
        self.end_sources_on_their_samples();
        let have: std::collections::HashSet<(usize, MediaId, u64)> = self.audio_sources.iter().map(|a| (a.track_index, a.media_id, a.audio_chain_fingerprint)).collect();
        let mut opened = false;
        // The sources opened here, resumed once their clips' settings are on.
        let mut started: Vec<usize> = Vec::new();
        let rows = rows.into_iter().map(|row| (row, None)).chain(ahead);
        for (row, starts) in rows {
            if have.contains(&identity(&row)) {
                continue;
            }
            let (track_idx, media_id, path, source_secs, fx, kfs) = row;
            let before = self.audio_sources.len();
            self.open_audio_source(track_idx, media_id, &path, source_secs, fx, kfs);
            if self.audio_sources.len() > before {
                if let Some(a) = self.audio_sources.last_mut() {
                    started.push(a.track_index);
                    if let Some((at, frame)) = starts {
                        a.mix.start_at(frame);
                        a.starts_at = Some(at);
                    }
                }
                debug!("audio source on track {track_idx} opened at {source_secs:.3}s for the clip beginning under {:.3}s", tc.to_secs_f64());
                opened = true;
            }
        }
        // Each source carries its own clip's volume, pan, gain, channel
        // config and speed: one opened needs its settings now, not at the
        // next tick's sync, and before it plays — resumed first, a
        // retimed clip's first chunk was read at 1x (#642).
        if opened || !retired.is_empty() {
            self.sync_clip_volumes();
        }
        for a in self.audio_sources.iter().filter(|a| started.contains(&a.track_index)) {
            a.source.resume();
        }
        if opened {
            if let Some(aout) = &self.audio_output {
                aout.0.resume();
            }
        }
    }
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

    /// Whether a parked frame's temporal neighbours are kept between its
    /// renders (#1863); on unless turned off. Off, every render decodes
    /// them, as before the hold: what a test compares the hold against.
    pub fn set_neighbour_hold(&mut self, on: bool) {
        self.hold_neighbours = on;
    }

    /// Whether the GPU stack's empty frame is drawn ahead (#2386); on
    /// unless turned off. Off, a strobe's first gap meets a cold stack:
    /// what a test of the recovery from a late hand-over plays against.
    pub fn set_prewarm_empty_stack(&mut self, on: bool) {
        self.prewarm_stack = on;
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
        sequence: Arc<klipp_timeline::SharedSequence>,
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
        // One rate decision for the pipeline and the device (48 kHz when
        // the device takes it), cached so the stream opens at the same rate.
        let cpal_sample_rate = {
            let _t = klipp_core::startup_timer::LogTimer::new("engine: cpal default device probe");
            crate::audio_output::probe_sample_rate()
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
            automated_tracks: std::sync::Mutex::new(Vec::new()),
            buses_held_sent: std::sync::Mutex::new(Vec::new()),
            automation_override: Default::default(),
            fps: Fps::film_24(),
            playhead: RationalTime::from_frames(0, 24),
            sequence,
            media_pool,
            repaint_cb,
            last_frame_time: std::time::Instant::now(),
            dropped_frames: 0,
            v1_decoder: None,
            step_back: None,
            streams: streams::ClipStreams::new(),
            seek_leads: seek_lead::SeekLeads::default(),
            neighbour_hold: Default::default(),
            held_scope: None,
            hold_neighbours: true,
            previous_primary: None,
            renderer: klipp_compose::Renderer::new(ofx_host, title_renderer),
            titles_warmed: std::collections::HashSet::new(),
            titles_warm_checked: std::time::Instant::now(),
            cut_diag_until: None,
            boundary_frame: None,
            audio_sources: Vec::new(),
            audio_tails: Vec::new(),
            audio_output,
            mix_thread: Some(mix_thread),
            decoder_restarts: 0,
            mix_restarts: 0,
            decoder_given_up: false,
            decoder_panic_clip: None,
            mixer,
            lufs_readings,
            lufs_reset,
            delivery: Default::default(),
            spectrum_buf,
            spectrum_sample_rate,
            cpal_sample_rate,
            last_audio_fingerprint: 0,
            last_audio_clips: Vec::new(),
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
            held_tail: None,
            clock_only: false,
            gap_hand_over: None,
            empty_stack_warm: None,
            prewarm_stack: true,
            report_floor: std::cell::Cell::new(None),
            expected_audio_generation: 0,
            last_stats_time: std::time::Instant::now(),
            last_quiet_heartbeat: std::time::Instant::now(),
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
            compare_effect_node: None,
            matte_preview: false,
            hdr_monitor: klipp_core::HdrMonitor::default(),
            hdr_reference_output: false,
            hdr_writer: None,
            inspector_matte_preview_target: None,
            measure_only_clip: None,
            node_sample: None,
            node_sample_since: None,
            solo: None,
            view_channel: klipp_core::ViewChannel::Picture,
            fast_solo: None,
            visible_node_thumbs: std::collections::HashSet::new(),
            thumbnail_request: 0,
            thumbnail_priority: Vec::new(),
            inspect_clip: None,
            pending_stream_refresh: false,
            pending_audio_reposition: None,
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
            source_audio_path: None,
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
            scope_settings: klipp_core::ScopeSettings::default(),
            // Off: at a large scope's size a glow washes the trace out.
            scope_bloom_enabled: false,
            scope_low_pass_enabled: true,
            scope_lut: None,
            active_scope_modes: std::collections::BTreeSet::new(),
            scope_sizes: std::collections::BTreeMap::new(),
            scopes_visible: false,
            last_scope_render: std::time::Instant::now() - std::time::Duration::from_secs(1),
            last_scope_cost: Duration::ZERO,
            scope_render_owed: false,
            matte_preview_node: None,
            cached_pregrade_frame: None,
            pregrade_source: None,
            cached_pregrade_at: None,
            retained_primary: None,
            presenting_progress: false,
            last_composite_path: None,
            layout_fingerprint: 0,
            under_playhead_fingerprint: (Timecode::zero(), 0),
            quiet_seek: false,
            sdr_transfer: klipp_core::SdrTransfer::Srgb,
            last_direct_present_blockers: Vec::new(),
            last_grab_wait: Duration::ZERO,
            frame_cache: FrameCache::new(120), // ~5s at 24fps (backward history + forward prefetch)
            render_cache_hot: crate::render_cache::HotCache::new(30),
            render_cache_disk,
            current_grade_hash: 0,
            current_grade_clip_id: None,
            current_graph_arc: None,
            in_node_param_edit: false,
            frame_phases: Default::default(),
            force_full_res_regrade: false,
            drag_started_at: None,
            drag_grade_count: 0,
            drag_downsampled_count: 0,
            foreground_activity: None,
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
            render_job: None,
            render_status_at: std::time::Instant::now(),
            scrub_audio_enabled: true,
            is_scrubbing: false,
            skim_pool: Default::default(),
            fadeable_media: FadeableMedia::new(),
            pre_seek_result: Arc::new(std::sync::Mutex::new(None)),
            pre_seek_generation: 0,
            pre_seek_started: false,
            preroll_decoder: None,
            preroll_pending: None,
            preroll_failed: None,
            preroll_clip_index: None,
            preroll_for: None,
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

    /// Start building the viewer's GPU pipeline now, on a thread of its
    /// own (#2108), so the first frame the GPU stack draws does not spend
    /// 0.8 s compiling every effect's shader on the engine's thread while a
    /// Play waits behind it. The app calls it as it starts the engine.
    pub fn prewarm_gpu(&mut self) {
        self.renderer.executor().prewarm_gpu();
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
        // The timeline the engine starts on is not an edit to show.
        self.note_layout_change(true);

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

            self.refresh_fadeable_media(had_commands);

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
                && self.parked_pregrade().is_none()
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
            self.tick_layout_check();
            // Full-quality refresh of the parked frame when playback ran
            // on the skim substitute (optimised playback). Runs every
            // tick: paused it debounces/polls/paints, any other state it
            // just tears down leftover refresh state and returns.
            self.tick_parked_refresh();
            // A pick waiting for its frame: answered once it is here.
            if self.state != PlaybackState::Playing {
                self.tick_node_sample();
            }

            // A frame parked with no shot of its own on the picture track,
            // every track a scene, leaves the engine Idle, not Paused: its
            // clip streams are waited on all the same, or a frame that
            // missed them is never redrawn and stays incomplete.
            if matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped) {
                self.tick_stream_refresh();
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
                // A paused frame shown before every clip stream had its
                // frame: re-render once they have.
                self.tick_stream_refresh();
                self.tick_audio_reposition();
                self.tick_paused_refresh();
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
                } else if self.scope_render_owed && self.should_render_scope() {
                    // The scopes still show an earlier picture, size or
                    // setting, and paused nothing else will render: render
                    // the parked frame again now that they are due. A due
                    // scope blocks every path that skips them, so this one
                    // draws them.
                    self.scope_render_owed = false;
                    self.dbg_regrade_count += 1;
                    self.regrade_cached_frame();
                } else if !had_commands && self.state != PlaybackState::Playing && self.render_job.is_some() {
                    // Render In to Out (#924): a frame a tick while
                    // paused, ahead of the nearby pre-render.
                    self.step_render_range();
                } else if !had_commands && self.last_command_at.elapsed() >= PRERENDER_AFTER_IDLE {
                    // Engine is genuinely idle — pre-render nearby frames
                    // into cache. Skip prerender when we just applied param
                    // changes so the next tick re-runs through the fresh
                    // graph instead of caching stale output. And not between
                    // the commands of someone stepping or clicking through
                    // frames: one pre-render is a full render (~100 ms at
                    // 4K), and the next step waited for it.
                    self.prerender_cache_frames();
                    if self.render_status_at.elapsed() >= Duration::from_secs(1) {
                        self.render_status_at = std::time::Instant::now();
                        self.update_render_cache_ranges();
                    }
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
                // Every 2 s while anything happens; quiet, every 30 s
                // (#2988), so a parked engine's beat doesn't bury what
                // the rest of the app logs.
                let busy = self.state == PlaybackState::Playing
                    || self.dbg_update_playback_count + self.dbg_regrade_count + self.dbg_param_changes_drained + self.dbg_commands_handled + rb_count > 0;
                let beat = busy || self.last_quiet_heartbeat.elapsed() >= Duration::from_secs(30);
                if !busy && beat {
                    self.last_quiet_heartbeat = std::time::Instant::now();
                }
                if beat { tracing::info!(
                    "ENGINE_HEARTBEAT state={:?} updates={} regrades={} param_changes={} cmds={} elapsed={:.2}s rate={:.1}/s drops_total={} hw_decode={:?} has_v1={} overlays={} cpu_readbacks={} readback_avg={:.2}ms readback_total={:.1}ms audio_gaps={}",
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
                    crate::audio_output::underrun_callbacks(),
                ); }
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
                {
                    let mixer = crate::mix_thread::lock_mixer(&self.mixer);
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
                    for (i, bus) in mixer.buses.iter().enumerate() {
                        let _ = self.event_tx.send(AppEvent::BusPeakLevels { bus_index: i, left: bus.peak_left, right: bus.peak_right });
                        master_l = master_l.max(bus.peak_left);
                        master_r = master_r.max(bus.peak_right);
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
                self.poll_delivery_measure();
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

            #[cfg(feature = "test-hooks")]
            AppCommand::InjectPanic { at } => self.inject_panic(&at),
            #[cfg(not(feature = "test-hooks"))]
            AppCommand::InjectPanic { .. } => warn!("inject_panic: this build has no test hooks"),

            AppCommand::LoadMediaFile { path, media_id } => {
                self.load_file(path, media_id);
            }

            AppCommand::Play => {
                self.forgive_panics();
                // A skim is over once the playhead moves or plays (#2945).
                self.end_skim();
                debug!("Play command received, state={:?}", self.state);
                // Before playback can reach a gap (#2386).
                self.prewarm_empty_stack();
                // Heard at the export's loudness: measured now if the
                // sequence changed since (#1565).
                self.refresh_delivery_gain();
                // A paused seek may still owe its audio.
                self.flush_pending_audio_reposition();
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
                    && (self.v1_decoder.is_some() || !self.audio_sources.is_empty() || self.clock_only)
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
                            self.fps.as_f64(),
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
                        let (queue_has_target, queue_empty) = {
                            let q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                            (
                                matches!(
                                    q.front(),
                                    Some(f) if !f.is_eof
                                        && (f.pts_secs() - source_secs).abs() < 0.1
                                ),
                                q.is_empty(),
                            )
                        };
                        // A seek to this very place is still decoding (the
                        // scrub that put the playhead here): its frames are
                        // on their way, and seeking again would start the
                        // group of pictures over.
                        let seek_under_way = queue_empty && dec.last_seek_secs().is_some_and(|t| (t - source_secs).abs() < 0.1);
                        if !queue_has_target && !seek_under_way {
                            dec.seek(source_secs);
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
                    self.note_view(format!("paused at {:?}", self.playhead.value));
                    if let Some(dec) = &self.v1_decoder {
                        dec.pause();
                    }
                    self.master_clock.pause();
                    for active in &self.audio_sources {
                        active.source.pause();
                    }
                    self.drop_audio_tails();
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
                    // The last presented primary is already the pause
                    // frame. Keep its GPU source instead of seeking and
                    // rebuilding it from the CPU pre-grade cache.
                    if let Some(primary) = self.retained_primary_at_playhead() {
                        self.present_frame(Some(primary));
                    } else {
                        let playhead = self.playhead;
                        self.seek_in_sequence(playhead);
                    }
                    // Stopped where it rests: the original-quality redraw
                    // of a skim frame starts now, not after the debounce a
                    // seek waits out (#2136). It painted about 0.7 s after
                    // Pause on the Stress Test; this saves the 0.4 s wait.
                    self.parked_since = Some((self.playhead, std::time::Instant::now().checked_sub(PARK_DEBOUNCE).unwrap_or_else(std::time::Instant::now)));
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
                    self.report_playhead();
                    (self.repaint_cb)();
                }
            }

            AppCommand::ResetEngine => {
                self.reset_to_idle();
            }

            AppCommand::SeekTo(tc) => {
                self.forgive_panics();
                // A skim is over once the playhead moves or plays (#2945).
                self.end_skim();
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
                    && self.parked_pregrade().is_some()
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
                // Reports follow on from here, whichever way it now plays
                // (#1806).
                self.report_floor.set(None);
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
                // A skim is over once the playhead moves or plays (#2945).
                self.end_skim();
                self.note_view(format!("asked to park at {:?} ({:?})", tc.value, self.state));
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
            AppCommand::SetSdrTransfer(transfer) => {
                if self.sdr_transfer != transfer {
                    info!("Engine: SDR viewer transfer {:?} → {transfer:?}", self.sdr_transfer);
                    self.sdr_transfer = transfer;
                    // The parked frame is shown again under the new encode.
                    self.regrade_cached_frame();
                }
            }
            AppCommand::SetLutTetrahedral(tetrahedral) => {
                self.color_pipeline.use_tetrahedral = tetrahedral;
                self.regrade_cached_frame();
            }

            AppCommand::SetTrackVolume { track_index, volume } => {
                // Into the sequence the levels are synced from too, so a
                // sequence update before the app's next sync does not put
                // the old one back (#3165), as mute and solo go.
                if let Some(track) = self.sequence.write().unwrap_or_else(|e| e.into_inner()).tracks.get_mut(track_index) {
                    track.volume = volume;
                }
                {
                    let mut m = crate::mix_thread::lock_mixer(&self.mixer);
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].volume = volume;
                }
            }
            AppCommand::OverrideTrackAutomation { track_index, param, value } => {
                if let Ok(mut o) = self.automation_override.lock() {
                    match value {
                        Some(v) => o.insert((track_index, param), v),
                        None => o.remove(&(track_index, param)),
                    };
                }
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                self.sync_track_automation(&seq, self.audio_heard_at().to_secs_f64());
            }
            // The switch goes into the sequence the mix reads it from, as
            // the app sets it (solo is one track at a time), so a
            // sequence update that comes before the app's next sync does
            // not put the old one back (#2672).
            AppCommand::SetTrackMute { track_index, muted } => {
                if let Some(track) = self.sequence.write().unwrap_or_else(|e| e.into_inner()).tracks.get_mut(track_index) {
                    track.muted = muted;
                }
                self.sync_track_switches();
            }
            AppCommand::SetTrackSolo { track_index, solo } => {
                for (i, track) in self.sequence.write().unwrap_or_else(|e| e.into_inner()).tracks.iter_mut().enumerate() {
                    track.solo = solo && i == track_index;
                }
                self.sync_track_switches();
            }
            AppCommand::SetTrackPan { track_index, pan } => {
                if let Some(track) = self.sequence.write().unwrap_or_else(|e| e.into_inner()).tracks.get_mut(track_index) {
                    track.pan = pan;
                }
                {
                    let mut m = crate::mix_thread::lock_mixer(&self.mixer);
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
                {
                    let mut m = crate::mix_thread::lock_mixer(&self.mixer);
                    m.master_volume = vol;
                }
            }
            AppCommand::SetDeliveryTarget(target) => self.set_delivery_target(target),
            AppCommand::ResetLufsMeter => {
                if let Some(ref flag) = self.lufs_reset {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }

            // The edit is in the sequence already: each source takes its
            // own clip's settings from there. The value alone does not say
            // which clip it is for, and set on the whole output it played
            // every track at it.
            AppCommand::SetClipVolume(_) | AppCommand::SetClipPan(_) | AppCommand::SetClipGain(_) | AppCommand::SetClipChannelConfig(_) => {
                self.sync_clip_volumes();
            }

            AppCommand::MediaDecodeChanged => {
                info!("Engine: media decode settings changed; re-decoding at the playhead");
                self.cached_pregrade_frame = None;
                self.pregrade_source = None;
                self.retained_primary = None;
                // Pre-grades decoded the old way.
                self.frame_cache.clear();
                // Rendered from pixels decoded the old way. (The disk
                // cache misses on its own: the log format is in the key.)
                self.render_cache_hot.clear();
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
            AppCommand::SetRenderCacheLimitGb(gb) => {
                self.render_cache_disk.set_max_size_gb(gb);
                debug!("Render cache cap {gb} GB");
            }
            AppCommand::SetRenderCacheEnabled(enabled) => {
                self.render_cache_disk.set_enabled(enabled);
                if !enabled {
                    self.render_cache_hot.clear();
                }
                debug!("Render cache enabled={enabled}");
            }
            AppCommand::RenderRange { from, to } => {
                self.start_render_range(from, to);
            }
            AppCommand::CancelRenderRange => {
                self.cancel_render_range();
            }
            AppCommand::ClearRenderCache { from, to } => {
                self.clear_render_cache_range(from, to);
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
            AppCommand::SkimTimeline { time } => {
                let composited = self.skim_timeline(time);
                let _ = self.event_tx.send(AppEvent::TimelineSkimShown { time, composited });
            }
            AppCommand::EndScrub => {
                self.end_skim();
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
                {
                    let mut mixer = crate::mix_thread::lock_mixer(&self.mixer);
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

            AppCommand::SetSplitCompareNode { clip_id, path } => {
                self.compare_effect_node = path.clone().filter(|p| p.node().is_some());
                self.compare_effect_active = clip_id.is_some() && self.compare_effect_node.is_some();
                self.compare_effect_clip_id = clip_id;
                debug!(
                    "SetSplitCompareNode: active={} clip={:?} node={:?}",
                    self.compare_effect_active, self.compare_effect_clip_id, self.compare_effect_node,
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

            AppCommand::SetMeasureOnlyClip(clip) => {
                self.measure_only_clip = clip;
                debug!("Measure only clip: {:?}", clip);
            }

            AppCommand::SetInspectorMattePreviewTarget(target) => {
                // Sent on every UI frame: logged only when it changes
                // (#2988), 10 000 lines in two minutes otherwise.
                if self.inspector_matte_preview_target != target {
                    debug!("Inspector matte preview target: {:?}", target);
                }
                self.inspector_matte_preview_target = target;
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
                // A pane just opened on a mode has nothing to show until it is drawn.
                self.scope_render_owed = true;
                // Force a fresh render on the next tick so newly-added
                // panes don't sit blank on a paused viewer.
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeSizes { sizes } => {
                self.scope_sizes = sizes;
                // Drawn at the new size on the next scope tick, paused or not.
                self.scope_render_owed = true;
            }
            AppCommand::SetScopeSettings { settings } => {
                self.scope_settings = settings;
                // The grade hash below does not redraw the scopes by itself.
                self.scope_render_owed = true;
                // Invalidate the grade hash so the next tick re-renders
                // the paused frame. Without this, scope-settings changes
                // (R/G/B pills, filter, intensity, opacities, …) only
                // take visible effect when playback or a grade edit
                // already triggers a re-grade.
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeLowPass { enabled } => {
                self.scope_low_pass_enabled = enabled;
                self.scope_render_owed = true;
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeLut { lut } => {
                self.scope_lut = lut;
                self.scope_render_owed = true;
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopeBloom { enabled } => {
                self.scope_bloom_enabled = enabled;
                self.scope_render_owed = true;
                self.current_grade_hash = 0;
            }
            AppCommand::SetScopesVisible(visible) => {
                self.scopes_visible = visible;
                self.scope_render_owed |= visible;
            }
            AppCommand::RenderScopesForFrame { rgba, width, height } => {
                // Draw the scopes of a UI-supplied RGBA frame the way the
                // playback pipeline does, into `fb.scope_textures`. Used by
                // media-browser hover-skim and timeline scrub so scope
                // quality matches playback.
                // The UI hands over an sRGB u8 frame (a skim or scrub
                // snapshot). The GPU scopes measure those codes as they
                // are; the remaining scopes linearise them.
                // Drawn now, so an owed timeline redraw waits its interval
                // rather than landing over a skim, and what was owed is
                // paid: left owed, the paused tick rendered the parked
                // frame over the skim a skim cycle later, again and again.
                self.draw_scopes(None, Some(DisplayScopeFrame::rec709(&rgba)), width, height);
            }
            AppCommand::RenderScopeSnapshot { modes, rgba, width, height } => {
                // Draw the requested scopes of a supplied frame into the
                // snapshot store, without touching the live scope state.
                let wanted: Vec<u8> = modes.iter().copied().filter(|m| *m <= 3).collect();
                if let Some(results) = self.render_scopes_on_gpu(None, Some(DisplayScopeFrame::rec709(&rgba)), width, height, &wanted) {
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    for (mode, res) in wanted.iter().zip(results) {
                        if let Some((bytes, w, h)) = res {
                            let e = fb.snapshot_scope_textures.entry(*mode).or_insert(crate::texture::ScopeTextureEntry { data: Vec::new(), w: 0, h: 0, generation: 0 });
                            e.data = bytes;
                            e.w = w;
                            e.h = h;
                            e.generation = e.generation.wrapping_add(1);
                        }
                    }
                }
            }
            AppCommand::SampleNodeInput { clip_id, node_id, at, nx, ny } => {
                self.sample_node_input(clip_id, node_id, &at, nx, ny);
            }
            AppCommand::SetViewChannel { channel } => {
                if self.view_channel != channel {
                    self.view_channel = channel;
                    debug!("View channel: {channel:?}");
                    self.regrade_cached_frame();
                }
            }
            AppCommand::SetNodeSolo { at } => {
                self.solo = at;
                debug!("Solo node: {:?}", self.solo);
            }
            AppCommand::SetVisibleNodeThumbs { request, clip_id, at } => {
                self.inspect_clip = Some(clip_id);
                self.thumbnail_request = request;
                self.thumbnail_priority = at.into_iter().take(crate::node_thumbnails::MAX_THUMBS).collect();
                self.visible_node_thumbs = self.thumbnail_priority.iter().cloned().collect();
                // Clear the old thumb cache so the next re-grade is
                // what the UI sees (stale thumbs from a different node
                // set would confuse the user mid-transition).
                {
                    let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    fb.node_thumbnail_batch = None;
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
                request,
                clip_id,
                playhead,
                src_rgba,
                src_w,
                src_h,
            } => {
                self.render_node_thumbs_from_source(
                    request, clip_id, playhead, src_rgba, src_w, src_h,
                );
            }

            AppCommand::SetSourceMedia { media_id, path, source_rotation } => {
                // The sound comes from the original file whatever the
                // picture is decoded from.
                let audio_path = self
                    .media_pool
                    .read()
                    .ok()
                    .and_then(|pool| pool.get(&media_id).map(|m| m.path.clone()))
                    .unwrap_or_else(|| path.clone());
                self.set_source_media(path, source_rotation, audio_path);
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
                // The pre-grade cache is keyed by playhead position: valid
                // only while the same media plays at each position. A
                // grade change leaves it alone; a cut, move, trim, speed
                // or rotation change empties it.
                let under_playhead_changed = self.note_layout_change(true);
                // The project's frame rate may be what changed.
                self.sync_fps_to_sequence();
                // Sync per-clip volume/pan to active audio sources
                self.sync_clip_volumes();
                self.sync_track_switches();
                self.sync_track_levels();
                // Restart audio sources only when audio-relevant state changed.
                // Title color/opacity/position edits fire SequenceUpdated but don't
                // affect audio — skipping the teardown+rebuild prevents a storm that
                // freezes the playhead during live editing.
                if !self.audio_sources.is_empty() {
                    if self.audio_edited_at(self.playhead) {
                        let tc = self.playhead;
                        self.start_all_audio(tc);
                        if self.state == PlaybackState::Playing {
                            for active in &self.audio_sources {
                                active.source.resume();
                            }
                        }
                    } else {
                        // Same chains, maybe new settings: into the running
                        // chains in place, no restart and no gap.
                        self.push_audio_effect_settings();
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
                            // A retimed clip's own sound is out of the
                            // mix (see `audio_rows`); the rest plays on (#638).
                        }
                    }
                    // The clip the primary decoder plays was reversed: it
                    // cannot play it on, and hands over to the clock (#617).
                    let now_reversed = self.v1_decoder.is_some()
                        && seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).is_some_and(|c| c.is_reversed());
                    drop(seq);
                    if now_reversed {
                        let at = self.playhead;
                        self.seek_in_sequence(at);
                    } else if speed_state_changed {
                        self.reset_ramp_anchor();
                    }
                } else if under_playhead_changed {
                    // Another clip, or another part of one, is under the
                    // playhead: the parked frame is the old picture.
                    self.show_edit_at_playhead();
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
            // An abandoned gesture ends the drag as a committed one does.
            AppCommand::EndNodeParamEdit { .. } | AppCommand::AbandonNodeParamEdit { .. } => {
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

    /// No decoder for the clip the playhead is on, and one is to be opened:
    /// not while it is left off for panicking (#3181).
    fn decoder_missing(&self) -> bool {
        self.v1_decoder.is_none() && !self.decoder_given_up
    }

    /// Tell the person that `what` kept failing and is left off until they
    /// move or play the playhead (#3181): a log line alone was not seen.
    fn say_given_up(&self, what: &str) {
        let _ = self.event_tx.send(AppEvent::PlaybackGaveUp { what: what.to_string() });
    }

    /// The person moved or played the playhead: what a panic ended may be
    /// made again as many times as at first (#3181).
    fn forgive_panics(&mut self) {
        self.decoder_restarts = 0;
        self.mix_restarts = 0;
        self.decoder_given_up = false;
        self.decoder_panic_clip = None;
        self.streams.forgive_panics();
    }

    fn handle_play(&mut self) {
        // Playing on: the frames held for stepping back are let go.
        self.step_back = None;
        // Sync fps from the active sequence so synthetic clips play at the correct rate.
        // Without this, a generator-only timeline plays at the engine's default 24fps
        // because self.fps is only updated by load_file() and generators never trigger it.
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.fps = seq.fps;
        }

        // Only reversed clips' pictures here: nothing for the primary
        // decoder, which reads forward. The renderer draws them from their
        // own readers while the clock runs (#617).
        if self.only_reversed_pictures_at(self.playhead) {
            self.play_on_the_clock();
            return;
        }

        // Check if no media clip exists at playhead but a title or generator clip does
        {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let has_media_clip = seq.tracks.iter()
                .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                .any(|track| {
                    track.clip_at_timecode(self.playhead)
                        .map(|(_, c)| c.enabled && c.media_id().is_some() && !c.is_reversed())
                        .unwrap_or(false)
                });
            let title_info = if !has_media_clip {
                seq.tracks.iter().enumerate()
                    .filter(|(_, t)| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                    .find_map(|(ti, track)| {
                        track.clip_at_timecode(self.playhead).and_then(|(idx, clip)| {
                            if !clip.enabled { return None; }
                            if let Some(td) = clip.title_data() {
                                return Some((ti, idx, td.clone(), clip.timeline_in, clip.source_in.to_secs_f64(), clip.speed));
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
                            Some((ti, idx, gd, clip.timeline_in, clip.source_in.to_secs_f64(), clip.speed))
                        })
                    })
            } else {
                None
            };
            drop(seq);

            if let Some((ti, idx, _title_data, timeline_in, clip_source_in, speed)) = title_info {
                info!("handle_play: title clip {idx} on track {ti} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_track_index = ti;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = speed;
                self.master_clock.set_speed(speed);
                self.refresh_speed_keyframes();

                // Start audio for all tracks
                self.start_all_audio(self.playhead);
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // The clock counts source seconds, matching the synthetic tick.
                let source_secs = self.clip_source_start + (self.playhead - timeline_in).to_secs_f64() * speed;
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
            if let Some((ti, idx, _gen_data, timeline_in, clip_source_in, speed)) = generator_info {
                info!("handle_play: generator clip {idx} on track {ti} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_track_index = ti;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = speed;
                self.master_clock.set_speed(speed);
                self.refresh_speed_keyframes();

                self.present_frame(None);

                // Start audio + clock for playback
                self.start_all_audio(self.playhead);
                let source_secs = self.clip_source_start + (self.playhead - timeline_in).to_secs_f64() * speed;
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
                if let Some((idx, clip)) = track.clip_at_timecode(self.playhead).filter(|(_, c)| !c.is_reversed()) {
                    if let Some(media_id) = clip.media_id() {
                        let source_secs = klipp_compose::render::source_secs(clip, self.playhead, seq.fps.as_f64());
                        let clip_source_in = clip.source_in.to_secs_f64();
                        return Some((track_idx, idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation));
                    }
                }
                // Find next non-gap clip at or after playhead
                for (idx, clip) in track.clips.iter().enumerate().filter(|(_, c)| !c.is_reversed()) {
                    if clip.timeline_in >= self.playhead {
                        if let Some(media_id) = clip.media_id() {
                            let si = clip.source_in.to_secs_f64();
                            return Some((track_idx, idx, media_id, si, clip.timeline_in, si, clip.speed, clip.source_rotation));
                        }
                    }
                }
                // Fallback: first non-gap clip
                for (idx, clip) in track.clips.iter().enumerate().filter(|(_, c)| !c.is_reversed()) {
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

                // A retimed clip's own sound is left out of the mix by
                // `audio_rows`; music and other tracks play on (#638).

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
    /// Put the audio at `tc`: seek the open sources when the clips with
    /// audio there are the ones they were opened for, otherwise open the
    /// set afresh.
    ///
    /// Every seek used to tear the whole audio pipeline down and build
    /// it again — each source's decoder, the mix thread and the cpal
    /// output stream — and only then seek, though the sources can seek
    /// in place and `seek_all_audio_to_timecode` already reopens when a
    /// clip's media or effect chain differs. That rebuild was most of a
    /// seek's cost, so a scrub ran at a handful of positions a second.
    /// [`Self::reposition_audio`], unless that means opening sources and
    /// nothing is waiting to hear them: paused, not scrubbing, the sound
    /// at `tc` coming from other clips than the open sources. Then the
    /// frame is shown first and the sources follow once the playhead has
    /// rested a moment ([`Self::tick_audio_reposition`]), or at once when
    /// playback starts. A click along the timeline showed its frame half
    /// a second late for this, and a frame grab that seeks there and back
    /// paid it twice.
    ///
    /// `true` when the sources are at `tc` on return.
    fn reposition_audio_or_defer(&mut self, tc: Timecode, source_secs: f64) -> bool {
        let cheap = !self.audio_sources.is_empty()
            && self.audio_output.is_some()
            && !self.audio_edited_at(tc)
            && self.audio_sources.len() == self.audio_source_count_at(tc);
        // A scrub only has a listener when scrub audio is on: the chirps play
        // from `audio_sources.first()`. Counting a silent scrub as listening
        // repositioned audio on every drag step, rebuilding the pipeline at
        // each clip boundary (a fresh cpal stream, ~17 ms) mid-drag.
        let nobody_listening =
            self.state != PlaybackState::Playing && !(self.is_scrubbing && self.scrub_audio_enabled);
        if nobody_listening && !cheap {
            self.pending_audio_reposition = Some((tc, source_secs, std::time::Instant::now()));
            return false;
        }
        self.pending_audio_reposition = None;
        self.reposition_audio(tc);
        true
    }

    /// Bring the sources to a deferred seek's time, now.
    fn flush_pending_audio_reposition(&mut self) {
        if let Some((tc, _, _)) = self.pending_audio_reposition.take() {
            self.reposition_audio(tc);
        }
    }

    /// The paused tick's share of a deferred audio reposition: once the
    /// playhead has rested, open the sources and play the scrub chirp the
    /// seek would have.
    fn tick_audio_reposition(&mut self) {
        const REST: Duration = Duration::from_millis(250);
        let Some((tc, source_secs, since)) = self.pending_audio_reposition else { return };
        if since.elapsed() < REST {
            return;
        }
        self.pending_audio_reposition = None;
        self.reposition_audio(tc);
        if self.scrub_audio_enabled {
            if let Some(active) = self.audio_sources.first() {
                active.source.chirp(source_secs, 0.04);
                if let Some(aout) = &self.audio_output {
                    aout.0.start_chirp();
                }
            }
        }
    }

    fn reposition_audio(&mut self, tc: Timecode) {
        let same_set = !self.audio_sources.is_empty()
            && self.audio_output.is_some()
            && !self.audio_edited_at(tc)
            && self.audio_sources.len() == self.audio_source_count_at(tc);
        if same_set {
            self.seek_all_audio_to_timecode(tc);
        } else {
            self.start_all_audio(tc);
            self.seek_all_audio_to_timecode(tc);
        }
    }

    /// How many sources `start_all_audio` would open at `tc`.
    fn audio_source_count_at(&self, tc: Timecode) -> usize {
        self.audio_rows_at(tc).len()
    }

    fn start_all_audio(&mut self, tc: Timecode) {
        self.pending_audio_reposition = None;
        // Update fingerprint so SequenceUpdated knows the current audio state
        // (prevents redundant restart on the next SequenceUpdated after a seek/play).
        self.record_audio_at(tc);
        self.stop_all_audio();

        let track_audio = self.audio_rows_at(tc);

        debug!("start_all_audio: {} tracks with audio at {:.3}s", track_audio.len(), tc.to_secs_f64());
        for (track_idx, media_id, path, source_secs, audio_effects, audio_effect_keyframes) in track_audio {
            debug!("start_all_audio: track {} has {} audio effects ({} kf tracks)",
                track_idx, audio_effects.len(), audio_effect_keyframes.len());
            self.open_audio_source(track_idx, media_id, &path, source_secs, audio_effects, audio_effect_keyframes);
        }
        self.open_ringing_tails(tc);
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

    /// Sync each source's clip volume, gain, pan, channel config and
    /// fade from the sequence: every source its own clip's, as the export
    /// mixes them. Called every tick while playing, on start_all_audio
    /// and on SequenceUpdated.
    /// Where the samples the mix thread is making now will be heard: the
    /// playhead plus the mix lead while playing, the playhead otherwise.
    /// The sounds of clips that start within [`OPEN_AHEAD_SECS`] of where
    /// the mix is, other than what sounds now (`now`): each opened from
    /// its clip's first source second, with the clip's start and the
    /// mixed frame it is heard at (#1588), on a silent track or at a cut
    /// on a sounding one (#1632). A cut within one file's sound is left
    /// to it: it plays on.
    fn audio_rows_starting_soon(&self, now: &[TrackAudioRow]) -> Vec<(TrackAudioRow, Option<(Timecode, u64)>)> {
        let Some(mt) = &self.mix_thread else { return Vec::new() };
        let sample_rate = self.cpal_sample_rate;
        let (mixed, buffered) = (mt.mixed_frames(), mt.buffered_samples());
        let heard = self.playhead.to_secs_f64() + mix_lead_secs(buffered, sample_rate);
        let soon = RationalTime::from_secs_f64(heard + OPEN_AHEAD_SECS, 48_000);
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        audio_rows(&seq, &pool, &self.fadeable_media, soon, self.use_proxies, false)
            .into_iter()
            .filter(|row| row.0 < LANE_STRIDE && !now.iter().any(|n| n.0 == row.0 && n.1 == row.1))
            .filter_map(|mut row| {
                // A clip whose sound comes in through a crossfade is the
                // crossfade's to start, on its lanes.
                let sounding = sounding_for_key(&seq, &self.fadeable_media, row.0, soon).filter(|s| s.extent.head_secs <= 0.0 && s.extent.fade_in_secs <= 0.0)?;
                let clip = sounding.clip;
                let frame = start_frame_for(clip.timeline_in.to_secs_f64(), heard, mixed, sample_rate)?;
                row.3 = klipp_compose::render::source_secs(clip, clip.timeline_in, seq.fps.as_f64());
                Some((row, Some((clip.timeline_in, frame))))
            })
            .collect()
    }

    /// Each sound whose clip ends within [`OPEN_AHEAD_SECS`] of where the
    /// mix is ends on its last sample (#1632): at a cut into another
    /// file's clip, the sample the incoming one starts on; before a gap,
    /// its out point. Retired at the next tick past it, it played that
    /// long past its clip. A clip whose sound runs on through a crossfade
    /// is left to the crossfade, and one the next clip continues (the
    /// same file, the same chain) plays on.
    fn end_sources_on_their_samples(&self) {
        let Some(mt) = &self.mix_thread else { return };
        let sample_rate = self.cpal_sample_rate;
        let (mixed, buffered) = (mt.mixed_frames(), mt.buffered_samples());
        let heard = self.playhead.to_secs_f64() + mix_lead_secs(buffered, sample_rate);
        let at = RationalTime::from_secs_f64(heard, 48_000);
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        for active in self.audio_sources.iter().filter(|a| a.track_index < LANE_STRIDE) {
            let Some(sounding) = sounding_for_key(&seq, &self.fadeable_media, active.track_index, active.starts_at.filter(|s| *s > at).unwrap_or(at)) else { continue };
            // One that rings on is fed silence past its out point and
            // ends itself when its tail has run (#2780).
            if active.may_ring || sounding.extent.tail_secs > 0.0 || sounding.extent.fade_out_secs > 0.0 || sounding.clip.media_id() != Some(active.media_id) {
                continue;
            }
            let end = sounding.clip.timeline_out;
            let Some(track) = seq.tracks.get(key_track(active.track_index)) else { continue };
            // Carried on by the next clip: the same sound, played on.
            let carried = carried_on(track, sounding.clip, active.media_id, active.audio_chain_fingerprint);
            if carried || end.to_secs_f64() > heard + OPEN_AHEAD_SECS {
                continue;
            }
            if let Some(frame) = start_frame_for(end.to_secs_f64(), heard, mixed, sample_rate) {
                active.mix.end_at(frame);
            }
        }
    }

    fn audio_heard_at(&self) -> Timecode {
        let lead = match (&self.mix_thread, self.state) {
            (Some(mt), PlaybackState::Playing) => mix_lead_secs(mt.buffered_samples(), self.cpal_sample_rate),
            _ => 0.0,
        };
        self.playhead + RationalTime::from_secs_f64(lead, self.fps.rate())
    }

    /// Each track's automated volume and pan into the mixer (#2306). The
    /// samples mixed next are heard from `heard_at`, and the mixer ramps
    /// to a new value over that block, so the value is taken at the
    /// block's end: the ramp then follows the keys over the block, as the
    /// export does per frame, and between engine ticks the level holds
    /// (a steep move steps once a tick, ramped). A track whose automation
    /// stopped applying (its keys gone, or Off) gets its controls as set
    /// back; one never automated is left as it is. The flags are by track
    /// position: after a track is moved or deleted one may be stale, which
    /// only sets that slot's controls as set once more.
    fn sync_track_automation(&self, seq: &klipp_timeline::Sequence, heard_at: f64) {
        use klipp_timeline::TrackParam;
        let block_secs = if self.cpal_sample_rate > 0 {
            (crate::mix_thread::MIX_CHUNK_SAMPLES / 2) as f64 / self.cpal_sample_rate as f64
        } else {
            0.0
        };
        let at = heard_at + block_secs;
        let Ok(mut was) = self.automated_tracks.lock() else { return };
        let overrides = self.automation_override.lock().map(|o| o.clone()).unwrap_or_default();
        let now: Vec<bool> = seq
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| t.automation.plays(TrackParam::Volume) || t.automation.plays(TrackParam::Pan) || overrides.keys().any(|(o, _)| *o == i))
            .collect();
        if now.iter().all(|a| !a) && was.iter().all(|a| !a) {
            return;
        }
        {
            let mut m = crate::mix_thread::lock_mixer(&self.mixer);
            m.ensure_tracks(seq.tracks.len());
            for (i, track) in seq.tracks.iter().enumerate() {
                if now[i] || was.get(i).copied().unwrap_or(false) {
                    let value = |param: TrackParam, set: f32| overrides.get(&(i, param)).copied().unwrap_or_else(|| track.automation.value_at(param, at, set));
                    m.tracks[i].volume = value(TrackParam::Volume, track.volume);
                    m.tracks[i].pan = value(TrackParam::Pan, track.pan);
                }
            }
        }
        *was = now;
    }

    /// The sequence's aux buses and each track's sends into the mixer
    /// (#2861), made here, on the edit side, so the mix thread only runs
    /// them: the buses, their effects and the order they are mixed in,
    /// and each send's level at the end of the block heard next, which
    /// the mixer ramps to as it does a fader. A send to a bus that is
    /// gone is left out.
    fn sync_routing(&self, m: &mut klipp_audio::AudioMixer, seq: &klipp_timeline::Sequence, heard_at: f64) {
        if seq.buses.is_empty() && m.buses.is_empty() {
            return;
        }
        let block_secs = if self.cpal_sample_rate > 0 {
            (crate::mix_thread::MIX_CHUNK_SAMPLES / 2) as f64 / self.cpal_sample_rate as f64
        } else {
            0.0
        };
        let at = heard_at + block_secs;
        m.set_bus_count(seq.buses.len());
        for (i, bus) in seq.buses.iter().enumerate() {
            let lane = &mut m.buses[i];
            (lane.volume, lane.pan, lane.muted) = (bus.volume, bus.pan, bus.muted);
            lane.output = bus.output.and_then(|o| seq.bus_index(o));
            if lane.name != bus.name {
                lane.name = bus.name.clone();
            }
            m.set_bus_inserts(i, &bus.effects);
        }
        m.set_bus_order(&seq.bus_order());
        m.ensure_tracks(seq.tracks.len());
        for (lane, track) in m.tracks.iter_mut().zip(&seq.tracks) {
            let sends = track.sends.iter().filter_map(|s| Some((seq.bus_index(s.bus)?, s)));
            let mut n = 0;
            for (bus, send) in sends {
                let gain = send.gain_at(at, track.automation.mode);
                match lane.sends.get_mut(n) {
                    // The same send: its level ramps from where it was.
                    Some(l) if l.bus == bus => {
                        (l.gain, l.pre_fader, l.muted) = (gain, send.pre_fader, send.muted);
                    }
                    Some(l) => *l = klipp_audio::SendLane::new(bus, gain, send.pre_fader, send.muted),
                    None => lane.sends.push(klipp_audio::SendLane::new(bus, gain, send.pre_fader, send.muted)),
                }
                n += 1;
            }
            lane.sends.truncate(n);
        }
        // The delay lines to the routing: a bus's effects or a send
        // changed may change how long each way is held back.
        m.refresh_pdc();
        // A way through the buses longer than is made up for is said, so
        // the mixer can show it, when that changes.
        let held = m.buses_held_at_cap();
        if let Ok(mut sent) = self.buses_held_sent.lock() {
            if *sent != held {
                let _ = self.event_tx.send(AppEvent::BusesHeldAtCap(held.clone()));
                *sent = held;
            }
        }
    }

    /// Each track's volume and pan as the sequence has them, onto its mixer
    /// lane (#3165): an undo or a redo of a fader puts them back there,
    /// which otherwise reached the mix only by a move of the fader. A
    /// control its automation plays, or that is being moved live, is left
    /// to [`Self::sync_track_automation`].
    fn sync_track_levels(&self) {
        use klipp_timeline::TrackParam;
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let overrides = self.automation_override.lock().map(|o| o.clone()).unwrap_or_default();
        let mut m = crate::mix_thread::lock_mixer(&self.mixer);
        m.ensure_tracks(seq.tracks.len());
        for (i, (lane, track)) in m.tracks.iter_mut().zip(&seq.tracks).enumerate() {
            let set = |param: TrackParam| !track.automation.plays(param) && !overrides.contains_key(&(i, param));
            if set(TrackParam::Volume) {
                lane.volume = track.volume;
            }
            if set(TrackParam::Pan) {
                lane.pan = track.pan;
            }
        }
    }

    /// Each track's mute and solo as the sequence has them, onto its
    /// mixer lane: a project opened, a sequence switched to or an undo
    /// brings its own, which otherwise reached the mix only by a click
    /// on the button (#2672).
    fn sync_track_switches(&self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut m = crate::mix_thread::lock_mixer(&self.mixer);
        m.ensure_tracks(seq.tracks.len());
        for (lane, track) in m.tracks.iter_mut().zip(&seq.tracks) {
            lane.muted = track.muted;
            lane.solo = track.solo;
        }
    }

    fn sync_clip_volumes(&self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // A blade join's continuing sound takes no fade-in; the flag is
        // for the first source with a clip, as it always was.
        let mut contiguous = self.contiguous_audio.swap(false, std::sync::atomic::Ordering::Relaxed);
        // The settings apply in the mix thread, ahead of the output ring,
        // so a keyframed volume, pan or gain is evaluated where the
        // samples mixed now will be heard, as the export evaluates each
        // sample at its own time; at the playhead a duck came ~200 ms early.
        let heard_tc = self.audio_heard_at();
        let heard_at = heard_tc.to_secs_f64();
        self.sync_track_automation(&seq, heard_at);
        // Each track's own effects, on its summed clips before its fader
        // (#2560): the mix thread runs them, as the export does.
        {
            let mut m = crate::mix_thread::lock_mixer(&self.mixer);
            for (i, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Audio) {
                m.set_track_inserts(i, &track.effects.audio, 8192);
            }
            self.sync_routing(&mut m, &seq, heard_at);
        }

        for active in &self.audio_sources {
            if seq.tracks.get(key_track(active.track_index)).is_some() {
                // A sound opened ahead of its clip takes the clip's
                // settings at its start until it gets there.
                let own_tc = own_clip_tc(&seq, &self.fadeable_media, active, self.playhead, heard_tc);
                let own_tc = match active.starts_at {
                    Some(at) if at > own_tc => at,
                    _ => own_tc,
                };
                if let Some(sounding) = sounding_for_key(&seq, &self.fadeable_media, active.track_index, own_tc) {
                    let audio_clip = sounding.clip;
                    // Its sound stops at the out point, its effects ringing
                    // on, unless the next clip plays it on (#2780).
                    if let (Some(ring), Some(track)) = (active.source.ring(), seq.tracks.get(key_track(active.track_index))) {
                        ring.set_stops(sounding.extent.tail_secs > 0.0 || !carried_on(track, audio_clip, active.media_id, active.audio_chain_fingerprint));
                    }
                    let ClipSound { volume: clip_volume, pan: clip_pan, gain: clip_gain, fade_in, fade_out, fade_in_curve: fi_curve, fade_out_curve: fo_curve, channel_config: ch_config } = clip_sound_at(&seq, audio_clip, heard_at, self.fps.as_f64());
                    let source_in_secs = audio_clip.source_in.to_secs_f64();
                    let clip_dur_secs = audio_clip.duration().to_secs_f64();
                    // Read at the clip's speed, backward when reversed (#642).
                    active.source.set_varispeed(audio_clip.speed, source_in_secs, audio_clip.source_out.to_secs_f64(), audio_clip.keep_pitch);

                    // Set fade params on the audio source
                    // When contiguous_audio is true, offset source_in backward so the
                    // fade envelope doesn't think we're at position 0 (which would
                    // trigger a fade-in from silence at blade cut boundaries)
                    let is_contiguous = std::mem::take(&mut contiguous);
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
                    active.mix.set(clip_volume * clip_gain, clip_pan, channel_config_to_u32(ch_config));
                }
            }
        }
        // Every source inside a crossfade gets its crossfade envelope in
        // place of the clip fade set above: both sides of the join fade.
        for active in &self.audio_sources {
            let own_tc = own_clip_tc(&seq, &self.fadeable_media, active, self.playhead, heard_tc);
            set_crossfade_envelope(&seq, &self.fadeable_media, active, own_tc);
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
        self.pending_audio_reposition = None;
        self.clear_preroll();
        self.preroll_source_in = 0.0;

        // Stop audio
        self.stop_all_audio();
        crate::pcm_reader::release_all();
        self.last_audio_fingerprint = 0;
        self.last_audio_clips.clear();

        // Clear caches (frame, render hot, pre-grade, grade hash)
        self.cached_pregrade_frame = None;
        self.pregrade_source = None;
        self.retained_primary = None;
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
            fb.pending_data.clear();
            fb.pending_width = 0;
            fb.pending_height = 0;
            fb.pending_ready = false;
            fb.render_cache_ranges.clear();
            fb.generation = fb.generation.wrapping_add(1);
        }

        info!("Engine reset to idle (project unloaded)");
        let _ = self.event_tx.send(AppEvent::PlaybackStopped);
        self.report_playhead();
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
    /// Record the sound at `tc` as the sound playing: its fingerprint and
    /// the clips it was taken over.
    fn record_audio_at(&mut self, tc: Timecode) {
        self.last_audio_fingerprint = self.audio_fingerprint_at(tc);
        self.last_audio_clips = self.clips_at(tc);
    }

    /// Whether the sound at `tc` was edited since it was recorded: the
    /// same clips, with other media, speed or effects. Other clips there
    /// mean playback crossed a cut, a swap took the next clip, or a seek
    /// landed elsewhere; the sound is recorded again, and nothing counts
    /// as edited. Compared as a fingerprint alone, every cut read as an
    /// edit, and the SequenceUpdated the UI sends around cuts restarted
    /// every source mid-play: the mix thread stopped, each track's PCM
    /// reopened, 100–190 ms and the playhead stepping back (#580).
    fn audio_edited_at(&mut self, tc: Timecode) -> bool {
        if self.clips_at(tc) != self.last_audio_clips {
            self.record_audio_at(tc);
            return false;
        }
        self.audio_fingerprint_at(tc) != self.last_audio_fingerprint
    }

    /// The clip under `tc` on each track, in track order.
    fn clips_at(&self, tc: Timecode) -> Vec<Option<klipp_core::ClipId>> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        seq.tracks.iter().map(|t| t.clip_at_timecode(tc).map(|(_, c)| c.id)).collect()
    }

    fn audio_fingerprint_at(&self, tc: Timecode) -> u64 {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut fp: u64 = 0;
        for track in &seq.tracks {
            if let Some((_, clip)) = track.clip_at_timecode(tc) {
                fp = fp.wrapping_mul(31).wrapping_add(audio_fingerprint_for_clip(clip));
            }
        }
        fp
    }
}

/// The fade envelope a source plays: what `AudioSource::set_fade`
/// takes, computed by [`envelope_of`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Envelope {
    source_in_secs: f64,
    duration_secs: f64,
    fade_in_secs: f64,
    fade_out_secs: f64,
    fade_in_curve: u32,
    fade_out_curve: u32,
    gain: f32,
}

impl Envelope {
    fn apply(&self, source: &AudioSource) {
        source.set_fade(
            self.source_in_secs,
            self.duration_secs,
            self.fade_in_secs,
            self.fade_out_secs,
            self.fade_in_curve,
            self.fade_out_curve,
            self.gain,
        );
    }
}

/// Put a source inside an audio crossfade on its crossfade envelope,
/// see [`envelope_of`]. A source outside one is left as it is: the
/// plain clip envelope, with its contiguous-cut offset, is
/// `sync_clip_volumes`'s.
///
/// Only the PCM feeder applies an envelope; a clip on the decoder
/// fallback is not crossfaded at all (see [`fadeable_media`]).
/// Where to look up the clip a source plays: the playhead while the
/// source still plays the clip under it, else the heard position. A
/// source swapped in for the next clip ahead of a cut (see
/// `reconcile_audio_sources`) takes that clip's fades and gain, not the
/// ones of the clip still on screen, whose range would fade it out.
fn own_clip_tc(seq: &klipp_timeline::Sequence, fadeable: &FadeableMedia, active: &ActiveAudioSource, playhead: Timecode, heard_at: Timecode) -> Timecode {
    source_clip_tc(seq, fadeable, (active.track_index, active.media_id, active.audio_chain_fingerprint), playhead, heard_at)
}

/// [`own_clip_tc`] over a source's key, media and chain fingerprint.
fn source_clip_tc(seq: &klipp_timeline::Sequence, fadeable: &FadeableMedia, (key, media, fp): (usize, MediaId, u64), playhead: Timecode, heard_at: Timecode) -> Timecode {
    let plays_it = sounding_for_key(seq, fadeable, key, playhead).is_some_and(|s| {
        s.clip.media_id() == Some(media) && audio_chain_fingerprint_for(&s.clip.audio_effects, &s.clip.audio_effect_keyframes) == fp
    });
    if plays_it {
        playhead
    } else {
        heard_at
    }
}

fn set_crossfade_envelope(seq: &klipp_timeline::Sequence, fadeable: &FadeableMedia, active: &ActiveAudioSource, tc: Timecode) {
    let Some(sounding) = sounding_for_key(seq, fadeable, active.track_index, tc) else { return };
    if sounding.extent.is_none() {
        return;
    }
    envelope_of(seq, &sounding, tc).apply(&active.source);
}

/// The envelope the clip `sounding` plays at `tc`: its own fades and
/// its gain, the fade settings the linked video clip's where there is
/// one, as everywhere else in playback. Inside an audio crossfade it is
/// the clip's range stretched by the handle the crossfade plays, the
/// constant-power ramp in over the crossfade at its head, out at its
/// tail. A crossfade takes the place of the clip's own fade at that
/// end, as in the render (`AudioClipShape::with_crossfades`); the
/// clip's own fade at the other end stays.
fn envelope_of(seq: &klipp_timeline::Sequence, sounding: &klipp_timeline::crossfade::Sounding<'_>, tc: Timecode) -> Envelope {
    let e = sounding.extent;
    let clip = sounding.clip;
    let settings = clip
        .linked_clip_id
        .and_then(|lid| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == lid))
        .unwrap_or(clip);
    let constant_power = fade_curve_to_u32(klipp_timeline::FadeCurve::ConstantPower);
    let (fade_in, in_curve) = if e.fade_in_secs > 0.0 {
        (e.fade_in_secs, constant_power)
    } else {
        (settings.fade_in_secs as f64, fade_curve_to_u32(settings.fade_in_curve))
    };
    let (fade_out, out_curve) = if e.fade_out_secs > 0.0 {
        (e.fade_out_secs, constant_power)
    } else {
        (settings.fade_out_secs as f64, fade_curve_to_u32(settings.fade_out_curve))
    };
    let clip_time = (tc - settings.timeline_in).to_secs_f64().max(0.0);
    let ctx = klipp_core::EvalContext {
        clip_time_secs: clip_time,
        clip_duration_secs: settings.duration().to_secs_f64(),
        timeline_time_secs: tc.to_secs_f64(),
        fps: seq.fps.as_f64(),
        seed: settings.id.0.as_u64_pair().0,
    };
    let gain = eval_with_modifiers(&settings.gain_keyframes, settings.param_modifiers(), &klipp_core::ParamId::Gain, clip_time, settings.gain, &ctx);
    Envelope {
        source_in_secs: clip.source_in.to_secs_f64() - e.head_secs,
        duration_secs: e.head_secs + clip.duration().to_secs_f64() + e.tail_secs,
        fade_in_secs: fade_in,
        fade_out_secs: fade_out,
        fade_in_curve: in_curve,
        fade_out_curve: out_curve,
        gain,
    }
}

/// A clip's sound settings at `heard_at` seconds on the timeline: its
/// volume, pan and gain from their keyframes, its fades and channel
/// layout; a linked picture's where its sound is linked to one. What a
/// playing source takes each tick, and a ringing tail opened after its
/// clip has ended, at its out point (#2793).
struct ClipSound {
    volume: f32,
    pan: f32,
    gain: f32,
    fade_in: f32,
    fade_out: f32,
    fade_in_curve: klipp_timeline::FadeCurve,
    fade_out_curve: klipp_timeline::FadeCurve,
    channel_config: klipp_timeline::ChannelConfig,
}

fn clip_sound_at(seq: &klipp_timeline::Sequence, audio_clip: &klipp_timeline::Clip, heard_at: f64, fps: f64) -> ClipSound {
    let at = |clip: &klipp_timeline::Clip| {
        let clip_time = (heard_at - clip.timeline_in.to_secs_f64()).max(0.0);
        let ctx = klipp_core::EvalContext {
            clip_time_secs: clip_time,
            clip_duration_secs: clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64(),
            timeline_time_secs: heard_at,
            fps,
            seed: clip.id.0.as_u64_pair().0,
        };
        let eval = |kfs: &[klipp_core::Keyframe], param: klipp_core::ParamId, base: f32| eval_with_modifiers(kfs, clip.param_modifiers(), &param, clip_time, base, &ctx);
        ClipSound {
            volume: eval(&clip.volume_keyframes, klipp_core::ParamId::Volume, clip.volume),
            pan: eval(&clip.pan_keyframes, klipp_core::ParamId::Pan, clip.pan),
            gain: eval(&clip.gain_keyframes, klipp_core::ParamId::Gain, clip.gain),
            fade_in: clip.fade_in_secs,
            fade_out: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            channel_config: clip.channel_config,
        }
    };
    // If linked to a video clip, the video clip's audio params.
    let linked = audio_clip.linked_clip_id.and_then(|id| seq.tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == id));
    at(linked.unwrap_or(audio_clip))
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
    fp = fp.wrapping_mul(31).wrapping_add(u64::from(clip.keep_pitch));
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
            fp = fp.wrapping_mul(31).wrapping_add(kf.interpolation.code());
        }
    }
    fp
}

/// Hash the SETTINGS of an audio chain: each effect's parameters and
/// wet/dry. A change here does not rebuild the chain (that would restart
/// the audio and gap it on every slider tick); it is handed to the
/// running chain in place (`AudioSource::update_effect_settings`). Without
/// it a slider, preset or mix change was never heard in playback.
pub(crate) fn audio_settings_fingerprint_for(audio_effects: &[klipp_core::AudioEffectInstance]) -> u64 {
    let mut fp: u64 = 0;
    for ae in audio_effects {
        fp = fp.wrapping_mul(31).wrapping_add(ae.wet_dry.to_bits() as u64);
        for (name, value) in &ae.params {
            for b in name.bytes() {
                fp = fp.wrapping_mul(31).wrapping_add(b as u64);
            }
            fp = fp.wrapping_mul(31).wrapping_add(value.to_bits() as u64);
        }
    }
    fp
}

/// Decide whether the audio pipeline must be resynced (mix-staging flush +
/// PCM reseek, or a full stop-and-reopen) when the video playhead lands on
/// `tc`, given the currently-streaming audio sources described by their
/// `(track_index, media_id, audio_chain_fingerprint)`.
///
/// Returns `false` — the audio is CONTINUOUS across this boundary and must
/// keep streaming untouched — when every active source still sits on the
/// same media file and the same audio chain as the clip under it at `tc`.
/// Returns `true` only when a source no longer matches: a different media /
/// source, a real gap (no clip under the source at `tc`), or an
/// audio-effect / chain change. Those are the only cases that genuinely
/// need a resync.
///
/// This is the single source of truth shared by the video-contiguous
/// clip-advance branch and the video jump-cut branches (same-file /
/// preroll-hit / full-open), so a video-only cut over a continuous audio
/// clip is treated identically no matter which branch handles it — the
/// audio never flushes or reseeks. Kept as a free function over the
/// sequence so it can be unit-tested without a live `PlaybackEngine`.
pub(crate) fn audio_resync_needed(
    seq: &klipp_timeline::Sequence,
    fadeable: &FadeableMedia,
    tc: Timecode,
    active: impl IntoIterator<Item = (usize, MediaId, u64)>,
) -> bool {
    active.into_iter().any(|(key, media_id, fingerprint)| {
        if seq.tracks.get(key_track(key)).is_none() {
            // The source's track vanished — safest to resync.
            return true;
        }
        match sounding_for_key(seq, fadeable, key, tc).map(|s| s.clip) {
            Some(clip) => {
                if clip.media_id() != Some(media_id) {
                    return true;
                }
                audio_chain_fingerprint_for(&clip.audio_effects, &clip.audio_effect_keyframes)
                    != fingerprint
            }
            // No clip under the source at `tc`: the source is silent
            // there and `reconcile_audio_sources` retires it alone on the
            // next tick. Restarting every source for it put a gap in the
            // music at each video cut after a sound effect had ended.
            None => false,
        }
    })
}

impl PlaybackEngine {

    fn stop_all_audio(&mut self) {
        // Clear mix thread staging first (stop reading from source ring buffers)
        if let Some(ref mt) = self.mix_thread {
            mt.clear_all();
        }
        for active in self.audio_sources.iter_mut().chain(self.audio_tails.iter_mut()) {
            active.source.stop_detached();
        }
        self.audio_sources.clear();
        self.audio_tails.clear();
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
        let settings_fp = audio_settings_fingerprint_for(&audio_effects);

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
                        let mix = self.ensure_mix_pipeline(consumer, track_index, zero_latency);
                        self.audio_sources.push(ActiveAudioSource {
                            track_index,
                            media_id,
                            audio_chain_fingerprint: chain_fp,
                            settings_fingerprint: settings_fp,
                            source: AudioSource::PcmFeeder(feeder),
                            mix,
                            starts_at: None,
                            may_ring: klipp_audio::dsp::AudioEffectChain::may_ring(&audio_effects, !audio_effect_keyframes.is_empty(), 48_000),
                            clip: None,
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
                let mix = self.ensure_mix_pipeline(consumer, track_index, latency_advertise);
                self.audio_sources.push(ActiveAudioSource {
                    track_index,
                    media_id,
                    audio_chain_fingerprint: chain_fp,
                    settings_fingerprint: settings_fp,
                    source: AudioSource::Decoder(adec),
                    mix,
                    starts_at: None,
                    may_ring: false,
                    clip: None,
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
    ) -> std::sync::Arc<crate::mix_thread::SourceMix> {
        // A mix thread that has ended (a panic past the block it mixed)
        // is made again with its output (#3181): it used to stay, and the
        // session was silent until a restart.
        if self.mix_restarts < PANIC_RESTARTS && self.mix_thread.as_ref().is_some_and(MixThread::is_finished) {
            self.mix_restarts += 1;
            warn!("the audio mix thread had ended; it is started again");
            self.mix_thread = None;
            self.audio_output = None;
        }
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

        let mix = std::sync::Arc::new(crate::mix_thread::SourceMix::default());
        if let Some(ref mt) = self.mix_thread {
            mt.add_staging(track_index, staging_consumer, latency_advertise, mix.clone());
        }
        mix
    }

    /// Look up the PCM cache path for a media item.
    fn pcm_cache_path(&self, media_id: MediaId) -> Option<std::path::PathBuf> {
        let pool = self.media_pool.read().ok()?;
        let item = pool.get(&media_id)?;
        item.audio_cache.as_ref().map(|c| c.pcm_path.clone())
    }

    /// Seek all audio sources to match the clip at `tc` on each source's track.
    /// Flushes the mix pipeline, bumps seek_gen, and waits for all sources to confirm.
    /// True when the audio must be resynced at `tc` — a different
    /// media/source, a real gap, or an audio-chain change on some active
    /// source. False means the audio is continuous across the boundary and
    /// must keep streaming untouched. Thin wrapper over the pure
    /// [`audio_resync_needed`] so the video-contiguous and video-jump
    /// clip-advance paths share one decision.
    fn audio_needs_resync_at(&self, tc: Timecode) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        audio_resync_needed(
            &seq,
            &self.fadeable_media,
            tc,
            self.audio_sources
                .iter()
                .map(|a| (a.track_index, a.media_id, a.audio_chain_fingerprint)),
        )
    }

    /// Refresh the fade / gain envelope on every active audio source from the
    /// audio clip under it at `tc`, WITHOUT reopening, reseeking, or flushing
    /// the source. Used when a clip boundary is crossed but the audio stays
    /// continuous — the video-contiguous cut and the video jump-cut over a
    /// continuous audio clip both land here so their audio is handled
    /// identically. The `-1.0 / +1.0` offset keeps the fade envelope from
    /// snapping the source back to "position 0" (which would re-trigger a
    /// fade-in from silence) at a boundary the audio is streaming straight
    /// through.
    fn refresh_audio_fades(&self, tc: Timecode) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        for active_src in &self.audio_sources {
            if seq.tracks.get(key_track(active_src.track_index)).is_some() {
                if let Some(new_clip) = sounding_for_key(&seq, &self.fadeable_media, active_src.track_index, tc).map(|s| s.clip) {
                    // A source already swapped in for a later clip keeps
                    // that clip's fades from `sync_clip_volumes`.
                    if new_clip.media_id() != Some(active_src.media_id) {
                        continue;
                    }
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
                    active_src.source.set_varispeed(new_clip.speed, new_source_in, new_clip.source_out.to_secs_f64(), new_clip.keep_pitch);
                }
            }
            set_crossfade_envelope(&seq, &self.fadeable_media, active_src, tc);
        }
    }

    /// Handle the AUDIO side of a video JUMP-cut clip advance (the same-file,
    /// preroll-hit and full-open branches), given `self.playhead` already set
    /// to the new clip's boundary.
    ///
    /// When the audio is CONTINUOUS across the boundary (every active source
    /// still matches the audio clip under it — same media, no gap, no chain
    /// change), leave every source streaming untouched (no mix-staging flush,
    /// no PCM reseek) and only refresh the fade envelope, then return the wall
    /// seconds elapsed since `t_cut`. The video branches add that to their
    /// master-clock anchor so the video lands exactly where the free-running
    /// audio now is — the audio never gaps and the A/V offset never
    /// accumulates across a montage of jump cuts.
    ///
    /// When the audio actually changes at the boundary (different media /
    /// source, a real gap, or an audio-chain change), reseed it exactly as
    /// before — `seek_all_audio_to_timecode` flushes + reseeks — and return
    /// `0.0` so the video anchors on the boundary, matching the freshly
    /// positioned audio.
    fn advance_audio_over_video_cut(&mut self, t_cut: std::time::Instant) -> f64 {
        self.swap_changed_audio_at_cut();
        self.refresh_audio_fades(self.playhead);
        t_cut.elapsed().as_secs_f64()
    }

    /// At a cut, swap only the audio sources whose track changes there
    /// (other media, a chain change, no clip), leaving the rest, the mix
    /// thread and the output running. Restarting every source reread
    /// every track's PCM cache on this thread, 140–250 ms at a cut
    /// between files, and put a gap in the music (#572). Usually
    /// `reconcile_audio_sources` has made the swap a mix lead ahead of
    /// the cut already and nothing is left to do.
    fn swap_changed_audio_at_cut(&mut self) {
        if self.audio_needs_resync_at(self.playhead) {
            self.reconcile_audio_sources();
        }
    }

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
                if seq.tracks.get(key_track(active.track_index)).is_none() {
                    return false;
                }
                let Some(clip) = sounding_for_key(&seq, &self.fadeable_media, active.track_index, tc).map(|s| s.clip) else {
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
        self.drop_audio_tails();
        self.open_ringing_tails(tc);

        // Compute per-track seek positions from the timeline
        let seek_positions: Vec<(usize, f64)> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.audio_sources
                .iter()
                .filter_map(|active| {
                    let clip = sounding_for_key(&seq, &self.fadeable_media, active.track_index, tc)?.clip;
                    let source_secs = klipp_compose::render::source_secs(clip, tc, seq.fps.as_f64());
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
            self.speed_ramp_enabled = clip.has_speed_keyframes();
        } else {
            self.speed_keyframes.clear();
            self.speed_ramp_enabled = false;
        }
        drop(seq);
        // The clock runs at the clip's speed (#2108): a seek that parked on
        // a clip at 50% set the speed here and left the clock at the last
        // clip's, so Play resumed it at 1x and the playhead, reading the
        // clip's source time back at its speed, ran at twice the timeline's
        // rate. A ramp sets its own speed every tick.
        if !self.speed_ramp_enabled {
            self.master_clock.set_speed(self.clip_speed);
        }
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

    /// Where the timeline is now, as export frames it (#2556): the start
    /// of the sequence frame the clock is in, and the source time of that
    /// frame through the clip's speed, as export renders a timeline frame.
    /// The frame shown is the file's nearest to that time, and it changes
    /// only when the timeline's frame does: a 30 fps file on a 23.976
    /// timeline showed every one of its frames, at its own times. `None`
    /// off a media clip, where the clock is used as it is.
    fn due_frame(&self, clock: f64) -> Option<(f64, RationalTime)> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip = seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index))?;
        clip.media_id()?;
        let fps = seq.fps.as_f64();
        let elapsed = self.ramp_clip_relative_secs().unwrap_or_else(|| (clock - self.clip_source_start) / self.clip_speed.max(0.001)).max(0.0);
        let now = self.clip_timeline_start.to_secs_f64() + elapsed;
        let rate = self.playhead.rate.max(1);
        let ticks = ((rate as f64 / fps).round() as i64).max(1);
        let frame = (now * fps + 1e-6).floor() as i64;
        let at = RationalTime { value: frame * ticks, rate }.max(clip.timeline_in);
        Some((klipp_compose::render::source_secs(clip, at, fps), at))
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
        // A decoder that ended with a panic is opened again where the
        // playhead is (#3181). It used to leave the picture stopped, and
        // every seek with it, until a restart.
        if self.v1_decoder.as_ref().is_some_and(VideoDecoder::has_panicked) {
            let clip = self.sequence.read().unwrap_or_else(|e| e.into_inner()).tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).map(|c| c.id);
            if clip != self.decoder_panic_clip {
                self.decoder_panic_clip = clip;
                self.decoder_restarts = 0;
                self.decoder_given_up = false;
            }
            if self.decoder_restarts < PANIC_RESTARTS {
                self.decoder_restarts += 1;
                warn!("the video decoder panicked; it is opened again at the playhead");
                self.seek_in_sequence(self.playhead);
            } else {
                error!("the video decoder panicked {PANIC_RESTARTS} times over; it is not opened again until the playhead is moved or played");
                self.v1_decoder = None;
                // Nor is the clip opened as a layer's stream in its place.
                if let Some(clip) = clip {
                    self.streams.give_up(clip);
                }
                if !std::mem::replace(&mut self.decoder_given_up, true) {
                    self.say_given_up("The picture");
                }
            }
        }
        // A mix thread that ended (a panic past its block's catch) is made
        // again, with the sound from the playhead (#3181): it was made again
        // only when a new source started, and playing on stayed silent.
        if self.mix_restarts <= PANIC_RESTARTS && self.mix_thread.as_ref().is_some_and(MixThread::is_finished) {
            if self.mix_restarts < PANIC_RESTARTS {
                self.mix_restarts += 1;
                warn!("the audio mix thread had ended; it is made again with the sound from the playhead");
                self.stop_all_audio();
                self.start_all_audio(self.playhead);
            } else {
                // Said once: past the limit, nothing more is tried.
                self.mix_restarts += 1;
                error!("the audio mix thread ended {PANIC_RESTARTS} times over; it is not made again until the playhead is moved or played");
                self.say_given_up("The sound");
            }
        }
        self.reconcile_audio_sources();
        self.warm_upcoming_titles();
        self.renderer.adopt_warm_titles();
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
        }
        // A retimed clip's own sound is out of the mix (see `audio_rows`):
        // its speed no longer mutes the music and the other tracks with it
        // (#638). What mutes here is a scrub with audio scrubbing off,
        // reasserted each tick so mid-scrub pref changes and EndScrub both
        // self-correct.
        let should_mute = self.is_scrubbing && !self.scrub_audio_enabled;
        for active in &self.audio_sources {
            active.source.muted().store(should_mute, Ordering::Release);
        }

        // Master clock is the single source of truth. Video frame
        // picking uses the PDC-compensated PTS — current_display_secs
        // subtracts the global max chain latency from the playhead
        // so the rendered frame matches what's currently being heard
        // out of the audio bus. With no plugins reporting latency,
        // this is identical to current_secs() (offset = 0).
        let current_source_secs = self.master_clock.current_display_secs();
        // Frame presentation tolerance: half of one of the *file's* frames,
        // so the frame shown is the nearest of the file, as export picks it
        // (#696). Half a sequence frame put playback a file frame off export
        // at a cut whenever the rates differ.
        let frame_tolerance = self.v1_decoder.as_ref().map_or(self.fps.frame_duration_secs() * 0.5, |d| d.half_source_frame(self.fps.as_f64()));
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
                    self.report_playhead();
                    (self.repaint_cb)();
                    return;
                }
                self.last_synth_frame_rendered = Some(current_frame);

                self.present_frame(None);

                self.frames_consumed += 1;
                self.report_playhead();
                (self.repaint_cb)();
                return;
            }
            if self.clock_only {
                self.tick_clock_only(current_source_secs);
                return;
            }
        }

        // Pop frames from the queue, presenting the one closest to current time
        // as the timeline frames it: due by the start of the clock's frame.
        let due = self.due_frame(current_source_secs);
        let due_secs = due.map_or(current_source_secs, |(secs, _)| secs);
        let mut display_frame = None;
        let mut q_len = 0usize;

        // A file that ends before its clip does — a still image's one
        // frame, a file shorter than the clip says — holds its last frame
        // to the clip's end, as the export does (#978): its end of stream
        // stays queued until then, and the playhead follows the clock.
        // Taken at once, it cut the clip off at the file's end, and a
        // still played from inside it snapped back and stalled.
        let clip_runs_on = self.clip_runs_on(current_source_secs);
        // The file has ended and the clip runs on: its end of stream waits
        // at the front of the queue.
        let mut held_at_end = false;
        // The frame taken is the file's last.
        let mut last_of_file = false;
        if let Some(dec) = &self.v1_decoder {
            let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            q_len = queue.len();

            loop {
                match queue.front() {
                    None => break,
                    Some(f) if f.is_eof => {
                        if clip_runs_on {
                            held_at_end = true;
                            break;
                        }
                        let frame = queue.pop().unwrap();
                        display_frame = Some(frame);
                        break;
                    }
                    Some(f) => {
                        let pts = f.pts_secs();
                        if pts <= due_secs + frame_tolerance {
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
                                if !next.is_eof && next.pts_secs() <= due_secs + frame_tolerance {
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
                            last_of_file = queue.front().is_some_and(|n| n.is_eof);
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

        // Kept to show again once the file has ended: a still's one frame
        // comes before its end of stream does, so it is known by the file.
        // A frame on the GPU is shared now, and one that may take a fast
        // path copied; one only on the CPU is taken over where it is
        // presented, not copied.
        let mut keep_tail = None;
        if let Some(f) = display_frame.as_ref().filter(|f| !f.is_eof) {
            if last_of_file || self.primary_is_still() {
                keep_tail = self.sequence.read().ok().and_then(|s| self.v1_clip_id(&s));
                let shared = streams::HeldFrame::from_gpu_frame(f).or_else(|| f.gpu_source.is_some().then(|| streams::HeldFrame::copy_of(f)));
                self.held_tail = keep_tail.zip(shared).map(|(id, held)| (id, held, self.playhead.value));
            }
        }

        if self.cut_diag_until.is_some_and(|t| std::time::Instant::now() < t) {
            let (front, len) = self.v1_decoder.as_ref().map(|d| {
                let q = d.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                (q.front().map(|f| f.pts_secs()), q.len())
            }).unwrap_or((None, 0));
            match &display_frame {
                Some(f) => debug!("CUT-DIAG FRAME pts={:.4} clock={:.4} q_len={len} front={front:?}", f.pts_secs(), current_source_secs),
                // No frame this tick: starved only when none is there to
                // show. A queue whose front is still ahead of the clock
                // is early, not starved; that was most of what it said.
                None => match front {
                    Some(next) if next > current_source_secs => debug!("CUT-DIAG AHEAD clock={:.4} q_len={len} next due in {:.1}ms", current_source_secs, (next - current_source_secs) * 1000.0),
                    _ => debug!("CUT-DIAG STARVED clock={:.4} q_len={len} front={front:?}", current_source_secs),
                },
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

                    self.report_playhead();
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
                // On the timeline frame the clock is in, as when a frame is
                // taken (#2556): a pause between two pictures parked between
                // two frames, and showed the file's frame nearest that, not
                // the one on screen.
                self.playhead = due.map_or(self.clip_timeline_start + elapsed_tc, |(_, at)| at);
                if held_at_end {
                    self.present_held_tail();
                    // The look-ahead below runs as frames are taken, and
                    // none are now: the next shot opens as it would.
                    self.preroll_before_out();
                }
                self.report_playhead();
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
            // On the timeline frame the frame was taken for, so one
            // picture is one timeline frame (#2556).
            self.playhead = due.map_or(clip.timeline_in + elapsed_tc, |(_, at)| at);

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
            self.take_ready_preroll(None);
            if dist_to_out < PREROLL_LEAD_SECS && dist_to_out > 0.0 && self.preroll_decoder.is_none() && self.preroll_pending.is_none() {
                self.start_preroll_for_next_clip();
            }

            // Check if we've passed source_out
            if frame.pts_secs() >= clip.source_out.to_secs_f64() - 0.01 {
                info!("CUT-DIAG BOUNDARY HIT: pts={:.4}s source_out={:.4}s timeline_out={:.4}s clock={:.4}s",
                    frame.pts_secs(), clip.source_out.to_secs_f64(),
                    clip.timeline_out.to_secs_f64(), current_source_secs);
                self.playhead = clip.timeline_out;
                self.cut_diag_until = Some(std::time::Instant::now() + Duration::from_millis(1500));
                self.boundary_frame = Some((clip.id, frame));
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

        // Keep one decoded primary for an immediate pause, including
        // the fast paths which return before present_frame.
        if let Some(held) = streams::HeldFrame::from_gpu_frame(&frame) {
            self.retain_primary(&held);
        }
        let (final_w, final_h) = (frame.width, frame.height);
        let gpu_source = frame.gpu_source.clone();

        // Direct present: the decoder's frame is already the frame the
        // viewer wants. When nothing grades or composites it, the whole
        // linear excursion below reproduces its own input, so hand the
        // decoded bytes straight to the viewer instead. See
        // `direct_present_blocker` for the conditions and why this is
        // byte-identical.
        if let Some(ref gpu) = gpu_source {
            let mut blockers = self.direct_present_blockers();
            blockers.extend(self.picture_shape_blocker((final_w, final_h)));
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
            // The stack first, when it presents without a readback: the
            // decoded frame seeded where it is, graded, its effects run
            // on it, blended, encoded into the viewer's texture. The fast
            // paths below read 33 MB back and the UI uploads them again;
            // the stack does neither. It takes the frame the direct path
            // would, the one only a grade keeps off it, and — because
            // `place_layer` applies them on the GPU — the one a grade
            // plus a GPU-capable builtin-effect chain or a keyframed
            // transform keeps off it too. When it declines — HDR
            // monitoring, another working space, a node with no GPU pass
            // — the fast paths take the frame as before.
            let stack_first =
                self.stack_present_handles(&blockers) && self.stack_presents_zero_copy();
            if stack_first {
                if let Some(held) = streams::HeldFrame::from_gpu_frame(&frame) {
                    if self.present_frame_with(Some(held), true) {
                        return;
                    }
                }
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
        let held = streams::HeldFrame::from_frame(frame);
        if let Some(id) = keep_tail.filter(|_| self.held_tail.is_none()) {
            self.held_tail = Some((id, held.clone(), self.playhead.value));
        }
        self.present_frame(Some(held));
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

        self.report_playhead();
    }

    /// Whether the primary decoder's clip shows a still image.
    pub(super) fn primary_is_still(&self) -> bool {
        let media = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).and_then(|c| c.media_id())
        };
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        media.and_then(|id| pool.get(&id)).is_some_and(|m| m.is_still)
    }

    /// The look-ahead for a clip whose file has ended and runs on (#978):
    /// the next shot on its track opens and seeks while the held frame
    /// plays out, as it does before a playing shot's end (#2097).
    fn preroll_before_out(&mut self) {
        let out = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).map(|c| c.timeline_out)
        };
        let Some(out) = out else { return };
        let dist_to_out = out.to_secs_f64() - self.playhead.to_secs_f64();
        self.take_ready_preroll(None);
        if dist_to_out < PREROLL_LEAD_SECS && dist_to_out > 0.0 && self.preroll_decoder.is_none() && self.preroll_pending.is_none() {
            self.start_preroll_for_next_clip();
        }
    }

    /// Show the primary clip's held last frame at the playhead, once per
    /// sequence frame, with everything over and under it as it is there
    /// (#2097).
    fn present_held_tail(&mut self) {
        let clip = self.sequence.read().ok().and_then(|s| self.v1_clip_id(&s));
        let at = self.playhead.value;
        let Some((id, frame, shown)) = self.held_tail.as_mut() else { return };
        if Some(*id) != clip || *shown == at {
            return;
        }
        *shown = at;
        let frame = frame.clone();
        self.present_frame(Some(frame));
        self.frames_consumed += 1;
    }

    /// Whether the current clip still runs at `current_source_secs` on
    /// the playback clock: the playhead the clock puts in it is short of
    /// its end on the timeline by more than half a frame (#978).
    fn clip_runs_on(&self, current_source_secs: f64) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let Some(clip) = seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)) else {
            return false;
        };
        let elapsed = if let Some(r) = self.ramp_clip_relative_secs() {
            r
        } else {
            ((current_source_secs - self.clip_source_start) / self.clip_speed.max(0.001)).max(0.0)
        };
        let at = self.clip_timeline_start.to_secs_f64() + elapsed;
        at < clip.timeline_out.to_secs_f64() - 0.5 * self.fps.frame_duration_secs()
    }

    fn advance_to_next_clip(&mut self) {
        // Wall-clock reference for A/V catch-up. At function entry the
        // playhead sits on the outgoing clip's boundary and the audio,
        // still streaming, is aligned with it. When we advance the VIDEO
        // over a JUMP cut but leave a continuous audio clip streaming
        // (no flush, no reseek), the audio keeps advancing in real time
        // while the master clock is briefly repositioned for the video
        // seek. Re-anchoring the master clock to exactly the boundary
        // would leave the video that many milliseconds behind the audio,
        // and the error would ACCUMULATE across a montage of jump cuts.
        // The video jump-cut branches below add `t_cut.elapsed()` to the
        // new anchor so the video catches up to where the free-running
        // audio actually is — zero drift, no accumulation.
        let t_cut = std::time::Instant::now();
        // Where the playhead is as the cut is taken: at the next shot's
        // start for a cut between adjacent shots, up to a tick past it
        // when the shot after a gap is taken over (#546). The sound,
        // streaming on, is there, so the picture is anchored there too.
        let entered_at = self.playhead;
        // A clip change leaves clock-only; the branches below enter it
        // again where the next place has no picture.
        self.clock_only = false;
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
        // A shot whose source has run out has been played. The decoder
        // reports the end of the file as the last frame is decoded, with
        // the playhead still on that frame: left there, the shot was
        // found under it again, reopened at its last frame, ran out
        // again, and playback never reached the end (#773).
        let current_out = seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).map(|c| c.timeline_out);
        if let Some(out) = current_out {
            let short = out.to_secs_f64() - self.playhead.to_secs_f64();
            if short > 0.0 && short <= self.fps.frame_duration_secs() * GAP_AS_CUT_FRAMES {
                self.playhead = out;
            }
        }
        drop(seq);

        let Some((actual_idx, next_clip)) = next_clip_info else {
            // The last shot on this track has ended. The timeline may
            // not have: an end card on a text track, music running on.
            // Play on the clock to its end rather than stopping here
            // (#542).
            if self.playhead < self.sequence_duration() {
                self.retire_v1_decoder(old_clip);
                let at = self.playhead;
                self.enter_clock_only(at);
                self.master_clock.resume();
                self.report_playhead();
                self.sync_streams();
                return;
            }
            self.stop_at_end();
            return;
        };

        // Empty time before the next shot is time too: it was skipped,
        // the playhead jumping from this shot's end to the next one's
        // start, and whatever other tracks hold there (a title over
        // black) never showed. Play it on the clock; the next shot takes
        // over when the playhead reaches it (#542).
        // A gap of a frame or so is a slip of the edit, not a pause: it
        // is cut across (#546).
        if next_clip.timeline_in.to_secs_f64() - self.playhead.to_secs_f64() > self.fps.frame_duration_secs() * GAP_AS_CUT_FRAMES {
            let from = self.current_clip_index;
            // The shot after the gap goes on in the same file from where
            // this one stopped, as far on as the gap is long: a strobe's
            // pieces, frames cut out. The playing decoder decodes on
            // through the gap, as that shot's preroll, rather than a new
            // one opening and seeking for each piece (#2376).
            let frame = self.fps.frame_duration_secs();
            // Only a decoder ahead of the clock: one that has decoded past
            // this shot's end. One behind it (opened late, held up) gave
            // the next pieces frames already past when they came, and a
            // strobe of short pieces stayed dark as long as it lagged
            // (#2386); a fresh preroll opens at the next shot instead.
            let ahead = |d: &VideoDecoder, source_out: f64| d.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).newest_pts_secs().is_some_and(|pts| pts >= source_out - 0.5 * frame);
            let hand_on = self.v1_decoder.is_some()
                && !next_clip.is_reversed()
                && (next_clip.speed - self.clip_speed).abs() < 1e-6
                && current_clip_info.as_ref().zip(current_out).is_some_and(|((_, media, source_out), out)| {
                    let gap = next_clip.timeline_in.to_secs_f64() - out.to_secs_f64();
                    let on = next_clip.source_in.to_secs_f64() - source_out;
                    *media == next_clip.media_id() && media.is_some() && gap <= frame * HAND_ON_GAP_FRAMES && (on - gap * self.clip_speed).abs() < 0.5 * frame
                        && self.v1_decoder.as_ref().is_some_and(|d| ahead(d, *source_out))
                });
            if hand_on {
                let decoder = self.v1_decoder.take();
                self.boundary_frame = None;
                self.held_tail = None;
                let at = self.playhead;
                self.enter_clock_only(at);
                self.master_clock.resume();
                self.clear_preroll();
                if let Some(media_id) = next_clip.media_id() {
                    self.preroll_decoder = decoder;
                    self.preroll_clip_index = Some(actual_idx);
                    self.preroll_for = Some((self.current_track_index, media_id));
                    self.gap_hand_over = Some((from, actual_idx));
                    DECODERS_HANDED_ON.fetch_add(1, Ordering::Relaxed);
                }
                self.report_playhead();
                self.sync_streams();
                return;
            }
            self.retire_v1_decoder(old_clip);
            let at = self.playhead;
            self.enter_clock_only(at);
            self.master_clock.resume();
            // The shot after the gap, opened and seeked while the gap
            // plays, so its first frame is there when the gap ends.
            if let Some(media_id) = next_clip.media_id() {
                self.start_preroll_for(actual_idx, media_id, next_clip.source_in.to_secs_f64(), next_clip.source_rotation, next_clip.timeline_in);
                self.gap_hand_over = Some((from, actual_idx));
            }
            self.report_playhead();
            self.sync_streams();
            return;
        }

        // A reversed clip next: no decoder for it to take over, as for a
        // gap; the clock runs on and the renderer draws it from its own
        // reader (#617). A clip after it is taken over as after a gap.
        if next_clip.is_reversed() {
            self.retire_v1_decoder(old_clip);
            let at = self.playhead;
            self.enter_clock_only(at);
            self.master_clock.resume();
            self.report_playhead();
            self.sync_streams();
            return;
        }

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
            self.clip_source_start = source_in_secs;
            self.clip_timeline_start = timeline_in;
            self.clip_speed = next_clip_speed;
            self.master_clock.set_speed(next_clip_speed);
            self.refresh_speed_keyframes();
            self.last_frame_time = std::time::Instant::now();

            // Synthetic playback uses the same source-to-timeline mapping as media.
            let source_secs = source_in_secs;
            let source_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(source_us);
            self.master_clock.resume();

            self.report_playhead();
            self.sync_streams();
            return;
        }

        // ── Branch: next clip needs a decoder (Media, Sequence) ──
        let media_id = match next_clip.media_id() {
            Some(mid) => mid,
            None => {
                // A scene with no file of its own (a precomposition, a
                // graded compound drawn whole): nothing for the primary
                // decoder. Play on the clock, as a seek there does; the
                // scene's own media opens with the other layers (#370).
                self.retire_v1_decoder(old_clip);
                self.playhead = timeline_in;
                self.enter_clock_only(timeline_in);
                self.master_clock.resume();
                self.report_playhead();
                self.sync_streams();
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
            self.report_playhead();

            // The `is_contiguous` check above only considers the video track
            // (same media, source_out == next.source_in). Audio tracks may
            // live independently and switch to a different media file at
            // this boundary — the existing `AudioSource`s are still decoding
            // the previous media, so a `set_fade` update would silence them
            // once the old media runs out. Scan the audio sources and, if
            // any sits on a media that no longer matches the clip at the
            // new playhead, swap that track's source (only that track's:
            // the others play on). Video stays contiguous regardless.
            //
            // We also swap when the AUDIO CHAIN changed — two clips
            // that share the same media file can carry different
            // audio_effects / audio_effect_keyframes, and without this
            // check the prior clip's chain bleeds into the next one
            // (manifests as "the effect is applied to the whole track").
            //
            // `audio_needs_resync_at` is the shared decision — the video
            // jump-cut branches below use the very same check so a
            // continuous audio clip is preserved identically whether the
            // video cut is contiguous or a jump.
            if self.audio_needs_resync_at(self.playhead) {
                info!(
                    "CUT-DIAG CONTIGUOUS: audio media or chain changed at boundary — swapping the changed audio sources",
                );
                self.swap_changed_audio_at_cut();
            }

            // CRITICAL: Immediately update fade parameters on audio sources,
            // but leave them streaming — the audio is continuous here.
            self.refresh_audio_fades(self.playhead);

            self.sync_streams();
            return;
        }

        self.current_clip_index = actual_idx;
        self.playhead = timeline_in;
        self.last_synth_frame_rendered = None;

        // Preroll hit: if we pre-decoded this clip, swap in the ready decoder — zero stall.
        // One still opening started well over a second ago and is all but
        // there: waiting for it is shorter than opening the file again.
        if self.preroll_is(actual_idx, media_id) {
            self.take_ready_preroll(Some(Duration::from_millis(250)));
        }
        if self.preroll_is(actual_idx, media_id) {
            if let Some(preroll_dec) = self.preroll_decoder.take() {
                info!("CUT-DIAG PREROLL HIT: swapping in pre-decoded clip idx={} source_in={:.4}s", actual_idx, source_in_secs);
                CUTS_FROM_PREROLL.fetch_add(1, Ordering::Relaxed);
                self.retire_v1_decoder(old_clip);
                self.v1_decoder = Some(preroll_dec);
                self.preroll_clip_index = None;
                self.preroll_for = None;

                if let Some(path) = self.path_for_media(media_id) {
                    self.current_file = Some(path);
                }

                // Audio continuity across a video-only jump cut: when every
                // active audio source still matches the audio clip at the new
                // playhead (same media, no gap, no chain change), the audio
                // is continuous across this boundary — leave it streaming
                // untouched (no mix-staging flush, no PCM reseek), exactly as
                // the video-contiguous branch does; only refresh its fade
                // envelope. Reseed audio only when it actually changes.
                let audio_catch_up = self.advance_audio_over_video_cut(t_cut) + (entered_at - timeline_in).to_secs_f64().max(0.0);

                let anchor_secs = source_in_secs + audio_catch_up * next_clip_speed;
                let start_us = (anchor_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.master_clock.resume();

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.playhead =
                    timeline_in + RationalTime::from_secs_f64(audio_catch_up, self.fps.rate());
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
                self.report_playhead();
                self.sync_streams();
                return;
            }
        }
        // Clear stale preroll if it didn't match
        self.clear_preroll();

        if let Some(path) = self.path_for_media(media_id) {
            // Same-file optimization: reuse decoder if same file
            let same_file = self.current_file.as_ref() == Some(&path);

            if same_file && self.v1_decoder.is_some() {
                info!("CUT-DIAG SAME-FILE seek: idx={} source_in={:.4}s (reusing decoder)", actual_idx, source_in_secs);
                if let Some(dec) = &self.v1_decoder {
                    dec.seek(source_in_secs);
                }

                self.master_clock.pause();
                self.wait_for_valid_frame(source_in_secs);
                // Audio continuity across a video-only jump cut — see
                // `advance_audio_over_video_cut`. When the audio is
                // continuous it keeps streaming untouched (no flush, no
                // reseek) through the video decoder's brief seek; the
                // returned catch-up advances the video anchor to where the
                // free-running audio now is (the master clock was paused for
                // the seek while the audio played on).
                let audio_catch_up = self.advance_audio_over_video_cut(t_cut) + (entered_at - timeline_in).to_secs_f64().max(0.0);

                let anchor_secs = source_in_secs + audio_catch_up * next_clip_speed;
                let start_us = (anchor_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.master_clock.resume();

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.playhead =
                    timeline_in + RationalTime::from_secs_f64(audio_catch_up, self.fps.rate());
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
                self.report_playhead();
                self.sync_streams();
                return;
            }

            info!("Advancing to clip {} ({:?})", actual_idx, path);
            CUTS_OPENED_COLD.fetch_add(1, Ordering::Relaxed);

            // Settle the audio BEFORE touching the video decoder. The audio
            // pipeline runs on its own threads, so it plays straight through
            // even while this thread blocks opening the new video decoder.
            // Only the tracks that change here are swapped, before the
            // decoder opens, so the old clip's sound stops on time.
            self.swap_changed_audio_at_cut();

            self.retire_v1_decoder(old_clip);

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

            // The audio streamed on through the video reopen; only the
            // fade envelope needs refreshing. Advance the video anchor to
            // where the free-running audio now is (opening the decoder
            // took wall time during which the audio played on).
            self.refresh_audio_fades(self.playhead);
            let audio_catch_up = t_cut.elapsed().as_secs_f64() + (entered_at - timeline_in).to_secs_f64().max(0.0);
            let anchor_secs = source_in_secs + audio_catch_up * next_clip_speed;
            let start_us = (anchor_secs * 1_000_000.0) as u64;
            self.master_clock.seek(start_us);
            self.master_clock.resume();
            self.playhead =
                timeline_in + RationalTime::from_secs_f64(audio_catch_up, self.fps.rate());

            self.clip_source_start = source_in_secs;
            self.clip_timeline_start = timeline_in;
            self.clip_speed = next_clip_speed;
            self.refresh_speed_keyframes();
            // Path-aware rotation; see effective_rotation_for.
            self.current_source_rotation = self.effective_decode_rotation(media_id, &path, source_rotation);
            self.last_frame_time = std::time::Instant::now();
            self.report_playhead();

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
        // Only this cut's: a frame left by an earlier one is gone either way.
        let boundary = self.boundary_frame.take();
        self.held_tail = None;
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
                // This clip's frame at this time, or none: within one of the
                // media's frames of the source time the stream is to show (a
                // clip slowed below 1x shows one across several of ours).
                let frame = 2.0 * dec.half_source_frame(self.fps.as_f64());
                let boundary = boundary.filter(|(clip, f)| *clip == id && (f.pts_secs() - secs).abs() < frame).map(|(_, f)| f);
                debug!("CUT-DIAG primary decoder handed to the clip streams for {id:?} at {secs:.3}s");
                self.note_view(format!("cut at {:?}: the outgoing clip's stream took the decoder over (frame at the cut {})", time.value, if boundary.is_some() { "handed on" } else { "none" }));
                // With the frame the decoder gave at the cut: the queue
                // starts after it, and a stream holding nothing would wait
                // for a frame that's never coming (#1494).
                self.streams.adopt(id, dec, secs, boundary.map(streams::HeldFrame::from_frame));
                return;
            }
        }
        dec.stop_detached();
    }

    /// Read the sound heard from `at` into the shared PCM cache on a worker
    /// thread, while the picture prerolls: whichever of it is not playing
    /// now opens at the cut from memory, not by reading a whole cache file
    /// on the engine thread, 120–150 ms for a long take (#572). The paths
    /// are found here, where the sequence is; only they cross.
    fn warm_sound_at(&self, at: Timecode) {
        let playing: std::collections::HashSet<MediaId> = self.audio_sources.iter().map(|s| s.media_id).collect();
        let mut paths: Vec<PathBuf> = Vec::new();
        for row in self.audio_rows_at(at) {
            if playing.contains(&row.1) {
                continue;
            }
            if let Some(pcm) = self.pcm_cache_path(row.1).filter(|p| p.exists()) {
                if !paths.contains(&pcm) {
                    paths.push(pcm);
                }
            }
        }
        if paths.is_empty() {
            return;
        }
        std::thread::spawn(move || crate::thread_guard::catch_panic("klipp-pcm-warm", move || {
            for path in paths {
                if let Err(e) = crate::pcm_reader::warm(&path) {
                    debug!("warming {}: {e}", path.display());
                }
            }
        }));
    }

    /// Move a preroll decoder the worker has finished opening into
    /// `preroll_decoder`, waiting up to `wait` for it (`None`: not at all).
    /// One that failed to open clears the preroll, so the cut opens the
    /// clip itself.
    fn take_ready_preroll(&mut self, wait: Option<Duration>) {
        use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
        let Some(rx) = &self.preroll_pending else { return };
        let got = match wait {
            None => rx.try_recv().map_err(|e| e == TryRecvError::Disconnected),
            Some(d) => rx.recv_timeout(d).map_err(|e| e == RecvTimeoutError::Disconnected),
        };
        match got {
            Ok(Some(dec)) => {
                self.preroll_pending = None;
                self.preroll_decoder = Some(dec);
            }
            // It failed to open, or its thread is gone: not again for
            // this clip.
            Ok(None) | Err(true) => {
                self.preroll_pending = None;
                self.preroll_failed = self.preroll_clip_index.take();
            }
            // Still opening.
            Err(false) => {}
        }
    }

    /// Drop the preroll, ready or still opening. A decoder that arrives
    /// after this finds no receiver and is stopped on its worker thread.
    fn clear_preroll(&mut self) {
        if let Some(mut old) = self.preroll_decoder.take() {
            old.stop_detached();
        }
        self.preroll_pending = None;
        self.preroll_clip_index = None;
        self.preroll_for = None;
        self.preroll_failed = None;
    }

    /// Whether the preroll is clip `idx` of the picture track, of
    /// `media_id`: its index, and the track and media it was opened for.
    fn preroll_is(&self, idx: usize, media_id: MediaId) -> bool {
        self.preroll_clip_index == Some(idx) && self.preroll_for == Some((self.current_track_index, media_id))
    }

    /// Start pre-decoding the next clip so frames are ready when the cut arrives.
    fn start_preroll_for_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let next_idx = self.current_clip_index + 1;

        // Find next non-gap clip on the SAME track as the current clip
        let next_info = {
            let mut result = None;
            if let Some(track) = seq.tracks.get(self.current_track_index) {
                if let Some(cur) = track.clips.get(self.current_clip_index).filter(|c| c.media_id().is_some()) {
                    for idx in next_idx..track.clips.len() {
                        let clip = &track.clips[idx];
                        if let Some(media_id) = clip.media_id() {
                            // Going on a little further in this file: the
                            // playing decoder decodes on to it at the cut.
                            // A preroll opened for each 4-frame piece of a
                            // strobe had the piece's 160 ms to seek in, and
                            // showed a third of its frames (#2397).
                            let decoded_on = self.v1_decoder.is_some() && decodes_on(track, cur, clip, self.fps.frame_duration_secs());
                            if !decoded_on {
                                result = Some((idx, media_id, clip.source_in.to_secs_f64(), clip.source_rotation, clip.timeline_in));
                            }
                            break;
                        }
                    }
                }
            }
            result
        };
        drop(seq);

        if let Some((idx, media_id, source_in_secs, preroll_rotation, cut_at)) = next_info {
            self.start_preroll_for(idx, media_id, source_in_secs, preroll_rotation, cut_at);
        }
    }

    /// Open and seek the decoder for clip `idx` of the picture track on a
    /// worker, for a cut to it at `cut_at`: at the end of the shot before
    /// it, or of the gap before it.
    fn start_preroll_for(&mut self, idx: usize, media_id: MediaId, source_in_secs: f64, preroll_rotation: i32, cut_at: Timecode) {
        {
            let same = self.preroll_for == Some((self.current_track_index, media_id));
            if same && (self.preroll_clip_index == Some(idx) || self.preroll_failed == Some(idx)) {
                return; // Already pre-rolling this clip, or it will not open
            }
            if let Some(path) = self.path_for_media(media_id) {
                let effective_preroll_rot = self.effective_decode_rotation(media_id, &path, preroll_rotation);
                // Close any previous preroll
                self.clear_preroll();
                // Opened on a worker thread, not this one: the open and
                // the seek cost the engine's tick 35–43 ms (#572).
                let (tx, rx) = std::sync::mpsc::channel();
                // Opened ahead, it is among the first to decode in
                // software when the GPU's decoder is full (#2116).
                let lease = crate::decode_budget::admit(self.media_pixel_rate(media_id, &path), crate::decode_budget::Priority::Overflow);
                // Counted while it opens (#2261), as a decoder's own thread is.
                let alive = crate::video_decoder::DecodeThreadToken::new();
                std::thread::spawn(move || crate::thread_guard::catch_panic("klipp-preroll", move || {
                    let _alive = alive;
                    match VideoDecoder::open_with(&path, effective_preroll_rot, lease) {
                        Ok(dec) => {
                            if source_in_secs > 0.01 {
                                dec.seek(source_in_secs);
                            }
                            dec.resume();
                            info!("CUT-DIAG PREROLL started: idx={} source_in={:.4}s", idx, source_in_secs);
                            // Nobody waiting any more: the playhead moved on.
                            if let Err(std::sync::mpsc::SendError(Some(mut dec))) = tx.send(Some(dec)) {
                                dec.stop_detached();
                            }
                        }
                        Err(e) => {
                            warn!("Failed to open preroll decoder: {e}");
                            let _ = tx.send(None);
                        }
                    }
                }));
                self.preroll_pending = Some(rx);
                self.preroll_clip_index = Some(idx);
                self.preroll_for = Some((self.current_track_index, media_id));
                self.preroll_source_in = source_in_secs;
                self.warm_sound_at(cut_at);
            }
        }
    }


    /// Play on from `at` with no picture clip under the playhead: the
    /// master clock runs in timeline seconds from `at`, and no clip of
    /// the picture track is current (its index one past the last, so
    /// the next-clip search finds nothing). See `clock_only`.
    fn enter_clock_only(&mut self, at: Timecode) {
        self.clock_only = true;
        self.gap_hand_over = None;
        self.v1_decoder = None;
        self.current_file = None;
        self.current_clip_index = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.get(self.current_track_index).map_or(0, |t| t.clips.len())
        };
        self.last_synth_frame_rendered = None;
        self.clip_source_start = at.to_secs_f64();
        self.clip_timeline_start = at;
        self.clip_speed = 1.0;
        // The clock counts timeline seconds here, at their own rate: a
        // retimed clip before left it running at that clip's speed.
        self.master_clock.set_speed(1.0);
        self.refresh_speed_keyframes();
        self.last_frame_time = std::time::Instant::now();
        self.master_clock.seek((at.to_secs_f64().max(0.0) * 1_000_000.0) as u64);
    }

    /// Whether the pictures at `tc` are reversed clips and nothing the
    /// primary decoder plays: there is a reversed clip on a picture track,
    /// and no forward media clip on any (#617).
    fn only_reversed_pictures_at(&self, tc: Timecode) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let mut reversed = false;
        for track in seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text)) {
            if let Some((_, c)) = track.clip_at_timecode(tc).filter(|(_, c)| c.enabled && c.media_id().is_some()) {
                if !c.is_reversed() {
                    return false;
                }
                reversed = true;
            }
        }
        reversed
    }

    /// Play from the playhead on the clock alone, the renderer drawing
    /// each frame: where the pictures are reversed clips (#617).
    fn play_on_the_clock(&mut self) {
        let at = self.playhead;
        info!("handle_play: reversed clips at {:.3}s, playing on the clock", at.to_secs_f64());
        self.enter_clock_only(at);
        self.present_frame(None);
        self.start_all_audio(at);
        if let Some(aout) = &self.audio_output {
            aout.0.pause();
        }
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
    }

    /// A tick of clock-only playback: the playhead follows the clock; a
    /// picture clip starting under it takes over as a seek there would;
    /// the end of the timeline stops; otherwise the frame is drawn once
    /// per new frame, whatever other tracks show there.
    fn tick_clock_only(&mut self, clock_secs: f64) {
        let secs = self.clip_timeline_start.to_secs_f64() + (clock_secs - self.clip_source_start).max(0.0);
        self.playhead = RationalTime::from_secs_f64(secs, self.fps.rate());
        let end = self.sequence_duration();
        if self.playhead >= end {
            self.playhead = end;
            self.report_playhead();
            self.stop_at_end();
            return;
        }
        let picture_here = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let tc = self.playhead;
            seq.tracks
                .iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                // A reversed clip's picture is drawn on the clock (#617).
                .any(|t| t.clip_at_timecode(tc).is_some_and(|(_, c)| (c.media_id().is_some() && !c.is_reversed()) || c.title_data().is_some() || c.generator_data().is_some()))
        };
        if picture_here {
            let tc = self.playhead;
            // The shot after a gap, and only it on screen: taken over as a
            // cut, from its preroll. Anything else there — a picture on
            // another track in the gap — is found by a seek, as before.
            if let Some((from, next)) = self.gap_hand_over.take() {
                let (only_it, media) = {
                    let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                    let on_track = seq.tracks.get(self.current_track_index).and_then(|t| t.clip_at_timecode(tc));
                    let layers = klipp_compose::active_layers(&seq, tc);
                    (on_track.as_ref().map(|(i, _)| *i) == Some(next) && layers.len() == 1, on_track.and_then(|(_, c)| c.media_id()))
                };
                if only_it && media.is_some_and(|m| self.preroll_is(next, m)) {
                    GAP_HAND_OVERS_WARM.fetch_add(1, Ordering::Relaxed);
                    self.current_clip_index = from;
                    self.advance_to_next_clip();
                    return;
                }
                // Come to too late, the playhead past the shot it was
                // for (#2386): its preroll, a decoder handed on across the
                // gap among them, is stale. Let go of it, and open the shot
                // here as a seek does: a clip stream taken over after a
                // late hand-over kept a strobe's next pieces dark.
                if self.preroll_clip_index == Some(next) {
                    self.clear_preroll();
                    GAP_HAND_OVERS_COLD.fetch_add(1, Ordering::Relaxed);
                    self.seek_in_sequence(tc);
                    if self.v1_decoder.is_none() && self.current_file.is_none() {
                        let at = self.clip_source_start + (tc - self.clip_timeline_start).to_secs_f64().max(0.0) * self.clip_speed;
                        self.master_clock.seek((at * 1_000_000.0) as u64);
                        self.master_clock.resume();
                    }
                    return;
                }
            }
            if self.take_over_stream_at(tc) {
                return;
            }
            GAP_HAND_OVERS_COLD.fetch_add(1, Ordering::Relaxed);
            self.seek_in_sequence(tc);
            // A title or generator takes over on the synthetic path, which
            // reads the clock as the clip's own seconds.
            if self.v1_decoder.is_none() && self.current_file.is_none() {
                let at = self.clip_source_start + (tc - self.clip_timeline_start).to_secs_f64().max(0.0) * self.clip_speed;
                self.master_clock.seek((at * 1_000_000.0) as u64);
                self.master_clock.resume();
            }
            return;
        }
        let frame = self.playhead.value;
        if Some(frame) != self.last_synth_frame_rendered {
            self.last_synth_frame_rendered = Some(frame);
            self.present_frame(None);
            self.frames_consumed += 1;
        }
        self.report_playhead();
        (self.repaint_cb)();
    }

    /// Take the shot at `tc` over as the primary from its clip stream, which
    /// opened and seeked it before it came up, rather than open its file
    /// again with a seek (#2097): the shot that comes up on another track
    /// as the primary's runs out. The same shot a seek makes the primary,
    /// the bottom track's. `false` when it has no stream ready.
    fn take_over_stream_at(&mut self, tc: Timecode) -> bool {
        let found = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks.iter().enumerate().rev()
                .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|(ti, track)| {
                    track.clip_at_timecode(tc).filter(|(_, c)| !c.is_reversed() && c.media_id().is_some()).map(|(idx, c)| (ti, idx, c.clone()))
                })
        };
        let Some((ti, idx, clip)) = found else { return false };
        let Some(media_id) = clip.media_id() else { return false };
        let Some(path) = self.path_for_media(media_id) else { return false };
        let Some((dec, held)) = self.streams.take(clip.id) else { return false };
        STREAM_TAKE_OVERS.fetch_add(1, Ordering::Relaxed);
        debug!("CUT-DIAG primary took {:?} over from its clip stream", clip.id);
        self.current_track_index = ti;
        self.current_clip_index = idx;
        self.v1_decoder = Some(dec);
        self.current_source_rotation = self.effective_decode_rotation(media_id, &path, clip.source_rotation);
        self.current_file = Some(path);
        self.clock_only = false;
        self.last_synth_frame_rendered = None;
        // The clock reads the shot's source seconds from here on, as after
        // a cut, where clock-only read the timeline's.
        let source_secs = klipp_compose::render::source_secs(&clip, tc, self.fps.as_f64());
        self.clip_source_start = clip.source_in.to_secs_f64();
        self.clip_timeline_start = clip.timeline_in;
        self.clip_speed = clip.speed;
        self.master_clock.set_speed(clip.speed);
        self.refresh_speed_keyframes();
        self.master_clock.seek((source_secs.max(0.0) * 1_000_000.0) as u64);
        self.master_clock.resume();
        self.playhead = tc;
        self.swap_changed_audio_at_cut();
        self.refresh_audio_fades(tc);
        self.last_frame_time = std::time::Instant::now();
        // The frame the stream holds is the one for here: shown now, the
        // queue goes on from the frame after it.
        if let Some(held) = held {
            self.present_frame(Some(held));
            self.frames_consumed += 1;
        }
        self.report_playhead();
        (self.repaint_cb)();
        true
    }

    /// Tell the UI where the playhead is, never past the timeline's end:
    /// a playing clock runs on past it until the end is seen, and the UI
    /// then showed a time beyond the end and jumped back at the stop.
    ///
    /// Nor, while playing, behind where it was last said, the way it
    /// plays (#1806): never back of it forwards, never ahead of it played
    /// backwards (J, a reverse shuttle). The playhead follows the frame
    /// shown when one is shown, and the clock (the sound) when none is new
    /// yet; a frame that comes late is behind the clock, and following it
    /// stepped the playhead back a frame or more under load, at the cut
    /// after a gap most of all.
    fn report_playhead(&self) {
        let end = self.sequence_duration();
        let mut at = if end.value > 0 { self.playhead.min(end) } else { self.playhead };
        if self.state == PlaybackState::Playing {
            let forwards = self.master_clock.speed() >= 0.0;
            match self.report_floor.get() {
                Some((floor, true)) if forwards => at = at.max(floor),
                Some((ceiling, false)) if !forwards => at = at.min(ceiling),
                _ => {}
            }
            self.report_floor.set(Some((at, forwards)));
        } else {
            self.report_floor.set(None);
        }
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(at));
    }

    /// Say a jump of the playhead to `at` (a seek, a loop back): from
    /// there on, reports while playing follow on from it, not from where
    /// it jumped from.
    fn report_jump(&self, at: RationalTime) {
        self.report_floor.set(None);
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(at));
    }

    fn stop_at_end(&mut self) {
        self.clear_preroll();
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
        self.clock_only = false;
        self.streams.clear();
        self.stop_all_audio();
        self.last_synth_frame_rendered = None;
        // Where it stopped, said once: a frame dropped under load may have
        // gone unreported, and the clock may have run past the end.
        self.playhead = self.playhead.min(self.sequence_duration());
        self.report_playhead();
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
            let fadeable = fadeable_media(&pool);
            let track_audio: Vec<PreSeekRow> = audio_rows(&seq, &pool, &fadeable, tc, use_proxies, true);
            // The envelope each source plays from its first sample. The
            // feeder shapes sound as it fills its buffer, about two
            // seconds of it in the moment it runs below, so an envelope
            // set at the swap-in came too late for a loop whose in
            // point sits inside a crossfade: both clips played the
            // window at full level and the outgoing one stopped dead.
            let envelopes: Vec<Option<Envelope>> = track_audio
                .iter()
                .map(|row| sounding_for_key(&seq, &fadeable, row.0, tc).map(|s| envelope_of(&seq, &s, tc)))
                .collect();
            drop(pool);
            drop(seq);

            // Open and seek audio sources
            let mut sources = Vec::new();
            for ((track_idx, media_id, path, source_secs, audio_effects, audio_effect_keyframes), envelope) in track_audio.into_iter().zip(envelopes) {
                let chain_fp = audio_chain_fingerprint_for(&audio_effects, &audio_effect_keyframes);
                // Try PCM cache
                if let Ok((feeder, consumer)) = PcmAudioFeeder::open(&path, sample_rate, audio_effects.clone(), audio_effect_keyframes.clone()) {
                    if source_secs > 0.01 {
                        feeder.seek(source_secs);
                    }
                    let source = AudioSource::PcmFeeder(feeder);
                    if let Some(envelope) = &envelope {
                        envelope.apply(&source);
                    }
                    let zero_latency = std::sync::Arc::new(
                        std::sync::atomic::AtomicU32::new(0),
                    );
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        media_id,
                        audio_chain_fingerprint: chain_fp,
                        source,
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
        self.clear_preroll();
        debug!("[Loop] Executing loop-back to {:.3}s", target.to_secs_f64());
        // The loop lands on its target by either path below; whatever
        // is there decides the mode, not the tail it left.
        self.clock_only = false;
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
                    let mix = self.ensure_mix_pipeline(ps_source.consumer, ps_source.track_index, latency);
                    self.audio_sources.push(ActiveAudioSource {
                        mix,
                        track_index: ps_source.track_index,
                        media_id: ps_source.media_id,
                        audio_chain_fingerprint: ps_source.audio_chain_fingerprint,
                        // Built with the settings of pre-seek time; the
                        // next settings push re-applies the current ones.
                        settings_fingerprint: 0,
                        source: ps_source.source,
                        starts_at: None,
                        may_ring: false,
                        clip: None,
                    });
                }
                // The pre-seek thread filled them on the envelopes of
                // the clips under the loop's in point, which is where
                // the playhead is from here on; the output's volume,
                // pan, gain and channel config still come from there,
                // and the envelopes are refreshed against the sequence
                // as it is now.
                self.playhead = target;
                self.sync_clip_volumes();

                // 3. Seek video decoder (still needed — video wasn't pre-seeked)
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let clip_info = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(target).filter(|(_, c)| !c.is_reversed()).and_then(|(idx, clip)| {
                            let media_id = clip.media_id()?;
                            let source_secs = klipp_compose::render::source_secs(clip, target, seq.fps.as_f64());
                            let clip_source_in = clip.source_in.to_secs_f64();
                            Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                        })
                    });
                drop(seq);

                let source_secs_for_clock = if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
                    let need_new = self.current_clip_index != idx || self.decoder_missing();
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
                        self.clip_source_start = clip_source_in;
                        self.clip_timeline_start = timeline_in;
                        self.clip_speed = spd;
                        self.refresh_speed_keyframes();
                    }
                    source_secs
                } else {
                    // No picture for the primary decoder there — a gap, or
                    // only reversed clips (#617): the clock plays it.
                    self.enter_clock_only(target);
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
                self.sync_streams();

                let _ = self.event_tx.send(AppEvent::LoopedBack);
                self.report_jump(target);
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
                track.clip_at_timecode(target).filter(|(_, c)| !c.is_reversed()).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = klipp_compose::render::source_secs(clip, target, seq.fps.as_f64());
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                })
            });
        drop(seq);

        // Handle media clip at loop target
        if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
            let need_new_decoder = self.current_clip_index != idx || self.decoder_missing();

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
                // The queue is the decoder's to clear, when it applies the
                // seek: a step forward keeps the frames it read ahead,
                // which are exactly the next positions of a scrub. Frames
                // from before the seek carry an older epoch and are
                // dropped when taken. Clearing here threw the read-ahead
                // away before the decoder could see it.
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
            self.report_jump(target);
            (self.repaint_cb)();
        } else {
            // No forward media at target: synthetic clips, audio-only
            // stretches and reversed pictures all start on the clock.
            self.enter_clock_only(target);

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
            self.report_jump(target);
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
        // A decoder that ended with a panic is opened again below (#3181).
        if self.v1_decoder.as_ref().is_some_and(VideoDecoder::has_panicked) {
            self.v1_decoder = None;
        }
        self.retained_primary = None;
        // A preroll made for where the playhead was, or remembered as
        // failing there, is not for where it lands.
        self.clear_preroll();
        self.gap_hand_over = None;
        let t_seek = std::time::Instant::now();
        self.clock_only = false;
        self.sync_fps_to_sequence();
        // A refresh armed for where the playhead was isn't this seek's;
        // a miss here arms one again.
        self.disarm_paused_refresh();
        // The pre-grade cache stays: a seek back onto a frame shown before
        // is answered from it below, without the decoder.
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
                                klipp_timeline::ClipKind::Generator(_) => clip.source_in.to_secs_f64(),
                                _ => return None,
                            };
                            Some((ti, idx, clip.timeline_in, source_start, clip.speed))
                        })
                    })
            } else {
                None
            };
            drop(seq);

            if let Some((ti, idx, timeline_in, source_start, speed)) = synth {
                self.v1_decoder = None;
                self.current_track_index = ti;
                self.current_clip_index = idx;
                self.current_file = None;
                self.clip_source_start = source_start;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = speed;
                self.master_clock.set_speed(speed);
                self.refresh_speed_keyframes();
                let source_secs = source_start + (tc - timeline_in).to_secs_f64() * speed;
                self.reposition_audio_or_defer(tc, source_secs);
                self.master_clock.seek((source_secs.max(0.0) * 1_000_000.0) as u64);
                self.playhead = tc;
                self.present_frame(None);
                self.report_jump(tc);
                (self.repaint_cb)();
                return;
            }
        }

        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // Search video tracks in reverse — last video track (bottom of panel) is primary.
        let clip_info = seq.tracks.iter().enumerate().rev()
            .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|(ti, track)| {
                // A reversed clip is drawn from its own reader, never the
                // primary decoder's (#617).
                track.clip_at_timecode(tc).filter(|(_, c)| !c.is_reversed()).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = klipp_compose::render::source_secs(clip, tc, seq.fps.as_f64());
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((ti, idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed, clip.source_rotation))
                })
            });
        drop(seq);

        if let Some((ti, idx, media_id, source_secs, timeline_in, clip_source_in, spd, rot)) = clip_info {
            // The decoder is the clip's only when it reads the clip's file
            // the clip's way up. The index alone is not the clip: an insert
            // or a drop at the playhead puts another clip at the same index,
            // which was then drawn from the decoder of the one it displaced.
            let file = self.resolve_media_path(media_id, false).map(|path| {
                let rot = self.effective_decode_rotation(media_id, &path, rot);
                (path, rot)
            });
            let same_file = file.as_ref().is_some_and(|(path, rot)| {
                self.current_file.as_ref() == Some(path) && self.current_source_rotation == *rot
            });
            let need_new_decoder = self.current_track_index != ti
                || self.current_clip_index != idx
                || self.decoder_missing()
                || !same_file;
            self.current_track_index = ti;

            if need_new_decoder {
                if let Some((path, effective_rot)) = file {
                    self.v1_decoder = None;
                    self.stop_all_audio();
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
                // The queue is the decoder's to clear, when it applies the
                // seek: a step forward keeps the frames it read ahead,
                // which are exactly the next positions of a scrub. Frames
                // from before the seek carry an older epoch and are
                // dropped when taken. Clearing here threw the read-ahead
                // away before the decoder could see it.
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = spd;
                self.refresh_speed_keyframes();
            }

            let was_playing = self.state == PlaybackState::Playing;
            let t_video = t_seek.elapsed();

            // 1. Pause clock + cpal output
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output {
                aout.0.pause();
            }

            // 2. Put the audio at the new time — or owe it, when that
            // means opening sources nobody is listening to yet.
            let audio_in_place = self.reposition_audio_or_defer(tc, source_secs);

            // 3. Set master clock to target
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);
            let t_audio = t_seek.elapsed();

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
            let t_prefill = t_seek.elapsed();
            // Sources already start paused, no need to pause them

            // The frame renders at the new time; the other clips' streams
            // start seeking now, alongside the primary decoder.
            self.playhead = tc;
            self.sync_streams();

            if !was_playing {
                // Shown before and still held: the picture is at hand, and
                // the decoder — already sent after it above, for the
                // frames around it — can idle until the next position.
                let cached = self.frame_cache.get(tc.value, self.primary_clip_now());
                let got_frame = match cached {
                    Some((pixels, cw, ch)) if !self.presenting_progress => {
                        self.present_frame(Some(streams::HeldFrame::from_pregrade(&pixels, cw, ch)));
                        if let Some(dec) = &self.v1_decoder {
                            dec.pause();
                        }
                        self.last_grab_wait = Duration::ZERO;
                        true
                    }
                    _ => self.grab_single_frame_timeout(grab_timeout),
                };
                let total = t_seek.elapsed();
                // Where a seek's time goes. Only seeks worth noticing are
                // logged at info; a scrub is a stream of these.
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                if total >= Duration::from_millis(20) {
                    info!(
                        "SEEK_TIMING total={:.1}ms video={:.1} audio={:.1} prefill={:.1} wait={:.1} present={:.1} got_frame={got_frame}",
                        ms(total), ms(t_video), ms(t_audio - t_video), ms(t_prefill - t_audio),
                        ms(self.last_grab_wait), ms(total - t_prefill - self.last_grab_wait)
                    );
                }
                if !got_frame {
                    // 4K / slow codec cold seeks routinely outrun the
                    // 50–200 ms grab budget. Instead of giving up and
                    // freezing V2 thumbs on the pre-seek frame until
                    // the user pokes an arrow key, keep the decoder
                    // running and let the per-tick paused branch
                    // post-process the frame the moment it lands.
                    self.pending_paused_refresh = true;
                    self.paused_refresh_since = Some(std::time::Instant::now() - grab_timeout);
                    self.note_view(format!("seek missed its frame at {:?}: paused refresh armed", self.playhead.value));
                }
                // Play scrub chirp (~40ms snippet at the seek position) unless
                // the user has disabled audio during a scrub.
                if audio_in_place && !self.quiet_seek && (!self.is_scrubbing || self.scrub_audio_enabled) {
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
            // The clip the playhead left is not this place's: without
            // this, Play resumed it past its end and the playhead stood
            // still. Where the timeline goes on, play on the clock (#542).
            if tc < self.sequence_duration() {
                self.enter_clock_only(tc);
            }
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

                // Put the audio at the new time
                self.reposition_audio(tc);
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
                    if !self.quiet_seek && (!self.is_scrubbing || self.scrub_audio_enabled) {
                        if let Some(active) = self.audio_sources.first() {
                            let source_secs = {
                                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                                seq.tracks.iter().find_map(|track| {
                                    let (_, clip) = track.clip_at_timecode(tc)?;
                                    Some(klipp_compose::render::source_secs(clip, tc, seq.fps.as_f64()))
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
        self.report_jump(tc);
        (self.repaint_cb)();
    }

    /// Show the frame at `target` (a step back while paused) from a window
    /// of the clip's frames decoded forward and held (#2798): the first
    /// step decodes the window ending there, the next ones are frames
    /// already held, and the window before is decoded as the steps near
    /// its start. `false` when the frame isn't there within a second or
    /// the clip under the playhead isn't one to read so; the step is then
    /// taken the old way.
    fn step_back_through_window(&mut self, target: RationalTime) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip = seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)).cloned();
        let fps = seq.fps.as_f64();
        drop(seq);
        let Some(clip) = clip.filter(|c| !c.is_reversed() && c.timeline_in <= target && target < c.timeline_out) else { return false };
        let Some(media) = clip.media_id() else { return false };
        let source = clip.source_secs_at((target - clip.timeline_in).to_secs_f64(), fps);
        if self.step_back.as_ref().is_none_or(|(id, _)| *id != clip.id) {
            let Some(path) = self.path_for_media(media) else { return false };
            let rotation = self.effective_decode_rotation(media, &path, clip.source_rotation);
            let lease = crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Fixed);
            let Ok(decoder) = VideoDecoder::open_with_queue(&path, rotation, lease, 8) else { return false };
            let bytes = self.media_pool.read().unwrap_or_else(|e| e.into_inner()).get(&media).map_or(0.0, |m| m.resolution.width as f64 * m.resolution.height as f64 * 4.0);
            self.step_back = Some((clip.id, reverse_gop::ReverseGop::for_memory(decoder, bytes, crate::decode_budget::free_vram(), 0.0)));
        }
        let Some((_, window)) = self.step_back.as_mut() else { return false };
        window.learn_free(crate::decode_budget::free_vram());
        window.want(source, false);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let frame = loop {
            if window.ready() {
                break window.frame(source);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
            window.pump();
        };
        let Some(frame) = frame else { return false };
        self.playhead = target;
        self.present_frame(Some(frame));
        self.report_playhead();
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
        true
    }

    /// Step forward or backward by one frame without a full seek.
    fn step_frame(&mut self, direction: i64) {
        // The step's frame is the playhead's now; a seek that runs below
        // arms a refresh of its own if it misses.
        self.disarm_paused_refresh();
        // Positions here are playhead ticks (frame_cache is keyed by
        // them too); a frame is `frame_ticks` of them.
        let step = self.frame_ticks();
        let target_frame = self.playhead.value + direction * step;

        // Check frame cache first (works for both forward AND backward).
        // The cache stores sRGB u8 — bridge at this viewer seam.
        if let Some((pixels, w, h)) = self.frame_cache.get(target_frame, self.primary_clip_now()) {
            self.playhead = RationalTime { value: target_frame, rate: self.playhead.rate };
            self.present_frame(Some(streams::HeldFrame::from_pregrade(&pixels, w, h)));
            self.report_playhead();
            // Keep decoder filling the queue for future steps
            self.prefetch_ahead();
            return;
        }

        // Nothing for the primary decoder to step through — only reversed
        // clips there (#617), or no decoder at all: the next position is
        // rendered as a seek renders it.
        let target_tc = RationalTime { value: target_frame, rate: self.playhead.rate };
        if self.v1_decoder.is_none() || self.only_reversed_pictures_at(target_tc) {
            self.seek_in_sequence(target_tc);
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
                    self.playhead = timeline_for_source(clip, frame.pts_secs(), seq.fps.as_f64(), self.fps.rate());
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
                let clip = self.primary_clip_now();
                self.frame_cache.insert_if_absent(self.playhead.value, clip, Pregrade::from_frame(&frame), w, h);
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
                self.report_playhead();
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
                        self.playhead = timeline_for_source(clip, frame.pts_secs(), seq.fps.as_f64(), self.fps.rate());
                    }
                    drop(seq);

                    let (w, h) = (frame.width, frame.height);
                    // Cache pre-grade frame as u8 for backward
                    // stepping; post_process_rgba stays linear.
                    frame.realise_hdr_on_cpu();
                    if frame.hdr.is_none() {
                        frame.ensure_cpu_data();
                    }
                    let clip = self.primary_clip_now();
                    self.frame_cache.insert_if_absent(self.playhead.value, clip, Pregrade::from_frame(&frame), w, h);
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
                    self.report_playhead();
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
        } else if self.step_back_through_window(target_tc) {
            // Backward, from the window of frames held for it (#2798).
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
                            timeline_for_source(clip, frame.pts_secs(), seq.fps.as_f64(), self.fps.rate()).value
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
                    let clip = self.primary_clip_now();
                    self.frame_cache.insert_if_absent(frame_num, clip, Pregrade::from_frame(&frame), w, h);

                    // When we reach the target frame, display it
                    if frame_num >= target_frame && !found_target {
                        found_target = true;
                        self.playhead = RationalTime { value: frame_num, rate: self.playhead.rate };
                        self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
                        self.report_playhead();
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
                .filter(|c| c.frames_cacheable())
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
            let Some((pixels, w, h)) = self.frame_cache.get(frame_num, self.primary_clip_now()) else { continue };
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
        let mut status = Vec::new();

        for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
            for clip in &track.clips {
                if !clip.frames_cacheable() {
                    continue;
                }
                let grade_hash = self.render_key_hash(&seq, clip, track.opacity);
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
                // The strip over the ruler (#924): frames in memory or on
                // disk, which the disk cache knows from its file names.
                let mut frames: Vec<i64> = self.render_cache_hot.frames_for(clip.id, grade_hash).collect();
                frames.extend(self.render_cache_disk.cached_frames(clip.id, grade_hash));
                frames.sort_unstable();
                frames.dedup();
                status.extend(crate::texture::render_spans(clip.timeline_in.value, clip.timeline_out.value, &frames, ticks_per_frame));
            }
        }
        drop(seq);

        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        fb.render_cache_ranges = ranges;
        fb.render_status = status;
    }

    /// Clear the render cache for every clip under `from..to` (#924):
    /// the frames in memory and on disk, at every grade and size.
    fn clear_render_cache_range(&mut self, from: RationalTime, to: RationalTime) {
        let clips: Vec<klipp_core::ClipId> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            seq.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| c.timeline_in < to && c.timeline_out > from)
                .map(|c| c.id)
                .collect()
        };
        let mut removed = 0;
        for clip in clips {
            self.render_cache_hot.remove_frames(clip, from.value..to.value);
            removed += self.render_cache_disk.remove_frames(clip, from.value..to.value);
        }
        info!("Render cache: cleared {removed} cached frame(s) from {:.2}s to {:.2}s", from.to_secs_f64(), to.to_secs_f64());
        self.update_render_cache_ranges();
        (self.repaint_cb)();
    }

    fn prefetch_ahead(&mut self) {
        // 1. Drain any currently queued forward frames into cache
        if let Some(dec) = &self.v1_decoder {
            let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            while let Some(mut frame) = queue.pop() {
                if frame.is_eof { break; }
                let pts = frame.pts;
                let (w, h) = (frame.width, frame.height);
                let pixels = cached_pregrade(&mut frame);
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let frame_num = if let Some(clip) = seq.tracks.get(self.current_track_index)
                    .and_then(|t| t.clips.get(self.current_clip_index))
                {
                    timeline_for_source(clip, pts.to_secs_f64(), seq.fps.as_f64(), self.fps.rate()).value
                } else {
                    continue;
                };
                drop(seq);
                let clip = self.primary_clip_now();
                self.frame_cache.insert_if_absent(frame_num, clip, pixels, w, h);
            }
        }

        // 2. Check if backward frames need prefilling
        // Thirty frames, in playhead ticks: a frame is `frame_ticks` of
        // them, 1001 at an NTSC rate, where stepping back one tick at a
        // time never met a cached frame and re-seeked on every step.
        let current = self.playhead.value;
        let step = self.frame_ticks();
        let backward_needed = (1..=30).any(|i| {
            let f = current - i * step;
            f >= 0 && self.frame_cache.get(f, self.primary_clip_now()).is_none()
        });

        if backward_needed {
            // Seek backward and decode forward to fill the cache behind us.
            // This is a one-time cost that makes all subsequent backward steps instant.
            let prefetch_start = (current - 30 * step).max(0);
            // The decoder seeks in source time, through the clip's speed
            // curve, not to the playhead's timeline seconds.
            let seek_secs = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let at = RationalTime { value: prefetch_start, rate: self.playhead.rate };
                seq.tracks.get(self.current_track_index)
                    .and_then(|t| t.clips.get(self.current_clip_index))
                    .map(|clip| klipp_compose::render::source_secs(clip, at.max(clip.timeline_in), seq.fps.as_f64()))
            };
            if let (Some(dec), Some(seek_secs)) = (&self.v1_decoder, seek_secs) {
                dec.seek(seek_secs);
                dec.resume();
            }

            let deadline = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < deadline {
                let popped = self.v1_decoder.as_ref()
                    .and_then(|dec| dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).pop());
                if let Some(mut frame) = popped {
                    if frame.is_eof { break; }
                    let (w, h) = (frame.width, frame.height);
                    let pixels = cached_pregrade(&mut frame);
                    let frame_num = {
                        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                        if let Some(clip) = seq.tracks.get(self.current_track_index)
                            .and_then(|t| t.clips.get(self.current_clip_index))
                        {
                            timeline_for_source(clip, frame.pts_secs(), seq.fps.as_f64(), self.fps.rate()).value
                        } else { break; }
                    };
                    let clip = self.primary_clip_now();
                    self.frame_cache.insert_if_absent(frame_num, clip, pixels, w, h);
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
            }
        }

        // 3. Resume decoder for continuous forward prefetch
        if let Some(dec) = &self.v1_decoder {
            dec.resume();
        }
    }

    /// The clip the primary decoder reads: what a frame from it, or from
    /// the frame cache, is shown as.
    fn primary_clip_now(&self) -> Option<klipp_core::ClipId> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        self.v1_clip_id(&seq)
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
        let Some(dec) = self.v1_decoder.as_ref() else { return true };
        // Half of one of the file's frames, as playback and export take it
        // (#696).
        let half = dec.half_source_frame(self.fps.as_f64());
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
        let started = std::time::Instant::now();
        let deadline = started + timeout;
        let mut best = None;
        let mut settled = false;
        while std::time::Instant::now() < deadline {
            if self.take_v1_frame_for_playhead(&mut best) && best.is_some() {
                settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.last_grab_wait = started.elapsed();
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

    /// Forget the paused refresh an earlier seek armed, and the frame it
    /// held back: they are for where the playhead was. Every move of the
    /// playhead while paused calls this first. Left armed, a move answered
    /// from the frame cache had that refresh drain on a later tick and
    /// present a frame short of the new playhead over the cached one, as
    /// incomplete; with the decoder paused, nothing completed it (#1092).
    fn disarm_paused_refresh(&mut self) {
        self.pending_paused_refresh = false;
        self.paused_refresh_since = None;
        self.paused_refresh_best = None;
    }

    /// A previous seek (Pause / SetPlayhead / SeekTo while paused) left
    /// the decoder running because its 50–200 ms grab window wasn't
    /// enough for a cold 4K seek. Pop any frame the decoder has produced
    /// since, run the full post_process pipeline (captures per-node
    /// thumbs), then pause the decoder. Silent no-op until the frame lands.
    fn tick_paused_refresh(&mut self) {
        if self.pending_paused_refresh && self.try_drain_paused_refresh_frame() {
            self.pending_paused_refresh = false;
            self.note_view(format!("paused refresh drained at {:?}", self.playhead.value));
        }
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
        self.presenting_progress = !settled;
        self.present_frame(Some(streams::HeldFrame::from_frame(frame)));
        self.presenting_progress = false;
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
        const REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

        // Not while a scrub is live. A drag sends no seek until it is
        // released, so `self.playhead` is frozen at the pre-drag position
        // and this debounce never restarts — about 400 ms in it would
        // re-decode the original at that stale position and present it
        // into the same buffer the drag's skim frames are written to. A
        // full-size graded frame from where the drag began, over a
        // reduced ungraded frame at the cursor: sharpness, colour and
        // time all jump at once. The debounce restarts at EndScrub.
        if self.state != PlaybackState::Paused || self.is_scrubbing {
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
                        let source_secs = klipp_compose::render::source_secs(clip, ph, seq.fps.as_f64());
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
    /// from a nearby timestamp. The UI marks this as a low-resolution
    /// preview, and a viewer render of the same request takes precedence.
    ///
    /// Source is sRGB u8 RGBA at `src_w × src_h`. The renderer runs the
    /// clip at that size through its inspected render (M3 step 16a), and
    /// the executor's thumb-capture stage downsamples each visible node's
    /// output to 192×108 (`THUMB_W/THUMB_H` in klipp-nodes). A node the
    /// run doesn't hold gets no thumbnail and says why in the log.
    /// Silently no-ops when the visible-set is empty, the clip is
    /// missing, or the source payload has the wrong length.
    fn render_node_thumbs_from_source(
        &mut self,
        request: u64,
        clip_id: klipp_core::ClipId,
        playhead: klipp_core::RationalTime,
        src_rgba: Vec<u8>,
        src_w: u32,
        src_h: u32,
    ) {
        if request != self.thumbnail_request || self.inspect_clip != Some(clip_id) || self.visible_node_thumbs.is_empty() {
            return;
        }
        // Drain before capturing the revision: a late queued edit must
        // never produce an answer labelled with the pre-edit token. The
        // UI rejects it until it observes that revision and requests again.
        self.drain_param_changes();
        if src_rgba.len() != (src_w as usize) * (src_h as usize) * 4 {
            return;
        }
        // The clip alone, its picture this bitmap, inspected through the
        // renderer as every inspection is (M3 step 16a): the production
        // plan, each thumbnail named by path, each node evaluated at the
        // playhead's time. A copy of the sequence, so the render doesn't
        // hold its lock.
        let (seq, revision) = self.sequence.snapshot_with_revision();
        if !seq.all_clips().any(|c| c.id == clip_id) {
            return;
        }
        struct Bitmap(klipp_core::ClipId, std::sync::Arc<Vec<f32>>, u32, u32);
        impl klipp_compose::FrameSource for Bitmap {
            fn frame(&mut self, clip: &klipp_timeline::Clip, _secs: f64, _w: u32, _h: u32) -> Option<klipp_compose::SourceFrame> {
                (clip.id == self.0).then(|| klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(self.1.clone()), width: self.2, height: self.3, pts: None })
            }
        }
        let mut source = Bitmap(clip_id, std::sync::Arc::new(u8_rgba_to_linear(&src_rgba)), src_w, src_h);
        let inspect = klipp_compose::Inspect {
            clip: Some(clip_id),
            only: Some(clip_id),
            thumbs: Some(self.visible_node_thumbs.clone()),
            ..Default::default()
        };
        let (_, found) = self.renderer.render_inspected(&seq, playhead, src_w, src_h, &mut source, klipp_compose::RenderOptions::default(), inspect);
        for (at, why) in &found.unavailable {
            tracing::debug!("node thumbnail {at:?}: {why}");
        }
        let batch = crate::node_thumbnails::ThumbnailBatch::new(
            request, crate::node_thumbnails::frame_index(playhead, seq.fps), crate::node_thumbnails::PreviewQuality::Filmstrip,
            revision, &self.thumbnail_priority, found.thumbs,
        )
        .with_unavailable(&self.thumbnail_priority, found.unavailable);
        self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).publish_node_thumbnails(batch);
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
        // Dithered as the GPU's display encode is (#2072): one picture
        // whichever route drew it.
        klipp_color::display::display_transform_ws_dithered_to(
            linear,
            self.tone_map_op,
            crate::video_decoder::working_colorspace(),
            self.sdr_transfer,
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

    /// Why a fast path cannot present `picture`, a frame it holds: a
    /// picture of another shape from the sequence's is barred by the
    /// renderer, in a frame of the sequence's shape, and the plain present
    /// would show it bare. Asked of the frame itself, not the media pool,
    /// whose size is a fallback when the file's could not be read.
    fn picture_shape_blocker(&self, picture: (u32, u32)) -> Option<&'static str> {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let sequence = (seq.resolution.width, seq.resolution.height);
        (!view::same_shape(picture, sequence)).then_some("picture is not the sequence's shape")
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
    /// `blockers` without the one a held heavy output may lift (#1863,
    /// #1939): whether it does is `regrade_parked_on_gpu_for`'s to say,
    /// for the parked frame it holds.
    fn held_may_lift(blockers: &[&'static str]) -> Vec<&'static str> {
        blockers.iter().copied().filter(|b| *b != "builtin effects on clip").collect()
    }

    fn only_the_graded_path_handles(blockers: &[&'static str]) -> bool {
        blockers.iter().all(|b| {
            matches!(
                *b,
                "grade graph is not an identity"
                    | "ACES working space"
                    | "BT.2020 working space"
                    | "HDR preview"
                    | "BT.1886 viewer transfer"
            )
        })
    }

    /// Whether the zero-copy GPU *stack* present can show this frame:
    /// everything [`Self::only_the_graded_path_handles`] accepts, plus
    /// the two things the stack applies that the lean graded present
    /// cannot — a GPU-capable builtin-effect chain and render keyframes.
    ///
    /// `present_frame_with(_, true)` runs the frame through
    /// `render_composite_on_gpu` → `place_layer`, which grades on the
    /// GPU *and* applies the clip's effects there (a keyframed pan/zoom
    /// is baked to a `Transform` in `clip_effects` and run on the GPU as
    /// a static one is), with no per-frame readback. So a graded clip
    /// carrying a reframe or an animated transform keeps the whole
    /// picture GPU-resident instead of falling to the CPU compositor.
    ///
    /// Only the *stack* gate may widen this far.
    /// `present_graded_frame_on_gpu` grades and nothing else, so routing
    /// an effect or a keyframed transform there would drop it from the
    /// picture — that gate stays on `only_the_graded_path_handles`.
    ///
    /// A builtin chain is admitted only when every effect has a GPU pass:
    /// a frei0r plug-in (`klipp_effects::has_frei0r`) runs as a node on the
    /// CPU timeline render and keeps the frame off this path. The keyframe-baked transform is always a
    /// `Transform`, which the GPU has, so "render keyframes on clip" is
    /// always safe here.
    fn stack_present_handles(&self, blockers: &[&'static str]) -> bool {
        if blockers.is_empty() {
            return true;
        }
        let builtin_on_gpu = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            self.v1_clip_id(&seq)
                .and_then(|id| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id))
                .is_some_and(|c| c.effect_chain_gpu_capable())
        };
        blockers.iter().all(|b| {
            matches!(
                *b,
                "grade graph is not an identity"
                    | "ACES working space"
                    | "BT.2020 working space"
                    | "HDR preview"
                    | "BT.1886 viewer transfer"
                    | "render keyframes on clip"
                    | "timeline grade active"
                    // The stack places it through the renderer (#1881).
                    | "matte sequence on clip"
            ) || (*b == "builtin effects on clip" && builtin_on_gpu)
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
        self.fast_path_blockers_for(if parked_drag { FastPath::ParkedDrag } else { FastPath::Playing })
    }

    /// [`Self::fast_path_blockers`] for one of the three uses of the fast
    /// path. `PausedPresent` is a paused present — a seek, a scrub step,
    /// a skim — of a frame the renderer would only pass through: the
    /// decoded sRGB bytes, which the pre-grade capture has already read
    /// back, are the picture, and the full path's expansion to linear
    /// floats, composite and re-encode (17 ms of a 24 ms 1080p present)
    /// is skipped. The pre-grade capture itself is not a blocker there:
    /// it has happened.
    /// A layer the fast path can lay over the presented frame itself:
    /// title clips only, on a track at full opacity, blended Normal with
    /// nothing else applied to them. Everything else needs the full
    /// composite.
    fn title_overlay_layer(seq: &klipp_timeline::Sequence, layer: &klipp_compose::ActiveLayer<'_>) -> bool {
        if layer.transition {
            return false;
        }
        let track_ok = seq.tracks.get(layer.track).is_some_and(|t| (t.opacity - 1.0).abs() < 1e-6);
        track_ok
            && layer.clips.iter().all(|c| {
                matches!(c.kind, klipp_timeline::ClipKind::Title(_))
                    // Laid over as drawn, so only where the camera does
                    // not move it.
                    && (seq.camera.is_still() || c.depth == 0.0)
                    && c.grade_is_identity()
                    && !c.has_enabled_plugins()
                    && !c.has_effect_entries()
                    && !c.has_animation()
                    && matches!(c.blend_mode, klipp_core::BlendMode::Normal)
                    && c.matte_source.is_none()
                    && c.matte_stencil.is_none()
                    && !c.consumed_as_matte
            })
    }

    /// Lay the title layers above the primary onto a presented frame's
    /// bytes, in stacking order. The renderer's cache makes a resting
    /// or scrolling title a row copy; the blend runs across threads.
    pub(super) fn overlay_titles_onto(&mut self, bytes: &mut [u8], width: u32, height: u32) {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let layers = klipp_compose::active_layers(&seq, self.playhead);
        if layers.len() < 2 {
            return;
        }
        let t = std::time::Instant::now();
        let mut n = 0;
        for layer in &layers[1..] {
            for clip in &layer.clips {
                if let Some(sprite) = self.renderer.render_title_layer_u8(clip, self.playhead, width, height) {
                    klipp_compose::over_u8_straight(bytes, &sprite, width);
                    CPU_TITLE_OVERLAYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    n += 1;
                }
            }
        }
        if n > 0 {
            tracing::trace!("fast path: {n} title layer(s) laid over the frame in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }

    /// The reasons a frame cannot be shown from its GPU-side bytes that
    /// have nothing to do with what is on the timeline: the switches,
    /// the working space, the display transfer, the monitor and compare
    /// modes. `fast_path_blockers_for` adds the layer analysis.
    fn display_path_blockers(&self, mode: FastPath) -> Vec<&'static str> {
        if !direct_present_enabled() {
            return vec!["KLIPP_DIRECT_PRESENT=0"];
        }
        // Steady-state playback, or a drag on the parked frame; any other
        // paused frame goes through `regrade_cached_frame`'s full render.
        // Checked first and alone so the sequence lock below is not taken
        // on every idle tick.
        if mode == FastPath::Playing && self.state != PlaybackState::Playing {
            return vec!["not playing"];
        }
        if mode == FastPath::ParkedDrag && (self.state != PlaybackState::Paused || !self.in_node_param_edit) {
            return vec!["not a slider drag on the parked frame"];
        }

        // Every remaining reason is collected rather than returned at the
        // first hit: finding out one run at a time which consumer opted
        // out is the slow way to learn why playback is not accelerating.
        let mut blockers: Vec<&'static str> = Vec::new();

        match mode {
            FastPath::ParkedDrag | FastPath::PausedPresent => {
                // What the grade tools read off the linear frame while paused.
                if self.node_sample.is_some() {
                    blockers.push("eyedropper read of a node's input");
                }
                if self.inspector_matte_preview_target.is_some() {
                    blockers.push("inspector matte preview");
                }
                if mode == FastPath::PausedPresent {
                    if self.in_node_param_edit {
                        blockers.push("node param edit in progress");
                    }
                    if !self.visible_node_thumbs.is_empty() && !self.thumb_capture_suspended {
                        blockers.push("node thumbnails wanted");
                    }
                }
            }
            FastPath::Playing => {
                // Compare modes, paused scrubbing and param drags read the
                // ungraded CPU frame.
                if self.pregrade_encode_needed() {
                    blockers.push("pre-grade CPU encode needed");
                }
                if self.in_node_param_edit {
                    blockers.push("node param edit in progress");
                }
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
        // The scopes measure the bytes a fast path puts on screen, so a
        // due refresh does not stop it — except under HDR monitoring,
        // whose bytes are a preview of the HDR levels the scopes show:
        // those frames need the linear picture. Deliberately "due" and
        // not "open": gating on the panel being visible turned the fast
        // path off for the entire session, which is exactly the case
        // colour work is done in.
        if self.should_render_scope() && self.hdr_monitor.renders_hdr() {
            blockers.push("scope refresh due this frame, and HDR levels need the linear frame");
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
        if self.compare_stabilization {
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
        // Switched on with no LUT loaded it applies nothing, and blocks
        // nothing (#1278).
        if self.color_pipeline.applies() {
            blockers.push("global colour pipeline active");
        }
        // Decoded bytes are sRGB; a BT.1886 viewer re-encodes them, which
        // the graded path does and a direct present does not.
        if self.sdr_transfer != klipp_core::SdrTransfer::Srgb {
            blockers.push("BT.1886 viewer transfer");
        }
        blockers
    }

    /// Whether the GPU stack may composite and encode this frame: the
    /// display-side blockers are empty (the working space, the
    /// transfer, the monitor, the compare modes) and the gate is on.
    /// What the layers are is the stack's own business; it declines
    /// what it cannot do and the CPU path takes over.
    /// Whether the GPU stack would show a playing frame without a
    /// readback: the viewer's texture is its to write, and the stack
    /// takes this project's picture (Rec.709 SDR). A scope pass no
    /// longer wants the bytes — the scopes measure the frame where the
    /// encode left it (#333) — so it does not send the frame to a path
    /// that reads it back. The stack itself still decides per frame;
    /// this is what makes it worth asking first.
    fn stack_presents_zero_copy(&self) -> bool {
        self.state == PlaybackState::Playing
            && should_publish_gpu_viewer()
            && !self.hdr_monitor.renders_hdr()
            && crate::video_decoder::working_colorspace() == klipp_core::WorkingColorspace::Rec709Linear
            && self.gpu_composite_allowed()
    }

    /// The viewer's texture for a `w`×`h` frame, when the GPU stack may
    /// write the frame there and skip the readback; see
    /// [`should_publish_gpu_viewer`].
    fn gpu_viewer_target(&mut self, w: u32, h: u32) -> Option<std::sync::Arc<wgpu::Texture>> {
        if !should_publish_gpu_viewer() {
            return None;
        }
        let pipeline = self.renderer.executor().gpu_pipeline_or_init()?;
        let (device, queue) = (pipeline.device(), pipeline.queue());
        self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).ensure_gpu_output(&device, &queue, w, h)
    }

    fn gpu_composite_allowed(&self) -> bool {
        if !klipp_compose::gpu_composite_enabled() {
            return false;
        }
        let mode = if self.state == PlaybackState::Playing { FastPath::Playing } else { FastPath::PausedPresent };
        self.display_path_blockers(mode).iter().all(|b| *b == "KLIPP_DIRECT_PRESENT=0")
    }

    /// Whether the GPU stack takes a slider step on the parked frame with
    /// titles over it (#2351): the paused present's blockers hold nothing
    /// but the drag itself. The parked regrade grades only the clip, so it
    /// laid the titles over from a CPU raster; the stack composites them
    /// on the GPU, as an export does, and reads the value being dragged
    /// from the sequence as the regrade does. A drag without titles keeps
    /// the regrade.
    fn titled_drag_on_stack(&self) -> bool {
        self.state == PlaybackState::Paused
            && self.in_node_param_edit
            && klipp_compose::gpu_composite_enabled()
            && self.display_path_blockers(FastPath::PausedPresent).iter().all(|b| matches!(*b, "KLIPP_DIRECT_PRESENT=0" | "node param edit in progress"))
    }

    /// Whether `seq` has title layers drawn plainly above the clip at
    /// `time`: what the fast paths lay over their bytes.
    pub(super) fn titles_over_clip(seq: &klipp_timeline::Sequence, time: RationalTime) -> bool {
        matches!(klipp_compose::active_layers(seq, time).as_slice(), [bottom, rest @ ..] if !bottom.transition && !rest.is_empty() && rest.iter().all(|l| Self::title_overlay_layer(seq, l)))
    }

    fn fast_path_blockers_for(&self, mode: FastPath) -> Vec<&'static str> {
        let mut blockers = self.display_path_blockers(mode);
        // Drawn by the GPU stack, which the fast paths are not (#2758): a
        // channel view is the stack's finish, and the A/B wipe of a node
        // the stack drawing the frame twice.
        if self.view_channel != klipp_core::ViewChannel::Picture {
            blockers.push("channel view");
        }
        if self.compare_effect_active {
            blockers.push("compare mode active");
        }
        // A node past the grade viewed: the stack draws the chain cut there
        // (#2879); the fast paths would show the whole chain.
        if let Some(at) = &self.solo {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            if at.path.root_clip().map(klipp_core::ClipId).is_some_and(|c| seq.grade_node_at(c, &at.path).is_err()) {
                blockers.push("node viewed past the grade");
            }
        }
        if blockers.iter().any(|b| matches!(*b, "KLIPP_DIRECT_PRESENT=0" | "not playing" | "not a slider drag on the parked frame")) {
            return blockers;
        }

        // The fast paths show the primary decoder's frame, so they are the
        // shared renderer's frame only when that renders exactly one clip,
        // the primary decoder's, on a fully opaque track: no other layer, no
        // transition, no other multicam angle. Asked of the renderer's own
        // stack, so the two cannot disagree about what is on screen.
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        // The timeline grade runs over the composite. The lean graded
        // present runs one clip's graph and nothing after it; the stack
        // present composites and so runs it.
        if klipp_timeline::Clip::timeline_grade_of(&seq).is_some() {
            blockers.push("timeline grade active");
        }
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
            // Titles drawn plainly above the primary ride the fast path
            // when the GPU stack cannot have them: their sprites are laid
            // over the presented bytes. The stack keeps the sprite on the
            // GPU and presents without a readback, so it takes them first.
            [bottom, rest @ ..] if !bottom.transition && rest.iter().all(|l| Self::title_overlay_layer(&seq, l)) => {
                if self.gpu_composite_allowed() || (mode == FastPath::ParkedDrag && self.titled_drag_on_stack()) {
                    blockers.push("title layers: the GPU stack");
                    return blockers;
                }
                match bottom.clips.as_slice() {
                    [clip] if Some(clip.id) == primary => (bottom, *clip),
                    _ => {
                        blockers.push("another clip on screen");
                        return blockers;
                    }
                }
            }
            _ => {
                blockers.push("overlay layers present");
                return blockers;
            }
        };
        if seq.tracks.get(layer.track).is_none_or(|t| (t.opacity - 1.0).abs() > 1e-6) {
            blockers.push("track opacity below 1.0");
        }
        // A moving sequence camera places the clip as surely as its own
        // keyframes do: the stack applies it, the plain present does not.
        if !seq.camera.is_still() && clip.depth > 0.0 {
            blockers.push("render keyframes on clip");
        }
        if clip.consumed_as_matte {
            blockers.push("clip is a matte source");
        }
        if !clip.grade_is_identity() {
            blockers.push("grade graph is not an identity");
        }
        // A tracked subject's matte is found from the clip's media, and a
        // matte sequence is read at the media's ends, by the renderer
        // (#1881); the lean graded present has neither.
        if reads_matte_sequence(clip) {
            blockers.push("matte sequence on clip");
        }
        if clip.has_enabled_plugins() {
            blockers.push("plugin effects on clip");
        }
        if clip.has_effect_entries() {
            blockers.push("builtin effects on clip");
        }
        if clip.has_param_modifiers() {
            blockers.push("param modifiers on clip");
        }
        if clip.has_render_keyframes()
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
            // The decoder's own sRGB picture.
            klipp_color::primaries::luminance_weights(klipp_core::WorkingColorspace::Rec709Linear),
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
                // With its corrections' mixes (M3 step 17a).
                let arc = Arc::new(clip.grade_to_run().into_owned());
                self.current_graph_arc = Some(Arc::clone(&arc));
                self.current_grade_clip_id = Some(clip.id);
                arc
            }
        };
        let (clip_time_secs, frame_number) = klipp_compose::render::grade_time(clip, self.playhead, seq.fps.as_f64());
        // The soloed node in the grade this runs, if it runs here.
        self.fast_solo = self.solo.as_ref().and_then(|at| seq.grade_node_at(clip.id, &at.path).ok());
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
        self.regrade_parked_on_gpu_for(FastPath::ParkedDrag)
    }

    /// [`Self::regrade_parked_on_gpu`] gated for `mode`: a slider drag, or
    /// a paused present of a graded clip — a seek, a scrub step, a skim —
    /// whose pre-grade the present has just captured. Through the renderer
    /// such a present cost a 1080p frame 41 ms, the grade on the CPU and
    /// the linear floats encoded after it; here it is the graph on the GPU
    /// and display bytes back.
    fn regrade_parked_on_gpu_for(&mut self, mode: FastPath) -> bool {
        let Some((pixels, width, height)) = self.parked_pregrade().cloned() else {
            debug!("gpu regrade ({mode:?}): no parked pre-grade");
            return false;
        };
        let mut blockers = self.fast_path_blockers_for(mode);
        blockers.extend(self.picture_shape_blocker((width, height)));
        // A slider step below a heavy node whose output the renderer holds
        // for this frame (#1863): the clip's effects all run at its source,
        // so its grade with its source node is the whole picture, and the
        // run takes the node's output held, or fails and the renderer draws
        // the frame. The key is the pre-grade's own frame, so a held output
        // is never another frame's.
        // The pre-grade is kept at the decoded picture's own size (the
        // frame the renderer reads is made from it at that size), so that
        // is the key's decoded size, whatever size the viewer shows.
        // The drag's steps and a paused present of the parked frame alike:
        // the release after a drag is one (#1939).
        let held = matches!(mode, FastPath::ParkedDrag | FastPath::PausedPresent).then(|| self.parked_held_source(&pixels, width, height)).flatten();
        if held.is_some() {
            blockers.retain(|b| *b != "builtin effects on clip");
        }
        if !Self::only_the_graded_path_handles(&blockers) {
            return false;
        }
        let t_frame = std::time::Instant::now();
        let Some((graph, clip_time_secs, frame_number, track_points)) = self.v1_graph_setup() else {
            debug!("gpu regrade ({mode:?}): no graph setup for the primary clip");
            return false;
        };
        let graph = match &held {
            Some(_) => {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let Some(clip) = seq.tracks.get(self.current_track_index).and_then(|t| t.clips.get(self.current_clip_index)) else { return false };
                Arc::new(klipp_compose::render::graph_with_source_nodes(clip, self.playhead, seq.fps.as_f64(), self.clip_media_fps(clip), &graph))
            }
            None => graph,
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
            solo: self.fast_solo,
            // Monitoring's preview. False colour is a different picture
            // and stays on the CPU, so `fast_path_blockers` refuses it.
            hdr_preview: self.hdr_monitor == klipp_core::HdrMonitor::Preview,
            // The same peak `view.rs` renders monitoring at.
            hdr_peak_nits: klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2,
        };
        let (tone_map, working, preview, transfer) = (self.tone_map_op, at.working, at.hdr_preview, self.sdr_transfer);
        // The frame the fallback gets is display light when monitoring is
        // on, so it takes the preview encode — the same substitution the
        // GPU branch makes.
        let display = move |linear: &[f32]| {
            if preview {
                klipp_color::hdr_view::preview_frame_dithered(linear, tone_map, working)
            } else {
                klipp_color::display::display_transform_ws_dithered_to(linear, tone_map, working, transfer)
            }
        };
        mark_stage(EngineStage::PostProcessInnerGradeExecute);
        // With a held source the run keys as the renderer's did: the same
        // source, and its picture's domain as the renderer gives it.
        let holding = held.is_some();
        if holding {
            let transfer = match &pixels.pixels {
                PregradePixels::Hdr(h) => Some(h.transfer),
                PregradePixels::Srgb8(_) => None,
            };
            self.renderer.executor().set_source_domain(klipp_compose::render::source_domain(transfer));
            self.renderer.executor().hold_outputs_for(held);
        }
        let graded = crate::fast_path::grade_to_display_on_gpu(self.renderer.executor(), seed, width, height, &at, &display);
        if holding {
            self.renderer.executor().hold_outputs_for(None);
            self.renderer.executor().set_source_domain(klipp_nodes::domains::PictureDomain::Display);
        }
        let Some(mut bytes) = graded else {
            debug!("gpu regrade ({mode:?}): the graph did not run on the GPU");
            return false;
        };
        let grade_ms = t_frame.elapsed().as_secs_f64() * 1000.0;
        // The scopes measure the bytes about to be shown, when due.
        let scope_ms = self.draw_due_scopes(DisplayScopeFrame::working(&bytes), width, height);
        let held_counts = self.renderer.executor().held_counts();
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, width, height);
            fb.set_presented(self.playhead, self.published_complete());
            fb.note(format!("graded on the GPU: {:?} {:?} complete {} (streams pending {}, progress {}) held outputs {} hit {} missed", self.state, self.playhead.value, self.published_complete(), self.pending_stream_refresh, self.presenting_progress, held_counts.hits, held_counts.misses));
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
                scope_ms: scope_ms as f32,
                composite_ms: 0.0,
                update_playback_ms: 0.0,
                in_param_edit: self.in_node_param_edit,
                phases: Default::default(),
                stills_kept: 0,
                stills_uploaded: 0,
                streams_gpu: 0,
                streams_software: 0,
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
            solo: self.fast_solo,
            // Monitoring's preview. False colour is a different picture
            // and stays on the CPU, so `fast_path_blockers` refuses it.
            hdr_preview: self.hdr_monitor == klipp_core::HdrMonitor::Preview,
            // The same peak `view.rs` renders monitoring at.
            hdr_peak_nits: klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2,
        };
        let (tone_map, working, preview, transfer) = (self.tone_map_op, at.working, at.hdr_preview, self.sdr_transfer);
        // The frame the fallback gets is display light when monitoring is
        // on, so it takes the preview encode — the same substitution the
        // GPU branch makes.
        let display = move |linear: &[f32]| {
            if preview {
                klipp_color::hdr_view::preview_frame_dithered(linear, tone_map, working)
            } else {
                klipp_color::display::display_transform_ws_dithered_to(linear, tone_map, working, transfer)
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
            // Encoded in the working space's primaries.
            klipp_color::primaries::luminance_weights(crate::video_decoder::working_colorspace()),
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
        scope_luma: [f32; 3],
    ) {
        self.overlay_titles_onto(&mut bytes, width, height);
        // The scopes measure the bytes about to be shown, when due.
        let scope_ms = self.draw_due_scopes(DisplayScopeFrame { rgba: &bytes, luma: scope_luma }, width, height);
        let t_fb = std::time::Instant::now();
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, width, height);
            fb.set_presented(self.playhead, self.published_complete());
            fb.note(format!("shown from the decoded frame: {:?} {:?} complete {} (streams pending {}, progress {})", self.state, self.playhead.value, self.published_complete(), self.pending_stream_refresh, self.presenting_progress));
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
                scope_ms: scope_ms as f32,
                composite_ms: 0.0,
                update_playback_ms: total_ms as f32,
                in_param_edit: self.in_node_param_edit,
                phases: Default::default(),
                stills_kept: 0,
                stills_uploaded: 0,
                streams_gpu: 0,
                streams_software: 0,
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

        self.report_playhead();
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
    /// The stabilisation compare and paused scrubbing still need it; the
    /// A/B wipe of a node draws both its halves on the GPU stack (#2758).
    /// The drag exception is deliberate: during a Playing param-drag with
    /// no compare mode active the encode runs at full resolution ~30
    /// times a second purely to refresh a scope preview the user is not
    /// looking at, so a slightly stale preview buys back the frame budget.
    fn pregrade_encode_needed(&self) -> bool {
        let drag_skip = drag_downsample_enabled()
            && self.in_node_param_edit
            && self.state == PlaybackState::Playing
            && !self.compare_stabilization
            && !self.bypass_color_grade;
        (self.state != PlaybackState::Playing
            || self.compare_stabilization
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

    /// The pregrade of the frame at the playhead, if that is what the
    /// cached one is.
    /// The parked frame's source as the renderer keyed its held outputs
    /// by (#1863), for the GPU path to take them: the frame `pregrade`
    /// shows (its own pts, decoded at `width × height`), its files, and
    /// the primary clip's retime there. `None` unless parked with the
    /// hold on, and the clip's effects all run at its source.
    fn parked_held_source(&self, pregrade: &Pregrade, width: u32, height: u32) -> Option<klipp_nodes::held::HeldSource> {
        if self.state == PlaybackState::Playing || !self.hold_neighbours {
            return None;
        }
        let pts = pregrade.pts?;
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let clip = seq.tracks.get(self.current_track_index)?.clips.get(self.current_clip_index)?;
        let fps = seq.fps.as_f64();
        if clip.has_enabled_plugins() || !klipp_compose::render::only_source_boundary_effects(clip, self.playhead, fps) {
            return None;
        }
        let (file, media_fps) = {
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            (view::held_file_of(&pool, clip, self.use_proxies)?, pool.get(&clip.media_id()?).map(|m| m.fps.as_f64()))
        };
        let (clip_time, _) = klipp_compose::render::grade_time(clip, self.playhead, fps);
        let at = klipp_compose::render::source_secs(clip, self.playhead, fps);
        Some(klipp_compose::render::held_source_of(clip, clip_time, at, pts, (width, height), file, media_fps))
    }

    fn retain_primary(&mut self, primary: &streams::HeldFrame) {
        if self.presenting_progress {
            return;
        }
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        self.retained_primary = Some((self.playhead, under_playhead_fingerprint(&seq, self.playhead), primary.clone()));
    }

    fn retained_primary_at_playhead(&self) -> Option<streams::HeldFrame> {
        let (at, layout, frame) = self.retained_primary.as_ref()?;
        if *at != self.playhead || self.pending_paused_refresh {
            return None;
        }
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        (*layout == under_playhead_fingerprint(&seq, self.playhead)).then(|| frame.clone())
    }

    fn parked_pregrade(&self) -> Option<&(Pregrade, u32, u32)> {
        if self.cached_pregrade_at == Some(self.playhead) {
            self.cached_pregrade_frame.as_ref()
        } else {
            None
        }
    }

    /// Whether the primary clip frame-blends at the playhead.
    fn parked_clip_frame_blends(&self) -> bool {
        let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
        let fps = seq.fps.as_f64();
        seq.tracks
            .get(self.current_track_index)
            .and_then(|t| t.clips.get(self.current_clip_index))
            .is_some_and(|clip| klipp_compose::frame_blends(clip, self.playhead, self.clip_media_fps(clip), fps))
    }

    /// The frame rate of `clip`'s media in the pool, as the viewer's
    /// source gives the renderer.
    fn clip_media_fps(&self, clip: &klipp_timeline::Clip) -> Option<f64> {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        Some(pool.get(&clip.media_id()?)?.fps.as_f64())
    }

    /// Look at the timeline's layout, and if it changed since the last
    /// look, empty what was cached under the old one. Whether what is
    /// under the playhead changed with it — the parked frame is then
    /// another picture's. `blocking` waits for the sequence; otherwise a
    /// busy lock means "look next time".
    fn note_layout_change(&mut self, blocking: bool) -> bool {
        let (layout, under) = {
            let seq = if blocking {
                self.sequence.read().unwrap_or_else(|e| e.into_inner())
            } else {
                match self.sequence.try_read() {
                    Ok(seq) => seq,
                    Err(_) => return false,
                }
            };
            (timeline_layout_fingerprint(&seq), under_playhead_fingerprint(&seq, self.playhead))
        };
        if layout == self.layout_fingerprint {
            return false;
        }
        self.layout_fingerprint = layout;
        self.frame_cache.clear();
        self.titles_warmed.clear();
        let before = std::mem::replace(&mut self.under_playhead_fingerprint, (self.playhead, under));
        if before == (self.playhead, under) {
            return false;
        }
        self.cached_pregrade_frame = None;
        self.pregrade_source = None;
        self.cached_pregrade_at = None;
        self.retained_primary = None;
        true
    }

    /// An edit changed what is under the parked playhead, whether or not
    /// anyone said so: the UI writes many edits to the sequence and sends
    /// nothing after them. Looked for every tick the engine is not
    /// playing (playback follows the timeline as it goes).
    fn tick_layout_check(&mut self) {
        if self.state != PlaybackState::Playing && self.note_layout_change(false) {
            self.show_edit_at_playhead();
        }
    }

    /// Show what is under the playhead now, after an edit: seek there
    /// again, so the primary decoder is the clip's and the frame is this
    /// timeline's. From Idle or Stopped this parks the engine, as a seek
    /// from the UI does, so the edits after it render too.
    fn show_edit_at_playhead(&mut self) {
        let ph = self.playhead;
        self.quiet_seek = true;
        self.seek_in_sequence(ph);
        self.quiet_seek = false;
        if matches!(self.state, PlaybackState::Idle | PlaybackState::Stopped)
            && (self.v1_decoder.is_some() || !self.audio_sources.is_empty())
        {
            self.state = PlaybackState::Paused;
            let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        }
    }

    /// Read the picture `node_id` receives at `(nx, ny)` of the parked
    /// frame, for the qualifier eyedropper (#587). The frame on screen is
    /// the grade's result, the key's own correction in it after a first
    /// pick; the node reads what the node before it made. That node's
    /// output comes back from one render with the scope tap on it, at
    /// the frame's size and in the clip's own layout, which is the
    /// screen's only while the clip sits unmoved in the frame.
    /// Read the input of the node at `at` (#1428): `clip_id` and `node_id`
    /// are the asker's names for it, given back with the answer. A clip's
    /// grade node is read from that clip's render; the timeline grade's
    /// from its proxy's, which grades the composite; a grade group stage's
    /// from the render of the group's topmost member on the frame, whose
    /// grade runs the stage; a track grade's from the render of the
    /// track's picture clip on the frame (#1655). The render that answers inspects that clip
    /// and taps the node feeding the asked one (`show_inspection`).
    fn sample_node_input(&mut self, clip_id: klipp_core::ClipId, node_id: klipp_nodes::NodeId, at: &klipp_graph_format::path::NodePath, nx: f32, ny: f32) {
        use klipp_graph_format::path::{NodePath, Root};
        let answer = |this: &Self, sample| {
            let _ = this.event_tx.send(AppEvent::NodeInputSampled { clip_id, node_id, nx, ny, sample });
        };
        fn why<T>(s: &str) -> Result<T, klipp_core::NodeSampleRefusal> {
            Err(klipp_core::NodeSampleRefusal::Why(s.to_string()))
        }
        if self.state == PlaybackState::Playing {
            return answer(self, why("pause playback first — it reads the parked frame"));
        }
        let Some(&node) = at.nodes.last() else {
            return answer(self, why("an empty path names no node"));
        };
        let found = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            // The clip whose render answers, after the frame checks a clip
            // of the timeline must pass to be read point for point.
            let on_frame = |clip: &klipp_timeline::Clip| -> Result<(), klipp_core::NodeSampleRefusal> {
                if self.playhead < clip.timeline_in || self.playhead >= clip.timeline_out {
                    return why("the frame on screen is not of this clip — park the playhead on it first");
                }
                if !sits_unmoved(clip, &seq, self.playhead) {
                    return Err(klipp_core::NodeSampleRefusal::Placed);
                }
                Ok(())
            };
            match at.root {
                Root::Clip { clip } => {
                    let clip = klipp_core::ClipId(clip);
                    match seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip) {
                        None => why("the clip is no longer on the timeline"),
                        Some(c) => on_frame(c).and_then(|()| {
                            c.grade().input_of(node, 0).map(|tap| (clip, klipp_compose::inspect::grade_node(clip, tap))).ok_or_else(|| klipp_core::NodeSampleRefusal::Why("the node has no input to read".into()))
                        }),
                    }
                }
                // The timeline grade grades the composite: no clip's place
                // to check, a point of the frame is a point of its input.
                Root::TimelineGrade { sequence } => match seq.timeline_grade() {
                    None => why("the sequence has no timeline grade"),
                    Some(grade) => grade
                        .input_of(node, 0)
                        .map(|tap| (seq.timeline_grade_clip_id(), klipp_compose::inspect::InspectAt::out(NodePath::new(Root::TimelineGrade { sequence }, [tap]))))
                        .ok_or_else(|| klipp_core::NodeSampleRefusal::Why("the node has no input to read".into())),
                },
                Root::GroupStage { group, stage } => {
                    let grade_stage = if stage == klipp_graph_format::lifted::Stage::Pre { klipp_timeline::GradeStage::Pre } else { klipp_timeline::GradeStage::Post };
                    let tap = seq.grade_group(klipp_core::GradeGroupId(group)).and_then(|g| g.stage(grade_stage)).and_then(|g| g.input_of(node, 0));
                    // The group's topmost member on the frame: the one the
                    // viewer shows at the point, its grade running the stage.
                    let member = seq.tracks.iter().rev().flat_map(|t| t.clips.iter()).find(|c| c.grade_group.map(|g| g.0) == Some(group) && self.playhead >= c.timeline_in && self.playhead < c.timeline_out);
                    match (tap, member) {
                        (None, _) => why("the node has no input to read"),
                        (_, None) => why("no clip of the group is on this frame — park the playhead on one first"),
                        (Some(tap), Some(m)) => on_frame(m).map(|()| (m.id, klipp_compose::inspect::InspectAt::out(NodePath::new(Root::GroupStage { group, stage }, [tap])))),
                    }
                }
                // A track's grade runs after the grade of each picture clip
                // of the track (#1655): read from the render of the one on
                // the frame, its node's input as that clip runs it.
                Root::Track { track, part } => match seq.tracks.iter().find(|t| t.id.0 == track) {
                    None => why("the track is no longer in the sequence"),
                    Some(_) if part != klipp_graph_format::path::TrackPart::Grade => why("a track's effect list has no node input to read: pick on a node of its grade"),
                    Some(t) => {
                        let tap = t.effects.track_grade().and_then(|g| g.input_of(node, 0));
                        let member = t.clips.iter().find(|c| self.playhead >= c.timeline_in && self.playhead < c.timeline_out && t.effects.grade_onto(t.kind, c).is_some());
                        match (tap, member) {
                            (None, _) => why("the node has no input to read"),
                            (_, None) => why("no picture clip of the track is on this frame — park the playhead on one first"),
                            (Some(tap), Some(m)) => on_frame(m).map(|()| (m.id, klipp_compose::inspect::InspectAt::out(NodePath::new(Root::Track { track, part }, [tap])))),
                        }
                    }
                },
            }
        };
        let (inspected, tap) = match found {
            Ok(found) => found,
            Err(refusal) => return answer(self, Err(refusal)),
        };
        self.inspect_clip = Some(inspected);
        self.node_sample = Some(NodeSample { clip: clip_id, node: node_id, tap, nx, ny });
        self.node_sample_since = None;
        self.regrade_cached_frame();
        // Answered by the render (`show_inspection`), unless it drew
        // without that grade. A pick that came before the parked frame did
        // (a seek still decoding, a paused refresh still to drain) waits
        // for it: the render that paints the frame answers, or the tick
        // once it is parked. Refused there, a pick sent right after a
        // seek on a loaded machine failed for want of the frame.
        if self.node_sample.is_none() {
            return;
        }
        if self.frame_owed() {
            self.node_sample_since = Some(std::time::Instant::now());
            return;
        }
        // The frame is here and the render didn't answer: once more.
        self.regrade_cached_frame();
        if self.node_sample.take().is_some() {
            answer(self, why("the node's picture could not be read on this frame"));
        }
    }

    /// Whether the parked frame is still coming: no pre-grade of it yet,
    /// and a decoder or a paused refresh that will bring it.
    fn frame_owed(&self) -> bool {
        self.parked_pregrade().is_none() && (self.v1_decoder.is_some() || self.pending_paused_refresh)
    }

    /// How long a pick waits for its frame before it is refused.
    const NODE_SAMPLE_WAIT: Duration = Duration::from_secs(20);

    /// A pick waiting for its frame (`sample_node_input`): once the frame
    /// is parked and nothing more is coming, the frame is rendered with
    /// the pick's tap, which answers; past [`Self::NODE_SAMPLE_WAIT`] it is
    /// refused.
    fn tick_node_sample(&mut self) {
        let (Some(since), Some(s)) = (self.node_sample_since, self.node_sample.as_ref()) else { return };
        let (clip_id, node_id, nx, ny) = (s.clip, s.node, s.nx, s.ny);
        if !self.frame_owed() && !self.pending_paused_refresh {
            self.regrade_cached_frame();
        }
        let refuse = |this: &mut Self, why: &str| {
            this.node_sample = None;
            this.node_sample_since = None;
            let _ = this.event_tx.send(AppEvent::NodeInputSampled { clip_id, node_id, nx, ny, sample: Err(klipp_core::NodeSampleRefusal::Why(why.to_string())) });
        };
        if self.node_sample.is_none() {
            self.node_sample_since = None;
        } else if !self.frame_owed() && !self.pending_paused_refresh {
            refuse(self, "the node's picture could not be read on this frame");
        } else if since.elapsed() >= Self::NODE_SAMPLE_WAIT {
            refuse(self, "the frame did not arrive to be read");
        }
    }

    /// Note `line` in the viewer's trail ([`crate::texture::FrameBuffer::note`]).
    fn note_view(&self, line: String) {
        self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).note(line);
    }

    fn regrade_cached_frame(&mut self) {
        mark_stage(EngineStage::PausedRegrade);
        // A slider step on a frame the renderer would only grade.
        if self.in_node_param_edit && self.regrade_parked_on_gpu() {
            return;
        }
        // A frame that blends with the next one (a slowed speed ramp)
        // cannot be regraded from the parked pregrade alone: the partner
        // is not in it, and the blend was silently dropped. Decode it
        // again, the way the first render did.
        if self.parked_clip_frame_blends() {
            let ph = self.playhead;
            self.seek_in_sequence(ph);
            return;
        }
        let primary = self.retained_primary_at_playhead().or_else(|| self
            .parked_pregrade()
            .map(|(pixels, w, h)| streams::HeldFrame::from_pregrade(pixels, *w, *h)));
        // A primary decoder whose first frame has not arrived yet: nothing
        // to show until it does (the paused refresh paints it).
        if primary.is_none() && self.v1_decoder.is_some() {
            self.note_view(format!("regrade skipped at {:?}: no parked pre-grade, the decoder still owes it (paused refresh pending {})", self.playhead.value, self.pending_paused_refresh));
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

    /// Whether the scopes are due: they are visible, and long enough has
    /// passed since they were last drawn. The scopes are paced by their
    /// own cost so that they never take more than a third of the engine's
    /// time: a pass that cost 4 ms is due every frame, one that cost
    /// 15 ms every 30 ms, and none waits longer than `SCOPE_MAX_INTERVAL`.
    fn should_render_scope(&self) -> bool {
        self.scopes_visible && self.last_scope_render.elapsed() >= self.scope_interval()
    }

    /// How long after one scope pass the next may start: twice what the
    /// last one cost, so the scopes keep to a third of the engine —
    /// bounded by [`SCOPE_MAX_INTERVAL`] so they cannot go unboundedly
    /// stale if a pass is pathological.
    fn scope_interval(&self) -> Duration {
        (self.last_scope_cost * 2).min(SCOPE_MAX_INTERVAL)
    }


    /// Draw the scopes from `linear` or `display` (see
    /// [`Self::render_active_scopes`]), note when and what it cost, and
    /// return the cost in ms. What was owed is paid.
    fn draw_scopes(&mut self, linear: Option<&[f32]>, display: Option<DisplayScopeFrame<'_>>, w: u32, h: u32) -> f64 {
        let t = std::time::Instant::now();
        self.render_active_scopes(linear, display, w, h);
        self.last_scope_cost = t.elapsed();
        self.last_scope_render = std::time::Instant::now();
        self.scope_render_owed = false;
        self.last_scope_cost.as_secs_f64() * 1000.0
    }

    /// The scopes from the display bytes a fast path is about to show, if
    /// they are due; otherwise they are owed. Not under HDR monitoring:
    /// its bytes are a preview of the levels the scopes show, so those
    /// frames leave the fast path when the scopes are due (see
    /// [`Self::fast_path_blockers_for`]). The cost in ms, 0 when not drawn.
    fn draw_due_scopes(&mut self, display: DisplayScopeFrame<'_>, w: u32, h: u32) -> f64 {
        if self.should_render_scope() && !self.hdr_monitor.renders_hdr() {
            self.draw_scopes(None, Some(display), w, h)
        } else {
            self.scope_render_owed |= self.scopes_visible;
            0.0
        }
    }

    /// [`Self::draw_due_scopes`] for a frame the GPU stack encoded for
    /// the screen and kept on the device: measured where the encode left
    /// it, with no readback. Call it straight after that encode, before
    /// anything encodes another frame. Its bytes are in the working
    /// space's primaries, as the stack encodes them. The cost in ms, 0
    /// when not drawn.
    fn draw_due_scopes_from_gpu_display(&mut self, w: u32, h: u32) -> f64 {
        if !(self.should_render_scope() && !self.hdr_monitor.renders_hdr()) {
            self.scope_render_owed |= self.scopes_visible;
            return 0.0;
        }
        let t = std::time::Instant::now();
        let modes = self.scope_modes_to_draw();
        let luma = klipp_color::primaries::luminance_weights(crate::video_decoder::working_colorspace());
        let step = self.scope_sample_step();
        let jobs = self.scope_jobs(&modes);
        let low_pass = self.scope_low_pass_enabled;
        let scope_lut = self.scope_lut.clone();
        let renders: Vec<(u8, Option<(Vec<u8>, u32, u32)>)> = match self
            .renderer
            .executor()
            .gpu_pipeline_or_init()
            .and_then(|gpu| {
                gpu.scopes().set_low_pass(low_pass);
                gpu.scopes().set_lut(scope_lut.as_ref().map(|l| (l.key, l.size, l.table.as_slice())));
                gpu.render_scopes_from_display(w, h, luma, step, &jobs)
            })
        {
            Some(results) => modes
                .iter()
                .zip(jobs.iter().zip(results))
                .map(|(&mode, (job, r))| (mode, r.map(|bytes| (bytes, job.out_size().0, job.out_size().1))))
                .collect(),
            None => Vec::new(),
        };
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            Self::apply_scope_renders(&mut fb, renders);
        }
        self.last_scope_cost = t.elapsed();
        self.last_scope_render = std::time::Instant::now();
        self.scope_render_owed = false;
        self.last_scope_cost.as_secs_f64() * 1000.0
    }

    /// Draw every mode in `(active_scope_modes ∪ {scope_render_mode})` on
    /// the GPU and return per-mode RGBA outputs. Does NOT touch the
    /// framebuffer; pair with [`Self::apply_scope_renders`] to write the
    /// results, so the playback path shares one `frame_buffer.lock()`
    /// with `write_frame`.
    ///
    /// A `None` entry means the mode was not drawn — the caller leaves
    /// the previous texture in place instead of clearing.
    ///
    /// The scopes are drawn from `display` when it is given (the bytes on
    /// screen, for a working space whose display transform has no GPU
    /// form) and from `linear` otherwise.
    fn render_active_scopes_pending(
        &mut self,
        linear: Option<&[f32]>,
        display: Option<DisplayScopeFrame<'_>>,
        src_w: u32,
        src_h: u32,
    ) -> Vec<(u8, Option<(Vec<u8>, u32, u32)>)> {
        // Union: always include the main-dock mode so its singular-field
        // back-compat readers (in-tree dock UI) never see a stale
        // texture, even when the floating window's active set excludes
        // the dock mode (e.g. four parade panes while dock shows
        // vectorscope).
        let wanted = self.scope_modes_to_draw();
        let mut rendered: Vec<(u8, Option<(Vec<u8>, u32, u32)>)> = Vec::with_capacity(wanted.len());
        if !wanted.is_empty() {
            if let Some(results) = self.render_scopes_on_gpu(linear, display, src_w, src_h, &wanted) {
                rendered.extend(wanted.iter().copied().zip(results));
            }
        }
        // A mode the GPU did not draw (no GPU, or nothing to measure) keeps
        // the texture it had.
        rendered
    }

    /// The scope modes a pass draws: every active one and the dock's,
    /// waveform (0), parade (1), vectorscope (2) and histogram (3). The
    /// CIE scope is in the active set but asks for sample data instead;
    /// see `cie_samples_pending`.
    fn scope_modes_to_draw(&self) -> Vec<u8> {
        let mut modes: std::collections::BTreeSet<u8> = self.active_scope_modes.clone();
        modes.insert(self.scope_render_mode);
        modes.into_iter().filter(|m| *m <= 3).collect()
    }

    /// Paused, a frame is studied rather than watched: every pixel is
    /// measured. The quality setting paces the scopes during playback.
    fn scope_sample_step(&self) -> u32 {
        if self.state == PlaybackState::Playing { self.scope_settings.quality.sample_step() as u32 } else { 1 }
    }

    /// One GPU scope job per mode, at the size the UI shows it.
    fn scope_jobs(&self, modes: &[u8]) -> Vec<klipp_effects::gpu_scopes::ScopeJob> {
        use klipp_effects::gpu_scopes::{HistogramJob, ScopeJob, VectorscopeJob, WaveformJob, WaveformMode};
        let settings = self.scope_settings;
        let glow = self.scope_bloom_enabled;
        modes
            .iter()
            .map(|&mode| {
                let (out_w, out_h) = crate::scopes::scope_out_size(&self.scope_sizes, mode);
                if mode == 2 {
                    let v = settings.vectorscope;
                    return ScopeJob::Vectorscope(VectorscopeJob {
                        zoom: v.zoom,
                        intensity: settings.waveform.intensity,
                        colorize: v.colour,
                        luma_lo: v.luma_lo,
                        luma_hi: v.luma_hi,
                        glow,
                        out_w,
                        out_h,
                    });
                }
                if mode == 3 {
                    let h = settings.histogram;
                    return ScopeJob::Histogram(HistogramJob {
                        layout: crate::scopes::histogram_layout(h.display_mode),
                        log: h.levels == klipp_core::HistogramLevels::Logarithmic,
                        fill: h.fill,
                        glow,
                        out_w,
                        out_h,
                    });
                }
                let wp = if mode == 1 { settings.parade } else { settings.waveform };
                let layout = match (settings.luma_only, mode) {
                    (true, _) => WaveformMode::Luma,
                    (false, 1) => WaveformMode::Parade,
                    _ => WaveformMode::RgbOverlay,
                };
                ScopeJob::Waveform(WaveformJob {
                    mode: layout,
                    intensity: wp.intensity,
                    channels: wp.channels.to_components() as u32,
                    envelope: wp.envelope,
                    glow,
                    out_w,
                    out_h,
                })
            })
            .collect()
    }

    /// The waveform (mode 0), parade (1), vectorscope (2) and histogram
    /// (3) from the GPU scope renderer, one result with its size per entry
    /// of `modes`.
    /// `None` when there is no GPU or nothing to measure.
    ///
    /// The picture is measured as it is delivered: the linear frame is
    /// encoded with the viewer's SDR transfer in the working space's own
    /// primaries, or with PQ in BT.2020 while HDR monitoring shows
    /// display light; display bytes are binned as the codes they are. Each
    /// is drawn at the physical size the UI shows it at.
    fn render_scopes_on_gpu(
        &mut self,
        linear: Option<&[f32]>,
        display: Option<DisplayScopeFrame<'_>>,
        src_w: u32,
        src_h: u32,
        modes: &[u8],
    ) -> Option<Vec<Option<(Vec<u8>, u32, u32)>>> {
        use klipp_effects::gpu_scopes::{ScopeInput, ScopeJob, ScopeTransfer};
        let ws = crate::video_decoder::working_colorspace();
        let hdr = self.hdr_monitor.renders_hdr();
        let sdr_transfer = self.sdr_transfer;
        let input = match (display, linear) {
            (Some(d), _) => ScopeInput::Display {
                rgba: d.rgba,
                width: src_w,
                height: src_h,
                luma: d.luma,
            },
            (None, Some(rgba)) if hdr => ScopeInput::Linear {
                rgba,
                width: src_w,
                height: src_h,
                matrix: klipp_color::primaries::working_to_bt2020(ws),
                luma: klipp_color::primaries::luminance_weights(klipp_core::WorkingColorspace::Bt2020Linear),
                transfer: ScopeTransfer::Pq,
                scale: klipp_color::hdr::HDR_REFERENCE_WHITE_NITS / 10_000.0,
            },
            (None, Some(rgba)) => ScopeInput::Linear {
                rgba,
                width: src_w,
                height: src_h,
                matrix: klipp_color::primaries::IDENTITY,
                luma: klipp_color::primaries::luminance_weights(ws),
                transfer: match sdr_transfer {
                    klipp_core::SdrTransfer::Srgb => ScopeTransfer::Srgb,
                    klipp_core::SdrTransfer::Bt1886 => ScopeTransfer::Bt1886,
                },
                scale: 1.0,
            },
            (None, None) => return None,
        };
        let jobs = self.scope_jobs(modes);
        let step = self.scope_sample_step();
        let low_pass = self.scope_low_pass_enabled;
        // The viewing LUT is an SDR display LUT: not under HDR monitoring.
        let scope_lut = self.scope_lut.clone().filter(|_| !hdr);
        let gpu = self.renderer.executor().gpu_pipeline_or_init()?;
        let sizes: Vec<(u32, u32)> = jobs.iter().map(ScopeJob::out_size).collect();
        gpu.scopes().set_low_pass(low_pass);
        gpu.scopes().set_lut(scope_lut.as_ref().map(|l| (l.key, l.size, l.table.as_slice())));
        let results = gpu.scopes().render(&input, step, &jobs);
        Some(results.into_iter().zip(sizes).map(|(r, (w, h))| r.map(|bytes| (bytes, w, h))).collect())
    }

    /// Apply the output of [`Self::render_active_scopes_pending`] to a
    /// `FrameBuffer` already held by the caller: one `scope_textures`
    /// entry per drawn mode.
    fn apply_scope_renders(
        fb: &mut crate::texture::FrameBuffer,
        renders: Vec<(u8, Option<(Vec<u8>, u32, u32)>)>,
    ) {
        for (mode, tex_opt) in renders.into_iter() {
            match tex_opt {
                Some((bytes, scope_w, scope_h)) => {
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
                    // Leave the existing entry alone — the UI keeps the
                    // previous frame.
                }
            }
        }
    }

    /// Combined render + apply for sites that don't need to share the
    /// FB lock with another write. Acquires the lock exactly once.
    fn render_active_scopes(
        &mut self,
        linear: Option<&[f32]>,
        display: Option<DisplayScopeFrame<'_>>,
        src_w: u32,
        src_h: u32,
    ) {
        let renders = self.render_active_scopes_pending(linear, display, src_w, src_h);
        // The chromaticity scope wants the linear picture; a display-only
        // frame leaves its previous samples in place.
        let Some(linear_rgba) = linear else {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            Self::apply_scope_renders(&mut fb, renders);
            return;
        };
        let cie = self.cie_samples_pending(linear_rgba, src_w, src_h);
        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        Self::apply_scope_renders(&mut fb, renders);
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
            .contains(&crate::scopes::CIE_SAMPLES_MODE)
        {
            return None;
        }
        if src_w == 0 || src_h == 0 || linear_rgba.len() < (src_w * src_h * 4) as usize {
            return None;
        }
        let edge = crate::scopes::CIE_SAMPLES_MAX_EDGE;
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
    /// Hold until the decoder has a frame near `target_secs` at the
    /// front of its queue, up to a second and a half: the clock and the
    /// sound start together with the picture, instead of running ahead
    /// while a 4K group of pictures decodes and the video then jumping
    /// to catch up.
    fn wait_for_valid_frame(&self, target_secs: f64) {
        let Some(dec) = &self.v1_decoder else { return };
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(1500);
        loop {
            {
                let mut queue = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                // Drain frames that are far from the seek target
                while let Some(front) = queue.front() {
                    let pts = front.pts_secs();
                    if (pts - target_secs).abs() < 2.0 || front.is_eof {
                        // Frame is close enough to target — good to go
                        let waited = started.elapsed();
                        if waited > Duration::from_millis(100) {
                            info!("play waited {:.0} ms for the first frame near {target_secs:.3}s", waited.as_secs_f64() * 1000.0);
                        }
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

    /// The pixels a second decoding `media_id` from `path` asks for, as a
    /// stream opening is counted against the GPU's decoder (#2116): its
    /// size times its rate. A proxy is counted as 1080p, as it is made no
    /// larger; the decoder corrects it once it reads the file's size.
    /// What a stream of `media_id` read from `path` asks for (#2128): its
    /// decode rate, and the bytes of one frame at the size it plays at,
    /// 8 bytes a pixel for HDR (decoded at 16 bits a channel), else 4.
    fn media_cost(&self, media_id: MediaId, path: &std::path::Path) -> streams::Cost {
        let rate = self.media_pixel_rate(media_id, path);
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        let (w, h, deep) = match pool.get(&media_id) {
            Some(item) if item.path == path => (item.resolution.width, item.resolution.height, matches!(item.colour_space, klipp_media::ColourSpace::Hdr10 | klipp_media::ColourSpace::Hlg)),
            _ => (1920, 1080, false),
        };
        let (pw, ph) = crate::video_decoder::playback_dimensions(w.max(1), h.max(1), 0);
        // What it reads: the item's probed rates for its own file; a proxy
        // or skim file's size over the item's length.
        let read_bytes = match pool.get(&media_id) {
            Some(item) if item.path == path => crate::disk_budget::read_bytes_per_sec(item.video_bitrate_kbps.unwrap_or(0), item.audio_bitrate_kbps.unwrap_or(0), item.file_size_bytes, item.duration.to_secs_f64()),
            Some(item) => crate::disk_budget::read_bytes_per_sec(0, 0, std::fs::metadata(path).map(|m| m.len()).unwrap_or(0), item.duration.to_secs_f64()),
            None => 0.0,
        };
        streams::Cost { rate, frame_bytes: pw as f64 * ph as f64 * if deep { 8.0 } else { 4.0 }, read_bytes, drive: Some(crate::disk_budget::drive_of(path)) }
    }

    fn media_pixel_rate(&self, media_id: MediaId, path: &std::path::Path) -> f64 {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        let Some(item) = pool.get(&media_id) else { return 0.0 };
        let fps = item.fps.as_f64().max(1.0);
        let (w, h) = if item.path == path { (item.resolution.width, item.resolution.height) } else { (1920, 1080) };
        w as f64 * h as f64 * fps
    }

    fn path_for_media(&self, media_id: MediaId) -> Option<PathBuf> {
        self.resolve_media_path(media_id, true)
    }

    /// [`Self::path_for_media`], and with `log` false without saying so:
    /// for asking which file a clip would read, every seek, rather than
    /// opening it.
    fn resolve_media_path(&self, media_id: MediaId, log: bool) -> Option<PathBuf> {
        self.media_pool
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&media_id)
            .map(|item| {
                // 1. Proxy files (explicit user choice, highest priority)
                if self.use_proxies {
                    if let Some(ref proxy_path) = item.proxy_path {
                        if item.proxy_ready && proxy_path.exists() {
                            if log {
                                tracing::trace!("resolve_media_path: using proxy for {}", item.filename);
                            }
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
                        if log {
                            tracing::info!(
                                "resolve_media_path: using skim ({} {}x{}@{:.2} fps) for {}",
                                match mode { 1 => "AlwaysOriginal", 2 => "AlwaysOptimized", _ => "Auto" },
                                item.resolution.width, item.resolution.height,
                                item.fps.as_f64(), item.filename,
                            );
                        }
                        return skim_path.clone();
                    }
                }
                // 3. Original source file.
                item.path.clone()
            })
    }

    fn load_file(&mut self, path: PathBuf, media_id: MediaId) {
        info!("Loading media: {:?}", path);

        match klipp_media::open_input(&path) {
            Ok(format_ctx) => {
                let duration_secs = format_ctx.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

                let mut video_w = 1920u32;
                let mut video_h = 1080u32;
                let mut container_fps = Fps::film_24();
                let mut video_codec = String::new();
                let mut audio_codec = String::new();
                let mut has_audio = false;
                let mut sample_rate: Option<u32> = None;
                let mut audio_channels: Option<u16> = None;
                let mut colour_space: u8 = 0; // 0=Unknown, 1=SDR, 2=HDR10, 3=HLG
                let mut dolby_vision_profile = None;

                // The file's picture: the stream the decoders will open. An
                // MP4 with embedded cover art has a second video stream, one
                // MJPEG frame marked as an attached picture; read in stream
                // order, it came last and the item took its codec and size.
                let picture_stream = picture_stream_index(&format_ctx);
                for stream in format_ctx.streams() {
                    let params = stream.parameters();
                    let medium = params.medium();
                    match medium {
                        ffmpeg_next::media::Type::Video => {
                            if Some(stream.index()) != picture_stream {
                                continue;
                            }
                            dolby_vision_profile = klipp_media::dolby_vision::stream_profile(&stream);
                            if let Some(warning) = klipp_media::dolby_vision::warning(dolby_vision_profile) {
                                warn!("{:?}: {warning}", path.file_name().unwrap_or_default());
                            }
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
                            let r = stream.rate();
                            let avg = stream.avg_frame_rate();
                            if let Some(fps) = fps_from_stream_rates((r.0, r.1), (avg.0, avg.1)) {
                                container_fps = fps;
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

                // A copy cut short keeps its header's duration while its
                // media data ends early: the clip is as long as the file
                // holds, and the header's claim is kept to say so.
                let truncated_from_secs = if !is_still && duration_secs > 5.0 {
                    truncated_end_secs(&path, duration_secs).map(|end| (end, duration_secs))
                } else {
                    None
                };
                let duration_secs = truncated_from_secs.map_or(duration_secs, |(end, _)| end);
                let truncated_from_secs = truncated_from_secs.map(|(_, header)| header);
                if let Some(header) = truncated_from_secs {
                    warn!("{:?} holds {duration_secs:.1} s of the {header:.1} s its header claims: the file is cut short", path.file_name().unwrap_or_default());
                }

                // A probe, and nothing else. It used to take over the
                // playback cursor as well — the engine's frame rate, the
                // playhead, the clip index and the file it believed its
                // decoder held — left over from when an import was shown
                // in the viewer. An import while parked then had the next
                // edit drawn from the wrong decoder.
                let fps = container_fps;
                let duration = probed_duration(duration_secs, picture_stream.is_some().then_some(fps), sample_rate);

                let fs_meta = std::fs::metadata(&path).ok();
                let file_size_bytes = fs_meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let file_modified_at = fs_meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());

                // Try to extract creation_time from container metadata (camera shoot date)
                let mut media_created_at = format_ctx.metadata().get("creation_time")
                    .and_then(parse_iso8601_timestamp);

                // Extract extended metadata from container and stream tags
                let fmt_meta = &format_ctx.metadata();
                let mut camera_make = fmt_meta.get("com.apple.quicktime.make")
                    .or_else(|| fmt_meta.get("make"))
                    .or_else(|| fmt_meta.get("manufacturer"))
                    .map(|s| s.to_string());
                let mut camera_model = fmt_meta.get("com.apple.quicktime.model")
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

                // Everything else the file says about the shot: every tag
                // the container and its streams carry, and among them the
                // start timecode (a MOV's timecode track, an MXF's
                // material package) and the reel.
                let mut shot = klipp_core::ShotMetadata::default();
                for (k, v) in fmt_meta.iter() {
                    shot.tags.push((format!("format:{k}"), v.to_string()));
                }
                for stream in format_ctx.streams() {
                    let kind = match stream.parameters().medium() {
                        ffmpeg_next::media::Type::Video => "video",
                        ffmpeg_next::media::Type::Audio => "audio",
                        ffmpeg_next::media::Type::Data => "data",
                        ffmpeg_next::media::Type::Subtitle => "subtitle",
                        _ => "stream",
                    };
                    for (k, v) in stream.metadata().iter() {
                        shot.tags.push((format!("{kind}:{k}"), v.to_string()));
                    }
                }
                let tag = |names: &[&str]| -> Option<String> {
                    names.iter().find_map(|n| {
                        shot.tags
                            .iter()
                            .find(|(k, _)| k.split_once(':').is_some_and(|(_, key)| key.eq_ignore_ascii_case(n)))
                            .map(|(_, v)| v.clone())
                    })
                };
                let start_timecode = tag(&["timecode"]);
                let reel_name = tag(&["reel_name", "com.apple.proapps.reel", "reel"]);
                shot.start_timecode = start_timecode;
                shot.reel_name = reel_name;
                // A still's EXIF, then the sidecar a Sony or Canon camera
                // writes beside a clip: each fills what is still missing.
                if is_still {
                    if let Some(still) = klipp_media::exif_read::read_exif(&path) {
                        camera_make = camera_make.or(still.camera_make);
                        camera_model = camera_model.or(still.camera_model);
                        media_created_at = media_created_at.or(still.taken_at);
                        shot.fill_missing_from(still.shot);
                    }
                }
                if let Some(sidecar) = klipp_media::sidecar::read_sidecar(&path) {
                    camera_make = camera_make.or(sidecar.camera_make);
                    camera_model = camera_model.or(sidecar.camera_model);
                    shot.fill_missing_from(sidecar.shot);
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
                        duration,
                        fps,
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
                        shot,
                        truncated_from_secs,
                        dolby_vision_profile,
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
                    path: path.clone(),
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
    fn set_source_media(&mut self, path: PathBuf, source_rotation: i32, audio_path: PathBuf) {
        if self.source_path.as_ref() == Some(&path) && self.source_audio_path.as_ref() == Some(&audio_path) {
            return;
        }
        // Tear down anything currently bound — clean slate.
        self.stop_source();

        match VideoDecoder::open(&path, source_rotation) {
            Ok(dec) => {
                dec.resume();
                self.source_decoder = Some(dec);
                self.source_path = Some(path.clone());
                self.source_audio_path = Some(audio_path.clone());
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
                // Open audio from the original file (silent attach —
                // `play_source` is what actually unmutes / resumes). The
                // picture may come from the skim transcode, which carries
                // no sound.
                self.open_source_audio(&audio_path, 0.0);
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
        let audio_path = self.source_audio_path.clone().unwrap_or_else(|| path.clone());
        // `set_source_media` early-returns when the path matches —
        // clear `source_path` so the rebuild takes the full open +
        // decode-thread spawn path.
        self.source_path = None;
        self.set_source_media(path, rotation, audio_path);
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
            // Already paused: say so anyway, so a UI that thought
            // otherwise falls back into step.
            let _ = self.event_tx.send(AppEvent::SourcePlaybackPaused);
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
        {
            let mut m = crate::mix_thread::lock_mixer(&self.mixer);
            m.ensure_tracks(self.source_mixer_track_index + 1);
            m.tracks[self.source_mixer_track_index].held = true;
        }
        self.source_path = None;
        // A source stopped mid-play — another clip picked while one was
        // playing — is a pause as far as the UI's play button knows;
        // left untold, the button stayed on "playing" and every click
        // sent a Pause the engine had nothing to pause.
        if self.source_state == PlaybackState::Playing {
            let _ = self.event_tx.send(AppEvent::SourcePlaybackPaused);
        }
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
        // Only a frame whose time has come. The wall clock is the pace,
        // not the decoder: a skim transcode decodes several times faster
        // than real time, and taking a frame every tick played it at
        // that speed. A frame the clock has passed, with a later one
        // already due, is skipped to catch up.
        let now_secs = self.source_clock_anchor_secs + self.source_clock_anchor_wall.elapsed().as_secs_f64();
        let half_frame = if self.source_fps > 0.0 { 0.5 / self.source_fps } else { 0.02 };
        let frame = {
            let mut q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let Some(front) = q.front() else { break None };
                if front.is_eof {
                    break q.pop();
                }
                let pts = front.pts_secs();
                let next = q.second().filter(|n| !n.is_eof).map(|n| n.pts_secs());
                match source_frame_verdict(pts, now_secs, half_frame, next) {
                    FrameVerdict::Wait => break None,
                    FrameVerdict::Skip => {
                        q.pop();
                        continue;
                    }
                    FrameVerdict::Show => break q.pop(),
                }
            }
        };
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
                {
                    let mut m = crate::mix_thread::lock_mixer(&self.mixer);
                    m.ensure_tracks(track + 1);
                    m.tracks[track].held = true;
                }
                self.source_audio_source = Some(AudioSource::Decoder(adec));
            }
            Err(e) => {
                debug!("Source viewer: no audio for {path:?}: {e}");
            }
        }
    }

    /// Toggle which viewer owns the audio mixer.
    /// `source_owns = true`: hold every timeline track silent, release
    /// the source viewer track. `source_owns = false`: inverse. The
    /// tracks' own mute and solo are left as the editor set them.
    /// The hold is on the mixer (cpal-callback-safe atomics), so the
    /// switch is instant and glitch-free.
    fn set_source_audio_owner(&mut self, source_owns: bool) {
        let mut m = crate::mix_thread::lock_mixer(&self.mixer);
        let n = m.tracks.len();
        for (i, t) in m.tracks.iter_mut().enumerate() {
            if i == self.source_mixer_track_index {
                t.held = !source_owns;
            } else if i < n {
                t.held = source_owns;
            }
        }
    }
}

/// The index of the video stream that is the file's picture: the one
/// ffmpeg ranks best, unless that is an attached picture (cover art). A
/// file whose *only* video stream is cover art — an MP3 or FLAC with
/// embedded album art — has no picture: it is audio, and returning `None`
/// keeps it off the video track (otherwise the cover art becomes a bogus
/// video clip linked to the sound, which cannot be dragged).
fn picture_stream_index(ictx: &ffmpeg_next::format::context::Input) -> Option<usize> {
    klipp_media::picture_stream(ictx)
}

/// One-shot ffmpeg probe to read a media file's frame rate. Returns
/// `None` on any failure (file unreadable, no video stream, zero
/// denominator) — callers fall back to 24 fps so step / playback keeps
/// working.
fn probe_source_fps(path: &std::path::Path) -> Option<f64> {
    let ictx = klipp_media::open_input(path).ok()?;
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

/// Where a file's media data ends when its header claims more. A copy
/// interrupted mid-transfer keeps the MOV header's full duration while its
/// samples stop early, and trusting the header made a nine-minute clip
/// that played for six seconds. Seeks near the claimed end and reads: a
/// packet there means the file is whole (`None`); none means it is short,
/// and a scan from the start finds where it ends. The scan reads only what
/// the file holds, so it is cheap exactly when it runs.
fn truncated_end_secs(path: &std::path::Path, header_secs: f64) -> Option<f64> {
    let mut ictx = klipp_media::open_input(path).ok()?;
    let time_bases: Vec<f64> = ictx.streams().map(|s| f64::from(s.time_base())).collect();
    let secs_of = |stream: usize, ts: i64| ts as f64 * time_bases.get(stream).copied().unwrap_or(0.0);
    let near_end = (header_secs - 3.0).max(0.0);
    let ts = (near_end * f64::from(ffmpeg_next::ffi::AV_TIME_BASE)) as i64;
    if ictx.seek(ts, ..ts).is_err() {
        return None;
    }
    // A long GOP puts the seek a few hundred packets before its target;
    // a file that has not reached the end by then is taken as whole.
    for (i, (stream, packet)) in ictx.packets().enumerate() {
        if let Some(pts) = packet.pts() {
            if secs_of(stream.index(), pts) >= near_end {
                return None;
            }
        }
        if i >= 600 {
            return None;
        }
    }
    let mut ictx = klipp_media::open_input(path).ok()?;
    let mut end = 0.0f64;
    for (stream, packet) in ictx.packets() {
        if let Some(pts) = packet.pts() {
            end = end.max(secs_of(stream.index(), pts) + secs_of(stream.index(), packet.duration().max(0)));
        }
    }
    (end > 0.0 && end < near_end).then_some(end)
}

/// What to do with the front of the source viewer's frame queue at
/// wall time `now`: wait for its time, show it, or skip it because it
/// is late and the frame behind it is already due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameVerdict {
    Wait,
    Show,
    Skip,
}

fn source_frame_verdict(pts: f64, now: f64, half_frame: f64, next_pts: Option<f64>) -> FrameVerdict {
    if pts > now + half_frame {
        FrameVerdict::Wait
    } else if now - pts > 2.0 * half_frame && next_pts.is_some_and(|n| n <= now + half_frame) {
        FrameVerdict::Skip
    } else {
        FrameVerdict::Show
    }
}

#[cfg(test)]
mod source_pacing_tests {
    use super::*;

    #[test]
    fn a_frame_is_shown_when_its_time_comes_not_when_it_is_decoded() {
        let hf = 0.5 / 30.0;
        assert_eq!(source_frame_verdict(1.0, 0.5, hf, None), FrameVerdict::Wait, "decoded early: wait");
        assert_eq!(source_frame_verdict(1.0, 1.0, hf, Some(1.033)), FrameVerdict::Show);
        assert_eq!(source_frame_verdict(1.0, 1.01, hf, Some(1.033)), FrameVerdict::Show, "within half a frame");
        assert_eq!(source_frame_verdict(1.0, 1.2, hf, Some(1.033)), FrameVerdict::Skip, "late, and the next is due too");
        assert_eq!(source_frame_verdict(1.0, 1.2, hf, None), FrameVerdict::Show, "late but the last one there is: show it");
    }
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

/// A video stream's frame rate, kept rational.
///
/// `r_frame_rate` is the stream's declared base rate (24000/1001 on a
/// 23.976 camera file) and is taken when the average agrees with it.
/// `avg_frame_rate` is frames over duration, so edit lists and a short
/// last frame move it: a 23.976 a6700 clip averages 525000/21883 =
/// 23.991, which the float snap below calls 24. The average is used only
/// when the base rate is missing or is not the rate the frames come at
/// (a variable-rate phone clip declares its timebase-scale 600/1 or 120/1).
/// `None` when neither is usable.
fn fps_from_stream_rates(r_frame_rate: (i32, i32), avg_frame_rate: (i32, i32)) -> Option<Fps> {
    let as_f64 = |(n, d): (i32, i32)| (n > 0 && d > 0).then(|| n as f64 / d as f64);
    let r = as_f64(r_frame_rate).filter(|r| *r <= 1000.0);
    let avg = as_f64(avg_frame_rate);
    match (r, avg) {
        (Some(r), Some(avg)) if (avg / r - 1.0).abs() > 0.05 => Some(fps_from_rational(avg_frame_rate)),
        (Some(_), _) => Some(fps_from_rational(r_frame_rate)),
        (None, Some(_)) => Some(fps_from_rational(avg_frame_rate)),
        (None, None) => None,
    }
}

/// An exact rate as itself when it is a whole or NTSC (x/1001) rate, and
/// otherwise snapped to the nearest standard one. The caller guarantees
/// both parts are positive.
fn fps_from_rational((n, d): (i32, i32)) -> Fps {
    fn gcd(a: u32, b: u32) -> u32 { if b == 0 { a } else { gcd(b, a % b) } }
    let (n, d) = (n as u32, d as u32);
    let g = gcd(n, d).max(1);
    let (n, d) = (n / g, d / g);
    if d == 1 || d == 1001 {
        Fps::new(n, d)
    } else {
        fps_from_float(n as f64 / d as f64)
    }
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

#[cfg(test)]
mod stream_rate_tests {
    use super::*;

    #[test]
    fn a_camera_file_keeps_its_exact_ntsc_rate() {
        // ffprobe of two a6700 23.976 clips: the averages sit either
        // side of 24 and the float snap called both 24.
        assert_eq!(fps_from_stream_rates((24000, 1001), (525000, 21883)), Some(Fps::ntsc_23976()));
        assert_eq!(fps_from_stream_rates((24000, 1001), (118400, 4933)), Some(Fps::ntsc_23976()));
        assert_eq!(fps_from_stream_rates((30000, 1001), (30000, 1001)), Some(Fps::ntsc_29970()));
        assert_eq!(fps_from_stream_rates((50, 2), (25, 1)), Some(Fps::pal_25()));
    }

    #[test]
    fn a_declared_rate_the_frames_do_not_follow_gives_way_to_the_average() {
        // Variable-rate phone clip: timebase-scale base rate.
        assert_eq!(fps_from_stream_rates((600, 1), (2997, 100)), Some(Fps::ntsc_29970()));
        // Interlaced: field rate declared, frame rate averaged.
        assert_eq!(fps_from_stream_rates((50, 1), (25, 1)), Some(Fps::pal_25()));
        assert_eq!(fps_from_stream_rates((0, 0), (60000, 1001)), Some(Fps::new(60000, 1001)));
        assert_eq!(fps_from_stream_rates((0, 0), (0, 0)), None);
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
    inner: klipp_media::frame_reader::SourceColour,
}

impl ThumbColour {
    /// The rules live in `klipp_media::frame_reader::SourceColour`, so
    /// export converts a frame the same way rather than reaching for
    /// plain swscale. What stays here is *which* log format applies:
    /// the viewer answers that from its registry, export from the
    /// media pool.
    fn new(path: &std::path::Path, format_ctx: &ffmpeg_next::format::context::Input, stream_index: usize) -> Self {
        let log_format = crate::video_decoder::log_format_for_path(path);
        Self {
            inner: klipp_media::frame_reader::SourceColour::from_stream(
                format_ctx,
                stream_index,
                log_format,
            ),
        }
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
        self.inner.scene_referred_rgba8(frame, dst_w, dst_h)
    }
}

fn frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    _is_hw: bool,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
    colour: &ThumbColour,
) -> Result<Vec<u8>, String> {
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
) -> Result<Vec<u8>, String> {
    use ffmpeg_next::ffi::*;

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return Err("out of memory allocating a frame".into());
    }

    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    if ret < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        return Err(format!("downloading the frame from the GPU failed: {}", describe(&ffmpeg_next::Error::from(ret))));
    }
    // The download drops most colour tags; take them from the HW frame.
    (*sw_frame).color_range = (*hw_frame.as_ptr()).color_range;
    (*sw_frame).colorspace = (*hw_frame.as_ptr()).colorspace;
    (*sw_frame).color_primaries = (*hw_frame.as_ptr()).color_primaries;
    (*sw_frame).color_trc = (*hw_frame.as_ptr()).color_trc;
    if let Some(rgba) = colour.scene_referred_rgba8(sw_frame, dst_w, dst_h) {
        av_frame_free(&mut (sw_frame as *mut _));
        return Ok(rgba);
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
        return Err(format!("swscale has no path from {sw_fmt:?} {src_w}x{src_h} to RGBA {dst_w}x{dst_h}"));
    }
    *sws_cache = Some(sws);

    let rgba_frame = av_frame_alloc();
    if rgba_frame.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return Err("out of memory allocating a frame".into());
    }
    (*rgba_frame).width = dst_w as i32;
    (*rgba_frame).height = dst_h as i32;
    (*rgba_frame).format = AVPixelFormat::AV_PIX_FMT_RGBA as i32;
    if av_frame_get_buffer(rgba_frame, 0) < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        av_frame_free(&mut (rgba_frame as *mut _));
        return Err(format!("out of memory for an RGBA frame of {dst_w}x{dst_h}"));
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
    Ok(rgba)
}

/// Convert a software-decoded frame to RGBA at target dimensions. The
/// error says what could not be done — which format, which size — so a
/// thumbnail or a walk that fails on it fails with a reason.
fn sw_frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    colour: &ThumbColour,
) -> Result<Vec<u8>, String> {
    use ffmpeg_next::software::scaling;

    if let Some(rgba) = unsafe { colour.scene_referred_rgba8(frame.as_ptr(), dst_w, dst_h) } {
        return Ok(rgba);
    }

    let (fmt, w, h) = (frame.format(), frame.width(), frame.height());
    let mut scaler = scaling::Context::get(fmt, w, h, ffmpeg_next::format::Pixel::RGBA, dst_w, dst_h, scaling::Flags::FAST_BILINEAR)
        .map_err(|e| format!("swscale has no path from {fmt:?} {w}x{h} to RGBA {dst_w}x{dst_h}: {}", describe(&e)))?;

    let mut rgba_frame = ffmpeg_next::frame::Video::empty();
    scaler
        .run(frame, &mut rgba_frame)
        .map_err(|e| format!("swscale failed converting {fmt:?} {w}x{h} to RGBA: {}", describe(&e)))?;

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
    Ok(rgba)
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
    // Media only, as klipp_media gives it to ffmpeg.
    let Ok(input) = klipp_media::input_args(path) else { return 0 };
    // First try ffprobe for side_data rotation (most reliable for modern files)
    if let Ok(output) = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream_side_data=rotation",
            "-of", "csv=p=0",
        ])
        .args(&input)
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
/// The two rules this follows, both the hard way:
///   - ALWAYS drain receive_frame BEFORE feeding new send_packet (avoids
///     EAGAIN packet loss)
///   - Some decoders only release frames after `send_eof()` — flush at end
///
/// Returns true if a frame was decoded into `out_frame`. An earlier version
/// of this loop interleaved send and receive per iteration, and several
/// codecs failed with "No frame decoded" as a result.
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
    let mut format_ctx = klipp_media::open_input(path)
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
        .map_err(|e| format!("converting the frame to RGBA: {e}"))?;

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
    let mut format_ctx = klipp_media::open_input(path)
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
        .map_err(|e| format!("converting the frame to RGBA: {e}"))?;

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
    let mut format_ctx = klipp_media::open_input(path)
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
        .map_err(|e| format!("converting the frame to RGBA: {e}"))?;

    if rotation != 0 {
        let (rotated, rw, rh) = rotate_rgba(&rgba, dst_w, dst_h, rotation);
        Ok((rotated, rw, rh))
    } else {
        Ok((rgba, dst_w, dst_h))
    }
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

/// What a filmstrip run made, and what it could not.
#[derive(Debug, Default)]
pub struct FilmstripOutcome {
    /// Tiles handed to `on_frame`.
    pub tiles: usize,
    /// Tiles that could not be made — the time each was wanted at, and
    /// why. A strip with a hole in it is still a strip; a strip with no
    /// tiles at all is not, and a caller must not cache it as one.
    pub skipped: Vec<(f64, String)>,
}

/// Generate filmstrip thumbnails incrementally with GPU acceleration (CUDA → VAAPI → CPU fallback).
/// Calls `on_frame` for each generated frame. Returns what was made and
/// what was not: a tile the decoder could not reach, seek to, or convert
/// is reported with its reason rather than left out in silence — before
/// this, a strip that lost every tile came back as `Ok(0)` and was
/// written to the disk cache as complete.
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
    on_frame: impl FnMut(f64, Vec<u8>, u32, u32),
    pause_token: Option<Arc<klipp_jobs::PauseToken>>,
) -> Result<FilmstripOutcome, String> {
    generate_filmstrip_range(path, 0.0, duration_secs, interval_secs, target_height, on_frame, pause_token)
}

/// [`generate_filmstrip_incremental`] over `start_secs..=end_secs` of the
/// file only: the tiles a zoomed-in timeline is missing. A refinement
/// used to walk every tile from the start of the file to the end of the
/// range and throw away the ones before it — on a half-hour source,
/// hundreds of decodes for a dozen tiles.
pub fn generate_filmstrip_range(
    path: &std::path::Path,
    start_secs: f64,
    end_secs: f64,
    interval_secs: f64,
    target_height: u32,
    mut on_frame: impl FnMut(f64, Vec<u8>, u32, u32),
    pause_token: Option<Arc<klipp_jobs::PauseToken>>,
) -> Result<FilmstripOutcome, String> {
    let start_secs = start_secs.max(0.0);
    let duration_secs = end_secs;
    let mut format_ctx = klipp_media::open_input(path)
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
    let mut outcome = FilmstripOutcome::default();

    let num_frames = (((duration_secs - start_secs).max(0.0) / interval_secs).ceil() as usize).min(500);
    let accel_label = if is_hw { "GPU" } else { "CPU" };
    tracing::info!("Filmstrip: generating {} frames via {} decode for {:?}", num_frames, accel_label, path);

    // Timestamp of the last frame the decoder produced, for deciding
    // whether a tile needs a seek at all.
    let mut decoded_to_us: Option<i64> = None;
    let tb = format_ctx.stream(stream_index).map(|s| s.time_base()).unwrap_or_else(|| ffmpeg_next::Rational::new(1, 1));
    let frame_us_of = |frame: &ffmpeg_next::frame::Video| -> i64 {
        frame.timestamp().unwrap_or(0) * 1_000_000 * tb.numerator() as i64 / tb.denominator() as i64
    };
    for i in 0..=num_frames {
        let seek_secs = start_secs + i as f64 * interval_secs;
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
            if let Err(e) = format_ctx.seek(ts, ..ts) {
                outcome.skipped.push((seek_secs, format!("seek failed: {}", describe(&e))));
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
                let frame_us = frame_us_of(&decoded_frame);
                decoded_to_us = Some(frame_us);
                if frame_us >= target_pts - 100_000 {
                    got_frame = true;
                    break;
                }
            }
            if got_frame { break; }
        }

        if !got_frame {
            // The packets ran out with frames still inside the decoder: a
            // B-frame pyramid, or frame threads, hold the last of a file
            // until told there is no more input — fifteen or more frames
            // in the software decoder. Without this flush the tile at the
            // end of every strip was missing, and nothing said so.
            let _ = decoder.send_eof();
            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let frame_us = frame_us_of(&decoded_frame);
                if frame_us >= target_pts - 100_000 {
                    got_frame = true;
                    break;
                }
            }
            // Drained, the decoder takes no more packets: the next tile
            // has to seek and flush, whatever its position.
            decoded_to_us = None;
        }

        if !got_frame {
            outcome.skipped.push((
                seek_secs,
                "no frame reached this time: the file ends before it, or nothing here could be decoded".into(),
            ));
            continue;
        }

        match frame_to_rgba(&decoded_frame, decode_w, decode_h, is_hw, &mut sws_cache.0, &colour) {
            Ok(rgba) => {
                if rotation != 0 {
                    let (rotated, rw, rh) = rotate_rgba(&rgba, decode_w, decode_h, rotation);
                    on_frame(seek_secs, rotated, rw, rh);
                } else {
                    on_frame(seek_secs, rgba, final_w, final_h);
                }
                outcome.tiles += 1;
            }
            Err(e) => outcome.skipped.push((seek_secs, e)),
        }
    }

    if !outcome.skipped.is_empty() {
        tracing::warn!(
            "Filmstrip: {} of {} tiles could not be made for {:?}; first at {:.2}s: {}",
            outcome.skipped.len(), outcome.tiles + outcome.skipped.len(), path, outcome.skipped[0].0, outcome.skipped[0].1,
        );
    }
    Ok(outcome)
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
    /// Frames handed back so far — names the frame in an error.
    frames_out: usize,
    /// How many frames the container says the stream has, or 0 when it
    /// does not say (Matroska usually does not; MP4 does).
    declared_frames: i64,
    /// After [`Self::seek_to`]: frames before this time, in the stream's
    /// time base, are decoded and dropped.
    skip_before: Option<i64>,
}

/// An error in words. ffmpeg-next's `Display` is blank for the codes that
/// matter most here — `InvalidData` among them — and "decode error: "
/// followed by nothing, or by `Error(1094995529: )`, tells nobody what
/// happened to their file.
fn describe(e: &ffmpeg_next::Error) -> String {
    use ffmpeg_next::Error as E;
    match e {
        E::InvalidData => "the data is not valid for this codec".into(),
        E::Eof => "end of file".into(),
        E::External => "the hardware decoder failed".into(),
        E::BufferTooSmall => "buffer too small".into(),
        E::Other { errno } => {
            let os = std::io::Error::from_raw_os_error(*errno).to_string();
            if os.trim().is_empty() { format!("os error {errno}") } else { os }
        }
        other => {
            let text = other.to_string();
            if text.trim().is_empty() { format!("{other:?}") } else { text }
        }
    }
}

impl SequentialFrameDecoder {
    /// Open `path` and prepare for sequential decode. Output frames are scaled
    /// so the longest visible (rotation-aware) edge is at most `max_width`
    /// pixels. Uses GPU decode when available.
    pub fn open(path: &std::path::Path, max_width: u32) -> Result<Self, String> {
        let format_ctx = klipp_media::open_input(path)
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
        let declared_frames = format_ctx.stream(stream_index).map(|s| s.frames()).unwrap_or(0);
        Ok(Self {
            colour,
            declared_frames,
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
            frames_out: 0,
            skip_before: None,
        })
    }

    /// Start the walk at the frame shown `secs` into the file rather than
    /// at its first: a seek to the keyframe before it, then decoding
    /// forward and handing back nothing earlier. A window track reads only
    /// the clip's part of what may be a long file (#238).
    pub fn seek_to(&mut self, secs: f64) -> Result<(), String> {
        let stream = self.format_ctx.stream(self.stream_index).ok_or("the video stream is gone")?;
        let tb = stream.time_base();
        let ticks_per_sec = tb.denominator() as f64 / tb.numerator().max(1) as f64;
        let rate = stream.avg_frame_rate();
        let fps = if rate.numerator() > 0 && rate.denominator() > 0 { rate.numerator() as f64 / rate.denominator() as f64 } else { 30.0 };
        let start = stream.start_time();
        let start = if start == ffmpeg_next::ffi::AV_NOPTS_VALUE { 0 } else { start };
        // Half a frame early, so the frame shown at `secs` is not dropped
        // for a rounding.
        let target = start + ((secs.max(0.0) - 0.5 / fps) * ticks_per_sec).round() as i64;
        let ts = (secs.max(0.0) * ffmpeg_next::ffi::AV_TIME_BASE as f64) as i64;
        self.format_ctx.seek(ts, ..ts).map_err(|e| format!("seek to {secs:.3} s: {}", describe(&e)))?;
        self.decoder.flush();
        self.eof_reached = false;
        self.drained = false;
        self.skip_before = Some(target);
        // A walk that starts part way cannot be held to the file's frame
        // count at its end.
        self.declared_frames = 0;
        Ok(())
    }

    /// The walk reached the end of the file. If the container declared
    /// how many frames it holds and fewer came out, that is an error, not
    /// a shorter clip: a demuxer that resyncs past a damaged block drops
    /// the packet without a word, and a file cut short ends cleanly.
    fn finished(&mut self) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
        self.drained = true;
        if self.declared_frames > 0 && (self.frames_out as i64) < self.declared_frames {
            return Err(format!(
                "the file declares {} frames and the decoder produced {}",
                self.declared_frames, self.frames_out
            ));
        }
        Ok(None)
    }

    /// Whether frames are being decoded on the GPU. A hardware decoder that
    /// declines the file falls back to software silently, so a caller — or
    /// a test meaning to cover that path — cannot tell from the frames.
    pub fn is_hardware(&self) -> bool {
        self.is_hw
    }

    /// Decode and return the next frame in source order; `Ok(None)` at EOF.
    ///
    /// Anything else that ends the walk is an `Err`, never a quiet `None`:
    /// a packet the container could not read, a packet the decoder refused
    /// twice, a frame it could not decode, or one that could not be
    /// converted. The callers walk every frame for stabilisation and motion
    /// analysis, where a frame missing from the middle corrupts the motion
    /// of everything after it, and a walk that stops early is
    /// indistinguishable from a shorter clip. All four used to come back as
    /// `None`, and a decode error was mistaken for "needs more input" and
    /// the frame skipped. After an `Err` the walk is over: later calls
    /// return `Ok(None)`.
    pub fn next_frame(&mut self) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
        if self.drained {
            return Ok(None);
        }
        let mut decoded = ffmpeg_next::frame::Video::empty();

        if let Some(f) = self.receive(&mut decoded)? {
            return Ok(Some(f));
        }

        loop {
            if self.eof_reached {
                if let Some(f) = self.receive(&mut decoded)? {
                    return Ok(Some(f));
                }
                return self.finished();
            }

            let mut packet = ffmpeg_next::codec::packet::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(()) => {
                    if packet.stream() != self.stream_index {
                        continue;
                    }
                    if let Err(first) = self.decoder.send_packet(&packet) {
                        // EAGAIN: the decoder's output queue is full, so it
                        // will not take another packet until one frame is
                        // collected. Take one, *then* retry the packet, and
                        // only then hand the frame back.
                        //
                        // The order is the whole point. This used to read
                        // `while receive_frame().is_ok() { return ... }`,
                        // which returned on the first frame and so never
                        // reached the retry — the packet was dropped and its
                        // frame never decoded. `clippy::never_loop` found it.
                        //
                        // In practice this branch is defensive: the walk
                        // drains before every send, which is the condition
                        // under which libavcodec takes a packet, so a
                        // refusal here is something other than EAGAIN. A
                        // second refusal means the packet's frame is lost,
                        // and that is an error, not a warning.
                        //
                        // Where the refusal is the packet's data, it is the
                        // same decode error the receive side reports, and is
                        // worded the same: a decoder may refuse a broken
                        // packet as it is sent rather than fail the frame it
                        // makes (Arch Linux ARM's ffmpeg does for FFV1), and
                        // the frame lost is the one after those decoded.
                        let drained = self.receive(&mut decoded)?;
                        if let Err(again) = self.decoder.send_packet(&packet) {
                            self.drained = true;
                            return Err(format!(
                                "frame {}: decode error: {} (the decoder refused its packet twice; first: {})",
                                self.frames_out, describe(&again), describe(&first)
                            ));
                        }
                        if drained.is_some() {
                            return Ok(drained);
                        }
                    }
                }
                Err(ffmpeg_next::Error::Eof) => {
                    self.eof_reached = true;
                    let _ = self.decoder.send_eof();
                    continue;
                }
                Err(e) => {
                    self.drained = true;
                    return Err(format!("after frame {}: packet read error: {}", self.frames_out, describe(&e)));
                }
            }

            if let Some(f) = self.receive(&mut decoded)? {
                return Ok(Some(f));
            }
        }
    }

    /// One frame from the decoder if it has one ready: `Ok(None)` when it
    /// needs more input or is flushed, `Err` for a frame it could not
    /// decode or convert. The distinction is the point — a decode error
    /// used to look like "needs more input", and the broken frame was
    /// skipped without a word.
    fn receive(&mut self, decoded: &mut ffmpeg_next::frame::Video) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
        // Frames before a seek's target are decoded, then dropped.
        if let Some(limit) = self.skip_before {
            loop {
                match self.decoder.receive_frame(decoded) {
                    Ok(()) => {
                        if decoded.timestamp().or(decoded.pts()).is_some_and(|p| p < limit) {
                            continue;
                        }
                        self.skip_before = None;
                        let frame = match self.convert(decoded) {
                            Ok(f) => f,
                            Err(e) => {
                                self.drained = true;
                                return Err(format!("frame {}: could not be converted to RGBA: {e}", self.frames_out));
                            }
                        };
                        self.frames_out += 1;
                        return Ok(Some(frame));
                    }
                    Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => return Ok(None),
                    Err(ffmpeg_next::Error::Eof) => return Ok(None),
                    Err(e) => {
                        self.drained = true;
                        return Err(format!("frame {}: decode error: {}", self.frames_out, describe(&e)));
                    }
                }
            }
        }
        match self.decoder.receive_frame(decoded) {
            Ok(()) => {
                let frame = match self.convert(decoded) {
                    Ok(f) => f,
                    Err(e) => {
                        self.drained = true;
                        return Err(format!("frame {}: could not be converted to RGBA: {e}", self.frames_out));
                    }
                };
                self.frames_out += 1;
                Ok(Some(frame))
            }
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => Ok(None),
            Err(ffmpeg_next::Error::Eof) => Ok(None),
            Err(e) => {
                self.drained = true;
                Err(format!("frame {}: decode error: {}", self.frames_out, describe(&e)))
            }
        }
    }

    fn convert(&mut self, decoded: &ffmpeg_next::frame::Video) -> Result<(Vec<u8>, u32, u32), String> {
        let rgba = frame_to_rgba(decoded, self.decode_w, self.decode_h, self.is_hw, &mut self.sws_cache, &self.colour)?;
        if self.rotation != 0 {
            let (rotated, rw, rh) = rotate_rgba(&rgba, self.decode_w, self.decode_h, self.rotation);
            Ok((rotated, rw, rh))
        } else {
            Ok((rgba, self.decode_w, self.decode_h))
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
    use klipp_timeline::test_clip::TestClip;
    use std::collections::HashMap;

    fn tc(frames: i64) -> Timecode { RationalTime::from_frames(frames, 24) }

    fn make_title_data(text: &str) -> TitleData {
        let tb = TextBox {
            text: text.to_string(),
            font_name: "Sans".to_string(),
            font_size: 72.0,
            font_style: FontStyle::Regular,
            alignment: TextAlignment::Center,
            ..Default::default()
        };
        TitleData::single_text(tb)
    }

    fn title_clip(text: &str) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c.kind = ClipKind::Title(make_title_data(text));
        c
    }

    fn media_clip_with_anchor(anchor_x: f32, anchor_y: f32) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c = TestClip::of(c).effect(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x, anchor_y,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }).build();

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
        consumer = TestClip::of(consumer).effect(BuiltinEffect::Transform {
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
        }).build();

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
        consumer = TestClip::of(consumer).effect(BuiltinEffect::Transform {
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
        }).build();

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
        consumer = TestClip::of(consumer).effect(BuiltinEffect::Transform {
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
        }).build();

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
        title = TestClip::of(title).effect(BuiltinEffect::Transform {
            position_x: 100.0, position_y: 50.0,  // offset 100 px right, 50 px down
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }).build();

        let title_id = title.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        consumer = TestClip::of(consumer).effect(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: fallback.0, anchor_y: fallback.1,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(FocalRef::Glyph {
                title_clip_id: title_id.0,
                char_index: 0,
                fallback_xy: fallback,
            }),
        }).build();

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
    use klipp_timeline::{GradeAccess, Sequence};
    use std::sync::Arc;

    /// Build a Sequence with a single clip and an Exposure node added
    /// to its graph. Returns (shared sequence, clip id, exposure node id).
    /// The default clip graph is bare Source → Output, so the test
    /// pushes the Exposure node it needs explicitly.
    fn make_fixture() -> (Arc<klipp_timeline::SharedSequence>, klipp_core::ClipId, klipp_nodes::NodeId) {
        let mut seq = Sequence::new_default();
        let media_id = MediaId::new();
        let duration = RationalTime::from_frames(120, 24);
        let track = seq.tracks.iter_mut().find(|t| matches!(t.kind, klipp_timeline::TrackKind::Video))
            .expect("default sequence has a video track");
        let clip_id = track.append_clip(media_id, duration);
        let exposure_id = klipp_nodes::NodeId::new_v4();
        for t in &mut seq.tracks {
            if let Some(c) = t.find_clip_mut(clip_id) {
                c.edit_grade(|g| g.nodes.push(klipp_nodes::Node {
                    id: exposure_id,
                    kind: klipp_nodes::NodeKind::Exposure,
                    label: "Exposure".into(),
                    enabled: true,
                    params: klipp_nodes::NodeParams::default(),
                    keyframes: klipp_nodes::NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: vec![],
                }));
                break;
            }
        }
        (Arc::new(klipp_timeline::SharedSequence::new(seq)), clip_id, exposure_id)
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
            seq.read().unwrap().all_clips().next().unwrap().grade().clone()
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
        let node = clip.grade().nodes.iter().find(|n| n.id == node_id).unwrap();
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
// `audio_fingerprint_at` is what gates chain rebuild on
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
            handles: Default::default(),
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

    /// Pin the intentional scope of audio_fingerprint_at: only
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

    /// A running chain holds the settings it was built with: a changed
    /// setting must reach it, and must not restart it.
    #[test]
    fn a_setting_changes_the_settings_fingerprint_not_the_chain() {
        use crate::engine::{audio_chain_fingerprint_for, audio_settings_fingerprint_for};
        let kfs = std::collections::HashMap::new();
        let chain = |reduction: f32, wet_dry: f32| {
            vec![AudioEffectInstance {
                effect_type: "voice_isolation".into(),
                enabled: true,
                wet_dry,
                params: vec![("reduction".into(), reduction), ("mode".into(), 0.0)],
            }]
        };
        // A slider move must not rebuild the chain (a restart gaps the
        // audio on every tick of a drag)...
        assert_eq!(audio_chain_fingerprint_for(&chain(30.0, 1.0), &kfs), audio_chain_fingerprint_for(&chain(12.0, 0.5), &kfs));
        // ...but it must reach the running chain.
        let base = audio_settings_fingerprint_for(&chain(30.0, 1.0));
        assert_eq!(base, audio_settings_fingerprint_for(&chain(30.0, 1.0)));
        assert_ne!(base, audio_settings_fingerprint_for(&chain(12.0, 1.0)), "a preset or slider change must be pushed");
        assert_ne!(base, audio_settings_fingerprint_for(&chain(30.0, 0.5)), "a wet/dry change must be pushed");
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

    /// A rendered frame shown from the render cache is the frame the
    /// viewer showed when it drew it (#2072): the cache stores it rounded
    /// with the viewer's dither, and in a Rec.709 project with no tone map
    /// the hit's table is the identity. A slow gradient, which the dither
    /// is for, is where an undithered cache would show bands instead.
    #[test]
    fn a_cache_hit_shows_the_rendered_frames_bytes() {
        let (w, h) = (960usize, 4usize);
        let mut linear = Vec::with_capacity(w * h * 4);
        for _ in 0..h {
            for x in 0..w {
                let v = klipp_color::transfer::srgb_to_linear((40.0 + 12.0 * x as f32 / (w - 1) as f32) / 255.0);
                linear.extend_from_slice(&[v, v * 0.97, v * 1.02, 1.0]);
            }
        }
        let ws = klipp_core::WorkingColorspace::Rec709Linear;
        let lut = klipp_color::display::display_transform_u8_lut(klipp_color::display::ToneMapOp::None, ws).expect("a table");
        let hit = map_u8_rgb(&rendered_frame_to_cache_u8(&linear), &lut);
        let drawn = klipp_color::display::display_transform_ws_dithered_to(
            &linear,
            klipp_color::display::ToneMapOp::None,
            ws,
            klipp_core::SdrTransfer::Srgb,
        );
        let differ = hit.iter().zip(&drawn).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "a cache hit differs from the drawn frame at {differ} samples");
    }

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

#[cfg(test)]
mod audio_crossfade_schedule_tests {
    //! Where playback schedules an audio crossfade's two sources. The
    //! render mixes the same clips over the same window
    //! (`klipp-render/tests/audio_crossfade.rs`); these pin the playback
    //! half against the same shape of timeline.
    use super::{audio_resync_needed, audio_rows, audio_chain_fingerprint_for, envelope_of, fade_curve_to_u32, fadeable_media, key_track, sounding_for_key, LANE_STRIDE};
    use klipp_core::{MediaId, RationalTime, Timecode, TransitionAlignment, TransitionId, TransitionKind};
    use klipp_media::{MediaItem, MediaPool};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind, Transition};

    fn tc(frames: i64) -> Timecode {
        RationalTime { value: frames, rate: 24 }
    }

    /// A1: A at 0–72 (source 24–96), B at 72–144 (source 48–120),
    /// crossfaded over 12 frames centred on the cut: frames 66–78. Both
    /// items have a PCM cache, as only the PCM feeder can fade.
    fn timeline() -> (Sequence, MediaPool, MediaId, MediaId) {
        let mut pool = MediaPool::new();
        let mut item = |name: &str| {
            let mut m = MediaItem::synthetic_for_test(format!("/tmp/{name}.wav").into(), None);
            m.has_audio = true;
            let pcm_path = klipp_testing::scratch::scratch_file("klipp-engine-xfade", &format!("{name}.pcm"));
            std::fs::write(&pcm_path, b"").unwrap();
            m.audio_cache = Some(klipp_media::AudioCacheEntry {
                media_id: m.id,
                pcm_path,
                peaks_path: Default::default(),
                sample_rate: 48000,
                channels: 2,
                duration_secs: 5.0,
                peaks: Vec::new(),
                channel_peaks: None,
            });
            pool.add(m)
        };
        let (am, bm) = (item("xfade-a"), item("xfade-b"));
        let mut s = Sequence::new_default();
        s.tracks.clear();
        let mut t = Track::new("A1", TrackKind::Audio);
        let a = Clip::media(am, tc(0), tc(72), tc(24), tc(96));
        let b = Clip::media(bm, tc(72), tc(144), tc(48), tc(120));
        t.transitions.push(Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: tc(12),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: TransitionAlignment::Center,
        });
        t.clips = vec![a, b];
        s.tracks.push(t);
        (s, pool, am, bm)
    }

    #[test]
    fn inside_the_window_both_clips_play_from_their_handles() {
        let (s, pool, am, bm) = timeline();
        let outside = audio_rows(&s, &pool, &fadeable_media(&pool), tc(60), false, false);
        assert_eq!(outside.len(), 1);
        let before_cut = audio_rows(&s, &pool, &fadeable_media(&pool), tc(69), false, false);
        assert_eq!(before_cut.len(), 2);
        let a = before_cut.iter().find(|r| r.1 == am).unwrap();
        let b = before_cut.iter().find(|r| r.1 == bm).unwrap();
        assert_ne!(a.0, b.0, "the two sides need two sources");
        assert_eq!(key_track(a.0), 0);
        assert_eq!(key_track(b.0), 0);
        // B is three frames before its in point: its head, from source 45.
        assert!((b.3 - 45.0 / 24.0).abs() < 1e-9, "B from {}", b.3);
        let after_cut = audio_rows(&s, &pool, &fadeable_media(&pool), tc(75), false, false);
        let a = after_cut.iter().find(|r| r.1 == am).unwrap();
        // A three frames past its out point: its tail, source 99.
        assert!((a.3 - 99.0 / 24.0).abs() < 1e-9, "A from {}", a.3);
    }

    /// #642: a retimed clip's sound is in the mix, read at its speed by
    /// its PCM source — not the live decoder, which reads at 1x, so it is
    /// silent until its cache is made. A ramped one is silent; the clip on
    /// the other track plays on throughout.
    #[test]
    fn a_retimed_clips_sound_is_heard_from_its_cache_and_a_ramp_is_not() {
        let (mut s, mut pool, am, bm) = timeline();
        // B onto a track of its own, under A, both sounding at 1 s.
        let b = s.tracks[0].clips.remove(1);
        s.tracks[0].transitions.clear();
        let mut t2 = Track::new("A2", TrackKind::Audio);
        t2.clips.push(Clip { timeline_in: tc(0), timeline_out: tc(72), ..b });
        s.tracks.push(t2);
        let heard = |s: &Sequence, pool: &klipp_media::MediaPool| audio_rows(s, pool, &fadeable_media(pool), tc(24), false, false).iter().map(|r| r.1).collect::<Vec<_>>();
        assert_eq!(heard(&s, &pool), vec![am, bm]);
        s.tracks[0].clips[0].speed = 2.0;
        assert_eq!(heard(&s, &pool), vec![am, bm], "the 2x clip heard, from its cache");
        s.tracks[0].clips[0].speed = -1.0;
        assert_eq!(heard(&s, &pool), vec![am, bm], "a reversed one too");
        let cache = pool.get_mut(&am).unwrap().audio_cache.take();
        assert_eq!(heard(&s, &pool), vec![bm], "no cache, no varispeed: silent rather than at 1x");
        pool.get_mut(&am).unwrap().audio_cache = cache;
        s.tracks[0].clips[0].speed = 1.0;
        s.tracks[0].clips[0].speed_keyframes = vec![klipp_core::Keyframe { time_secs: 0.0, value: 2.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
        assert_eq!(heard(&s, &pool), vec![bm], "a ramp has no one speed: silent");
    }

    #[test]
    fn the_incoming_source_keeps_its_key_after_the_window() {
        let (s, pool, _, bm) = timeline();
        let in_window = audio_rows(&s, &pool, &fadeable_media(&pool), tc(70), false, false);
        let after = audio_rows(&s, &pool, &fadeable_media(&pool), tc(100), false, false);
        let key = |rows: &[super::TrackAudioRow]| rows.iter().find(|r| r.1 == bm).unwrap().0;
        assert_eq!(key(&in_window), key(&after), "B would be reopened at full level as the window ends");
        // And its source carries on: nothing to resync past the window.
        let fp = audio_chain_fingerprint_for(&[], &Default::default());
        assert!(!audio_resync_needed(&s, &fadeable_media(&pool), tc(100), [(key(&after), bm, fp)]));
    }

    #[test]
    fn a_clip_without_a_pcm_cache_is_cut_to_not_crossfaded() {
        // The incoming clip's conform has not finished: its source would
        // be the decoder, which cannot fade. So the join is a plain
        // cut: one source at a time, neither clip extended, one key.
        let (s, mut pool, am, bm) = timeline();
        pool.get_mut(&bm).unwrap().audio_cache = None;
        let before_cut = audio_rows(&s, &pool, &fadeable_media(&pool), tc(69), false, false);
        assert_eq!(before_cut.len(), 1, "B would play at full level under A");
        assert_eq!(before_cut[0].1, am);
        let after_cut = audio_rows(&s, &pool, &fadeable_media(&pool), tc(75), false, false);
        assert_eq!(after_cut.len(), 1, "A would play on past its out point at full level");
        assert_eq!(after_cut[0].1, bm);
        assert!((after_cut[0].3 - 51.0 / 24.0).abs() < 1e-9, "B from {}", after_cut[0].3);
        assert_eq!(before_cut[0].0, after_cut[0].0, "a plain cut keeps one key");
        // And the source that is playing needs no crossfade envelope.
        let fp = audio_chain_fingerprint_for(&[], &Default::default());
        assert!(audio_resync_needed(&s, &fadeable_media(&pool), tc(75), [(before_cut[0].0, am, fp)]), "A's source must give way to B at the cut");
    }

    #[test]
    fn a_plain_cut_keeps_one_source_key() {
        let (mut s, pool, am, bm) = timeline();
        s.tracks[0].transitions.clear();
        let a = audio_rows(&s, &pool, &fadeable_media(&pool), tc(70), false, false);
        let b = audio_rows(&s, &pool, &fadeable_media(&pool), tc(75), false, false);
        assert_eq!((a.len(), b.len()), (1, 1));
        assert_eq!((a[0].1, b[0].1), (am, bm));
        assert_eq!(a[0].0, b[0].0);
        assert!(a[0].0 < LANE_STRIDE);
    }

    /// The envelope a source is given before it fills, as the loop's
    /// pre-seek does for a source it opens inside the window: each
    /// clip stretched by its handle and ramped over the crossfade at
    /// that end, its own fade kept at the other.
    #[test]
    fn a_source_opened_inside_the_window_gets_its_crossfade_envelope() {
        let (mut s, pool, am, bm) = timeline();
        s.tracks[0].clips[0].fade_in_secs = 0.1;
        s.tracks[0].clips[1].fade_out_secs = 0.2;
        let rows = audio_rows(&s, &pool, &fadeable_media(&pool), tc(69), false, false);
        let envelope = |media: MediaId| {
            let key = rows.iter().find(|r| r.1 == media).unwrap().0;
            envelope_of(&s, &sounding_for_key(&s, &fadeable_media(&pool), key, tc(69)).unwrap(), tc(69))
        };
        let constant_power = fade_curve_to_u32(klipp_timeline::FadeCurve::ConstantPower);
        let a = envelope(am);
        // A: source 24–96 plus a 6-frame tail, fading out over the 12-frame window.
        assert!((a.source_in_secs - 1.0).abs() < 1e-9, "{a:?}");
        assert!((a.duration_secs - 3.25).abs() < 1e-9, "{a:?}");
        assert!((a.fade_out_secs - 0.5).abs() < 1e-9, "{a:?}");
        assert_eq!(a.fade_out_curve, constant_power);
        assert!((a.fade_in_secs - 0.1).abs() < 1e-6, "A's own fade in stays: {a:?}");
        let b = envelope(bm);
        // B: a 6-frame head before source 48, fading in over the window.
        assert!((b.source_in_secs - 1.75).abs() < 1e-9, "{b:?}");
        assert!((b.duration_secs - 3.25).abs() < 1e-9, "{b:?}");
        assert!((b.fade_in_secs - 0.5).abs() < 1e-9, "{b:?}");
        assert_eq!(b.fade_in_curve, constant_power);
        assert!((b.fade_out_secs - 0.2).abs() < 1e-6, "B's own fade out stays: {b:?}");
        // A clip in no crossfade plays its own envelope.
        s.tracks[0].transitions.clear();
        let plain_cut = audio_rows(&s, &pool, &fadeable_media(&pool), tc(60), false, false);
        let plain = envelope_of(&s, &sounding_for_key(&s, &fadeable_media(&pool), plain_cut[0].0, tc(60)).unwrap(), tc(60));
        assert!((plain.source_in_secs - 1.0).abs() < 1e-9 && (plain.duration_secs - 3.0).abs() < 1e-9, "{plain:?}");
        assert!((plain.fade_in_secs - 0.1).abs() < 1e-6, "{plain:?}");
        assert_eq!((plain.fade_out_secs, plain.gain), (0.0, 1.0));
    }
}

#[cfg(test)]
mod audio_continuity_tests {
    //! KLIPP-AUDIO-JUMPCUT — the pure audio-continuity decision that keeps a
    //! continuous music clip streaming across a video-only jump cut.
    //!
    //! During live playback, advancing the VIDEO clip must NOT flush the mix
    //! staging or reseek the audio PCM feed when the audio track is one
    //! continuous clip across the boundary — that flush + reseek is the
    //! audible micro-stutter. `audio_resync_needed` is the shared gate the
    //! video-contiguous branch and every video jump-cut branch consult:
    //! `false` = leave audio streaming untouched, `true` = a genuine audio
    //! change (media/source, gap, or chain) → resync exactly as before.
    use super::{audio_chain_fingerprint_for, audio_resync_needed};
    use klipp_core::{AudioEffectInstance, MediaId, RationalTime, Timecode};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};
    use std::collections::HashMap;

    fn tc(frames: i64) -> Timecode {
        RationalTime::from_frames(frames, 24)
    }

    /// Fingerprint of an empty audio chain — what a source opened on a clip
    /// with no audio effects carries.
    fn empty_fp() -> u64 {
        audio_chain_fingerprint_for(&[], &HashMap::new())
    }

    #[test]
    fn continuous_audio_under_video_jumpcut_needs_no_resync() {
        // V1 — two clips, contiguous in the TIMELINE but a JUMP in SOURCE
        // (clip B reads source [200,248) instead of continuing from 48).
        // A1 — ONE continuous music clip spanning both video clips.
        let mut s = Sequence::new_default();
        s.tracks.clear(); // deterministic indices — drop new_default's tracks
        let vid = MediaId::new();
        let music = MediaId::new();

        let mut v = Track::new("V1", TrackKind::Video);
        v.clips.push(Clip::media(vid, tc(0), tc(48), tc(0), tc(48)));
        v.clips.push(Clip::media(vid, tc(48), tc(96), tc(200), tc(248)));
        s.tracks.push(v);

        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips.push(Clip::media(music, tc(0), tc(96), tc(0), tc(96)));
        s.tracks.push(a);

        // Active audio source: the A1 music clip (track index 1), empty chain.
        let active = [(1usize, music, empty_fp())];
        // At the video cut boundary (frame 48) the audio clip under the
        // source is unchanged → NO resync: the music keeps streaming.
        assert!(
            !audio_resync_needed(&s, &Default::default(), tc(48), active),
            "continuous audio across a video jump cut must NOT trigger a resync",
        );
    }

    /// #572: a source swapped in for the next clip a mix lead ahead of
    /// the cut takes that clip's fades, while the source still playing
    /// the clip on screen keeps its own.
    #[test]
    fn a_source_swapped_in_ahead_of_a_cut_reads_its_own_clip() {
        let music_a = MediaId::new();
        let music_b = MediaId::new();
        let mut s = Sequence::new_default();
        s.tracks.clear();
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips.push(Clip::media(music_a, tc(0), tc(48), tc(0), tc(48)));
        a.clips.push(Clip::media(music_b, tc(48), tc(96), tc(0), tc(48)));
        s.tracks.push(a);
        let (playhead, heard_at) = (tc(44), tc(49));
        let fadeable = Default::default();
        let own = |media| super::source_clip_tc(&s, &fadeable, (0, media, empty_fp()), playhead, heard_at);
        assert_eq!(own(music_a), playhead, "A's source plays the clip on screen");
        assert_eq!(own(music_b), heard_at, "B's source plays the clip heard next");
    }

    #[test]
    fn audio_media_change_at_boundary_needs_resync() {
        let music_a = MediaId::new();
        let music_b = MediaId::new();
        let mut s = Sequence::new_default();
        s.tracks.clear();
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips.push(Clip::media(music_a, tc(0), tc(48), tc(0), tc(48)));
        a.clips.push(Clip::media(music_b, tc(48), tc(96), tc(0), tc(48)));
        s.tracks.push(a);
        // Source is on music_a; at frame 48 the clip under it is music_b.
        let active = [(0usize, music_a, empty_fp())];
        assert!(
            audio_resync_needed(&s, &Default::default(), tc(48), active),
            "a real audio media change at the boundary MUST resync",
        );
    }

    #[test]
    fn audio_chain_change_at_boundary_needs_resync() {
        let music = MediaId::new();
        let mut s = Sequence::new_default();
        s.tracks.clear();
        let mut a = Track::new("A1", TrackKind::Audio);
        // One continuous media, but the clip carries an audio effect the
        // streaming source was not opened with.
        let mut clip = Clip::media(music, tc(0), tc(96), tc(0), tc(96));
        clip.audio_effects.push(AudioEffectInstance {
            effect_type: "reverb".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![],
        });
        a.clips.push(clip);
        s.tracks.push(a);
        // Source opened with an EMPTY chain fingerprint → chain differs.
        let active = [(0usize, music, empty_fp())];
        assert!(
            audio_resync_needed(&s, &Default::default(), tc(10), active),
            "an audio-chain change MUST resync even when the media is unchanged",
        );
    }

    #[test]
    fn gap_under_audio_source_needs_no_resync() {
        let music = MediaId::new();
        let mut s = Sequence::new_default();
        s.tracks.clear();
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips.push(Clip::media(music, tc(0), tc(48), tc(0), tc(48)));
        s.tracks.push(a);
        // At frame 60 there is no clip under the source: the source is
        // retired on its own; the others must not be restarted for it.
        let active = [(0usize, music, empty_fp())];
        assert!(
            !audio_resync_needed(&s, &Default::default(), tc(60), active),
            "a gap under one source must not restart every source",
        );
    }

    #[test]
    fn no_active_sources_never_needs_resync() {
        let s = Sequence::new_default();
        let active: [(usize, MediaId, u64); 0] = [];
        assert!(
            !audio_resync_needed(&s, &Default::default(), tc(0), active),
            "with no active audio sources there is nothing to resync",
        );
    }
}

#[cfg(test)]
mod scope_pacing {
    use super::*;

    /// The rule the scopes are supposed to obey, stated as arithmetic.
    fn duty(cost_ms: u64) -> f64 {
        let cost = Duration::from_millis(cost_ms);
        let wait = (cost * 2).min(SCOPE_MAX_INTERVAL);
        cost.as_secs_f64() / (cost + wait).as_secs_f64()
    }

    #[test]
    fn scopes_keep_to_a_third_of_the_engine() {
        // Every cost the scopes actually reach, cheap to pathological.
        // With the cap at 100 ms this failed from 51 ms upward: a 1 s
        // pass took 90% of the engine, the UI fell to 1 fps and the app
        // read as frozen while audio kept playing.
        for cost_ms in [1, 4, 15, 30, 50, 100, 200, 500, 1000] {
            let d = duty(cost_ms);
            assert!(
                d <= 0.34,
                "a {cost_ms} ms scope pass would take {:.0}% of the engine; \
                 the pacing is meant to hold it to a third",
                d * 100.0
            );
        }
    }

    #[test]
    fn a_cheap_pass_still_refreshes_promptly() {
        // The cap must not have been bought by making cheap scopes
        // sluggish: 15 ms costs 30 ms of wait, as the docs say.
        let wait = (Duration::from_millis(15) * 2).min(SCOPE_MAX_INTERVAL);
        assert_eq!(wait, Duration::from_millis(30));
    }
}

#[cfg(test)]
mod mix_lead_tests {
    /// The mix thread's ~200 ms main ring at 48 kHz stereo is 19 200
    /// samples: the keyframes are evaluated 0.2 s past the playhead.
    #[test]
    fn the_lead_is_the_main_ring_in_seconds() {
        assert!((super::mix_lead_secs(48_000 * 2 / 5, 48_000) - 0.2).abs() < 1e-9);
        assert_eq!(super::mix_lead_secs(0, 48_000), 0.0);
        assert_eq!(super::mix_lead_secs(1024, 0), 0.0);
    }
}

#[cfg(test)]
mod audio_edited_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #580: playback crossing a cut changes the clips under the playhead
    /// and so the audio fingerprint; that is not an edit, and the
    /// SequenceUpdated the UI sends around cuts must not restart every
    /// source. The same clips with other speed or effects are an edit.
    #[test]
    fn a_cut_is_not_an_edit_and_a_speed_change_is() {
        let (a, b) = (MediaId::new(), MediaId::new());
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut seq = Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![
            klipp_timeline::Clip::media(a, tc(0.0), tc(1.0), tc(0.0), tc(1.0)),
            klipp_timeline::Clip::media(b, tc(1.0), tc(2.0), tc(0.0), tc(1.0)),
        ];
        let mut music = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        music.clips = vec![klipp_timeline::Clip::media(b, tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        seq.tracks = vec![v1, music];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.record_audio_at(tc(0.5));

        // A video-only cut over continuous music: other clips, no edit.
        let before = engine.last_audio_fingerprint;
        assert!(!engine.audio_edited_at(tc(1.2)), "crossing a cut is not an edit");
        assert_ne!(engine.last_audio_fingerprint, before, "the sound at the new clip is recorded");
        assert!(!engine.audio_edited_at(tc(1.5)), "nor is playing on in it");

        // The clip under the playhead sped up: an edit.
        sequence.write().unwrap().tracks[0].clips[1].speed = 2.0;
        assert!(engine.audio_edited_at(tc(1.5)), "a speed change is an edit");

        // A seek back to the first clips is not an edit either.
        assert!(!engine.audio_edited_at(tc(0.2)));
    }
}

#[cfg(test)]
mod colour_pipeline_gate_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #1278: the colour pipeline switched on with no LUT loaded changes no
    /// pixel, so it keeps no frame off the fast paths. With a LUT loaded it
    /// does, as the renderer then applies it.
    #[test]
    fn the_colour_pipeline_blocks_the_fast_paths_only_with_a_lut() {
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default()));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            sequence, Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        // A paused present: no "not playing" ahead of the display gates.
        let blocked = |e: &PlaybackEngine| e.display_path_blockers(FastPath::PausedPresent).contains(&"global colour pipeline active");
        assert!(!blocked(&engine), "off");
        engine.color_pipeline.enabled = true;
        assert!(!blocked(&engine), "on, with no LUT loaded");
        let path = std::env::temp_dir().join(format!("klipp_gate_lut_{}.cube", std::process::id()));
        klipp_color::lut::write_cube(&path, &[[0.0; 3]; 8], 2, "gate-test").unwrap();
        let loaded = engine.color_pipeline.load_global_lut(&path);
        let _ = std::fs::remove_file(&path);
        loaded.unwrap();
        assert!(blocked(&engine), "on, with a LUT");
        engine.color_pipeline.enabled = false;
        assert!(!blocked(&engine), "a LUT loaded but switched off");
    }
}

#[cfg(test)]
mod title_fast_path_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #2351: a title over the primary clip keeps the frame off both fast
    /// paths while the GPU stack may composite: the stack lays the title out
    /// on the GPU, as an export does, where the fast paths would lay a CPU
    /// raster over the decoded bytes. Only with the stack switched off
    /// (`KLIPP_GPU_COMPOSITE=0`) do they take it, as the no-GPU fallback.
    #[test]
    fn a_title_over_the_clip_takes_the_gpu_stack() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut seq = Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![klipp_timeline::Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        let mut text = klipp_timeline::Track::new("T1", klipp_timeline::TrackKind::Text);
        let title = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Over the clip".into(), ..Default::default() });
        text.clips = vec![klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(title), ..klipp_timeline::Clip::gap(tc(0.0), tc(2.0)) }];
        // Tracks run top first: the text over V1.
        seq.tracks = vec![text, v1];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.playhead = tc(1.0);
        engine.current_track_index = 1;
        if !klipp_compose::gpu_composite_enabled() || !direct_present_enabled() {
            return;
        }
        for (state, mode) in [(PlaybackState::Paused, FastPath::PausedPresent), (PlaybackState::Playing, FastPath::Playing)] {
            engine.state = state;
            let blockers = engine.fast_path_blockers_for(mode);
            assert!(blockers.contains(&"title layers: the GPU stack"), "{mode:?}: {blockers:?}");
        }
        // A slider dragged on the parked frame too: the stack takes it.
        engine.state = PlaybackState::Paused;
        engine.in_node_param_edit = true;
        let blockers = engine.fast_path_blockers_for(FastPath::ParkedDrag);
        assert!(blockers.contains(&"title layers: the GPU stack"), "ParkedDrag: {blockers:?}");
        assert!(engine.titled_drag_on_stack());
        engine.in_node_param_edit = false;

        // Without the title, the clip alone is no reason to leave them,
        // a drag included.
        sequence.write().unwrap().tracks.remove(0);
        engine.current_track_index = 0;
        engine.state = PlaybackState::Paused;
        let blockers = engine.fast_path_blockers_for(FastPath::PausedPresent);
        assert!(!blockers.iter().any(|b| b.contains("title")), "{blockers:?}");
        engine.in_node_param_edit = true;
        let blockers = engine.fast_path_blockers_for(FastPath::ParkedDrag);
        assert!(!blockers.iter().any(|b| b.contains("title")), "ParkedDrag: {blockers:?}");
        assert!(!PlaybackEngine::titles_over_clip(&sequence.read().unwrap(), engine.playhead));
    }
}

#[cfg(test)]
mod viewer_mode_route_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #2758: a channel view and the A/B wipe of a node are drawn by the
    /// GPU stack, so each keeps the frame off both fast paths, by name,
    /// and leaves the stack free to take it. Named on the display
    /// blockers they sent the viewer to the CPU's reference route.
    #[test]
    fn a_channel_view_and_a_node_wipe_take_the_gpu_stack() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut seq = Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        let clip = klipp_timeline::Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let id = clip.id;
        v1.clips = vec![clip];
        seq.tracks = vec![v1];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.playhead = tc(1.0);
        if !klipp_compose::gpu_composite_enabled() || !direct_present_enabled() {
            return;
        }
        let node = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: id.0 }, [uuid::Uuid::new_v4()]);
        let modes: [(&str, Box<dyn Fn(&mut PlaybackEngine, bool)>); 2] = [
            ("channel view", Box::new(|e, on| {
                e.handle_command(AppCommand::SetViewChannel { channel: if on { klipp_core::ViewChannel::Alpha } else { klipp_core::ViewChannel::Picture } });
            })),
            ("compare mode active", Box::new(move |e, on| {
                e.handle_command(AppCommand::SetSplitCompareNode { clip_id: on.then_some(id), path: on.then(|| node.clone()) });
            })),
        ];
        for (name, set) in &modes {
            set(&mut engine, true);
            for (state, mode) in [(PlaybackState::Paused, FastPath::PausedPresent), (PlaybackState::Playing, FastPath::Playing)] {
                engine.state = state;
                assert!(engine.fast_path_blockers_for(mode).contains(name), "{name}, {mode:?}: off the fast paths");
                assert!(engine.gpu_composite_allowed(), "{name}, {mode:?}: the stack may take the frame");
            }
            engine.state = PlaybackState::Paused;
            set(&mut engine, false);
            assert!(!engine.fast_path_blockers_for(FastPath::PausedPresent).contains(name), "{name} off");
        }
    }
}

#[cfg(test)]
mod view_past_grade_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #2879: a node of a clip's chain past its grade, viewed, is drawn by
    /// cutting the chain there: the frame's sequence holds the clip cut,
    /// the inspection carries no solo, the fast paths stand aside by name
    /// and the GPU stack may take the frame. A grade's own node is left to
    /// the inspection, as before.
    #[test]
    fn a_node_past_the_grade_is_drawn_cut_on_the_stack() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut clip = klipp_timeline::Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let root = klipp_graph_format::path::Root::Clip { clip: clip.id.0 };
        let display = clip.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SCENE_TO_DISPLAY).unwrap().id;
        let mk = |e: klipp_effects::BuiltinEffect| -> klipp_graph_format::Node {
            let payload = serde_json::to_value(&e).unwrap();
            serde_json::from_value(serde_json::json!({
                "id": klipp_graph_format::NodeId::new_v4(),
                "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
                "legacy": { "payload": payload },
            })).unwrap()
        };
        let (glow, invert) = (mk(klipp_effects::BuiltinEffect::Glow { radius: 6.0, intensity: 0.8, threshold: 0.5 }), mk(klipp_effects::BuiltinEffect::Invert { mix: 1.0 }));
        let at = |id| klipp_graph_format::path::NodePath::new(root, [id]);
        clip.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::AddNode { after: at(display), node: Box::new(glow.clone()) },
            klipp_graph_lift::ops::GraphOp::AddNode { after: at(glow.id), node: Box::new(invert.clone()) },
        ]).unwrap();
        let id = clip.id;
        let mut seq = Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![clip];
        seq.tracks = vec![v1];

        let solo = klipp_compose::InspectAt::out(at(glow.id));
        let mut cut = seq.clone();
        assert!(super::view::cut_for_solo(&mut cut, &solo), "the glow is past the grade, on the chain");
        let enabled = |s: &Sequence, n| s.all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap().picture.graph.nodes.iter().find(|x| x.id == n).unwrap().enabled;
        assert!(enabled(&cut, glow.id) && !enabled(&cut, invert.id), "the invert after it is off");
        assert!(enabled(&seq, invert.id), "only the frame's copy is cut");
        // A grade's own node: the inspection shows it.
        let grade = seq.all_clips().find(|c| c.id == id).unwrap().grade().nodes[0].id;
        assert!(!super::view::cut_for_solo(&mut seq.clone(), &klipp_compose::inspect::grade_node(id, grade)));

        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.playhead = tc(1.0);
        engine.solo = Some(solo);
        assert!(engine.inspect_request(&sequence.read().unwrap(), Some(id), true).solo.is_none(), "drawn cut, not inspected");
        if !klipp_compose::gpu_composite_enabled() || !direct_present_enabled() {
            return;
        }
        for (state, mode) in [(PlaybackState::Paused, FastPath::PausedPresent), (PlaybackState::Playing, FastPath::Playing)] {
            engine.state = state;
            assert!(engine.fast_path_blockers_for(mode).contains(&"node viewed past the grade"), "{mode:?}");
            assert!(engine.gpu_composite_allowed(), "{mode:?}: the stack may take it");
        }
    }
}

#[cfg(test)]
mod preroll_retry_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #576: the look-ahead asks for the next clip's preroll on every frame
    /// of the last 1.5 s. When that clip's file will not open, the failure
    /// is remembered, so no second worker is started for it; clearing the
    /// preroll (a cut, a seek, a stop) forgets it.
    #[test]
    fn a_preroll_that_failed_is_not_started_again_for_that_clip() {
        let missing = MediaId::new();
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/nonexistent/klipp-preroll-test.mov".into(), None);
        item.id = missing;
        pool.add(item);
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut seq = Sequence::new_default();
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![
            klipp_timeline::Clip::media(missing, tc(0.0), tc(1.0), tc(0.0), tc(1.0)),
            klipp_timeline::Clip::media(missing, tc(1.0), tc(2.0), tc(0.0), tc(1.0)),
        ];
        seq.tracks = vec![v1];
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.current_track_index = 0;
        engine.current_clip_index = 0;

        engine.start_preroll_for_next_clip();
        assert!(engine.preroll_pending.is_some(), "the first ask starts a worker");
        engine.take_ready_preroll(Some(Duration::from_secs(5)));
        assert!(engine.preroll_pending.is_none() && engine.preroll_decoder.is_none());
        assert_eq!(engine.preroll_failed, Some(1), "the failure is remembered for clip 1");

        engine.start_preroll_for_next_clip();
        assert!(engine.preroll_pending.is_none(), "no second worker for a clip that will not open");

        engine.clear_preroll();
        engine.start_preroll_for_next_clip();
        assert!(engine.preroll_pending.is_some(), "after a cut it is tried again");

        // A seek forgets it too, and drops a preroll made for where the
        // playhead was.
        engine.take_ready_preroll(Some(Duration::from_secs(5)));
        assert_eq!(engine.preroll_failed, Some(1));
        engine.seek_in_sequence_with_timeout(tc(0.2), Duration::from_millis(50));
        assert_eq!(engine.preroll_failed, None, "a seek forgets the failure");
        assert!(engine.preroll_pending.is_none() && engine.preroll_clip_index.is_none());
        engine.clear_preroll();
    }
}

#[cfg(test)]
mod paused_refresh_tests {
    use super::*;
    use std::time::Instant;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// A two-second 64×48 test pattern at 30 fps. `None` without ffmpeg.
    fn source() -> Option<PathBuf> {
        let out = std::env::temp_dir().join(format!("klipp-paused-refresh-{}-{}.mp4", std::process::id(), klipp_core::ClipId::new()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=64x48:r=30:d=2", "-pix_fmt", "yuv420p"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    }

    /// #2136: stopping midway through a keyed move publishes that exact
    /// frame and wakes the reactive UI, without seeking the decoder or
    /// replacing its GPU source with the CPU pre-grade cache.
    #[test]
    fn pause_keeps_the_gpu_source_and_wakes_the_viewer_within_100ms() {
        let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else { return };
        let Some(path) = source() else { return };
        let tc = |f| RationalTime::from_frames(f, 30);
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.fps = klipp_core::Fps::new(30, 1);
        item.duration = tc(60);
        item.resolution = klipp_core::Resolution { width: 64, height: 48 };
        item.has_audio = false;
        let id = item.id;
        let mut pool = klipp_media::MediaPool::new();
        pool.add(item);
        let clip = klipp_timeline::test_clip::TestClip::of(
            klipp_timeline::Clip::media(id, tc(0), tc(60), tc(0), tc(60)))
            .lane("scale", 0.0, 0.5).lane("scale", 2.0, 0.9).build();
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(30, 1);
        seq.resolution = klipp_core::Resolution { width: 64, height: 48 };
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        let overlay = klipp_timeline::Clip {
            kind: klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [20, 200, 40, 255] }),
            ..klipp_timeline::Clip::gap(tc(0), tc(60))
        };
        let overlay = klipp_timeline::test_clip::TestClip::of(overlay)
            .lane("scale", 0.0, 0.1).lane("pos_x", 0.0, -20.0).build();
        let mut upper = klipp_timeline::Track::new("overlay", klipp_timeline::TrackKind::Video);
        upper.clips = vec![overlay];
        seq.tracks = vec![upper, track];
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let shown = Arc::new(Mutex::new(crate::texture::FrameBuffer::new(64, 48)));
        let wakes = Arc::new(AtomicU64::new(0));
        let wake_count = Arc::clone(&wakes);
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)),
            Box::new(move || { wake_count.fetch_add(1, Ordering::Relaxed); }),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.state = PlaybackState::Paused;
        engine.seek_in_sequence_with_timeout(tc(30), Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.pending_paused_refresh && Instant::now() < deadline {
            engine.tick_paused_refresh();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!engine.pending_paused_refresh);
        // A known decoded GPU picture, deliberately unlike the parked
        // CPU cache, so selecting the old cache cannot pass by accident.
        let bytes: Vec<u8> = [180, 40, 80, 255].repeat(64 * 48);
        let buffer = Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pause regression source"), size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        ctx.queue.write_buffer(&buffer, 0, &bytes);
        let mut frame = crate::types::LinearFrame::eof();
        frame.is_eof = false;
        frame.pts = tc(30);
        frame.width = 64;
        frame.height = 48;
        frame.gpu_source = Some(Arc::new(crate::frame::GpuFrame {
            device: Arc::clone(&ctx.device), queue: Arc::clone(&ctx.queue),
            rgba_buffer: buffer, width: 64, height: 48,
        }));
        let held = streams::HeldFrame::from_frame(frame);
        engine.state = PlaybackState::Playing;
        engine.present_frame(Some(held));
        // Warm the paused render's pipelines too; the deadline measures
        // pause delivery, not driver compilation on this test machine.
        engine.state = PlaybackState::Paused;
        engine.regrade_cached_frame();
        engine.state = PlaybackState::Playing;
        let epoch = engine.v1_decoder.as_ref().unwrap().requested_seek();
        let before = wakes.load(Ordering::Relaxed);
        let generation = shown.lock().unwrap().latest_generation();
        ev_rx.try_iter().for_each(drop);
        let start = Instant::now();
        engine.handle_command(AppCommand::Pause);
        assert!(start.elapsed() < Duration::from_millis(100), "pause took {:?}", start.elapsed());
        assert_eq!(engine.state, PlaybackState::Paused);
        let residency = klipp_effects::residency::last_frame();
        assert_eq!(residency.fallback_count(), 0, "paused GPU composition fell back: {residency:?}");
        assert_eq!(engine.renderer.executor().last_run_chain_stats().cpu_nodes, 0);
        assert_eq!(engine.v1_decoder.as_ref().unwrap().requested_seek(), epoch, "pause sought again");
        assert!(wakes.load(Ordering::Relaxed) > before);
        assert!(ev_rx.try_iter().any(|e| matches!(e, AppEvent::FrameReady)));
        let mut fb = shown.lock().unwrap();
        assert!(fb.latest_generation() > generation);
        assert_eq!(fb.shown_presented(), Some(tc(30)));
        assert!(fb.latest_complete());
        fb.swap_pending();
        let center = (24 * 64 + 32) * 4;
        assert!(fb.data[center] > 150 && fb.data[center + 1] < 70, "wrong paused picture");
        drop(fb);
        assert!(matches!(engine.retained_primary_at_playhead().unwrap().source_pixels(), klipp_compose::SourcePixels::Gpu(_)));
        engine.regrade_cached_frame();
        assert!(matches!(engine.retained_primary_at_playhead().unwrap().source_pixels(), klipp_compose::SourcePixels::Gpu(_)));
        let retained = engine.retained_primary.clone();
        let pregrade = engine.cached_pregrade_frame.clone();
        assert!(retained.is_some() && pregrade.is_some());
        engine.playhead = tc(31);
        assert!(engine.retained_primary_at_playhead().is_none(), "another time reused the frame");
        engine.playhead = tc(30);
        engine.sequence.write().unwrap().tracks[1].clips[0].source_in = tc(1);
        assert!(engine.retained_primary_at_playhead().is_none(), "a source edit reused the frame");
        assert!(engine.note_layout_change(true));
        assert!(engine.cached_pregrade_frame.is_none());
        assert!(engine.retained_primary.is_none(), "a layout change kept the old source");

        engine.retained_primary = retained.clone();
        engine.cached_pregrade_frame = pregrade.clone();
        engine.reset_to_idle();
        assert!(engine.cached_pregrade_frame.is_none());
        assert!(engine.retained_primary.is_none(), "reset kept the old source");

        // Stopped playback does not seek on this command, so this pins
        // the command's own invalidation instead of the seek's clearing.
        engine.state = PlaybackState::Stopped;
        engine.retained_primary = retained;
        engine.cached_pregrade_frame = pregrade;
        engine.handle_command(AppCommand::MediaDecodeChanged);
        assert!(engine.cached_pregrade_frame.is_none());
        assert!(engine.retained_primary.is_none(), "decode settings kept the old source");
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    /// A pick on a node's input that comes while the parked frame is still
    /// on its way (no pre-grade yet, a paused refresh still to drain)
    /// waits for the frame and is answered once it is parked, never
    /// refused for want of it: under load a pick sent after a seek was
    /// refused with "could not be read on this frame".
    #[test]
    fn a_pick_while_its_frame_is_coming_waits_for_it() {
        use klipp_timeline::GradeAccess;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = klipp_core::Fps::new(30, 1);
        let tc = |f: i64| RationalTime { value: f, rate: 30 };
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.fps = fps;
        item.duration = tc(60);
        item.resolution = klipp_core::Resolution { width: 64, height: 48 };
        item.has_audio = false;
        let id = item.id;
        let mut pool = klipp_media::MediaPool::new();
        pool.add(item);
        // Source -> Qualifier -> Output: the pick reads the Qualifier's input.
        let node = |kind, params| klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
        let nodes = vec![
            node(klipp_nodes::NodeKind::Source, Default::default()),
            node(klipp_nodes::NodeKind::Qualifier, klipp_nodes::NodeParams { qualifier: Some(Default::default()), ..Default::default() }),
            node(klipp_nodes::NodeKind::Output, Default::default()),
        ];
        let q = nodes[1].id;
        let edges = nodes.windows(2).map(|p| klipp_nodes::Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
        let graph = klipp_nodes::NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
        let mut clip = klipp_timeline::Clip::media(id, tc(0), tc(60), tc(0), tc(60));
        clip.set_grade(graph);
        let clip_id = clip.id;
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = klipp_core::Resolution { width: 64, height: 48 };
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![clip];
        seq.tracks = vec![v1];
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let shown = Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.state = PlaybackState::Paused;
        engine.seek_in_sequence_with_timeout(tc(10), Duration::from_secs(5));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.pending_paused_refresh && std::time::Instant::now() < deadline {
            engine.tick_paused_refresh();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(engine.parked_pregrade().is_some(), "frame 10 is parked");
        let picks = |rx: &crossbeam_channel::Receiver<AppEvent>| rx.try_iter().filter_map(|e| match e { AppEvent::NodeInputSampled { sample, .. } => Some(sample), _ => None }).collect::<Vec<_>>();
        let _ = picks(&ev_rx);

        // The frame still coming, as after a seek that missed it: no
        // pre-grade of it, and a paused refresh still to drain.
        let pregrade = engine.cached_pregrade_frame.take();
        engine.pending_paused_refresh = true;
        let at = klipp_compose::inspect::grade_node(clip_id, q).path;
        engine.sample_node_input(clip_id, q, &at, 0.5, 0.5);
        assert_eq!(picks(&ev_rx), vec![], "a pick refused while its frame was coming");
        engine.tick_node_sample();
        assert_eq!(picks(&ev_rx), vec![], "a pick refused while its frame was coming");

        // The frame lands and is parked: the pick is answered from it on
        // the next tick.
        engine.cached_pregrade_frame = pregrade;
        engine.pending_paused_refresh = false;
        engine.tick_node_sample();
        let answered = picks(&ev_rx);
        assert!(matches!(answered.as_slice(), [Ok(_)]), "{answered:?}");
    }

    /// #1092: a seek that missed its frame arms the paused refresh. A
    /// later park or step answered from the frame cache must disarm it:
    /// left
    /// armed, the refresh drained on a later tick and presented a frame
    /// short of the playhead over the cached one, marked incomplete, and
    /// with the decoder paused nothing ever completed it.
    #[test]
    fn a_move_answered_from_the_cache_disarms_an_earlier_refresh() {
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = klipp_core::Fps::new(30, 1);
        let tc = |f: i64| RationalTime { value: f, rate: 30 };
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.fps = fps;
        item.duration = tc(60);
        item.resolution = klipp_core::Resolution { width: 64, height: 48 };
        item.has_audio = false;
        let id = item.id;
        let mut pool = klipp_media::MediaPool::new();
        pool.add(item);
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![klipp_timeline::Clip::media(id, tc(0), tc(60), tc(0), tc(60))];
        seq.tracks = vec![v1];
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let shown = Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.state = PlaybackState::Paused;

        // Park on frame 10 and wait for it, which opens the clip's
        // decoder; then hold frame 10 in the cache, as a frame shown
        // while playing is.
        engine.seek_in_sequence_with_timeout(tc(10), Duration::from_secs(5));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.pending_paused_refresh && std::time::Instant::now() < deadline {
            engine.tick_paused_refresh();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!engine.pending_paused_refresh, "frame 10 arrived");
        let grey = Pregrade::new(PregradePixels::Srgb8(Arc::new(vec![128; 64 * 48 * 4])));
        engine.frame_cache.insert(10, engine.primary_clip_now(), grey, 64, 48);

        // A seek elsewhere that missed its frame, long enough ago that its
        // refresh would show a frame short of the playhead.
        engine.pending_paused_refresh = true;
        engine.paused_refresh_since = Some(std::time::Instant::now() - Duration::from_secs(1));

        // Back to frame 10: answered from the cache, shown complete.
        engine.seek_in_sequence_with_timeout(tc(10), Duration::from_millis(50));
        // The newest frame written; no UI thread swaps it in here.
        let presented = |fb: &Mutex<crate::texture::FrameBuffer>| {
            let fb = fb.lock().unwrap();
            (fb.shown_presented(), fb.latest_complete())
        };
        assert_eq!(presented(&shown), (Some(tc(10)), true), "the cached frame is shown, complete");
        assert!(!engine.pending_paused_refresh, "the earlier seek's refresh is disarmed");

        // Ticks while paused leave it so.
        for _ in 0..20 {
            engine.tick_paused_refresh();
            std::thread::sleep(Duration::from_millis(5));
            assert_eq!(presented(&shown), (Some(tc(10)), true), "a tick replaced the cached frame");
        }

        // A step onto a cached frame disarms it the same way.
        let grey = Pregrade::new(PregradePixels::Srgb8(Arc::new(vec![96; 64 * 48 * 4])));
        engine.frame_cache.insert(11, engine.primary_clip_now(), grey, 64, 48);
        engine.pending_paused_refresh = true;
        engine.paused_refresh_since = Some(std::time::Instant::now() - Duration::from_secs(1));
        engine.step_frame(1);
        assert_eq!(presented(&shown), (Some(tc(11)), true), "the stepped-to cached frame is shown, complete");
        assert!(!engine.pending_paused_refresh, "the step disarmed the earlier refresh");
        for _ in 0..20 {
            engine.tick_paused_refresh();
            std::thread::sleep(Duration::from_millis(5));
            assert_eq!(presented(&shown), (Some(tc(11)), true), "a tick replaced the stepped-to frame");
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod report_floor_tests {
    use super::*;
    use klipp_timeline::Sequence;
    use std::sync::Mutex;

    /// #1806: while playing, the playhead said never steps back behind
    /// the last one said (a late frame behind the clock), the stop's
    /// report included; a seek or a loop says its jump back, and reports
    /// go on from there; paused, a report says where the playhead is.
    #[test]
    fn a_playing_report_never_steps_back_but_a_jump_does() {
        let tc = |f: i64| RationalTime { value: f, rate: 25 };
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![klipp_timeline::Clip::gap(tc(0), tc(100))];
        seq.tracks = vec![v1];
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16))), Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let said = |rx: &crossbeam_channel::Receiver<AppEvent>| {
            rx.try_iter().filter_map(|e| match e { AppEvent::PlayheadMoved(t) => Some(t.value), _ => None }).collect::<Vec<_>>()
        };
        let _ = said(&ev_rx);
        engine.state = PlaybackState::Playing;
        for f in [25, 26, 25, 27, 26, 28] {
            engine.playhead = tc(f);
            engine.report_playhead();
        }
        assert_eq!(said(&ev_rx), vec![25, 26, 26, 27, 27, 28], "a late frame never steps the playhead back");
        // A loop back while playing says its jump, and reports follow on.
        engine.report_jump(tc(5));
        engine.playhead = tc(6);
        engine.report_playhead();
        assert_eq!(said(&ev_rx), vec![5, 6]);
        // Played backwards (J): a ceiling, not a floor, so the playhead
        // runs back and a late frame never steps it forwards.
        engine.handle_command(AppCommand::SetSpeed(-1.0));
        for f in [25, 24, 25, 23] {
            engine.playhead = tc(f);
            engine.report_playhead();
        }
        assert_eq!(said(&ev_rx), vec![25, 24, 24, 23], "backwards, never forwards");
        // Turned round (L): forwards from where it is, at once.
        engine.handle_command(AppCommand::SetSpeed(1.0));
        for f in [20, 21, 20] {
            engine.playhead = tc(f);
            engine.report_playhead();
        }
        assert_eq!(said(&ev_rx), vec![20, 21, 21], "forwards again from where it turned");
        // A ceiling left from backwards play is not kept by a change of
        // direction found on the clock alone either.
        engine.master_clock.set_speed(-2.0);
        engine.playhead = tc(30);
        engine.report_playhead();
        assert_eq!(said(&ev_rx), vec![30]);
        engine.master_clock.set_speed(1.0);
        // Paused: where the playhead is, back or not.
        engine.state = PlaybackState::Paused;
        engine.playhead = tc(3);
        engine.report_playhead();
        assert_eq!(said(&ev_rx), vec![3]);
    }
}

/// A probed file's duration (#1910): counted in frames of its picture, or,
/// for sound alone, in its own samples. A WAV of 1.1 s timed at a frame
/// rate it doesn't have lost its last 0.017 s to the rounding, and a trim
/// to its real end was refused.
fn probed_duration(secs: f64, picture_fps: Option<Fps>, sample_rate: Option<u32>) -> RationalTime {
    match (picture_fps, sample_rate) {
        (Some(fps), _) => RationalTime::from_secs_f64(secs, fps.rate()),
        (None, Some(rate)) if rate > 0 => RationalTime::from_secs_f64(secs, rate),
        (None, _) => RationalTime::from_secs_f64(secs, Fps::film_24().rate()),
    }
}

#[cfg(test)]
mod probed_duration_tests {
    use super::*;

    /// Sound alone keeps its length to the sample; a picture is timed in
    /// its frames as before.
    #[test]
    fn a_sound_is_timed_in_its_samples() {
        let d = probed_duration(1.1, None, Some(48000));
        assert_eq!((d.value, d.rate), (52800, 48000));
        assert!((d.to_secs_f64() - 1.1).abs() < 1.0 / 48000.0);
        let v = probed_duration(1.1, Some(Fps::film_24()), Some(48000));
        assert_eq!(v.rate, Fps::film_24().rate(), "a picture keeps its frame rate");
        assert_eq!(probed_duration(2.0, None, None).rate, Fps::film_24().rate(), "nothing to time it by: the default");
    }
}


#[cfg(test)]
mod track_automation_override_tests {
    use super::*;
    use klipp_core::automation::TrackParam;
    use klipp_core::Keyframe;
    use std::sync::Mutex;

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// #2306: a control moved or recorded plays at the value it is set to,
    /// over its keys or its value as set, so what is heard is what is
    /// written; let go, it plays from them again.
    #[test]
    fn an_overridden_control_plays_its_override() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut keyed = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        keyed.automation.lanes.insert(TrackParam::Volume, vec![key(0.0, 1.0), key(10.0, 1.0)]);
        let mut plain = klipp_timeline::Track::new("A2", klipp_timeline::TrackKind::Audio);
        plain.volume = 0.5;
        seq.tracks = vec![keyed, plain];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq.clone()));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let volumes = || engine.mixer.lock().unwrap().tracks.iter().map(|t| t.volume).collect::<Vec<f32>>();
        engine.sync_track_automation(&seq, 1.0);
        assert_eq!(volumes()[0], 1.0, "from its keys");
        engine.automation_override.lock().unwrap().extend([((0, TrackParam::Volume), 0.25), ((1, TrackParam::Volume), 0.8)]);
        engine.sync_track_automation(&seq, 1.0);
        assert_eq!(volumes()[..2], [0.25, 0.8], "as moved, over the keys and the value as set");
        engine.automation_override.lock().unwrap().clear();
        engine.sync_track_automation(&seq, 1.0);
        assert_eq!(volumes()[..2], [1.0, 0.5], "let go: the keys, and the value as set");
    }
}

#[cfg(test)]
mod track_switch_tests {
    use super::*;
    use std::sync::Mutex;

    fn engine(seq: klipp_timeline::Sequence) -> (PlaybackEngine, Arc<klipp_timeline::SharedSequence>) {
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        (engine, sequence)
    }

    fn two_tracks() -> klipp_timeline::Sequence {
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks = vec![
            klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio),
            klipp_timeline::Track::new("A2", klipp_timeline::TrackKind::Audio),
        ];
        seq
    }

    /// The mix of a loud A1 and a quiet A2, as the mix thread calls it.
    fn mix(engine: &PlaybackEngine) -> Vec<f32> {
        let (a1, a2) = (vec![0.5_f32; 256], vec![0.1_f32; 256]);
        let mut m = engine.mixer.lock().unwrap();
        // Each track has its lane by the time it plays.
        m.ensure_tracks(2);
        m.mix(&[(0, &a1), (1, &a2)])
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    /// #2672: a muted track stays silent once the timeline takes the
    /// sound back from the source viewer, as every Play does.
    #[test]
    fn a_muted_track_stays_muted_through_the_viewer_handoff() {
        let (mut engine, _seq) = engine(two_tracks());
        let both = peak(&mix(&engine));
        engine.handle_command(AppCommand::SetTrackMute { track_index: 0, muted: true });
        let muted = peak(&mix(&engine));
        assert!(muted < both * 0.5, "muted A1 still heard: {muted} of {both}");
        engine.set_source_audio_owner(true);
        assert_eq!(peak(&mix(&engine)), 0.0, "the source viewer has the sound");
        engine.set_source_audio_owner(false);
        assert_eq!(peak(&mix(&engine)), muted, "handing the sound back unmuted A1");
        engine.handle_command(AppCommand::SetTrackMute { track_index: 0, muted: false });
        assert_eq!(peak(&mix(&engine)), both);
    }

    /// Solo is one track at a time, in the mix as in the app: soloing A2
    /// after A1 hears A2 alone.
    #[test]
    fn a_second_solo_replaces_the_first() {
        let (mut engine, seq) = engine(two_tracks());
        engine.handle_command(AppCommand::SetTrackSolo { track_index: 1, solo: true });
        let a2 = peak(&mix(&engine));
        engine.handle_command(AppCommand::SetTrackSolo { track_index: 0, solo: true });
        engine.handle_command(AppCommand::SetTrackSolo { track_index: 1, solo: true });
        assert_eq!(peak(&mix(&engine)), a2);
        assert!(!seq.read().unwrap().tracks[0].solo);
        engine.set_source_audio_owner(true);
        engine.set_source_audio_owner(false);
        assert_eq!(peak(&mix(&engine)), a2, "the solo survives the handoff");
    }

    /// A mute the sequence carries, from a project opened or an undo,
    /// is heard as one, and a sequence update keeps a mute set since.
    #[test]
    fn the_sequences_mute_reaches_the_mix() {
        let both = peak(&mix(&engine(two_tracks()).0));
        let mut muted_a1 = two_tracks();
        muted_a1.tracks[0].muted = true;
        let (mut engine, _seq) = engine(muted_a1);
        engine.handle_command(AppCommand::SequenceUpdated);
        let heard = peak(&mix(&engine));
        assert!(heard < both * 0.5, "A1 muted in the sequence still heard: {heard} of {both}");
        engine.handle_command(AppCommand::SetTrackMute { track_index: 1, muted: true });
        engine.handle_command(AppCommand::SequenceUpdated);
        assert_eq!(peak(&mix(&engine)), 0.0, "a later update put A2's old switch back");
    }
}

#[cfg(test)]
mod routing_sync_tests {
    use super::*;
    use std::sync::Mutex;

    /// #2861: the sequence's buses and sends reach the mixer by index,
    /// each bus before the bus it feeds; a send keeps its ramp while it
    /// is the same send, a send to a bus that is gone is left out, and
    /// the buses go with the last one.
    #[test]
    fn the_sequences_routing_reaches_the_mixer() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let (mut verb, delay) = (klipp_timeline::AuxBus::new("Verb"), klipp_timeline::AuxBus::new("Delay"));
        verb.output = Some(delay.id);
        verb.volume = 0.5;
        let mut a1 = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        a1.sends = vec![klipp_timeline::AuxSend::new(verb.id), klipp_timeline::AuxSend { pre_fader: true, level: 0.25, ..klipp_timeline::AuxSend::new(delay.id) }];
        seq.tracks = vec![a1];
        // Listed delay first, but verb feeds it.
        seq.buses = vec![delay, verb];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq.clone()));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let sync = |seq: &klipp_timeline::Sequence| {
            let mut m = engine.mixer.lock().unwrap();
            engine.sync_routing(&mut m, seq, 0.0);
        };
        sync(&seq);
        {
            let mut m = engine.mixer.lock().unwrap();
            assert_eq!(m.buses.len(), 2);
            assert_eq!((m.buses[1].output, m.buses[1].volume, m.buses[0].output), (Some(0), 0.5, None));
            let lanes: Vec<(usize, f32, bool)> = m.tracks[0].sends.iter().map(|s| (s.bus, s.gain, s.pre_fader)).collect();
            assert_eq!(lanes, vec![(1, 1.0, false), (0, 0.25, true)]);
            m.tracks[0].sends[0].ramp_from = Some(0.75);
        }
        let mut changed = seq.clone();
        changed.tracks[0].sends[0].level = 0.5;
        sync(&changed);
        assert_eq!(engine.mixer.lock().unwrap().tracks[0].sends[0].ramp_from, Some(0.75), "the same send ramps on");
        changed.buses.remove(1);
        sync(&changed);
        let lanes: Vec<usize> = engine.mixer.lock().unwrap().tracks[0].sends.iter().map(|s| s.bus).collect();
        assert_eq!(lanes, vec![0], "the send to the bus gone is left out");
        changed.buses.clear();
        sync(&changed);
        let m = engine.mixer.lock().unwrap();
        assert!(m.buses.is_empty() && m.tracks[0].sends.is_empty());
    }
}

#[cfg(test)]
mod track_level_sync_tests {
    use super::*;
    use std::sync::Mutex;

    /// #3165: a track's volume and pan as the sequence has them reach its
    /// mixer lane on a sequence update, as an undo of a fader brings them;
    /// one its automation plays is left to the automation; a fader moved
    /// is kept in the sequence the update reads, so it is not put back.
    #[test]
    fn an_undone_fader_reaches_the_mix() {
        use klipp_timeline::TrackParam;
        let mut seq = klipp_timeline::Sequence::new_default();
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut keyed = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        keyed.automation.lanes.insert(TrackParam::Volume, vec![key(0.0, 1.0), key(5.0, 0.2)]);
        seq.tracks = vec![klipp_timeline::Track::new("A0", klipp_timeline::TrackKind::Audio), keyed];
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::clone(&sequence), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        engine.handle_command(AppCommand::SetTrackVolume { track_index: 0, volume: 0.3 });
        assert_eq!(engine.sequence.read().unwrap().tracks[0].volume, 0.3, "the move kept where the update reads");
        {
            let mut s = engine.sequence.write().unwrap();
            s.tracks[0].volume = 0.7;
            s.tracks[0].pan = -0.4;
            s.tracks[1].volume = 0.5;
        }
        {
            let mut m = engine.mixer.lock().unwrap();
            m.ensure_tracks(2);
            m.tracks[1].volume = 0.9;
        }
        engine.sync_track_levels();
        let m = engine.mixer.lock().unwrap();
        assert_eq!((m.tracks[0].volume, m.tracks[0].pan), (0.7, -0.4), "undone, heard");
        assert_eq!(m.tracks[1].volume, 0.9, "the automation's, untouched");
    }
}
