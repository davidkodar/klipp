//! Video decoder thread: ffmpeg demux + decode + colour convert →
//! [`FrameQueue`]. Owns one `ffmpeg_next::format::context::Input`
//! plus a `ffmpeg_next::decoder::Video` and drives them on a
//! dedicated OS thread; the playback engine only talks to it via
//! [`DecodeCommand`] and the queue.
//!
//! # Two-phase decode rule
//!
//! FFmpeg's `avcodec_send_packet` / `avcodec_receive_frame` pair is
//! **asymmetric** under the hood: the decoder may buffer several
//! input packets before producing a frame, and may produce several
//! frames from one packet. Mixing `send` and `receive` in a naïve
//! loop dead-locks on `EAGAIN`, and worst of all silently drops
//! packets when the send loses data after a queue-full refusal.
//!
//! Every send/receive site in this crate MUST follow:
//!
//! 1. **Drain first**: call `decoder.receive_frame(&mut f)` in a
//!    tight loop until it returns `EAGAIN`. Process each frame.
//! 2. **Then send**: call `decoder.send_packet(&p)`. If it returns
//!    `EAGAIN`, the packet is NOT consumed — stash it, drain again,
//!    retry. Never `let _ = send_packet(...)`; silent drops make
//!    A/V desync symptoms that are impossible to diagnose after the
//!    fact.
//! 3. **Non-target stream packets**: when the demuxed packet belongs
//!    to a stream we're not decoding (e.g. audio packets in the
//!    video decoder), do NOT `continue` the outer loop; fall through
//!    to the next demux step so the two-phase cycle stays intact.
//!
//! # Bit depth
//!
//! 8-bit sources → `AV_PIX_FMT_RGBA` (1 byte/channel through swscale).
//! 10+-bit sources → `AV_PIX_FMT_RGBA64LE` (2 bytes/channel) so the
//! extra precision reaches the linear-light conversion step without
//! being quantised at swscale time.
//!
//! # Hardware paths
//!
//! Decoder selection is CUDA NVDEC → VAAPI → software. VAAPI is
//! currently broken on NVIDIA for AV1/VP9 (all-zero frames from the
//! nvidia-vaapi-driver), so those codecs skip directly to software
//! even when VAAPI is available. See `open_gpu_decoder`.

use crate::frame::{Frame, GpuFrame};
use crate::types::{FrameQueue, LinearFrame, SharedFrameQueue};
use crossbeam_channel::{Receiver, Sender};
use klipp_color::transfer::srgb_to_linear;
use klipp_core::RationalTime;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Gate for the GPU-resident decode path. When on, NV12 / P010
/// hardware-decode output stays on the GPU as an `Arc<wgpu::Buffer>`
/// and the decoded `LinearFrame` carries a `gpu_source` attachment
/// alongside the usual CPU `data`. Phase 4d's grade chain binds the
/// buffer directly; until then this is a no-op for playback — same
/// shader dispatch, same readback, just the buffer lingers for the
/// consumer.
///
/// Read once per process via `OnceLock` so the env check doesn't
/// poke syscalls on the decode hot path.
fn should_attach_gpu_source() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        std::env::var("KLIPP_GPU_FRAMES").ok().as_deref() == Some("1")
    })
}

/// Detect whether the decoded source pixel format carries more than
/// 8 bits per component. Used to pick a matching swscale output
/// format: 8-bit sources use `AV_PIX_FMT_RGBA` (8 bpc), 10+-bit
/// sources use `AV_PIX_FMT_RGBA64LE` (16 bpc) so the extra precision
/// reaches the linear-light conversion step instead of being
/// quantised at swscale time.
unsafe fn source_is_high_bit_depth(fmt: ffmpeg_next::ffi::AVPixelFormat) -> bool {
    use ffmpeg_next::ffi::*;
    let desc = av_pix_fmt_desc_get(fmt);
    if desc.is_null() {
        return false;
    }
    let nb = (*desc).nb_components as usize;
    for i in 0..nb {
        if (*desc).comp[i].depth > 8 {
            return true;
        }
    }
    false
}

/// 256-entry precomputed LUT: `SRGB_U8_TO_LINEAR[b]` = linear f32 value
/// for the u8 sample `b`. Replaces the per-channel `powf(2.4)` call that
/// cost ~27 ms/frame at 1920×818 (measured via PERF hw_download
/// `linear_enc`) — a LUT lookup is a single L1 load, so the conversion
/// goes from ~5M pow calls/frame to ~5M loads/frame. Built once on first
/// use via `OnceLock`; 1 KB of static data.
fn srgb_u8_to_linear_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0_f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = srgb_to_linear(i as f32 / 255.0);
        }
        lut
    })
}

/// Same LUT but values pre-divided by 255 for the alpha channel (alpha
/// stays linear per sRGB spec — no gamma decode, just /255 normalisation).
fn u8_linear_div255_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0_f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = i as f32 / 255.0;
        }
        lut
    })
}

/// Per-camera-log override of the colour-tagged baseline. Read once
/// per process from `KLIPP_FORCE_LOG_FORMAT` (case-insensitive
/// tokens: `slog3`, `slog2`, `vlog`, `clog`, `clog3`, `logc3`,
/// `logc4`, `redlog3g10`, `bmdfilmgen5`, `dlog`, `flog`, `applelog`,
/// `nlog`, `goproflat`). Replaces only `transfer` + `primaries` —
/// matrix and range still come from the file (Sony / ARRI / Canon
/// log workflows all use BT.709 matrix on the YUV samples).
///
/// Stop-gap until the per-clip dropdown lands; the env var is the
/// least-invasive way to get colour-correct decoding for the user's
/// A6700 clip today. Tokens map to the same `(transfer, primaries)`
/// pair that the per-clip UI will produce later.
fn apply_env_log_override(
    base: klipp_core::SourceColorspace,
) -> klipp_core::SourceColorspace {
    use klipp_core::{ColorPrimaries, ColorTransfer};
    use std::sync::OnceLock;
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    let token = TOKEN
        .get_or_init(|| std::env::var("KLIPP_FORCE_LOG_FORMAT").ok().map(|s| s.to_ascii_lowercase()))
        .as_deref();
    let Some(t) = token else { return base; };
    let (transfer, primaries) = match t {
        "slog3" => (ColorTransfer::SLog3, ColorPrimaries::SGamut3Cine),
        "slog3-sgamut3" => (ColorTransfer::SLog3, ColorPrimaries::SGamut3),
        "slog2" => (ColorTransfer::SLog2, ColorPrimaries::SGamut3),
        "vlog" => (ColorTransfer::VLog, ColorPrimaries::VGamut),
        "clog" => (ColorTransfer::CanonLog, ColorPrimaries::CanonCinemaGamut),
        "clog3" => (ColorTransfer::CanonLog3, ColorPrimaries::CanonCinemaGamut),
        "logc3" => (ColorTransfer::LogC3, ColorPrimaries::AlexaWideGamut3),
        "logc4" => (ColorTransfer::LogC4, ColorPrimaries::AlexaWideGamut4),
        "redlog3g10" => (ColorTransfer::RedLog3G10, ColorPrimaries::RedWideGamut),
        "bmdfilmgen5" => (ColorTransfer::BmdFilmGen5, ColorPrimaries::BmdWideGamut),
        "dlog" => (ColorTransfer::DJIDLog, ColorPrimaries::Bt709),
        "flog" => (ColorTransfer::FLog, ColorPrimaries::Bt2020),
        "applelog" => (ColorTransfer::AppleLog, ColorPrimaries::Bt2020),
        "nlog" => (ColorTransfer::NLog, ColorPrimaries::Bt2020),
        "goproflat" => (ColorTransfer::GoProFlat, ColorPrimaries::Bt709),
        _ => return base,
    };
    klipp_core::SourceColorspace { transfer, primaries, ..base }
}

/// Look up swscale's per-matrix coefficient table via
/// `sws_getCoefficients`. The four-entry table (`[Cr, Cb, Cu, Cv]`)
/// drives the YUV→RGB matrix swscale uses internally.
///
/// SAFETY: `sws_getCoefficients` returns a pointer to static data
/// owned by libswscale — valid for the program's lifetime, no
/// allocation, no thread-safety concerns. The returned `[i32; 4]`
/// is a copy.
unsafe fn sws_get_coefficients_for_matrix(matrix: klipp_core::ColorMatrix) -> [i32; 4] {
    use ffmpeg_next::ffi::*;
    // libswscale colorspace IDs (see swscale.h `SWS_CS_*` constants).
    // The values map onto the corresponding AVColorSpace enum values
    // for the matrix-defining members; FCC and SMPTE 240M have their
    // own slots, BT.2020 has one entry, anything else falls back to
    // ITU709 which is also the "no metadata" default.
    const SWS_CS_ITU709: i32 = 1;
    const SWS_CS_FCC: i32 = 4;
    const SWS_CS_SMPTE170M: i32 = 5;   // also ITU601
    const SWS_CS_SMPTE240M: i32 = 7;
    const SWS_CS_BT2020: i32 = 9;
    let cs_id = match matrix {
        klipp_core::ColorMatrix::Bt709 => SWS_CS_ITU709,
        klipp_core::ColorMatrix::Bt601 => SWS_CS_SMPTE170M,
        klipp_core::ColorMatrix::Bt2020Ncl
        | klipp_core::ColorMatrix::Bt2020Cl => SWS_CS_BT2020,
        klipp_core::ColorMatrix::Smpte240 => SWS_CS_SMPTE240M,
        klipp_core::ColorMatrix::Fcc => SWS_CS_FCC,
        // Identity / YCgCo / ICtCp / chroma-derived: swscale doesn't
        // have a dedicated table, fall back to BT.709.
        _ => SWS_CS_ITU709,
    };
    let ptr = sws_getCoefficients(cs_id);
    // libswscale returns a 4-element table — copy it out.
    [ *ptr.add(0), *ptr.add(1), *ptr.add(2), *ptr.add(3) ]
}

/// sRGB-decode a RGBA u8 byte slice into a linear f32 RGBA buffer.
/// Alpha stays linear (no gamma on alpha, per sRGB spec).
fn rgba_u8_to_linear(rgba: &[u8]) -> Vec<f32> {
    let srgb_lut = srgb_u8_to_linear_lut();
    let a_lut = u8_linear_div255_lut();
    let n_pixels = rgba.len() / 4;
    let mut out = Vec::with_capacity(n_pixels * 4);
    // SAFETY: we're writing exactly n_pixels * 4 values below.
    unsafe { out.set_len(n_pixels * 4); }
    // Unrolled RGB+A path per pixel. Compiler auto-vectorises this on
    // x86_64 with AVX2 or better enabled by the default `cargo` profile.
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let o = i * 4;
        out[o]     = srgb_lut[chunk[0] as usize];
        out[o + 1] = srgb_lut[chunk[1] as usize];
        out[o + 2] = srgb_lut[chunk[2] as usize];
        out[o + 3] = a_lut[chunk[3] as usize];
    }
    out
}

/// sRGB-decode a RGBA64LE u16 byte slice (little-endian, 16 bpc) into
/// a linear f32 RGBA buffer. Used when the source is 10+-bit and
/// swscale was configured with `AV_PIX_FMT_RGBA64LE` — the extra
/// precision survives the sRGB decode because we're now in float.
fn rgba64_le_to_linear(bytes: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(8) {
        // Each pixel is 4 × u16 little-endian.
        let r = u16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 65535.0;
        let g = u16::from_le_bytes([chunk[2], chunk[3]]) as f32 / 65535.0;
        let b = u16::from_le_bytes([chunk[4], chunk[5]]) as f32 / 65535.0;
        let a = u16::from_le_bytes([chunk[6], chunk[7]]) as f32 / 65535.0;
        out.push(srgb_to_linear(r));
        out.push(srgb_to_linear(g));
        out.push(srgb_to_linear(b));
        out.push(a);
    }
    out
}

/// Commands sent to the decode thread.
pub enum DecodeCommand {
    SeekTo(f64),
    Pause,
    Resume,
    Stop,
}

/// Errors from the video decoder.
#[derive(Debug)]
pub enum DecoderError {
    Open(String),
    NoVideoStream,
    CodecNotFound,
    Codec(String),
    Scaler(String),
}

impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "failed to open: {e}"),
            Self::NoVideoStream => write!(f, "no video stream found"),
            Self::CodecNotFound => write!(f, "codec not found"),
            Self::Codec(e) => write!(f, "codec error: {e}"),
            Self::Scaler(e) => write!(f, "scaler error: {e}"),
        }
    }
}

pub struct VideoDecoder {
    frame_queue: SharedFrameQueue,
    decode_thread: Option<JoinHandle<()>>,
    decode_cmd_tx: Sender<DecodeCommand>,
    /// Set to true by the decode thread if VAAPI surfaces are actually produced
    pub hw_active: Arc<std::sync::atomic::AtomicBool>,
    /// Set to true on stop — checked by decode thread to exit quickly
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl VideoDecoder {
    /// Open a video file for decoding with optional VAAPI hardware acceleration.
    /// `source_rotation` is the media rotation in degrees (0, 90, 180, 270) from metadata.
    pub fn open(path: &Path, source_rotation: i32) -> Result<Self, DecoderError> {
        // Validate file can be opened before spawning the thread
        {
            let ctx = ffmpeg_next::format::input(path)
                .map_err(|e| DecoderError::Open(e.to_string()))?;
            let _stream = ctx
                .streams()
                .best(ffmpeg_next::media::Type::Video)
                .ok_or(DecoderError::NoVideoStream)?;
        }

        let queue_cap = FRAME_QUEUE_SIZE_PREF.load(std::sync::atomic::Ordering::Relaxed) as usize;
        let frame_queue: SharedFrameQueue = Arc::new(Mutex::new(FrameQueue::new(queue_cap)));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let queue_clone = frame_queue.clone();
        let path_owned = path.to_path_buf();
        let hw_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hw_flag = hw_active.clone();
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        let thread = std::thread::Builder::new()
            .name("klipp-vdec".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-vdec", || {
                    decode_thread_main(path_owned, queue_clone, cmd_rx, hw_flag, stop_flag_clone, source_rotation);
                });
            })
            .map_err(|e| DecoderError::Open(format!("spawn thread: {e}")))?;

        Ok(Self {
            frame_queue,
            decode_thread: Some(thread),
            decode_cmd_tx: cmd_tx,
            hw_active,
            stop_flag,
        })
    }

    pub fn frame_queue(&self) -> &SharedFrameQueue {
        &self.frame_queue
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::SeekTo(secs));
    }

    pub fn resume(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Resume);
    }

    pub fn pause(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Pause);
    }

    pub fn stop_detached(&mut self) {
        self.stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        self.decode_thread.take();
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        if let Some(thread) = self.decode_thread.take() {
            // Give the thread a reasonable time to exit, then detach
            let start = std::time::Instant::now();
            loop {
                if thread.is_finished() {
                    let _ = thread.join();
                    return;
                }
                if start.elapsed() > Duration::from_millis(500) {
                    warn!("decode_thread: join timed out, detaching");
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Hardware acceleration: CUDA (NVDEC) and VAAPI
// ---------------------------------------------------------------------------

/// Which hardware acceleration is active
#[derive(Debug, Clone, Copy, PartialEq)]
enum HwAccel {
    Cuda,
    Vaapi,
    Software,
}

/// Callback for FFmpeg codec negotiation — selects the preferred HW pixel format.
unsafe extern "C" fn cuda_get_format(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;

    let mut p = pix_fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == AVPixelFormat::AV_PIX_FMT_CUDA {
            info!("CUDA get_format: selected AV_PIX_FMT_CUDA");
            return AVPixelFormat::AV_PIX_FMT_CUDA;
        }
        p = p.add(1);
    }
    warn!("CUDA get_format: AV_PIX_FMT_CUDA not in candidate list, using fallback");
    *pix_fmts
}

unsafe extern "C" fn vaapi_get_format(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;

    let mut p = pix_fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == AVPixelFormat::AV_PIX_FMT_VAAPI {
            info!("VAAPI get_format: selected AV_PIX_FMT_VAAPI");
            return AVPixelFormat::AV_PIX_FMT_VAAPI;
        }
        p = p.add(1);
    }
    warn!("VAAPI get_format: AV_PIX_FMT_VAAPI not in candidate list, using fallback");
    *pix_fmts
}

/// Try to initialize CUDA (NVDEC) on the codec context. Returns true if successful.
/// CUDA works for all codecs on NVIDIA GPUs including AV1/VP9 (unlike VAAPI which
/// has broken av_hwframe_transfer for AV1 via nvidia-vaapi-driver).
pub(crate) unsafe fn try_init_cuda(codec_ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext) -> bool {
    use ffmpeg_next::ffi::*;

    let device_type = av_hwdevice_find_type_by_name(c"cuda".as_ptr());
    if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        warn!("CUDA: device type not found");
        return false;
    }

    let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
    let ret = av_hwdevice_ctx_create(
        &mut hw_device_ctx,
        device_type,
        std::ptr::null(), // auto-detect device
        std::ptr::null_mut(),
        0,
    );
    if ret < 0 {
        warn!("CUDA: av_hwdevice_ctx_create failed (ret={ret})");
        return false;
    }

    (*codec_ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ctx);
    (*codec_ctx_ptr).get_format = Some(cuda_get_format);
    av_buffer_unref(&mut hw_device_ctx);
    info!("Hardware decode: CUDA (NVDEC) initialized");
    true
}

/// Try to initialize VAAPI on the codec context. Returns true if successful.
pub(crate) unsafe fn try_init_vaapi(codec_ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext) -> bool {
    use ffmpeg_next::ffi::*;

    let device_type = av_hwdevice_find_type_by_name(c"vaapi".as_ptr());
    if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        warn!("VAAPI: device type not found");
        return false;
    }

    let device_paths: &[&[u8]] = &[
        b"/dev/dri/renderD128\0",
        b"/dev/dri/renderD129\0",
        b"\0",
    ];

    let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();

    for device_path in device_paths {
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            device_type,
            device_path.as_ptr() as *const _,
            std::ptr::null_mut(),
            0,
        );
        if ret >= 0 {
            (*codec_ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ctx);
            (*codec_ctx_ptr).get_format = Some(vaapi_get_format);
            let path_str = std::str::from_utf8(device_path)
                .unwrap_or("?")
                .trim_end_matches('\0');
            let path_display = if path_str.is_empty() { "auto" } else { path_str };
            info!("Hardware decode: VAAPI active on {path_display}");
            av_buffer_unref(&mut hw_device_ctx);
            return true;
        }
    }

    warn!("Hardware decode: VAAPI init failed on all device paths");
    false
}

/// Maximum playback resolution — frames larger than this are downscaled during
/// format conversion. This is the single biggest performance win: swscale does
/// both NV12/P010→RGBA conversion AND resize in one pass, and the resulting
/// RGBA buffer is 4× smaller (1920×1080 = 8MB vs 3840×2160 = 33MB).
static HARDWARE_DECODE_PREF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static MAX_PB_HEIGHT_PREF: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1080);
static FRAME_QUEUE_SIZE_PREF: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(16);
static DECODE_THREADS_PREF: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Optimised-playback policy. Encoded as a u8 atomic so the engine
/// can read it lock-free from the playback path:
///   0 = Auto (default) — use skim when source > 1080p OR > 30fps,
///       fall back to source otherwise.
///   1 = AlwaysOriginal — never substitute, even for heavy 4K60 sources.
///   2 = AlwaysOptimized — always use skim if available.
/// Unrecognised values fall back to Auto so a future enum extension
/// degrades gracefully.
static OPTIMISED_PLAYBACK_PREF: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_optimised_playback_mode(mode: u8) {
    OPTIMISED_PLAYBACK_PREF.store(mode, std::sync::atomic::Ordering::Relaxed);
}

pub fn optimised_playback_mode() -> u8 {
    OPTIMISED_PLAYBACK_PREF.load(std::sync::atomic::Ordering::Relaxed)
}

/// Decide whether the playback decoder should open the skim file
/// instead of the original source. Pure function — separated from the
/// engine so it's unit-testable. Three inputs:
///   * `mode`: 0 Auto / 1 AlwaysOriginal / 2 AlwaysOptimized.
///   * `source_height`, `source_fps`: source media stats.
///   * `skim_ready`: whether a finalised skim file exists on disk.
///
/// Heuristic in Auto mode: any one of "taller than 1080p" or "faster
/// than ~30 fps" pushes us off real-time on the current pipeline, so
/// substitute the skim when it's available.
pub fn should_use_skim_for_playback(
    mode: u8,
    source_height: u32,
    source_fps: f64,
    skim_ready: bool,
) -> bool {
    match mode {
        1 => false, // AlwaysOriginal
        2 => skim_ready,
        _ => {
            // Auto. Only meaningful when the skim file is actually on disk —
            // we don't want to silently swallow play because the proxy is
            // still being generated.
            if !skim_ready { return false; }
            let too_big = source_height > 1080;
            let too_fast = source_fps > 30.5;
            too_big || too_fast
        }
    }
}

/// Set whether hardware decode (NVDEC / VAAPI) is enabled from preferences.
pub fn set_hardware_decode(enabled: bool) {
    HARDWARE_DECODE_PREF.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Set decode thread count from preferences (0 = auto/all cores).
pub fn set_decode_threads(count: u32) {
    DECODE_THREADS_PREF.store(count, std::sync::atomic::Ordering::Relaxed);
}

/// Set the frame queue capacity from preferences.
pub fn set_frame_queue_size(size: u32) {
    FRAME_QUEUE_SIZE_PREF.store(size.max(4), std::sync::atomic::Ordering::Relaxed);
}

/// Set the max playback height from preferences.
pub fn set_max_playback_height(h: u32) {
    MAX_PB_HEIGHT_PREF.store(h, std::sync::atomic::Ordering::Relaxed);
}

fn max_playback_height() -> u32 {
    MAX_PB_HEIGHT_PREF.load(std::sync::atomic::Ordering::Relaxed)
}

fn max_playback_width() -> u32 {
    // Use 2:1 ratio to accommodate all aspect ratios (16:9, 21:9, 4:3, etc.)
    // The actual downscale is aspect-preserving — this just sets the bounding box.
    max_playback_height() * 2
}

/// Compute output dimensions that fit within MAX_PLAYBACK, preserving aspect ratio.
/// For 90°/270° rotation, the effective display dimensions are swapped before scaling.
fn playback_dimensions(src_w: u32, src_h: u32, rotation: i32) -> (u32, u32) {
    // For rotation, the effective display dimensions are swapped
    let (eff_w, eff_h) = if rotation == 90 || rotation == 270 {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    };
    if eff_w <= max_playback_width() && eff_h <= max_playback_height() {
        return (eff_w, eff_h);
    }
    let scale = (max_playback_width() as f64 / eff_w as f64)
        .min(max_playback_height() as f64 / eff_h as f64);
    let out_w = ((eff_w as f64 * scale) as u32) & !1; // even width
    let out_h = ((eff_h as f64 * scale) as u32) & !1; // even height
    (out_w.max(2), out_h.max(2))
}

/// Download a hardware surface frame (CUDA or VAAPI) to CPU, then convert+downscale to RGBA.
/// Uses raw FFI to avoid format enum mismatches between ffmpeg-next and system ffmpeg.
///
/// When `gpu_converter` is available and the transferred frame is NV12, uses the GPU compute
/// shader for YUV→RGBA conversion + resize + rotation (much faster than CPU swscale).
/// Falls back to swscale for non-NV12 formats (e.g. P010) or when the GPU converter is unavailable.
unsafe fn download_hw_frame(
    hw_frame: &ffmpeg_next::frame::Video,
    sws_ctx: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
    gpu_converter: Option<&mut klipp_effects::nv12_gpu::GpuNv12Converter>,
    gpu_p010: &mut Option<klipp_effects::p010_gpu::GpuP010Converter>,
    source_rotation: i32,
    cs_baseline: klipp_core::SourceColorspace,
) -> Option<LinearFrame> {
    use ffmpeg_next::ffi::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Accumulated per-stage timing across frames. Drained + logged every 60
    // frames via `PERF hw_download`. Diagnoses the "transfer=28ms/frame"
    // mystery by splitting it into: GPU→CPU download, NV12 GPU convert,
    // sRGB→linear CPU encode, CPU swscale fallback.
    static CALL_COUNT: AtomicU64 = AtomicU64::new(0);
    static PERF_HW_DL_US: AtomicU64 = AtomicU64::new(0);
    static PERF_NV12_CONV_US: AtomicU64 = AtomicU64::new(0);
    static PERF_LINEAR_ENCODE_US: AtomicU64 = AtomicU64::new(0);
    static PERF_SW_SCALE_US: AtomicU64 = AtomicU64::new(0);
    static PERF_GPU_PATH_COUNT: AtomicU64 = AtomicU64::new(0);
    static PERF_CPU_PATH_COUNT: AtomicU64 = AtomicU64::new(0);

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return None;
    }

    // Transfer from GPU to CPU (NV12 or P010)
    let t_hw_dl = std::time::Instant::now();
    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    PERF_HW_DL_US.fetch_add(t_hw_dl.elapsed().as_micros() as u64, Ordering::Relaxed);
    if ret < 0 {
        debug!(
            "HW transfer failed: ret={ret}, frame format={}, w={}, h={}",
            (*hw_frame.as_ptr()).format,
            (*hw_frame.as_ptr()).width,
            (*hw_frame.as_ptr()).height,
        );
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    let src_w = (*sw_frame).width as u32;
    let src_h = (*sw_frame).height as u32;
    let sw_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);
    let is_hi_bit_depth = source_is_high_bit_depth(sw_fmt);
    let (out_w, out_h) = playback_dimensions(src_w, src_h, source_rotation);

    // Resolve the source's colour metadata once per frame. Per-frame
    // tags from the AVFrame override the codec-context baseline only
    // where actually specified — NVDEC's `av_hwframe_transfer_data`
    // strips `color_range` (and sometimes the matrix) from downloaded
    // frames, so the AVCodecParameters values captured at decoder
    // init are the authoritative source for those tags. Every
    // `LinearFrame` constructed below stamps `source_cs` onto the
    // frame so downstream shaders can dispatch the correct YUV→RGB
    // matrix, range divisor, EOTF and gamut conversion.
    let source_cs = klipp_core::SourceColorspace::resolve_with_baseline(
        (*sw_frame).color_range as i32,
        (*sw_frame).colorspace as i32,
        (*sw_frame).color_primaries as i32,
        (*sw_frame).color_trc as i32,
        cs_baseline,
    );
    {
        use std::sync::atomic::AtomicBool;
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "decode colorspace: {} ({}x{}, sw_fmt={:?}, baseline={})",
                source_cs.describe(), src_w, src_h, sw_fmt,
                cs_baseline.describe(),
            );
        }
    }

    // Try GPU NV12→RGBA path: only for NV12 format with an available converter
    // Temporary diagnostic — log first frame only
    {
        use std::sync::atomic::AtomicBool;
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!("download_hw_frame: format={:?}, src={}x{}, out={}x{}, rotation={}, gpu_available={}, hi_bit_depth={}", sw_fmt, src_w, src_h, out_w, out_h, source_rotation, gpu_converter.is_some(), is_hi_bit_depth);
        }
    }
    // GPU P010 fast path — 10-bit YUV with optional HDR (PQ/BT.2020).
    // Lazy-created so SDR-only sessions never pay the wgpu init cost.
    // The shader handles BT.2020 → BT.709 gamut conversion + PQ EOTF
    // + Hable tone-map for HDR sources, or BT.709 + sRGB for SDR-10.
    // Outputs sRGB-u8 RGBA so the rest of the engine consumes it via
    // the existing rgba_u8_to_linear LUT (same path as NV12 below).
    if sw_fmt == AVPixelFormat::AV_PIX_FMT_P010LE && is_hi_bit_depth {
        if gpu_p010.is_none() {
            *gpu_p010 = klipp_effects::p010_gpu::GpuP010Converter::new();
            if gpu_p010.is_some() {
                tracing::info!("decode_thread: GPU P010→RGBA converter active");
            } else {
                tracing::info!("decode_thread: GPU P010 converter unavailable, falling back to CPU swscale");
            }
        }
        if let Some(converter) = gpu_p010.as_mut() {
            let y_stride = (*sw_frame).linesize[0] as u32;
            let uv_stride = (*sw_frame).linesize[1] as u32;
            let y_data_len = (y_stride * src_h) as usize;
            let uv_data_len = (uv_stride * (src_h / 2)) as usize;
            let y_data = std::slice::from_raw_parts((*sw_frame).data[0], y_data_len);
            let uv_data = std::slice::from_raw_parts((*sw_frame).data[1], uv_data_len);

            let t_p010 = std::time::Instant::now();
            // The shader picks PQ/HLG vs BT.709 transfer, BT.2020 vs
            // BT.709 matrix/primaries, and limited vs full range from
            // these IDs — no ad-hoc HDR sniffing in the host code.
            let color_ids = klipp_core::color_ids::ids(source_cs);
            let (rgba_data, gpu_source) = if should_attach_gpu_source() {
                let buffer = converter.convert_p010_to_rgba_on_gpu(
                    y_data, uv_data, src_w, src_h, y_stride, uv_stride,
                    out_w, out_h, source_rotation as u32, color_ids,
                );
                let device = converter.device();
                let queue = converter.queue();
                let readback = Frame::Gpu(GpuFrame::new(
                    device.clone(), queue.clone(), Arc::clone(&buffer), out_w, out_h,
                ));
                let bytes = readback.into_cpu().data;
                let attach = Arc::new(GpuFrame::new(device, queue, buffer, out_w, out_h));
                (bytes, Some(attach))
            } else {
                let bytes = converter.convert_p010_to_rgba(
                    y_data, uv_data, src_w, src_h, y_stride, uv_stride,
                    out_w, out_h, source_rotation as u32, color_ids,
                );
                (bytes, None)
            };
            // Reuse the NV12 perf bucket — the meaning is identical
            // (GPU YUV→RGBA encode time per frame).
            PERF_NV12_CONV_US.fetch_add(t_p010.elapsed().as_micros() as u64, Ordering::Relaxed);

            av_frame_free(&mut (sw_frame as *mut _));
            let t_linear = std::time::Instant::now();
            let linear = rgba_u8_to_linear(&rgba_data);
            PERF_LINEAR_ENCODE_US.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);

            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            PERF_GPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);
            maybe_log_hw_download_stats(
                count, &CALL_COUNT, &PERF_HW_DL_US, &PERF_NV12_CONV_US,
                &PERF_LINEAR_ENCODE_US, &PERF_SW_SCALE_US,
                &PERF_GPU_PATH_COUNT, &PERF_CPU_PATH_COUNT,
            );

            return Some(LinearFrame {
                pts: RationalTime::zero(),
                width: out_w,
                height: out_h,
                data: linear,
                is_eof: false,
                gpu_source,
                colorspace: source_cs,
            });
        }
    }

    // GPU NV12 fast path is 8-bit-only. For 10-bit sources (P010 etc.)
    // fall through to the CPU swscale path that knows about RGBA64LE.
    if !is_hi_bit_depth && sw_fmt == AVPixelFormat::AV_PIX_FMT_NV12 {
        if let Some(converter) = gpu_converter {
            let y_stride = (*sw_frame).linesize[0] as u32;
            let uv_stride = (*sw_frame).linesize[1] as u32;
            let y_data_len = (y_stride * src_h) as usize;
            let uv_data_len = (uv_stride * (src_h / 2)) as usize;

            let y_data = std::slice::from_raw_parts((*sw_frame).data[0], y_data_len);
            let uv_data = std::slice::from_raw_parts((*sw_frame).data[1], uv_data_len);

            let t_nv12 = std::time::Instant::now();
            let color_ids = klipp_core::color_ids::ids(source_cs);
            let (rgba_data, gpu_source) = if should_attach_gpu_source() {
                let buffer = converter.convert_nv12_to_rgba_on_gpu(
                    y_data, uv_data, src_w, src_h, y_stride, uv_stride,
                    out_w, out_h, source_rotation as u32, color_ids,
                );
                let device = converter.device();
                let queue = converter.queue();
                let readback = Frame::Gpu(GpuFrame::new(
                    device.clone(), queue.clone(), Arc::clone(&buffer), out_w, out_h,
                ));
                let bytes = readback.into_cpu().data;
                let attach = Arc::new(GpuFrame::new(device, queue, buffer, out_w, out_h));
                (bytes, Some(attach))
            } else {
                let bytes = converter.convert_nv12_to_rgba(
                    y_data, uv_data, src_w, src_h, y_stride, uv_stride,
                    out_w, out_h, source_rotation as u32, color_ids,
                );
                (bytes, None)
            };
            PERF_NV12_CONV_US.fetch_add(t_nv12.elapsed().as_micros() as u64, Ordering::Relaxed);

            av_frame_free(&mut (sw_frame as *mut _));
            let t_linear = std::time::Instant::now();
            let linear = rgba_u8_to_linear(&rgba_data);
            PERF_LINEAR_ENCODE_US.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);

            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            PERF_GPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);
            maybe_log_hw_download_stats(
                count,
                &CALL_COUNT,
                &PERF_HW_DL_US,
                &PERF_NV12_CONV_US,
                &PERF_LINEAR_ENCODE_US,
                &PERF_SW_SCALE_US,
                &PERF_GPU_PATH_COUNT,
                &PERF_CPU_PATH_COUNT,
            );

            return Some(LinearFrame {
                pts: RationalTime::zero(),
                width: out_w,
                height: out_h,
                data: linear,
                is_eof: false,
                gpu_source,
                colorspace: source_cs,
            });
        }
    }
    // Reaching here = CPU fallback path
    PERF_CPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);

    // Fallback: CPU swscale path. Bit-depth-aware output format so 10+-bit
    // sources keep their extra precision through the YUV→RGB step.
    let target_fmt = if is_hi_bit_depth {
        AVPixelFormat::AV_PIX_FMT_RGBA64LE
    } else {
        AVPixelFormat::AV_PIX_FMT_RGBA
    };
    let bytes_per_pixel: usize = if is_hi_bit_depth { 8 } else { 4 };
    let (sws_out_w, sws_out_h) = playback_dimensions(src_w, src_h, 0);
    let sws = sws_getCachedContext(
        sws_ctx.unwrap_or(std::ptr::null_mut()),
        src_w as i32,
        src_h as i32,
        sw_fmt,
        sws_out_w as i32,
        sws_out_h as i32,
        target_fmt,
        1, // SWS_FAST_BILINEAR
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
    );
    if sws.is_null() {
        warn!("HW: sws_getCachedContext failed for fmt {:?} {}x{}", sw_fmt, src_w, src_h);
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    if sws_ctx.is_none() {
        info!(
            "HW download: scaler created for {:?} {}x{} → {}x{} ({:?})",
            sw_fmt, src_w, src_h, sws_out_w, sws_out_h, target_fmt,
        );
    }
    *sws_ctx = Some(sws);

    // Allocate output frame at playback resolution (unrotated)
    let rgba_frame = av_frame_alloc();
    if rgba_frame.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    (*rgba_frame).width = sws_out_w as i32;
    (*rgba_frame).height = sws_out_h as i32;
    (*rgba_frame).format = target_fmt as i32;
    if av_frame_get_buffer(rgba_frame, 0) < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        av_frame_free(&mut (rgba_frame as *mut _));
        return None;
    }

    let t_swscale = std::time::Instant::now();
    sws_scale(
        sws,
        (*sw_frame).data.as_ptr() as *const *const u8,
        (*sw_frame).linesize.as_ptr(),
        0,
        src_h as i32,
        (*rgba_frame).data.as_ptr(),
        (*rgba_frame).linesize.as_ptr(),
    );
    PERF_SW_SCALE_US.fetch_add(t_swscale.elapsed().as_micros() as u64, Ordering::Relaxed);

    av_frame_free(&mut (sw_frame as *mut _));

    // Extract the raw buffer (handle stride != width*bpp).
    let stride = (*rgba_frame).linesize[0] as usize;
    let expected_stride = (sws_out_w as usize) * bytes_per_pixel;
    let data_slice = std::slice::from_raw_parts((*rgba_frame).data[0], stride * sws_out_h as usize);

    let raw_data: Vec<u8> = if stride == expected_stride {
        data_slice.to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * sws_out_h as usize);
        for row in 0..sws_out_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&data_slice[start..start + expected_stride]);
        }
        buf
    };

    av_frame_free(&mut (rgba_frame as *mut _));

    // Linearise, then rotate in float space (preserves precision) OR
    // rotate the u8 then linearise (cheaper for 8-bit, same numerical result).
    let linear_data = if is_hi_bit_depth {
        let linear = rgba64_le_to_linear(&raw_data);
        if source_rotation != 0 {
            rotate_linear_rgba(&linear, sws_out_w, sws_out_h, source_rotation).0
        } else {
            linear
        }
    } else if source_rotation != 0 {
        let t_linear = std::time::Instant::now();
        let (rotated, _rw, _rh) = crate::engine::rotate_rgba(&raw_data, sws_out_w, sws_out_h, source_rotation);
        let r = rgba_u8_to_linear(&rotated);
        PERF_LINEAR_ENCODE_US.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);
        r
    } else {
        let t_linear = std::time::Instant::now();
        let r = rgba_u8_to_linear(&raw_data);
        PERF_LINEAR_ENCODE_US.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);
        r
    };

    let (final_w, final_h) = if source_rotation == 90 || source_rotation == 270 {
        (sws_out_h, sws_out_w)
    } else {
        (sws_out_w, sws_out_h)
    };

    let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    maybe_log_hw_download_stats(
        count,
        &CALL_COUNT,
        &PERF_HW_DL_US,
        &PERF_NV12_CONV_US,
        &PERF_LINEAR_ENCODE_US,
        &PERF_SW_SCALE_US,
        &PERF_GPU_PATH_COUNT,
        &PERF_CPU_PATH_COUNT,
    );

    Some(LinearFrame {
        pts: RationalTime::zero(),
        width: final_w,
        height: final_h,
        data: linear_data,
        is_eof: false,
        gpu_source: None,
        colorspace: source_cs,
    })
}

/// Log + reset accumulated per-stage timing every 60 frames so the
/// download breakdown shows up in the log without being per-frame noisy.
fn maybe_log_hw_download_stats(
    count: u64,
    call_count: &std::sync::atomic::AtomicU64,
    hw_dl: &std::sync::atomic::AtomicU64,
    nv12_conv: &std::sync::atomic::AtomicU64,
    linear_enc: &std::sync::atomic::AtomicU64,
    sw_scale: &std::sync::atomic::AtomicU64,
    gpu_path: &std::sync::atomic::AtomicU64,
    cpu_path: &std::sync::atomic::AtomicU64,
) {
    use std::sync::atomic::Ordering;
    if count % 60 != 0 {
        return;
    }
    let hw_us = hw_dl.swap(0, Ordering::Relaxed);
    let nv12_us = nv12_conv.swap(0, Ordering::Relaxed);
    let lin_us = linear_enc.swap(0, Ordering::Relaxed);
    let sws_us = sw_scale.swap(0, Ordering::Relaxed);
    let gpu_n = gpu_path.swap(0, Ordering::Relaxed);
    let cpu_n = cpu_path.swap(0, Ordering::Relaxed);
    call_count.store(0, Ordering::Relaxed);
    let total = gpu_n + cpu_n;
    if total == 0 {
        return;
    }
    let avg = |sum: u64| sum as f64 / total as f64 / 1000.0;
    tracing::debug!(
        "PERF hw_download: 60 frames | path=gpu:{gpu_n}/cpu:{cpu_n} | \
         hw_dl={:.1}ms/frame nv12_conv={:.1}ms/frame sw_scale={:.1}ms/frame linear_enc={:.1}ms/frame",
        avg(hw_us),
        avg(nv12_us),
        avg(sws_us),
        avg(lin_us),
    );
}

// ---------------------------------------------------------------------------
// Decode thread
// ---------------------------------------------------------------------------

#[allow(clippy::drop_non_drop)]
fn decode_thread_main(
    path: PathBuf,
    frame_queue: SharedFrameQueue,
    cmd_rx: Receiver<DecodeCommand>,
    hw_active_flag: Arc<std::sync::atomic::AtomicBool>,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    source_rotation: i32,
) {
    let mut format_ctx = match ffmpeg_next::format::input(&path) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("decode_thread: failed to open {:?}: {e}", path);
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            return;
        }
    };

    let video_stream = match format_ctx.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) => s,
        None => {
            warn!("decode_thread: no video stream in {:?}", path);
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            return;
        }
    };

    let stream_index = video_stream.index();
    let time_base = video_stream.time_base();

    // Decoder selection: CUDA (NVDEC) → VAAPI → Software with multi-threading
    // CUDA works for all codecs on NVIDIA (including AV1/VP9).
    // VAAPI works for H.264/HEVC but nvidia-vaapi-driver has broken transfers for AV1/VP9.
    // Software fallback uses multi-threaded libdav1d/libvpx.
    let mut codec_ctx = match ffmpeg_next::codec::context::Context::from_parameters(video_stream.parameters()) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("decode_thread: codec context error: {e}");
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            return;
        }
    };

    // Try hardware acceleration: CUDA first, then VAAPI (skipped when pref disabled)
    let hw_enabled = HARDWARE_DECODE_PREF.load(std::sync::atomic::Ordering::Relaxed);
    let hw_accel = unsafe {
        if hw_enabled && try_init_cuda(codec_ctx.as_mut_ptr()) {
            HwAccel::Cuda
        } else if hw_enabled && try_init_vaapi(codec_ctx.as_mut_ptr()) {
            HwAccel::Vaapi
        } else {
            HwAccel::Software
        }
    };

    // Enable multi-threaded decoding — helps SW fallback (libdav1d uses all cores),
    // and doesn't hurt HW decode (ffmpeg ignores thread_count for HW decoders).
    unsafe {
        let tc = DECODE_THREADS_PREF.load(std::sync::atomic::Ordering::Relaxed);
        (*codec_ctx.as_mut_ptr()).thread_count = tc as i32;
    }

    let mut decoder = match codec_ctx.decoder().video() {
        Ok(d) => d,
        Err(e) => {
            warn!("decode_thread: video decoder error: {e}");
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            return;
        }
    };

    if hw_accel != HwAccel::Software {
        hw_active_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // Capture the source's colourspace from the AVCodecParameters
    // before the decoder starts producing frames. NVDEC's
    // `av_hwframe_transfer_data` typically strips per-frame colour
    // tags during download (we've seen `color_range=pc` files arrive
    // here as `unspecified`), so the codec-level values are the
    // authoritative baseline for everything downstream. Each frame
    // overrides individual fields where its own AVFrame tags are
    // actually specified.
    let cs_baseline_from_codecpar: klipp_core::SourceColorspace = unsafe {
        let par_raw: *mut ffmpeg_next::ffi::AVCodecParameters = (*video_stream.as_ptr()).codecpar;
        let (range_r, space_r, pri_r, trc_r, w, h) = if par_raw.is_null() {
            (0, 0, 0, 0, 1920u32, 1080u32)
        } else {
            (
                (*par_raw).color_range as i32,
                (*par_raw).color_space as i32,
                (*par_raw).color_primaries as i32,
                (*par_raw).color_trc as i32,
                (*par_raw).width.max(1) as u32,
                (*par_raw).height.max(1) as u32,
            )
        };
        klipp_core::SourceColorspace::resolve(range_r, space_r, pri_r, trc_r, w, h)
    };

    // KLIPP_FORCE_LOG_FORMAT is the test/dev override before the
    // per-clip UI exists. Setting it to one of the recognised tokens
    // replaces the baseline's transfer + primaries with the matching
    // camera-native pair (e.g. S-Log3 + S-Gamut3.Cine), which the
    // shaders and the CPU swscale path then dispatch on. Container
    // metadata never tags these — every NLE asks the user.
    let cs_baseline = apply_env_log_override(cs_baseline_from_codecpar);
    info!(
        "decode_thread: source colourspace baseline: {}{}",
        cs_baseline.describe(),
        if cs_baseline != cs_baseline_from_codecpar {
            format!(" (overridden from {})", cs_baseline_from_codecpar.describe())
        } else {
            " (from codec params)".to_string()
        },
    );

    // Software scaler — created lazily on first SW frame
    let mut sw_scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    // Scaler for HW downloaded frames (raw sws context, created lazily)
    let mut hw_scaler: Option<*mut ffmpeg_next::ffi::SwsContext> = None;

    // GPU NV12→RGBA converter — own wgpu device on the decode thread.
    // Only created for HW decode paths (CUDA/VAAPI produce NV12 frames).
    let mut gpu_nv12: Option<klipp_effects::nv12_gpu::GpuNv12Converter> = if hw_accel != HwAccel::Software {
        match klipp_effects::nv12_gpu::GpuNv12Converter::new() {
            Some(c) => {
                info!("decode_thread: GPU NV12→RGBA converter active");
                Some(c)
            }
            None => {
                info!("decode_thread: GPU NV12 converter unavailable, using CPU swscale");
                None
            }
        }
    } else {
        None
    };

    // GPU P010→RGBA converter — handles 10-bit HDR/SDR sources that
    // NVDEC produces in P010 (semi-planar 16-bit YUV with 10 valid
    // bits in the upper bits). Without this we'd fall back to CPU
    // sws_scale which is ~21 ms / 4K frame AND mishandles HDR
    // colorspace (BT.2020 chroma decoded as BT.709, PQ samples
    // decoded as sRGB → green/magenta blotches in HDR clips).
    // Lazy-created on first 10-bit frame so SDR-only sessions don't
    // pay the wgpu init cost.
    let mut gpu_p010: Option<klipp_effects::p010_gpu::GpuP010Converter> = None;

    let tb_num = time_base.numerator() as f64;
    let tb_den = time_base.denominator() as f64;
    let tb_f64 = if tb_den != 0.0 { tb_num / tb_den } else { 1.0 / 90000.0 };

    debug!(
        "decode_thread: started, stream={stream_index}, time_base={tb_num}/{tb_den}, hw={:?}",
        hw_accel
    );

    let mut paused = true;
    let mut eof_sent = false;
    let mut seek_target: Option<f64> = None;
    let mut packets = format_ctx.packets();
    let mut eof_from_demuxer = false;

    // Performance logging: print timing summary every N frames
    let mut perf_frame_count: u64 = 0;
    let mut perf_decode_us: u64 = 0;     // time in receive_frame
    let mut perf_transfer_us: u64 = 0;   // time in hw download OR sw scaler
    let mut perf_copy_us: u64 = 0;       // time copying RGBA data
    let mut perf_queue_us: u64 = 0;      // time pushing to frame queue
    let mut perf_interval_start = std::time::Instant::now();
    const PERF_LOG_INTERVAL: u64 = 60;   // log every 60 frames

    // Pending packet that couldn't be sent (EAGAIN — decoder buffer full).
    // We must drain frames before retrying.
    let mut pending_packet: Option<ffmpeg_next::Packet> = None;
    let mut received_frame = ffmpeg_next::frame::Video::empty();

    loop {
        if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            debug!("decode_thread: stop flag set, exiting");
            return;
        }

        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecodeCommand::SeekTo(secs) => {
                    let ts = (secs * 1_000_000.0) as i64;
                    debug!("decode_thread: seek to {secs:.3}s (ts={ts} us)");
                    drop(packets);
                    let _ = format_ctx.seek(ts, ..ts);
                    packets = format_ctx.packets();
                    decoder.flush();
                    frame_queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    seek_target = Some(secs);
                    eof_sent = false;
                    eof_from_demuxer = false;
                    pending_packet = None;
                }
                DecodeCommand::Pause => paused = true,
                DecodeCommand::Resume => {
                    paused = false;
                    eof_sent = false;
                }
                DecodeCommand::Stop => {
                    debug!("decode_thread: stop");
                    return;
                }
            }
        }

        if paused {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        if eof_sent {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // ── Phase 1: Drain all available frames from the decoder ──
        // Always drain before feeding new packets. This prevents the decoder's
        // internal buffer from filling up, which would cause send_packet to
        // return EAGAIN and silently lose packets.
        let mut drained_any = false;
        loop {
            if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            // Wait if queue is full — but stay in the drain loop so we
            // retry receive_frame once a slot opens.
            if frame_queue.lock().unwrap_or_else(|e| e.into_inner()).is_full() {
                std::thread::sleep(Duration::from_millis(1));
                // Re-check commands (seek/stop) while waiting
                if let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        DecodeCommand::SeekTo(secs) => {
                            let ts = (secs * 1_000_000.0) as i64;
                            debug!("decode_thread: seek to {secs:.3}s (ts={ts} us) (in drain)");
                            drop(packets);
                            let _ = format_ctx.seek(ts, ..ts);
                            packets = format_ctx.packets();
                            decoder.flush();
                            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
                            seek_target = Some(secs);
                            eof_sent = false;
                            eof_from_demuxer = false;
                            pending_packet = None;
                            break; // restart outer loop
                        }
                        DecodeCommand::Pause => { paused = true; break; }
                        DecodeCommand::Resume => { paused = false; eof_sent = false; }
                        DecodeCommand::Stop => { return; }
                    }
                }
                continue; // retry drain
            }

            let t_decode = std::time::Instant::now();
            let got_frame = decoder.receive_frame(&mut received_frame).is_ok();
            perf_decode_us += t_decode.elapsed().as_micros() as u64;

            if !got_frame {
                // No more buffered frames — need to feed more packets
                break;
            }

            let pts_secs = received_frame
                .pts()
                .map(|pts| pts as f64 * tb_f64)
                .unwrap_or(0.0);

            // Skip pre-seek frames
            if let Some(target) = seek_target {
                if pts_secs < target - 0.05 {
                    continue; // skip this frame, try next
                }
                seek_target = None;
            }

            // Convert frame to RGBA
            let is_hw_frame = unsafe {
                let fmt = (*received_frame.as_ptr()).format;
                fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_CUDA as i32
                    || fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32
            };

            let t_transfer = std::time::Instant::now();
            let frame_result = if is_hw_frame {
                hw_active_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                unsafe { download_hw_frame(&received_frame, &mut hw_scaler, gpu_nv12.as_mut(), &mut gpu_p010, source_rotation, cs_baseline) }
            } else {
                convert_sw_frame(&received_frame, &mut sw_scaler, &mut perf_copy_us, source_rotation, cs_baseline)
            };

            let mut linear_frame = match frame_result {
                Some(result) => result,
                None => {
                    warn!("decode_thread: frame conversion failed, skipping");
                    continue; // skip this frame but keep draining
                }
            };

            if is_hw_frame {
                perf_transfer_us += t_transfer.elapsed().as_micros() as u64;
            } else {
                // For SW path, transfer time is the scaler time minus copy time
                perf_transfer_us += t_transfer.elapsed().as_micros() as u64;
            }

            // Stamp pts on the decoder-produced linear frame, then push
            // it straight into the queue — the queue is linear-typed
            // as of Phase 10a. Consumers on the legacy u8 path call
            // [`FrameQueue::pop_u8`] which does the conversion on their
            // side; linear consumers read it directly.
            linear_frame.pts = RationalTime::from_secs_f64(pts_secs, 90000);
            let (frame_w, frame_h) = (linear_frame.width, linear_frame.height);

            let t_queue = std::time::Instant::now();
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(linear_frame);
            perf_queue_us += t_queue.elapsed().as_micros() as u64;
            drained_any = true;
            perf_frame_count += 1;

            if perf_frame_count.is_multiple_of(PERF_LOG_INTERVAL) {
                let elapsed = perf_interval_start.elapsed();
                let fps = PERF_LOG_INTERVAL as f64 / elapsed.as_secs_f64();
                debug!(
                    "PERF decode_thread: {PERF_LOG_INTERVAL} frames in {:.0}ms ({fps:.1} fps) | \
                     decode={:.1}ms transfer={:.1}ms copy={:.1}ms queue={:.1}ms | \
                     frame={}x{} hw={:?}",
                    elapsed.as_secs_f64() * 1000.0,
                    perf_decode_us as f64 / 1000.0,
                    perf_transfer_us as f64 / 1000.0,
                    perf_copy_us as f64 / 1000.0,
                    perf_queue_us as f64 / 1000.0,
                    frame_w, frame_h, hw_accel,
                );
                perf_decode_us = 0;
                perf_transfer_us = 0;
                perf_copy_us = 0;
                perf_queue_us = 0;
                perf_interval_start = std::time::Instant::now();
            }
        }

        if paused { continue; } // broke out of drain loop due to pause command

        // ── Phase 2: Feed next packet to the decoder ──

        // Retry pending packet first (was EAGAIN last time)
        if let Some(pkt) = pending_packet.take() {
            match decoder.send_packet(&pkt) {
                Ok(()) => {}
                Err(e) => {
                    // Still EAGAIN? Shouldn't happen since we just drained. Keep it.
                    warn!("decode_thread: send_packet retry failed: {e}");
                    pending_packet = Some(pkt);
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
            }
        } else if !eof_from_demuxer {
            // Read next packet from demuxer
            match packets.next() {
                Some((stream, packet)) => {
                    if stream.index() == stream_index {
                        match decoder.send_packet(&packet) {
                            Ok(()) => {}
                            Err(_) => {
                                // EAGAIN: decoder buffer full. Save packet and drain first.
                                pending_packet = Some(packet);
                            }
                        }
                    }
                    // Non-video packets: just skip, loop will continue to drain+feed
                }
                None => {
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        } else if !drained_any {
            // EOF from demuxer and no frames drained — we're done
            debug!("decode_thread: EOF reached");
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            eof_sent = true;
        }
    }
}

/// Convert a software-decoded frame to RGBA, with optional downscaling and rotation.
fn convert_sw_frame(
    received_frame: &ffmpeg_next::frame::Video,
    sw_scaler: &mut Option<ffmpeg_next::software::scaling::Context>,
    perf_copy_us: &mut u64,
    source_rotation: i32,
    cs_baseline: klipp_core::SourceColorspace,
) -> Option<LinearFrame> {
    // One-shot per-process: log the source format and resolved
    // colourspace baseline. Useful when triaging future colour bugs
    // ("what was the file actually tagged as?") and cheap enough to
    // leave on in production.
    {
        use std::sync::atomic::AtomicBool;
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            info!(
                "convert_sw_frame: format={:?}, {}x{}, baseline={}",
                received_frame.format(),
                received_frame.width(), received_frame.height(),
                cs_baseline.describe(),
            );
        }
    }
    // YUV420P10LE direct path: skip swscale entirely. swscale's
    // YUV420P10LE → RGBA64LE conversion was observed producing
    // channel-clamped output (R/G hitting 0/1.0 across most pixels)
    // for valid Sony S-Log3 source frames regardless of
    // `sws_setColorspaceDetails` configuration. The direct
    // converter in `klipp_color::camera_log` reads the AVFrame's
    // raw 10-bit planes, applies the resolved colourspace's matrix
    // + range + EOTF + primaries, and outputs linear BT.709 f32
    // ready for the engine. Same EOTFs / primaries matrices as the
    // GPU shaders.
    if received_frame.format() == ffmpeg_next::format::Pixel::YUV420P10LE {
        // The per-frame AVFrame's tags: `LinearFrame.colorspace`
        // wants the merged value, but the conversion math itself
        // only needs `cs_baseline` (per-frame tags are usually
        // unspecified; baseline carries the codec / override).
        let source_cs = unsafe {
            let avf = received_frame.as_ptr();
            klipp_core::SourceColorspace::resolve_with_baseline(
                (*avf).color_range as i32,
                (*avf).colorspace as i32,
                (*avf).color_primaries as i32,
                (*avf).color_trc as i32,
                cs_baseline,
            )
        };
        let src_w = received_frame.width();
        let src_h = received_frame.height();
        let (dst_w, dst_h) = playback_dimensions(src_w, src_h, 0);
        let y_stride = received_frame.stride(0) as usize;
        let u_stride = received_frame.stride(1) as usize;
        let v_stride = received_frame.stride(2) as usize;
        let y_data = received_frame.data(0);
        let u_data = received_frame.data(1);
        let v_data = received_frame.data(2);
        let linear = klipp_color::camera_log::yuv420p10le_to_linear_bt709(
            y_data, u_data, v_data,
            y_stride, u_stride, v_stride,
            src_w, src_h, dst_w, dst_h,
            source_cs,
        );
        let (final_w, final_h) = if source_rotation == 90 || source_rotation == 270 {
            (dst_h, dst_w)
        } else {
            (dst_w, dst_h)
        };
        let linear = if source_rotation != 0 {
            rotate_linear_rgba(&linear, dst_w, dst_h, source_rotation).0
        } else {
            linear
        };
        return Some(LinearFrame {
            pts: RationalTime::zero(),
            width: final_w,
            height: final_h,
            data: linear,
            is_eof: false,
            gpu_source: None,
            colorspace: source_cs,
        });
    }

    // Bit-depth-aware swscale target: 8-bit sources go through RGBA (1 byte
    // per channel); 10+-bit sources go through RGBA64LE (2 bytes per channel)
    // so the extra precision survives the YUV→RGB conversion and reaches the
    // linear-light decode step.
    let is_hi_bit_depth = unsafe {
        source_is_high_bit_depth(received_frame.format().into())
    };
    let target_format = if is_hi_bit_depth {
        ffmpeg_next::format::Pixel::RGBA64LE
    } else {
        ffmpeg_next::format::Pixel::RGBA
    };

    // swscale does resize only — use unrotated dimensions, then rotate RGBA output
    let (out_w, out_h) = playback_dimensions(received_frame.width(), received_frame.height(), 0);
    let need_scaler = match sw_scaler {
        Some(s) => {
            let inp = s.input();
            let out = s.output();
            inp.format != received_frame.format()
                || inp.width != received_frame.width()
                || inp.height != received_frame.height()
                || out.format != target_format
        }
        None => true,
    };
    if need_scaler {
        match ffmpeg_next::software::scaling::Context::get(
            received_frame.format(),
            received_frame.width(),
            received_frame.height(),
            target_format,
            out_w,
            out_h,
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
        ) {
            Ok(mut s) => {
                if received_frame.width() != out_w || received_frame.height() != out_h {
                    info!(
                        "decode_thread: SW scaler downscale {}x{} → {}x{} ({:?})",
                        received_frame.width(),
                        received_frame.height(),
                        out_w,
                        out_h,
                        target_format,
                    );
                }
                // ffmpeg_next's `Context::get` doesn't pass the
                // source's colourspace details to swscale, so swscale
                // defaults to "BT.709 limited regardless." For full-
                // range or BT.601/BT.2020 sources that produces a
                // chroma cast (the A6700 magenta/green symptom).
                // Reconfigure via sws_setColorspaceDetails so the
                // YUV→RGB matrix and Y/UV range divisors match what
                // the file actually is. Doesn't help with non-sRGB-
                // family transfers (S-Log3, PQ, HLG) — those need the
                // GPU shaders or Phase D's log-curve override — but
                // it eliminates the dominant chroma cast every time.
                //
                // KLIPP_DISABLE_SWS_COLORSPACE=1 skips the call as an
                // A/B test toggle — leaves swscale at its default
                // (BT.709 limited) for verifying that the call itself
                // is what's producing or fixing a given symptom.
                let skip = std::env::var("KLIPP_DISABLE_SWS_COLORSPACE").as_deref() == Ok("1");
                if skip {
                    info!("decode_thread: sws colourspace SKIPPED via KLIPP_DISABLE_SWS_COLORSPACE=1");
                } else {
                    unsafe {
                        let sws_ctx = s.as_mut_ptr();
                        let inv_table = sws_get_coefficients_for_matrix(cs_baseline.matrix);
                        let dst_table = sws_get_coefficients_for_matrix(klipp_core::ColorMatrix::Bt709);
                        let src_range: i32 = match cs_baseline.range {
                            klipp_core::ColorRange::Full => 1,
                            klipp_core::ColorRange::Limited => 0,
                        };
                        // brightness=0, contrast=1<<16, saturation=1<<16
                        // are the swscale "no adjustment" identity values.
                        let rc = ffmpeg_next::ffi::sws_setColorspaceDetails(
                            sws_ctx as *mut _,
                            inv_table.as_ptr(),
                            src_range,
                            dst_table.as_ptr(),
                            0, // dst is RGBA — range doesn't apply
                            0,
                            1 << 16,
                            1 << 16,
                        );
                        if rc < 0 {
                            warn!(
                                "decode_thread: sws_setColorspaceDetails returned {rc} for {}; \
                                 swscale will use its default (BT.709 limited)",
                                cs_baseline.describe(),
                            );
                        } else {
                            info!(
                                "decode_thread: sws colourspace configured = {} (src_range={src_range})",
                                cs_baseline.describe(),
                            );
                        }
                        let _ = dst_table; // silence unused-binding warning
                    }
                }
                *sw_scaler = Some(s);
            }
            Err(e) => {
                warn!("decode_thread: scaler creation failed: {e}");
                return None;
            }
        }
    }
    let scaler = sw_scaler.as_mut().unwrap();
    let mut rgba_frame = ffmpeg_next::frame::Video::empty();
    if scaler.run(received_frame, &mut rgba_frame).is_err() {
        warn!("decode_thread: scaler failed, skipping frame");
        return None;
    }

    let width = rgba_frame.width();
    let height = rgba_frame.height();
    let bytes_per_pixel = if is_hi_bit_depth { 8 } else { 4 };
    let stride = rgba_frame.stride(0);
    let data_ptr = rgba_frame.data(0);

    let t_copy = std::time::Instant::now();
    let expected_stride = (width as usize) * bytes_per_pixel;
    let raw_data: Vec<u8> = if stride == expected_stride {
        data_ptr[..expected_stride * height as usize].to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * height as usize);
        for row in 0..height as usize {
            let start = row * stride;
            let end = start + expected_stride;
            buf.extend_from_slice(&data_ptr[start..end]);
        }
        buf
    };
    *perf_copy_us += t_copy.elapsed().as_micros() as u64;

    // Read source colourspace tags from the AVFrame underlying
    // `received_frame`, with the codec-params baseline (captured
    // once at decoder init) as the fallback for any tag the per-
    // frame AVFrame leaves unspecified. Same merge rule as the GPU
    // path so `LinearFrame.colorspace` stays consistent. Computed
    // before linear-decode because the chosen EOTF + primaries
    // matrix depend on it.
    let source_cs = unsafe {
        let avf = received_frame.as_ptr();
        klipp_core::SourceColorspace::resolve_with_baseline(
            (*avf).color_range as i32,
            (*avf).colorspace as i32,
            (*avf).color_primaries as i32,
            (*avf).color_trc as i32,
            cs_baseline,
        )
    };

    // Rotation still operates on sRGB u8 via rotate_rgba. For hi-bit-
    // depth sources we'd lose precision rotating the u16 buffer
    // through the u8 helper, so: rotate *after* linearisation. For
    // 8-bit we rotate the u8 then linearise — cheaper and bit-
    // identical to the old output. Either way, the linear-decode
    // step uses the colourspace-aware path so log curves and
    // wide-gamut sources reach BT.709 working space correctly.
    let linear_data = if is_hi_bit_depth {
        let linear = klipp_color::camera_log::rgba64_le_to_linear_bt709(&raw_data, source_cs);
        if source_rotation != 0 {
            rotate_linear_rgba(&linear, width, height, source_rotation).0
        } else {
            linear
        }
    } else if source_rotation != 0 {
        let (rotated, _rw, _rh) = crate::engine::rotate_rgba(&raw_data, width, height, source_rotation);
        klipp_color::camera_log::rgba_u8_to_linear_bt709(&rotated, source_cs)
    } else {
        klipp_color::camera_log::rgba_u8_to_linear_bt709(&raw_data, source_cs)
    };

    let (final_w, final_h) = if source_rotation == 90 || source_rotation == 270 {
        (height, width)
    } else {
        (width, height)
    };

    Some(LinearFrame {
        pts: RationalTime::zero(), // filled in by caller
        width: final_w,
        height: final_h,
        data: linear_data,
        is_eof: false,
        gpu_source: None,
        colorspace: source_cs,
    })
}

/// Rotate a linear-f32 RGBA buffer by 90/180/270 degrees. Mirrors
/// `engine::rotate_rgba` but stays in float precision — used on the
/// hi-bit-depth decode path where a u8 round-trip would lose the
/// headroom we just paid to preserve.
fn rotate_linear_rgba(src: &[f32], w: u32, h: u32, deg: i32) -> (Vec<f32>, u32, u32) {
    match deg {
        90 => {
            let mut dst = vec![0f32; src.len()];
            let (nw, nh) = (h, w);
            for y in 0..h {
                for x in 0..w {
                    let src_idx = ((y * w + x) * 4) as usize;
                    let new_x = h - 1 - y;
                    let new_y = x;
                    let dst_idx = ((new_y * nw + new_x) * 4) as usize;
                    dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                }
            }
            (dst, nw, nh)
        }
        180 => {
            let mut dst = vec![0f32; src.len()];
            for y in 0..h {
                for x in 0..w {
                    let src_idx = ((y * w + x) * 4) as usize;
                    let new_x = w - 1 - x;
                    let new_y = h - 1 - y;
                    let dst_idx = ((new_y * w + new_x) * 4) as usize;
                    dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                }
            }
            (dst, w, h)
        }
        270 => {
            let mut dst = vec![0f32; src.len()];
            let (nw, nh) = (h, w);
            for y in 0..h {
                for x in 0..w {
                    let src_idx = ((y * w + x) * 4) as usize;
                    let new_x = y;
                    let new_y = w - 1 - x;
                    let dst_idx = ((new_y * nw + new_x) * 4) as usize;
                    dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                }
            }
            (dst, nw, nh)
        }
        _ => (src.to_vec(), w, h),
    }
}

#[cfg(test)]
mod decoder_linearisation_tests {
    use super::*;

    #[test]
    fn source_bit_depth_detection_recognises_common_formats() {
        use ffmpeg_next::ffi::AVPixelFormat;
        unsafe {
            // 8-bit sources
            assert!(!source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_NV12));
            assert!(!source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_YUV420P));
            assert!(!source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_RGBA));
            // 10-bit sources
            assert!(source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_P010LE));
            assert!(source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_YUV420P10LE));
            // 16-bit
            assert!(source_is_high_bit_depth(AVPixelFormat::AV_PIX_FMT_RGBA64LE));
        }
    }

    #[test]
    fn rgba_u8_to_linear_encodes_mid_grey_correctly() {
        // sRGB 118 (≈ 18 % perceptual mid-grey) decodes to ~0.18 linear.
        let src = vec![118u8, 118, 118, 255];
        let linear = rgba_u8_to_linear(&src);
        assert_eq!(linear.len(), 4);
        assert!((linear[0] - 0.18).abs() < 0.005);
        assert!((linear[1] - 0.18).abs() < 0.005);
        assert!((linear[2] - 0.18).abs() < 0.005);
        assert_eq!(linear[3], 1.0); // alpha stays linear (not gamma-decoded)
    }

    #[test]
    fn rgba_u8_to_linear_handles_extremes() {
        let src = vec![0u8, 0, 0, 255, 255, 255, 255, 0];
        let linear = rgba_u8_to_linear(&src);
        assert_eq!(linear[0], 0.0);
        assert_eq!(linear[3], 1.0);
        assert!((linear[4] - 1.0).abs() < 1e-4);
        assert_eq!(linear[7], 0.0);
    }

    #[test]
    fn rgba64_le_to_linear_preserves_precision_above_8bit() {
        // Build an RGBA64LE pixel: values that would both land on the
        // same u8 code value (sRGB 118) after quantisation. The
        // underlying sRGB values are 118.0/255 and 118.2/255 — a real
        // 10-bit source carries the second; we prove the linear decode
        // distinguishes them.
        fn srgb_pixel_u16(srgb_frac: f32) -> [u8; 8] {
            let v = (srgb_frac * 65535.0).round() as u16;
            let a = 65535u16;
            [
                (v & 0xFF) as u8, (v >> 8) as u8,
                (v & 0xFF) as u8, (v >> 8) as u8,
                (v & 0xFF) as u8, (v >> 8) as u8,
                (a & 0xFF) as u8, (a >> 8) as u8,
            ]
        }
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&srgb_pixel_u16(118.0 / 255.0));
        bytes.extend_from_slice(&srgb_pixel_u16(118.2 / 255.0));

        let linear = rgba64_le_to_linear(&bytes);
        // Both pixels are close to 0.18 linear but must be distinct.
        assert!((linear[0] - 0.18).abs() < 0.01);
        assert!((linear[4] - 0.18).abs() < 0.01);
        assert!(
            linear[4] > linear[0],
            "10-bit precision lost: both pixels decoded to {:.6}",
            linear[0],
        );
    }

    #[test]
    fn rgba64_le_alpha_is_normalised_not_gamma_decoded() {
        // A max-alpha u16 (65535) must produce 1.0 linear, not the
        // gamma-decoded version of ~1.0.
        let pixel = [
            0u8, 0, // R = 0
            0, 0, // G = 0
            0, 0, // B = 0
            0xFF, 0xFF, // A = 65535
        ];
        let linear = rgba64_le_to_linear(&pixel);
        assert_eq!(linear[3], 1.0);
    }

    #[test]
    fn rotate_linear_rgba_90_degrees_moves_corner() {
        // A 2×1 frame with a red pixel then a green pixel. Rotating
        // 90° clockwise should land the red pixel at the bottom.
        let w: u32 = 2;
        let h: u32 = 1;
        let src = vec![
            1.0f32, 0.0, 0.0, 1.0, // red
            0.0, 1.0, 0.0, 1.0, // green
        ];
        let (rotated, nw, nh) = rotate_linear_rgba(&src, w, h, 90);
        assert_eq!(nw, 1);
        assert_eq!(nh, 2);
        // After 90° CW: layout becomes [red row, green row] stacked.
        // Rotation maps (x=0, y=0) red → (new_x = h-1-y = 0, new_y = x = 0)
        // so red is at the top, green at the bottom.
        assert_eq!(rotated[0], 1.0); // top pixel R
        assert_eq!(rotated[4], 0.0); // bottom pixel R
        assert_eq!(rotated[5], 1.0); // bottom pixel G
    }

    #[test]
    fn rotate_linear_rgba_180_flips_both_axes() {
        let w = 2;
        let h = 2;
        // Top-left red, rest black
        let mut src = vec![0f32; 16];
        src[0] = 1.0; // R of (0,0)
        src[3] = 1.0; // A of (0,0)
        let (rotated, nw, nh) = rotate_linear_rgba(&src, w, h, 180);
        assert_eq!((nw, nh), (2, 2));
        // After 180°: red should be at (1, 1) → index 12..16
        assert_eq!(rotated[12], 1.0);
    }

    #[test]
    fn rotate_linear_rgba_zero_is_identity() {
        let src = vec![0.5f32; 16]; // 2x2 grey
        let (rotated, nw, nh) = rotate_linear_rgba(&src, 2, 2, 0);
        assert_eq!((nw, nh), (2, 2));
        assert_eq!(rotated, src);
    }

    // ─── should_use_skim_for_playback ──────────────────────────────
    // Pure decision function: takes pref mode + source media stats +
    // skim-readiness, returns whether the playback decoder should
    // open the skim instead of the source. Tested in isolation so
    // the engine call site can stay a one-liner.

    #[test]
    fn auto_uses_skim_for_4k60_when_skim_ready() {
        // Mode 0 (Auto), 4K60 source, skim built — must substitute.
        assert!(should_use_skim_for_playback(0, 2160, 60.0, true));
    }

    #[test]
    fn auto_uses_skim_for_4k30_when_skim_ready() {
        // 4K resolution alone is enough to swap.
        assert!(should_use_skim_for_playback(0, 2160, 30.0, true));
    }

    #[test]
    fn auto_uses_skim_for_1080p60_when_skim_ready() {
        // 60 fps alone is enough to swap.
        assert!(should_use_skim_for_playback(0, 1080, 60.0, true));
    }

    #[test]
    fn auto_keeps_source_for_1080p30() {
        // Comfortable for the current pipeline — no swap.
        assert!(!should_use_skim_for_playback(0, 1080, 29.97, true));
    }

    #[test]
    fn auto_keeps_source_when_skim_not_ready() {
        // Even a heavy 4K60 source plays from original until skim
        // finishes. (Engine should still try to start the skim
        // transcode in the background.)
        assert!(!should_use_skim_for_playback(0, 2160, 60.0, false));
    }

    #[test]
    fn always_original_never_swaps_even_for_4k60() {
        // Mode 1 explicit — user wants source quality at all costs.
        assert!(!should_use_skim_for_playback(1, 2160, 60.0, true));
    }

    #[test]
    fn always_optimized_swaps_whenever_skim_exists() {
        // Mode 2 — even on a 720p30 source we use skim if available.
        assert!(should_use_skim_for_playback(2, 720, 23.976, true));
        // …but not if the skim hasn't been built yet.
        assert!(!should_use_skim_for_playback(2, 720, 23.976, false));
    }

    #[test]
    fn unknown_mode_falls_back_to_auto() {
        // Defensive default — a future enum variant we forgot to
        // teach the decoder about should NOT brick playback.
        assert_eq!(
            should_use_skim_for_playback(99, 2160, 60.0, true),
            should_use_skim_for_playback(0, 2160, 60.0, true),
        );
    }

    #[test]
    fn fps_threshold_is_just_above_30() {
        // Boundary: NTSC 29.97 stays on source; PAL 50 / 60 swaps.
        assert!(!should_use_skim_for_playback(0, 1080, 29.97, true));
        assert!(should_use_skim_for_playback(0, 1080, 50.0, true));
        assert!(should_use_skim_for_playback(0, 1080, 59.94, true));
    }
}
