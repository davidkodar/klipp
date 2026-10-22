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

use crate::frame::GpuFrame;
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
/// inside `LinearFrame.gpu_source`, and the decode thread does NOT
/// pay the GPU→CPU readback fence. Engine consumers that still need
/// CPU `data` realise it lazily on demand via
/// [`LinearFrame::ensure_cpu_data`], so the readback (when actually
/// required) moves off the decode thread onto the engine thread,
/// pipelined with other engine work instead of serialising the
/// decoder behind `device.poll(Wait)`.
///
/// **On by default** since the viewer's GPU paths came to depend on it:
/// without it `LinearFrame.gpu_source` is never populated, and
/// `present_graded_frame_on_gpu` declines every frame — so the grade, the
/// ACES and BT.2020 display transforms and HDR monitoring's preview all
/// fall back to the CPU pipeline, at 15.6 fps against 59.9 on a 4K HDR10
/// clip. It was opt-in while the path was being built.
///
/// `KLIPP_GPU_FRAMES=0` forces the readback back onto the decode thread,
/// which is the way to check whether a suspected picture problem comes
/// from the GPU-resident hand-off — the same shape as
/// `KLIPP_DIRECT_PRESENT=0`.
///
/// Nothing to weigh up on a slow machine: both call sites sit inside the
/// hardware-decode branch with a live converter, so a box with no usable
/// GPU decode never reaches them and takes the software path either way.
///
/// Read once per process via `OnceLock` so the env check doesn't
/// poke syscalls on the decode hot path.
pub(crate) fn should_attach_gpu_source() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| gpu_frames_from_env(std::env::var("KLIPP_GPU_FRAMES").ok().as_deref()))
}

/// The decision [`should_attach_gpu_source`] caches, as a function of the
/// variable alone.
///
/// Split out so it can be tested: the `OnceLock` above is read once per
/// process, so a test cannot flip it, and the polarity is the part that
/// would rot unnoticed now that the default is on — every test run and
/// every launch takes the enabled branch, and nothing else would say if
/// `=0` stopped disabling it.
fn gpu_frames_from_env(var: Option<&str>) -> bool {
    var != Some("0")
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

/// Camera log formats by media file, set from the media pool
/// (`set_media_log_formats`). Log footage is tagged BT.709 by almost every
/// camera — container metadata has no way to say "S-Log3" — so the user's
/// choice (or the media's detected format) is the only source of truth.
static MEDIA_LOG_FORMATS: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<PathBuf, klipp_core::LogFormat>>,
> = std::sync::OnceLock::new();

thread_local! {
    /// The file this decode thread is decoding. Each decoder owns one
    /// thread, so the per-frame colourspace code can find its file's log
    /// format without every conversion function taking a path.
    static DECODE_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Replace the per-file camera log formats the decoders use. Call whenever
/// the media pool's log formats may have changed; decoders pick the change
/// up on their next frame, with no reopen.
///
/// Returns whether anything changed. Frames already decoded — cached, or
/// read ahead by a decoder — were decoded under the old formats, so on
/// `true` the caller must send `AppCommand::MediaDecodeChanged`; otherwise
/// a paused viewer keeps showing the old picture.
pub fn set_media_log_formats(
    entries: impl IntoIterator<Item = (PathBuf, klipp_core::LogFormat)>,
) -> bool {
    let map: std::collections::HashMap<_, _> = entries
        .into_iter()
        .filter(|(_, f)| f.needs_camera_decode())
        .collect();
    let lock = MEDIA_LOG_FORMATS.get_or_init(Default::default);
    let mut current = lock.write().unwrap_or_else(|e| e.into_inner());
    if *current == map {
        return false;
    }
    *current = map;
    true
}

/// The camera log format registered for a file, if any — originals,
/// proxies and skim files alike.
pub fn registered_log_format(path: &std::path::Path) -> Option<klipp_core::LogFormat> {
    MEDIA_LOG_FORMATS.get()?.read().ok()?.get(path).copied()
}

/// Disk-cache key for a file's *pictures* (thumbnails, filmstrips): the
/// content key plus the camera log format they were converted with, so a
/// format change never serves a picture made under the old one, and
/// switching back finds the earlier one still cached. The content key alone
/// stays right for format-independent caches (audio, skim transcodes).
pub fn picture_cache_key(path: &std::path::Path) -> Option<String> {
    let key = klipp_media::disk_cache::compute_cache_key(path)?;
    Some(match registered_log_format(path) {
        Some(f) => format!("{key}.{f:?}"),
        None => key,
    })
}

/// `KLIPP_FORCE_LOG_FORMAT` — a dev override that applies one log format to
/// every file, for testing without touching the media pool. Tokens as the
/// old stop-gap: slog3, slog2, vlog, clog, clog3, logc3, logc4, redlog3g10,
/// bmdfilmgen5, dlog, flog, applelog, nlog, goproflat.
fn env_forced_log_format() -> Option<klipp_core::LogFormat> {
    use klipp_core::LogFormat as L;
    static TOKEN: std::sync::OnceLock<Option<klipp_core::LogFormat>> = std::sync::OnceLock::new();
    *TOKEN.get_or_init(|| {
        let t = std::env::var("KLIPP_FORCE_LOG_FORMAT").ok()?.to_ascii_lowercase();
        Some(match t.as_str() {
            "slog3" => L::SLog3, "slog2" => L::SLog2, "vlog" => L::VLog,
            "clog" => L::CanonLog, "clog3" => L::CanonLog3, "logc3" => L::LogC3,
            "logc4" => L::LogC4, "redlog3g10" => L::RedLog3G10, "bmdfilmgen5" => L::BmdFilmGen5,
            "dlog" => L::DJIDLog, "flog" => L::FLog, "applelog" => L::AppleLog,
            "nlog" => L::NLog, "goproflat" => L::GoProFlat,
            "llog" | "leicallog" => L::LeicaLLog,
            _ => return None,
        })
    })
}

/// Apply this decode thread's camera log format to a frame's resolved
/// colourspace: transfer and primaries only — range and matrix still come
/// from the file (Sony, ARRI and Canon log all use the BT.709 matrix).
///
/// Must run *after* `resolve_with_baseline`. The frame's own tags win over
/// the baseline, and log footage is tagged BT.709, so an override applied
/// to the baseline — which is where the old env-var stop-gap put it — was
/// silently replaced by the file's tag on every tagged frame.
fn apply_log_override(cs: klipp_core::SourceColorspace) -> klipp_core::SourceColorspace {
    DECODE_PATH.with(|p| log_override_for_path(p.borrow().as_deref(), cs))
}

/// The camera log format that applies to a file: what the media pool
/// registered, or the `KLIPP_FORCE_LOG_FORMAT` override.
///
/// The viewer's answer to the question
/// `klipp_media::frame_reader::SourceColour` takes as an argument.
/// Export answers it from `MediaFile.log_format` instead, which is why
/// the shared type is given the format rather than looking it up.
pub fn log_format_for_path(path: &std::path::Path) -> Option<klipp_core::LogFormat> {
    registered_log_format(path).or_else(env_forced_log_format)
}

/// [`apply_log_override`] for code that is not a decode thread and knows
/// its file directly — thumbnails, filmstrips, skim frames. Proxy and skim
/// files are registered under their own paths with their media's format,
/// so passing whichever file is actually being read is right.
pub(crate) fn log_override_for_path(
    path: Option<&std::path::Path>,
    cs: klipp_core::SourceColorspace,
) -> klipp_core::SourceColorspace {
    let from_media = path.and_then(|path| MEDIA_LOG_FORMATS.get()?.read().ok()?.get(path).copied());
    match from_media.or_else(env_forced_log_format) {
        Some(format) => format.apply_to(cs),
        None => cs,
    }
}

/// The software decoders' scene-referred route: the CPU twin of the GPU
/// converters' codes output, used when a PQ, HLG or camera-log frame did
/// not come through NVDEC (ProRes and other formats NVDEC cannot take, the
/// no-GPU fallback). Returns the display-referred preview and the codes the
/// grade reads, exactly as the P010 readback branch does, so a clip looks
/// the same whichever decoder ran. `None` for display-referred sources or
/// unreadable formats.
///
/// `dst_w`/`dst_h` are the final (rotated) dimensions, as for the shaders.
///
/// SAFETY: `frame` must be a valid, allocated software AVFrame.
pub(crate) unsafe fn scene_referred_sw_frame(
    frame: *const ffmpeg_next::ffi::AVFrame,
    source_cs: klipp_core::SourceColorspace,
    dst_w: u32,
    dst_h: u32,
    rotation: u32,
) -> Option<(Vec<f32>, crate::types::HdrInput)> {
    let ids = klipp_core::color_ids::ids(source_cs);
    let transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(ids[2])?;
    let yuv = klipp_media::yuv_samples::read_yuv_samples(frame)?;
    let codes = klipp_color::hdr::yuv_to_hdr_codes(&yuv.samples(), dst_w, dst_h, rotation, ids);
    let (source_to_working, _) = primaries_matrix_for_source(source_cs);
    let mut display = codes.to_scene_linear_rgba(transfer, source_to_working);
    klipp_color::hdr::apply_hdr_display_tonemap(&mut display, transfer);
    Some((
        display,
        crate::types::HdrInput {
            codes: crate::types::HdrCodes::Cpu(Arc::new(codes)),
            transfer,
            source_to_working,
            width: dst_w,
            height: dst_h,
        },
    ))
}

/// [`scene_referred_sw_frame`] packaged as the decoder's `LinearFrame`, at
/// playback dimensions with the source rotation applied.
///
/// SAFETY: `frame` must be a valid, allocated software AVFrame.
unsafe fn scene_referred_linear_frame(
    frame: *const ffmpeg_next::ffi::AVFrame,
    source_cs: klipp_core::SourceColorspace,
    source_rotation: i32,
) -> Option<LinearFrame> {
    let (src_w, src_h) = ((*frame).width.max(1) as u32, (*frame).height.max(1) as u32);
    let (out_w, out_h) = playback_dimensions(src_w, src_h, source_rotation);
    let (display, hdr) =
        scene_referred_sw_frame(frame, source_cs, out_w, out_h, source_rotation.rem_euclid(360) as u32)?;
    Some(LinearFrame {
        pts: RationalTime::zero(),
        width: out_w,
        height: out_h,
        data: display,
        is_eof: false,
        gpu_source: None,
        colorspace: source_cs,
        hdr: Some(hdr),
        seek_epoch: 0,
    })
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

/// Compute the source-primaries → working-space matrix (packed for
/// the WGSL `Params` uniform) for the active project working space.
///
/// Reads `WORKING_COLORSPACE_PREF` lock-free, calls into klipp-color
/// for the matrix, then packs to the 3-row vec4 layout the decoder
/// shaders expect. Centralised here so all four GPU call sites
/// (NV12 + P010, each with `_on_gpu` + readback variants) share one
/// matrix-selection rule.
///
/// Caveat for non-`Rec709Linear` working spaces: the GPU decoder's
/// final stage re-encodes via the sRGB EOTF for the existing CPU
/// hand-off. That round-trip is bit-stable for linear BT.709 values
/// in `[0, 1]`, but ACEScg values can exceed that range and will
/// quantise/clip on the encode. The clean fix is to keep the
/// GPU-resident `_on_gpu` path GPU-resident through the grade chain
/// (PERF-GPU-RESIDENT-SCOPE in the tracker); for now non-`Rec709`
/// projects accept the quantisation as a known limitation of the
/// CPU-readback path.
///
/// Returns the plain 3×3 (kept on `HdrInput` for the grade) and the packed
/// shader form.
fn primaries_matrix_for_source(
    source_cs: klipp_core::SourceColorspace,
) -> ([[f32; 3]; 3], [[f32; 4]; 3]) {
    let m = klipp_color::camera_log::primaries_to_working_matrix(
        source_cs.primaries,
        working_colorspace(),
    );
    (m, klipp_effects::color_wgsl::pack_primaries_matrix(m))
}

/// sRGB-decode a RGBA u8 byte slice into a linear f32 RGBA buffer.
/// Alpha stays linear (no gamma on alpha, per sRGB spec).
fn rgba_u8_to_linear(rgba: &[u8]) -> Vec<f32> {
    let srgb_lut = srgb_u8_to_linear_lut();
    let a_lut = u8_linear_div255_lut();
    let n_pixels = rgba.len() / 4;
    // Capacity reserved once and filled by appending. `set_len` over
    // uninitialised memory is what `clippy::uninit_vec` denies, and the
    // same change in `klipp_color::camera_log` measured *faster* than the
    // unsafe version it replaced, so there is nothing to trade away here.
    let mut out = Vec::with_capacity(n_pixels * 4);
    // Unrolled RGB+A path per pixel. Compiler auto-vectorises this on
    // x86_64 with AVX2 or better enabled by the default `cargo` profile.
    for chunk in rgba.chunks_exact(4) {
        out.extend_from_slice(&[
            srgb_lut[chunk[0] as usize],
            srgb_lut[chunk[1] as usize],
            srgb_lut[chunk[2] as usize],
            a_lut[chunk[3] as usize],
        ]);
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
    /// Seek to a time, under the epoch the caller claimed for it. The
    /// decode thread stamps every frame it produces afterwards with that
    /// epoch, so consumers can tell a frame decoded for this seek from one
    /// converted just before it — see [`LinearFrame::seek_epoch`].
    SeekTo(f64, u64),
    /// Seek to a time that lies just ahead of the last decoded frame, by
    /// decoding on and skipping up to it instead of seeking the
    /// container. The requester has already kept the queued frames at or
    /// after the time, restamped with the epoch.
    SeekOn(f64, u64),
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
    /// Decode + GPU-download/swscale wall time of the most recently
    /// queued frame, in microseconds. Written by the decode thread,
    /// read by the engine when building perf-overlay samples.
    last_decode_us: Arc<std::sync::atomic::AtomicU32>,
    /// Counts seeks asked for. Bumped by [`Self::seek`] before the command
    /// goes out, so a consumer can read it without having tracked the
    /// return value of every seek call.
    seek_epoch: Arc<std::sync::atomic::AtomicU64>,
    /// The second the newest seek asked for, as f64 bits; `u64::MAX`
    /// for none. So a play right after a seek to the same place waits
    /// for that seek's frames instead of seeking again.
    last_seek_bits: std::sync::atomic::AtomicU64,
    /// The presentation time of the last frame the decode thread decoded,
    /// as `f64` bits; [`NO_PTS`] before the first frame and after a
    /// container seek. Read by [`VideoDecoder::seek`] to decide whether a
    /// seek can be answered by decoding on.
    last_decoded_pts: Arc<std::sync::atomic::AtomicU64>,
    /// The stream's declared average frame rate, when it declares one.
    source_fps: Option<f64>,
    /// Its place in the GPU decoder's count (#2116), and how it decodes.
    lease: crate::decode_budget::Lease,
}

/// [`VideoDecoder::last_decoded_pts`] when there is no frame.
const NO_PTS: u64 = u64::MAX;

/// How far before a seek target a queued frame may lie and still be the
/// frame for it: the rounding between a timeline time and a stream
/// timestamp, well under any frame.
const SEEK_PTS_SLACK: f64 = 1e-3;

fn publish_pts(slot: &std::sync::atomic::AtomicU64, pts: Option<f64>) {
    slot.store(pts.map_or(NO_PTS, f64::to_bits), std::sync::atomic::Ordering::Relaxed);
}

fn read_pts(slot: &std::sync::atomic::AtomicU64) -> Option<f64> {
    match slot.load(std::sync::atomic::Ordering::Relaxed) {
        NO_PTS => None,
        bits => Some(f64::from_bits(bits)),
    }
}

impl VideoDecoder {
    /// Open a video file for decoding with optional VAAPI hardware acceleration.
    /// `source_rotation` is the media rotation in degrees (0, 90, 180, 270) from metadata.
    /// It asks the GPU, always, and is counted as asking (#2116).
    pub fn open(path: &Path, source_rotation: i32) -> Result<Self, DecoderError> {
        Self::open_inner(path, source_rotation, None, None)
    }

    /// Open as [`Self::open`], decoding as `lease` was admitted: in
    /// software when the GPU's decoder was full (#2116).
    pub fn open_with(path: &Path, source_rotation: i32, lease: crate::decode_budget::Lease) -> Result<Self, DecoderError> {
        Self::open_inner(path, source_rotation, Some(lease), None)
    }

    /// Open as [`Self::open_with`], holding at most `frames` decoded frames
    /// rather than the preference's: a layer opened while memory is short
    /// (#2128).
    pub fn open_with_queue(path: &Path, source_rotation: i32, lease: crate::decode_budget::Lease, frames: usize) -> Result<Self, DecoderError> {
        Self::open_inner(path, source_rotation, Some(lease), Some(frames))
    }

    /// How this decoder was asked to decode.
    pub fn decoding(&self) -> crate::decode_budget::Decoding {
        self.lease.decoding
    }

    fn open_inner(path: &Path, source_rotation: i32, lease: Option<crate::decode_budget::Lease>, frames: Option<usize>) -> Result<Self, DecoderError> {
        // Validate file can be opened before spawning the thread, and read
        // the stream's own frame rate: which of its frames a time shows
        // is decided at half of *its* frame, as export's decoder does (#696).
        let source_fps = {
            let ctx = ffmpeg_next::format::input(path)
                .map_err(|e| DecoderError::Open(e.to_string()))?;
            let stream = ctx
                .streams()
                .best(ffmpeg_next::media::Type::Video)
                .ok_or(DecoderError::NoVideoStream)?;
            let r = stream.avg_frame_rate();
            (r.numerator() > 0 && r.denominator() > 0).then(|| r.numerator() as f64 / r.denominator() as f64)
        };
        // Its rate is set by the decode thread, which reads the size.
        let lease = lease.unwrap_or_else(|| crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Fixed));
        let (lease_id, decoding) = (lease.id(), lease.decoding);

        let queue_cap = frames.unwrap_or_else(frame_queue_size);
        let frame_queue: SharedFrameQueue = Arc::new(Mutex::new(FrameQueue::new(queue_cap)));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let queue_clone = frame_queue.clone();
        let path_owned = path.to_path_buf();
        let hw_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hw_flag = hw_active.clone();
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();
        let last_decode_us = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let last_decode_us_clone = last_decode_us.clone();
        let last_decoded_pts = Arc::new(std::sync::atomic::AtomicU64::new(NO_PTS));
        let last_decoded_pts_clone = last_decoded_pts.clone();

        let alive = DecodeThreadToken::new();
        let thread = std::thread::Builder::new()
            .name("klipp-vdec".into())
            .spawn(move || {
                // Held for the thread's whole life, drop included, so
                // `wait_for_decode_threads` cannot return while this one
                // is still inside ffmpeg's teardown.
                let _alive = alive;
                crate::thread_guard::catch_panic("klipp-vdec", || {
                    decode_thread_main(path_owned, queue_clone, cmd_rx, hw_flag, stop_flag_clone, source_rotation, last_decode_us_clone, last_decoded_pts_clone, lease_id, decoding, source_fps.unwrap_or(30.0));
                });
            })
            .map_err(|e| DecoderError::Open(format!("spawn thread: {e}")))?;

        Ok(Self {
            frame_queue,
            seek_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_seek_bits: std::sync::atomic::AtomicU64::new(u64::MAX),
            decode_thread: Some(thread),
            decode_cmd_tx: cmd_tx,
            hw_active,
            stop_flag,
            last_decode_us,
            last_decoded_pts,
            source_fps,
            lease,
        })
    }

    /// Half of one of the file's frames, in seconds: how far past the time
    /// asked for a frame may be stamped and still be the frame shown for it,
    /// which makes the shown frame the nearest one of the file. The same
    /// rule as export's `ClipStream::seek_forward` (klipp-render), so
    /// playback and export show the same frame of the file at the same
    /// moment when the file's rate is not the sequence's (#696). Half of
    /// `fallback_fps`, the sequence's rate, for a stream that declares none,
    /// as export falls back too.
    pub fn half_source_frame(&self, fallback_fps: f64) -> f64 {
        0.5 / self.source_fps.unwrap_or(fallback_fps).max(1.0)
    }

    pub fn frame_queue(&self) -> &SharedFrameQueue {
        &self.frame_queue
    }

    /// Decode + transfer wall time of the most recently queued frame,
    /// in milliseconds. 0.0 until the first frame is produced.
    pub fn last_decode_ms(&self) -> f32 {
        self.last_decode_us.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0
    }

    /// Ask the decoder to seek, and claim an epoch for it.
    ///
    /// Returns the epoch, which is also readable later through
    /// [`Self::requested_seek`]: frames stamped with anything lower were
    /// decoded before this seek and are stale.
    pub fn seek(&self, secs: f64) -> u64 {
        self.last_seek_bits.store(secs.to_bits(), std::sync::atomic::Ordering::Relaxed);
        let epoch = self.seek_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        // Decided here, not on the decode thread: the epoch is already
        // the new one, so the caller's next look at the queue treats every
        // frame stamped with the old one as stale. Frames that a step
        // forward can use are restamped now, before that look.
        let last = read_pts(&self.last_decoded_pts);
        let mut queue = self.frame_queue.lock().unwrap_or_else(|e| e.into_inner());
        let (oldest, queued) = (queue.oldest_pts_secs(), queue.len());
        let decode_on = seek_is_just_ahead(last, oldest, secs);
        let kept = if decode_on { queue.keep_from(secs - SEEK_PTS_SLACK, epoch) } else { queue.clear(); 0 };
        drop(queue);
        debug!(
            "VideoDecoder::seek {secs:.3}s: last decoded {last:?}, queued {queued} from {oldest:?} → {} (kept {kept}, epoch {epoch})",
            if decode_on { "decode on" } else { "container seek" }
        );
        let cmd = if decode_on { DecodeCommand::SeekOn(secs, epoch) } else { DecodeCommand::SeekTo(secs, epoch) };
        let _ = self.decode_cmd_tx.send(cmd);
        epoch
    }

    /// The second the newest seek asked for, if any.
    pub fn last_seek_secs(&self) -> Option<f64> {
        let bits = self.last_seek_bits.load(std::sync::atomic::Ordering::Relaxed);
        (bits != u64::MAX).then(|| f64::from_bits(bits))
    }

    /// The epoch of the newest seek asked for — 0 if there has been none.
    ///
    /// A queued frame whose [`LinearFrame::seek_epoch`] is below this was
    /// decoded for an older position, whatever its timestamp says.
    pub fn requested_seek(&self) -> u64 {
        self.seek_epoch.load(std::sync::atomic::Ordering::SeqCst)
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

/// How many decode threads are alive, so the process can avoid exiting
/// out from under one.
static LIVE_DECODE_THREADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Counts a decode thread as alive for as long as it is held.
pub(crate) struct DecodeThreadToken(&'static std::sync::atomic::AtomicUsize);

impl DecodeThreadToken {
    /// Counted until dropped: held by every thread that runs ffmpeg or the
    /// GPU for a decoder, its own decode thread and one that opens it
    /// (#2261), so [`wait_for_decode_threads`] waits for them all.
    pub(crate) fn new() -> Self {
        Self::counted_in(&LIVE_DECODE_THREADS)
    }

    /// A token on a count of its own, so a test can watch it register
    /// and deregister while real decoders in other tests come and go on
    /// the shared count.
    fn counted_in(count: &'static std::sync::atomic::AtomicUsize) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(count)
    }
}

impl Drop for DecodeThreadToken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Wait for every decode thread to finish, up to `timeout`. `true` if
/// they all did.
///
/// **Call this before the process exits.** `VideoDecoder::stop` waits
/// half a second for its thread and then detaches it, and
/// `stop_detached` does not wait at all — both of which are fine while
/// the process lives, because a detached thread finishing late harms
/// nobody. It is not fine at exit: `exit()` runs atexit handlers and
/// tears down libc while other threads keep running, so a decode thread
/// still inside ffmpeg's teardown then dereferences state that has gone.
///
/// That is a real crash and not a theoretical one. It took down
/// `viewer_matches_export` about one full-suite run in five, with the
/// main thread in `exit()` unwinding NVIDIA's EGL teardown and the
/// decode thread in `av_buffer_unref` → `av_log` → SIGSEGV. It only
/// showed under load because that is when a thread is slow enough to
/// still be there when the process leaves.
pub fn wait_for_decode_threads(timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if LIVE_DECODE_THREADS.load(std::sync::atomic::Ordering::Acquire) == 0 {
            return true;
        }
        if start.elapsed() >= timeout {
            warn!(
                "{} decode thread(s) still running after {:?} — exiting anyway",
                LIVE_DECODE_THREADS.load(std::sync::atomic::Ordering::Acquire),
                timeout,
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Hardware acceleration: CUDA (NVDEC) and VAAPI
// ---------------------------------------------------------------------------

/// Which hardware acceleration is active
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum HwAccel {
    Cuda,
    Vaapi,
    Software,
}

/// Callback for FFmpeg codec negotiation — selects the preferred HW pixel format.
///
/// Returns `AV_PIX_FMT_CUDA` whenever it's offered so ffmpeg keeps the frame
/// on the GPU (`AVHWFrame`) instead of auto-downloading. When CUDA isn't in
/// the candidate list, falls back to `*pix_fmts` (ffmpeg's own first
/// preference) — that lets the decoder still produce a software frame for
/// codecs / profiles NVDEC can't handle. The downstream `is_hw_frame` check
/// on `AVPixelFormat::AV_PIX_FMT_CUDA` then routes those software frames to
/// `convert_sw_frame` instead of `download_hw_frame`.
///
/// Strict mode (`KLIPP_STRICT_HW_DECODE=1`) flips the fallback to
/// `AV_PIX_FMT_NONE` instead — the official ffmpeg `hw_decode.c` example
/// pattern. That forces ffmpeg to back off to its own software decoder
/// rather than silently auto-downloading. Useful when investigating "HW
/// negotiated at init then SW frames showed up mid-stream" symptoms;
/// off by default because it's a bigger behaviour change for every codec
/// that doesn't have a CUDA hwaccel built in.
/// Convert an `AVPixelFormat` to its ffmpeg name string for human-
/// readable diagnostic logs (e.g. "cuda", "yuv420p10le", "vaapi").
/// Falls back to the integer value when ffmpeg returns NULL (out-of-
/// range / unknown format).
unsafe fn pix_fmt_name(f: ffmpeg_next::ffi::AVPixelFormat) -> String {
    let p = ffmpeg_next::ffi::av_get_pix_fmt_name(f);
    if p.is_null() {
        format!("?{}", f as i32)
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// Set up an `AVHWFramesContext` on the codec context so the decoder
/// has somewhere to put HW frames in `hw_pix_fmt`. Mirrors ffmpeg's
/// `hw_decode.c` example pattern. Without this step, returning a HW
/// pixel format from `get_format` succeeds at the API level but the
/// decoder silently falls back to a software output format because
/// it has no hw_frames_ctx to allocate output buffers from — exactly
/// the symptom we observed on Sony A6700 HEVC Main 10 (first call
/// picks CUDA, follow-up calls offer no CUDA → SW auto-download).
///
/// Returns `true` on success. On failure, logs the cause and lets
/// the caller decide whether to still accept the HW format (ffmpeg
/// will then fall back internally — no worse than the pre-fix state).
unsafe fn install_hw_frames_ctx(
    ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    hw_pix_fmt: ffmpeg_next::ffi::AVPixelFormat,
) -> bool {
    use ffmpeg_next::ffi::*;

    // Say each distinct failure once. `get_format` runs on every
    // decoder open, and a decoder is opened per seek — so a session
    // spent skimming logged the same warning 81 times, at WARN, in the
    // file that is the first place anyone looks when something is
    // wrong. The condition is worth knowing; the eighty-first copy of
    // it is not. The neighbouring `info!` already does this.
    fn warn_once(slot: &std::sync::atomic::AtomicBool, msg: std::fmt::Arguments<'_>) {
        use std::sync::atomic::Ordering;
        if !slot.swap(true, Ordering::Relaxed) {
            warn!("{msg} (further copies suppressed)");
        }
    }
    use std::sync::atomic::AtomicBool;
    static NULL_DEVICE: AtomicBool = AtomicBool::new(false);
    static NO_PARAMS: AtomicBool = AtomicBool::new(false);
    static INIT_FAILED: AtomicBool = AtomicBool::new(false);

    if (*ctx).hw_device_ctx.is_null() {
        warn_once(&NULL_DEVICE, format_args!("install_hw_frames_ctx: hw_device_ctx is NULL"));
        return false;
    }
    let device_ref = (*ctx).hw_device_ctx;
    let mut frames_ref: *mut AVBufferRef = std::ptr::null_mut();
    let rc = avcodec_get_hw_frames_parameters(ctx, device_ref, hw_pix_fmt, &mut frames_ref);
    if rc < 0 || frames_ref.is_null() {
        warn_once(
            &NO_PARAMS,
            format_args!("install_hw_frames_ctx: avcodec_get_hw_frames_parameters returned {rc}; falling back to software decode"),
        );
        return false;
    }
    let init_rc = av_hwframe_ctx_init(frames_ref);
    if init_rc < 0 {
        warn_once(&INIT_FAILED, format_args!("install_hw_frames_ctx: av_hwframe_ctx_init returned {init_rc}"));
        av_buffer_unref(&mut frames_ref);
        return false;
    }
    (*ctx).hw_frames_ctx = av_buffer_ref(frames_ref);
    av_buffer_unref(&mut frames_ref);
    true
}

unsafe extern "C" fn cuda_get_format(
    ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;

    // Walk the candidate list once, hunting for CUDA + collecting
    // names for the diagnostic log. Most lists are 2–4 entries long
    // so the double pass is negligible.
    let mut p = pix_fmts;
    let mut candidates: Vec<String> = Vec::with_capacity(8);
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        candidates.push(pix_fmt_name(*p));
        if *p == AVPixelFormat::AV_PIX_FMT_CUDA {
            // Install hw_frames_ctx before returning the HW format —
            // see `install_hw_frames_ctx` for why this matters.
            // Already installed (re-negotiation pass with the same
            // codec context) is fine; we only set it the first time.
            let already_set = !(*ctx).hw_frames_ctx.is_null();
            let ok = already_set
                || install_hw_frames_ctx(ctx, AVPixelFormat::AV_PIX_FMT_CUDA);
            use std::sync::atomic::{AtomicBool, Ordering};
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                info!(
                    "CUDA get_format: selected AV_PIX_FMT_CUDA (candidates={candidates:?}, \
                     hw_frames_ctx={})",
                    if already_set { "already set" }
                    else if ok { "installed" }
                    else { "INSTALL FAILED — decoder will fall back to SW" },
                );
            }
            return AVPixelFormat::AV_PIX_FMT_CUDA;
        }
        p = p.add(1);
    }
    let strict = std::env::var("KLIPP_STRICT_HW_DECODE").as_deref() == Ok("1");
    if strict {
        warn!(
            "CUDA get_format: AV_PIX_FMT_CUDA not offered (candidates={candidates:?}); \
             KLIPP_STRICT_HW_DECODE=1 → returning AV_PIX_FMT_NONE"
        );
        AVPixelFormat::AV_PIX_FMT_NONE
    } else {
        warn!(
            "CUDA get_format: AV_PIX_FMT_CUDA not offered (candidates={candidates:?}); \
             falling back to ffmpeg's first preference (software decode for this pass)"
        );
        *pix_fmts
    }
}

unsafe extern "C" fn vaapi_get_format(
    ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;

    let mut p = pix_fmts;
    let mut candidates: Vec<String> = Vec::with_capacity(8);
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        candidates.push(pix_fmt_name(*p));
        if *p == AVPixelFormat::AV_PIX_FMT_VAAPI {
            let already_set = !(*ctx).hw_frames_ctx.is_null();
            let ok = already_set
                || install_hw_frames_ctx(ctx, AVPixelFormat::AV_PIX_FMT_VAAPI);
            use std::sync::atomic::{AtomicBool, Ordering};
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                info!(
                    "VAAPI get_format: selected AV_PIX_FMT_VAAPI (candidates={candidates:?}, \
                     hw_frames_ctx={})",
                    if already_set { "already set" }
                    else if ok { "installed" }
                    else { "INSTALL FAILED" },
                );
            }
            return AVPixelFormat::AV_PIX_FMT_VAAPI;
        }
        p = p.add(1);
    }
    let strict = std::env::var("KLIPP_STRICT_HW_DECODE").as_deref() == Ok("1");
    if strict {
        warn!("VAAPI get_format: AV_PIX_FMT_VAAPI not offered (candidates={candidates:?}); KLIPP_STRICT_HW_DECODE=1 → AV_PIX_FMT_NONE");
        AVPixelFormat::AV_PIX_FMT_NONE
    } else {
        warn!("VAAPI get_format: AV_PIX_FMT_VAAPI not offered (candidates={candidates:?}); falling back to first preference");
        *pix_fmts
    }
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

/// Manual FFI declarations for libavcodec's bitstream-filter API.
///
/// `ffmpeg-sys-next` 8.1.0's bindgen invocation only includes
/// `libavcodec/avcodec.h`; the BSF API was split into a separate
/// `libavcodec/bsf.h` upstream and isn't bound. The symbols are
/// already linked transitively (libavcodec is in our link line) so
/// we just need the declarations.
///
/// **ABI assumption:** the `AVBSFContext` field order below mirrors
/// ffmpeg's public header `libavcodec/bsf.h` exactly. The layout has
/// been stable since the BSF API was added (~2017); any reordering
/// would be an ABI break across the entire libavcodec ecosystem so
/// it's vanishingly unlikely. We only access `par_in` and `par_out`
/// — the other fields are present so the field offsets are correct.
mod bsf_ffi {
    use ffmpeg_next::ffi::{AVCodecParameters, AVPacket, AVRational};
    use std::os::raw::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct AVClass {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    pub struct AVBitStreamFilter {
        _opaque: [u8; 0],
    }

    /// Mirror of `AVBSFContext` from `libavcodec/bsf.h`. We only
    /// touch `par_in` / `par_out`; the other fields are declared so
    /// the offsets are correct.
    #[repr(C)]
    pub struct AVBSFContext {
        pub av_class: *const AVClass,
        pub filter: *const AVBitStreamFilter,
        pub priv_data: *mut c_void,
        pub par_in: *mut AVCodecParameters,
        pub par_out: *mut AVCodecParameters,
        pub time_base_in: AVRational,
        pub time_base_out: AVRational,
    }

    extern "C" {
        pub fn av_bsf_get_by_name(name: *const c_char) -> *const AVBitStreamFilter;
        pub fn av_bsf_alloc(filter: *const AVBitStreamFilter, ctx: *mut *mut AVBSFContext) -> c_int;
        pub fn av_bsf_init(ctx: *mut AVBSFContext) -> c_int;
        pub fn av_bsf_send_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
        pub fn av_bsf_receive_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
        pub fn av_bsf_flush(ctx: *mut AVBSFContext);
        pub fn av_bsf_free(ctx: *mut *mut AVBSFContext);
    }
}

/// Map an ffmpeg codec ID to its `*_cuvid` decoder name when one
/// exists. cuvid decoders are NVIDIA's dedicated NVDEC implementations
/// — they produce CUDA hwframes natively (`AV_PIX_FMT_CUDA`) and
/// don't rely on the fragile "standard codec + cuda hwaccel +
/// hw_frames_ctx" negotiation that we observed silently falling back
/// to software in our build. Used by every NLE's NVDEC path.
fn cuvid_decoder_name(codec_id: ffmpeg_next::codec::Id) -> Option<&'static str> {
    use ffmpeg_next::codec::Id;
    Some(match codec_id {
        Id::HEVC => "hevc_cuvid",
        Id::H264 => "h264_cuvid",
        Id::AV1 => "av1_cuvid",
        Id::VP9 => "vp9_cuvid",
        Id::VP8 => "vp8_cuvid",
        Id::MPEG2VIDEO => "mpeg2_cuvid",
        Id::MPEG1VIDEO => "mpeg1_cuvid",
        Id::MPEG4 => "mpeg4_cuvid",
        Id::VC1 => "vc1_cuvid",
        Id::MJPEG => "mjpeg_cuvid",
        _ => return None,
    })
}

/// Decide whether a bitstream filter is needed in front of the cuvid
/// decoder for this codec, based on the extradata format. cuvid wants
/// annex-B framing for H.264 / HEVC; MP4/MOV containers store these
/// in length-prefixed avcc / hvcc ("isobmff") format, distinguishable
/// by the first byte of `extradata` being `0x01` (the version byte
/// of the avcC / hvcC box). Without the matching `*_mp4toannexb`
/// BSF, cuvid rejects the packet stream with `Invalid argument`.
///
/// Other codecs (AV1, VP9, MPEG-{1,2,4}, VC-1) don't need a BSF in
/// front of cuvid because their bitstream format is the same in MP4
/// as on the wire.
fn cuvid_bsf_name(
    codec_id: ffmpeg_next::codec::Id,
    extradata: &[u8],
) -> Option<&'static str> {
    use ffmpeg_next::codec::Id;
    if extradata.is_empty() || extradata[0] != 0x01 {
        // Annex-B already (or no extradata at all) — no BSF needed.
        return None;
    }
    match codec_id {
        Id::H264 => Some("h264_mp4toannexb"),
        Id::HEVC => Some("hevc_mp4toannexb"),
        _ => None,
    }
}

/// Owned wrapper around `*mut AVBSFContext` that frees on drop. The
/// raw pointer is `Send` because we only use it from the decoder
/// thread that owns the context — the BSF API itself is single-
/// threaded.
pub(crate) struct BsfContext {
    ptr: *mut bsf_ffi::AVBSFContext,
    name: &'static str,
}

unsafe impl Send for BsfContext {}

impl BsfContext {
    pub(crate) fn as_ptr(&self) -> *mut bsf_ffi::AVBSFContext {
        self.ptr
    }

    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Reset the filter for a seek: drop what it holds and clear its
    /// end-of-stream state. Once the end of the file has been read, the
    /// filter otherwise refuses every packet ("A non-NULL packet sent
    /// after an EOF") and the decoder never produces another frame.
    pub(crate) fn flush(&self) {
        unsafe { bsf_ffi::av_bsf_flush(self.ptr) };
    }
}

impl Drop for BsfContext {
    fn drop(&mut self) {
        unsafe { bsf_ffi::av_bsf_free(&mut self.ptr) };
    }
}

/// Allocate + init a bitstream filter for `codec_params`. Returns
/// `None` if the named filter doesn't exist in this ffmpeg build,
/// allocation fails, or init fails — caller falls back to whatever
/// decoder path doesn't need the filter.
unsafe fn try_create_bsf(
    name: &'static str,
    codec_params: *mut ffmpeg_next::ffi::AVCodecParameters,
) -> Option<BsfContext> {
    use ffmpeg_next::ffi::*;
    use std::ffi::CString;
    let cname = CString::new(name).ok()?;
    let filter = bsf_ffi::av_bsf_get_by_name(cname.as_ptr());
    if filter.is_null() {
        warn!("BSF: av_bsf_get_by_name({name}) returned NULL");
        return None;
    }
    let mut ctx: *mut bsf_ffi::AVBSFContext = std::ptr::null_mut();
    let alloc_rc = bsf_ffi::av_bsf_alloc(filter, &mut ctx);
    if alloc_rc < 0 || ctx.is_null() {
        warn!("BSF: av_bsf_alloc({name}) returned {alloc_rc}");
        return None;
    }
    let owned = BsfContext { ptr: ctx, name };
    let copy_rc = avcodec_parameters_copy((*owned.ptr).par_in, codec_params);
    if copy_rc < 0 {
        warn!("BSF: avcodec_parameters_copy({name}) returned {copy_rc}");
        return None; // Drop frees the BSF
    }
    let init_rc = bsf_ffi::av_bsf_init(owned.ptr);
    if init_rc < 0 {
        warn!("BSF: av_bsf_init({name}) returned {init_rc}");
        return None;
    }
    Some(owned)
}

/// Build a codec context backed by a `*_cuvid` decoder, with a
/// matching `*_mp4toannexb` bitstream filter if the source extradata
/// is in avcc / hvcc format. Returns `(codec_context, optional_bsf)`
/// on success — the caller owns both and must keep the BSF alive for
/// the decoder's lifetime.
///
/// Returns `None` when:
///   * cuvid isn't built into this ffmpeg, OR
///   * this codec id has no cuvid variant, OR
///   * the required BSF (if any) can't be created, OR
///   * CUDA device init fails.
///
/// The decode loop must route demuxer packets through the BSF (when
/// `Some`) via `bsf_send_packet` / `bsf_receive_packet` before each
/// `decoder.send_packet()` — see the loop in `decode_thread_main`.
fn try_open_cuvid_decoder(
    stream: &ffmpeg_next::format::stream::Stream,
) -> Option<(ffmpeg_next::codec::Context, Option<BsfContext>)> {
    use ffmpeg_next::ffi::*;
    let codec_id = stream.parameters().id();
    let cuvid_name = cuvid_decoder_name(codec_id)?;
    let codec = ffmpeg_next::codec::decoder::find_by_name(cuvid_name)?;

    // Read extradata to decide whether a BSF is needed.
    let params_ptr = unsafe { stream.parameters().as_ptr() } as *mut AVCodecParameters;
    let extradata: &[u8] = unsafe {
        if (*params_ptr).extradata.is_null() || (*params_ptr).extradata_size <= 0 {
            &[]
        } else {
            std::slice::from_raw_parts(
                (*params_ptr).extradata,
                (*params_ptr).extradata_size as usize,
            )
        }
    };
    let bsf = if let Some(bsf_name) = cuvid_bsf_name(codec_id, extradata) {
        let b = unsafe { try_create_bsf(bsf_name, params_ptr)? };
        Some(b)
    } else {
        None
    };

    // The codec context must use the BSF's *output* parameters when
    // a BSF is in play (the BSF rewrites the extradata format), and
    // the stream's parameters otherwise.
    let codec_params: *const AVCodecParameters = match &bsf {
        Some(b) => unsafe { (*b.as_ptr()).par_out as *const _ },
        None => unsafe { stream.parameters().as_ptr() },
    };

    let mut ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
    let (src_w, src_h) = unsafe {
        let p = stream.parameters().as_ptr();
        ((*p).width.max(0) as u32, (*p).height.max(0) as u32)
    };
    // For HW decode the PCIe transfer cost of the hwframe dominates
    // — at 4K HEVC, `av_hwframe_transfer_data` is ~41 ms / frame,
    // which alone exceeds the 24 fps budget. Every pro NLE handles
    // this by capping playback resolution to a smaller "preview"
    // size (Resolve / Premiere / FCP all do "Half Res" or similar
    // by default) and reserving full-res for export. We mirror
    // that: cuvid downscales to fit a 1080p bounding box on the GPU
    // via NVDEC, then we transfer the smaller frame. Saves ~4× over
    // PCIe and keeps the playhead smooth.
    //
    // The user's `max_playback_height` pref still drives the SW
    // decode path + export pipeline — it just doesn't override the
    // HW transfer cap because that's about PCIe physics, not user
    // intent.
    let (resize_w, resize_h) = hw_transfer_cap_dimensions(src_w, src_h);
    let want_resize = resize_w != src_w || resize_h != src_h;
    unsafe {
        let ctx_ptr = ctx.as_mut_ptr();
        if ctx_ptr.is_null() {
            warn!("cuvid: avcodec_alloc_context3 returned NULL for {cuvid_name}");
            return None;
        }
        let rc = avcodec_parameters_to_context(ctx_ptr, codec_params);
        if rc < 0 {
            warn!("cuvid: avcodec_parameters_to_context failed for {cuvid_name}: rc={rc}");
            return None;
        }
        // Wire `pkt_timebase` from the stream so cuvid stops warning
        // "Invalid pkt_timebase, passing timestamps as-is." — no
        // visible behaviour change (PTS already worked via the
        // fallback) but removes the ambiguity flag for any future
        // frame-accurate seek code that consults the timebase.
        let stb = stream.time_base();
        (*ctx_ptr).pkt_timebase = AVRational {
            num: stb.numerator(),
            den: stb.denominator(),
        };
        // GPU-side resize via cuvid's `resize` private option. NVDEC
        // does the downscale before frames are handed off — saves
        // ~3× the transfer cost at 4K because we move 1080p worth
        // of pixels over PCIe instead of 4K. At 4K 24p HEVC this
        // drops `av_hwframe_transfer_data` from ~41 ms / frame down
        // to ~10 ms, which clears the budget for everything
        // downstream and stops the playhead racing the decode.
        // Skipped when source is already at playback resolution
        // (passing `resize=WxH` with the same dimensions still
        // configures NVDEC's resize path unnecessarily).
        if want_resize {
            use std::ffi::CString;
            let resize_str = format!("{resize_w}x{resize_h}");
            let cname = CString::new("resize").unwrap();
            let cval = CString::new(resize_str.as_str()).unwrap();
            let opt_rc = av_opt_set(
                (*ctx_ptr).priv_data,
                cname.as_ptr(),
                cval.as_ptr(),
                AV_OPT_SEARCH_CHILDREN,
            );
            if opt_rc < 0 {
                warn!(
                    "cuvid: av_opt_set(resize={resize_str}) failed rc={opt_rc} on {cuvid_name}; \
                     decode will run at source resolution and downscale on the host"
                );
            }
        }
        if !try_init_cuda(ctx_ptr) {
            return None;
        }
    }
    info!(
        "cuvid: opened {cuvid_name} for codec_id={codec_id:?} src={src_w}x{src_h}{}{}",
        if want_resize { format!(" → resize={resize_w}x{resize_h} (NVDEC)") } else { String::new() },
        match &bsf {
            Some(b) => format!(" / BSF '{}'", b.name()),
            None => String::new(),
        },
    );
    Some((ctx, bsf))
}

/// Push a packet through a BSF and drain *all* of its output into
/// the decoder. Returns `Ok(())` on success or after EAGAIN (BSF
/// needs more input); other errors propagate.
unsafe fn bsf_route_packet_to_decoder(
    bsf: &BsfContext,
    decoder: &mut ffmpeg_next::decoder::Video,
    packet: &mut ffmpeg_next::Packet,
) -> Result<(), i32> {
    use ffmpeg_next::codec::packet::Mut;
    let send_rc = bsf_ffi::av_bsf_send_packet(bsf.as_ptr(), packet.as_mut_ptr());
    if send_rc < 0 {
        return Err(send_rc);
    }
    loop {
        let mut filtered = ffmpeg_next::Packet::empty();
        let recv_rc = bsf_ffi::av_bsf_receive_packet(bsf.as_ptr(), filtered.as_mut_ptr());
        const AVERROR_EAGAIN: i32 = -11; // EAGAIN
        const AVERROR_EOF: i32 = -541478725; // FFERRTAG('E','O','F',' ')
        if recv_rc == AVERROR_EAGAIN || recv_rc == AVERROR_EOF {
            return Ok(());
        }
        if recv_rc < 0 {
            return Err(recv_rc);
        }
        // Send to decoder; if EAGAIN here, we need to drain decoder
        // first — but we've already drained via the caller's loop.
        // A second EAGAIN is rare; log and stash if it happens.
        if let Err(e) = decoder.send_packet(&filtered) {
            warn!("BSF: decoder.send_packet returned {e}");
        }
    }
}

/// Flush the BSF on EOF: send a NULL packet, drain remaining outputs.
unsafe fn bsf_flush_to_decoder(
    bsf: &BsfContext,
    decoder: &mut ffmpeg_next::decoder::Video,
) {
    use ffmpeg_next::codec::packet::Mut;
    bsf_ffi::av_bsf_send_packet(bsf.as_ptr(), std::ptr::null_mut());
    loop {
        let mut filtered = ffmpeg_next::Packet::empty();
        let rc = bsf_ffi::av_bsf_receive_packet(bsf.as_ptr(), filtered.as_mut_ptr());
        if rc < 0 {
            return;
        }
        let _ = decoder.send_packet(&filtered);
    }
}

/// Result of opening a video decoder: the opened `Video`, the
/// hardware tier it ended up using, and any bitstream filter that
/// must run in front of `decoder.send_packet()` for the decode loop
/// (only present on the cuvid path for H.264 / HEVC in MP4).
pub(crate) struct OpenedDecoder {
    pub(crate) decoder: ffmpeg_next::decoder::Video,
    pub(crate) hw_accel: HwAccel,
    pub(crate) bsf: Option<BsfContext>,
}

/// Apply the `thread_count` policy to a fresh codec context. Pure
/// software decoders read `DECODE_THREADS_PREF` (0 = ffmpeg auto);
/// HW decoders default to the same but `KLIPP_HW_SINGLE_THREAD=1`
/// pins them to a single thread (NVDEC parallelises on the GPU; CPU
/// worker threads can confuse hwaccel negotiation — see the
/// commentary on `KLIPP_HW_SINGLE_THREAD`).
/// `sw_threads` caps a software decoder moved off a full GPU (#2116).
unsafe fn apply_thread_count(
    ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext,
    hw_accel: HwAccel,
    sw_threads: Option<usize>,
) {
    let hw_single_thread = hw_accel != HwAccel::Software
        && std::env::var("KLIPP_HW_SINGLE_THREAD").as_deref() == Ok("1");
    let tc = if hw_single_thread {
        1
    } else if let Some(n) = sw_threads.filter(|_| hw_accel == HwAccel::Software) {
        n as i32
    } else {
        DECODE_THREADS_PREF.load(std::sync::atomic::Ordering::Relaxed) as i32
    };
    (*ctx_ptr).thread_count = tc;
    if hw_accel != HwAccel::Software {
        info!(
            "decode_thread: HW={:?}, thread_count set to {tc}{}",
            hw_accel,
            if hw_single_thread { " (KLIPP_HW_SINGLE_THREAD=1)" } else { " (default)" },
        );
    }
}

/// Open the video decoder for `stream`, walking the cuvid → standard +
/// CUDA → standard + VAAPI → software ladder. Each tier is
/// attempted; if its `decoder.video()` open fails (codec / profile
/// not supported, hwaccel init failed, …), we drop the half-built
/// context and try the next tier.
///
/// Returns the first tier that successfully produced an opened
/// `Video`. The returned `OpenedDecoder.bsf` is `Some` only on the
/// cuvid path for codecs that require an `*_mp4toannexb` filter
/// (H.264 + HEVC out of MP4 / MOV containers).
pub(crate) fn open_video_decoder(
    stream: &ffmpeg_next::format::stream::Stream,
    hw_enabled: bool,
    try_cuvid: bool,
    sw_threads: Option<usize>,
) -> Result<OpenedDecoder, ffmpeg_next::Error> {
    // Tier 1: cuvid (standalone NVDEC). Only attempted when the
    // pref AND the env-var gate both authorise it.
    if try_cuvid {
        if let Some((mut ctx, bsf)) = try_open_cuvid_decoder(stream) {
            unsafe { apply_thread_count(ctx.as_mut_ptr(), HwAccel::Cuda, None) };
            match ctx.decoder().video() {
                Ok(decoder) => {
                    return Ok(OpenedDecoder {
                        decoder,
                        hw_accel: HwAccel::Cuda,
                        bsf,
                    });
                }
                Err(e) => {
                    // Most common cause: NVDEC doesn't support this
                    // codec / profile combination (e.g. H.264 Hi10P
                    // is unsupported on every consumer NVIDIA GPU).
                    // Fall through to the standard codec path which
                    // will hand off to software for unsupported HW
                    // codecs.
                    warn!(
                        "cuvid: avcodec_open2 failed for codec {:?}: {e} \
                         — falling back to standard codec",
                        stream.parameters().id(),
                    );
                    // `bsf` and `ctx` drop here automatically.
                    drop(bsf);
                }
            }
        }
    }

    // Tier 2 / 3 / 4: standard codec context with HW-accel
    // attempted in order. `try_init_cuda` and `try_init_vaapi`
    // mutate the context with `hw_device_ctx` + `get_format`; if
    // both decline (HW pref off or device init fails) we end up on
    // the software path with no extra plumbing.
    let mut ctx = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())?;
    let hw_accel = unsafe {
        if hw_enabled && try_init_cuda(ctx.as_mut_ptr()) {
            HwAccel::Cuda
        } else if hw_enabled && try_init_vaapi(ctx.as_mut_ptr()) {
            HwAccel::Vaapi
        } else {
            HwAccel::Software
        }
    };
    unsafe { apply_thread_count(ctx.as_mut_ptr(), hw_accel, sw_threads) };
    let decoder = ctx.decoder().video()?;
    Ok(OpenedDecoder {
        decoder,
        hw_accel,
        bsf: None,
    })
}

/// A software decoder for a stream's `parameters`, on `threads` (all
/// the machine's when `None`): what a stream goes on with once the GPU
/// refuses its hardware frames (#2394). Made from the parameters alone,
/// so the stream's container stays free to seek.
fn open_software_decoder(parameters: ffmpeg_next::codec::Parameters, threads: Option<usize>) -> Result<OpenedDecoder, ffmpeg_next::Error> {
    let mut ctx = ffmpeg_next::codec::context::Context::from_parameters(parameters)?;
    ctx.set_threading(ffmpeg_next::threading::Config { kind: ffmpeg_next::threading::Type::Frame, count: threads.unwrap_or(0) });
    Ok(OpenedDecoder { decoder: ctx.decoder().video()?, hw_accel: HwAccel::Software, bsf: None })
}

/// Send `packet` to `decoder`, through the stream's bitstream filter
/// when it has one (cuvid reading H.264 / HEVC out of MP4).
fn send_to_decoder(bsf: Option<&BsfContext>, decoder: &mut ffmpeg_next::decoder::Video, packet: &mut ffmpeg_next::Packet) -> Result<(), ffmpeg_next::Error> {
    match bsf {
        Some(bsf) => unsafe { bsf_route_packet_to_decoder(bsf, decoder, packet) }.map_err(ffmpeg_next::Error::from),
        None => decoder.send_packet(packet),
    }
}

/// How long a hardware decoder may take to open and make its first
/// frame before the stream is decoded in software: a driver refusing
/// new work blocked the open, or the first frame, for good (#2394). A
/// cold cuvid open at 4K takes well under a second.
const HW_OPEN_LIMIT: Duration = Duration::from_secs(3);

/// How long after a hardware open ran out of time new streams skip
/// the GPU, so each does not wait [`HW_OPEN_LIMIT`] on a driver that is
/// still refusing work.
const HW_SHUNNED_FOR: Duration = Duration::from_secs(30);

/// When the newest hardware open ran out of time.
static HW_OPEN_TIMED_OUT_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Hardware opens that ran out of time.
static HW_OPENS_TIMED_OUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many hardware decoder opens, since the process began, ran out of
/// [`HW_OPEN_LIMIT`] and went on in software (#2394).
pub fn hw_opens_timed_out() -> u64 {
    HW_OPENS_TIMED_OUT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a hardware open ran out of time in the last [`HW_SHUNNED_FOR`].
fn hw_shunned() -> bool {
    HW_OPEN_TIMED_OUT_AT.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| at.elapsed() < HW_SHUNNED_FOR)
}

/// Milliseconds the next hardware open waits before it starts, as one
/// blocked in a refusing driver does: a test's (#2394).
static STALL_NEXT_HW_OPEN_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Have the next hardware decoder open stall for `stall`, and forget an
/// earlier open that ran out of time. For tests.
#[doc(hidden)]
pub fn stall_next_hw_open(stall: Duration) {
    *HW_OPEN_TIMED_OUT_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    STALL_NEXT_HW_OPEN_MS.store(stall.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
}

/// Open `path`'s video stream on the GPU and decode up to its first
/// frame, on a thread of its own, within [`HW_OPEN_LIMIT`]. The decoder
/// comes back flushed, ready for the caller's packets from the start.
/// `None` when the open ran out of time: that thread is left behind,
/// blocked in the driver, and its decoder dropped should it ever return.
fn open_hardware_in_time(path: &Path, try_cuvid: bool) -> Option<Result<OpenedDecoder, ffmpeg_next::Error>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = path.to_path_buf();
    let spawned = std::thread::Builder::new().name("klipp-hw-open".into()).spawn(move || {
        let stall = STALL_NEXT_HW_OPEN_MS.swap(0, std::sync::atomic::Ordering::Relaxed);
        if stall > 0 {
            std::thread::sleep(Duration::from_millis(stall));
        }
        let _ = tx.send(open_hardware_to_first_frame(&owned, try_cuvid));
    });
    if spawned.is_err() {
        return Some(Err(ffmpeg_next::Error::Bug));
    }
    match rx.recv_timeout(HW_OPEN_LIMIT) {
        Ok(opened) => Some(opened),
        Err(_) => {
            warn!("decode_thread: the GPU decoder for {path:?} did not open within {HW_OPEN_LIMIT:?}; decoding in software");
            HW_OPENS_TIMED_OUT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *HW_OPEN_TIMED_OUT_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
            None
        }
    }
}

/// [`open_hardware_in_time`]'s work: open the decoder on a container of
/// its own and decode until the first frame comes.
fn open_hardware_to_first_frame(path: &Path, try_cuvid: bool) -> Result<OpenedDecoder, ffmpeg_next::Error> {
    let mut format_ctx = ffmpeg_next::format::input(path)?;
    let stream = format_ctx.streams().best(ffmpeg_next::media::Type::Video).ok_or(ffmpeg_next::Error::StreamNotFound)?;
    let stream_index = stream.index();
    let mut opened = open_video_decoder(&stream, true, try_cuvid, None)?;
    drop(stream);
    if opened.hw_accel == HwAccel::Software {
        return Ok(opened);
    }
    let mut frame = ffmpeg_next::frame::Video::empty();
    // Enough packets for any decoder's delay before its first frame.
    for (stream, mut packet) in format_ctx.packets().take(256) {
        if stream.index() != stream_index {
            continue;
        }
        let _ = send_to_decoder(opened.bsf.as_ref(), &mut opened.decoder, &mut packet);
        if opened.decoder.receive_frame(&mut frame).is_ok() {
            break;
        }
    }
    opened.decoder.flush();
    if let Some(bsf) = opened.bsf.as_ref() {
        bsf.flush();
    }
    Ok(opened)
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

/// How many hardware frames in a row the GPU may refuse (their transfer
/// or conversion failing) before the stream is decoded in software: a
/// driver that refuses new work otherwise left the picture empty for
/// good, every frame dropped (#2394).
const HW_REFUSALS_TO_SOFTWARE: u32 = 3;

/// Streams moved to software decode after the GPU refused their frames.
static STREAMS_TO_SOFTWARE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many streams went on in software decode, since the process began,
/// after the GPU refused their hardware frames (#2394).
pub fn streams_moved_to_software() -> u64 {
    STREAMS_TO_SOFTWARE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Hardware frames to refuse on purpose, as a driver refusing work does:
/// a test's (#2394).
static REFUSE_HW_FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Have the next `n` hardware-decoded frames fail as a refused transfer
/// does. For tests.
#[doc(hidden)]
pub fn refuse_next_hw_frames(n: u32) {
    REFUSE_HW_FRAMES.store(n, std::sync::atomic::Ordering::Relaxed);
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

/// A delay added to every decoded frame, for tests: a decoder slower than
/// the timeline is what a loaded machine makes, and the frames that race
/// ahead of their streams under load only show up then. Zero outside
/// tests.
static DECODE_DELAY_FOR_TESTS_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Slow every decoder in the process by `ms` per frame (0 to stop). For
/// tests only, and a test that sets it must keep the other engines in its
/// binary from running meanwhile: it slows them too.
#[doc(hidden)]
pub fn set_decode_delay_for_tests(ms: u64) {
    DECODE_DELAY_FOR_TESTS_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// Playback Quality: the viewer renders at `1 / divisor` of its size while
/// playing — 1 full, 2 half, 4 quarter. Paused frames stay full size, and
/// export never reads it.
static PLAYBACK_QUALITY_DIVISOR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub fn set_playback_quality_divisor(divisor: u8) {
    PLAYBACK_QUALITY_DIVISOR.store(divisor.clamp(1, 4), std::sync::atomic::Ordering::Relaxed);
}

pub fn playback_quality_divisor() -> u8 {
    PLAYBACK_QUALITY_DIVISOR.load(std::sync::atomic::Ordering::Relaxed)
}

/// Working colorspace the decoder lands frames in. u8 tag matches
/// `klipp_core::WorkingColorspace::as_u8`:
///   0 = Rec709Linear (default — pre-upgrade pipeline behaviour)
///   1 = AcesCG       (linear AP1 primaries, D60 white)
///   2 = Bt2020Linear (linear BT.2020 primaries, D65 white)
/// Unknown tags fall back to `Rec709Linear` via
/// `WorkingColorspace::from_u8`. The setter is the only sanctioned way
/// to change this — driven by project state when a `.klipp` file
/// loads / changes.
static WORKING_COLORSPACE_PREF: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the active working colorspace. Called by the engine when the
/// loaded project's `working_colorspace` changes.
pub fn set_working_colorspace(ws: klipp_core::WorkingColorspace) {
    WORKING_COLORSPACE_PREF.store(ws.as_u8(), std::sync::atomic::Ordering::Relaxed);
}

/// Read the active working colorspace. Lock-free; safe to call from
/// the decode thread per-frame and from the engine's grade-chain
/// dispatch.
pub fn working_colorspace() -> klipp_core::WorkingColorspace {
    klipp_core::WorkingColorspace::from_u8(
        WORKING_COLORSPACE_PREF.load(std::sync::atomic::Ordering::Relaxed),
    )
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

/// Whether hardware decode is enabled in preferences.
pub fn hardware_decode_enabled() -> bool {
    HARDWARE_DECODE_PREF.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set decode thread count from preferences (0 = auto/all cores).
pub fn set_decode_threads(count: u32) {
    DECODE_THREADS_PREF.store(count, std::sync::atomic::Ordering::Relaxed);
}

/// Set the frame queue capacity from preferences.
pub fn set_frame_queue_size(size: u32) {
    FRAME_QUEUE_SIZE_PREF.store(size.max(4), std::sync::atomic::Ordering::Relaxed);
}

/// The frames a decoder holds decoded ahead, from preferences.
pub(crate) fn frame_queue_size() -> usize {
    FRAME_QUEUE_SIZE_PREF.load(std::sync::atomic::Ordering::Relaxed) as usize
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

/// NVDEC resize target for the hardware-decode path.
///
/// Honours the user's "Max playback resolution" preference, the same as
/// the software path — [`playback_dimensions`] with no rotation, since
/// the NVDEC resize happens before rotation is applied.
///
/// This used to hardcode a 1920×1080 bounding box "regardless of user
/// prefs", justified by PCIe transfer of full-res hwframes costing
/// ~41 ms/frame at 4K. Two things retired that reasoning. NVDEC now
/// performs the downscale itself before the frame is transferred, so the
/// 41 ms figure described a cost the current pipeline no longer pays at
/// all. And the preference it overrode offers a 4K option in the UI,
/// which meant selecting it silently did nothing for hardware-decoded
/// media — i.e. for most media. A setting that cannot be observed to
/// work is worse than no setting.
fn hw_transfer_cap_dimensions(src_w: u32, src_h: u32) -> (u32, u32) {
    playback_dimensions(src_w, src_h, 0)
}

#[cfg(test)]
mod gpu_frames_gate_tests {
    use super::gpu_frames_from_env;

    /// `KLIPP_GPU_FRAMES=0` disables the GPU-resident hand-off, and
    /// nothing else does.
    ///
    /// Worth a test precisely because the default is now on: every launch
    /// and every test run takes the enabled branch, so a polarity slip
    /// here would leave the escape hatch silently broken — and the escape
    /// hatch is what someone reaches for when a picture looks wrong. The
    /// two branches producing the same pixels is covered separately, by
    /// `nv12_gpu_frame_identity` and `p010_gpu_frame_identity`.
    #[test]
    fn only_zero_turns_it_off() {
        assert!(gpu_frames_from_env(None), "unset means on");
        assert!(!gpu_frames_from_env(Some("0")), "0 means off");
        assert!(gpu_frames_from_env(Some("1")), "1 means on");
        // Anything else is on, rather than off-by-accident: a typo should
        // not quietly cost the viewer its GPU paths.
        for v in ["", "true", "yes", "00", "0 ", "2", "false", "off"] {
            assert!(gpu_frames_from_env(Some(v)), "{v:?} should leave it on");
        }
    }
}

#[cfg(test)]
mod log_format_registry_tests {
    use super::*;
    use klipp_core::LogFormat;

    /// The caller re-decodes only when told something changed, so the
    /// answer has to be right both ways: a real change says so, a resync of
    /// the same pool does not (a re-decode on every media sync would reseek
    /// the viewer constantly), and sRGB entries count as "no entry".
    #[test]
    fn reports_changes_only() {
        let a = PathBuf::from("/registry-test/a.mp4");
        let b = PathBuf::from("/registry-test/b.mp4");
        set_media_log_formats([(a.clone(), LogFormat::Srgb)]);
        assert!(set_media_log_formats([(a.clone(), LogFormat::SLog3)]), "Srgb -> SLog3 is a change");
        assert!(!set_media_log_formats([(a.clone(), LogFormat::SLog3)]), "same map again is not");
        assert!(
            !set_media_log_formats([(a.clone(), LogFormat::SLog3), (b.clone(), LogFormat::Srgb)]),
            "adding an sRGB file changes nothing the decoders use",
        );
        assert!(set_media_log_formats([(a, LogFormat::Srgb), (b, LogFormat::Srgb)]), "SLog3 -> Srgb is a change");
    }
}

#[cfg(test)]
mod hw_cap_tests {
    use super::*;

    /// The hardware path must honour "Max playback resolution".
    ///
    /// It previously hardcoded 1920×1080 and ignored the preference, so
    /// selecting 4K in the UI did nothing for hardware-decoded media and
    /// nothing caught it — there was no test on this function at all.
    ///
    /// Both directions are asserted in one test on purpose: the
    /// preference is a process-global atomic, so splitting this across
    /// two tests would have them race each other under the default
    /// parallel runner.
    #[test]
    fn hw_resize_target_follows_the_playback_resolution_pref() {
        let restore = max_playback_height();

        // 4K selected: a 4K source must pass through untouched.
        set_max_playback_height(2160);
        assert_eq!(
            hw_transfer_cap_dimensions(3840, 2160),
            (3840, 2160),
            "with the pref at 4K, a 4K source must not be downscaled",
        );

        // 1080p selected: the same source must come back halved,
        // aspect-preserved.
        set_max_playback_height(1080);
        assert_eq!(
            hw_transfer_cap_dimensions(3840, 2160),
            (1920, 1080),
            "with the pref at 1080p, a 4K source must be downscaled",
        );

        // Sub-cap sources are never touched either way.
        assert_eq!(hw_transfer_cap_dimensions(1280, 720), (1280, 720));

        // Portrait: the box turns with the picture. A portrait 4K clip at
        // the 4K setting is decoded whole, not cut to 1214x2160, and at
        // 1080p it is the portrait of 1080p. The software path sizes after
        // rotation and the hardware path before it; they agree.
        set_max_playback_height(2160);
        assert_eq!(playback_dimensions(3840, 2160, 90), (2160, 3840), "a portrait 4K clip at the 4K setting is not downscaled");
        assert_eq!(playback_dimensions(2160, 3840, 0), (2160, 3840));
        set_max_playback_height(1080);
        assert_eq!(playback_dimensions(3840, 2160, 90), (1080, 1920), "a portrait 4K clip at 1080p decodes at 1080x1920");
        assert_eq!(playback_dimensions(2160, 3840, 0), (1080, 1920));
        assert_eq!(hw_transfer_cap_dimensions(3840, 2160), (1920, 1080), "the hardware path, before rotation, agrees");

        set_max_playback_height(restore);
    }
}

/// The size a frame is decoded at under "Max playback resolution": fitted,
/// aspect preserved, into a box the preference's height on its short side
/// and twice that on its long side, turned with the picture. For 90° and
/// 270° rotation the display dimensions are the swapped ones.
///
/// The box used to be landscape whatever the picture, so a portrait clip
/// was fitted by its long side into the preference's height: at the 4K
/// setting a 2160x3840 iPhone clip decoded at 1214x2160, and the viewer and
/// every scope saw 56 % of it. Turned, the software path (sized after
/// rotation) and the hardware path (sized before it) agree.
pub(crate) fn playback_dimensions(src_w: u32, src_h: u32, rotation: i32) -> (u32, u32) {
    let (eff_w, eff_h) = if rotation == 90 || rotation == 270 {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    };
    let (box_w, box_h) = if eff_h > eff_w {
        (max_playback_height(), max_playback_width())
    } else {
        (max_playback_width(), max_playback_height())
    };
    if eff_w <= box_w && eff_h <= box_h {
        return (eff_w, eff_h);
    }
    let scale = (box_w as f64 / eff_w as f64).min(box_h as f64 / eff_h as f64);
    let out_w = ((eff_w as f64 * scale) as u32) & !1; // even width
    let out_h = ((eff_h as f64 * scale) as u32) & !1; // even height
    (out_w.max(2), out_h.max(2))
}

/// A semi-planar YUV 4:2:0 frame in memory: NV12 (8-bit) or P010 (10 bits
/// in the top of 16), its luma plane and its interleaved chroma plane.
pub(crate) struct SemiPlanar<'a> {
    pub y: &'a [u8],
    pub uv: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub y_stride: u32,
    pub uv_stride: u32,
}

/// The GPU's YUV converters, NV12 and P010, as one: a frame downloaded
/// from the hardware decoder and one decoded in software take the same
/// route to the GPU (#2116).
pub(crate) trait SemiPlanarGpu {
    fn on_gpu(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> klipp_effects::p010_gpu::P010GpuOutput;
    fn hdr_codes(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Option<Vec<u32>>;
    fn to_rgba(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Vec<u8>;
    fn device(&self) -> Arc<wgpu::Device>;
    fn queue(&self) -> Arc<wgpu::Queue>;
}

impl SemiPlanarGpu for klipp_effects::p010_gpu::GpuP010Converter {
    fn on_gpu(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> klipp_effects::p010_gpu::P010GpuOutput {
        self.convert_p010_on_gpu(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn hdr_codes(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Option<Vec<u32>> {
        self.convert_p010_hdr_codes(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn to_rgba(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Vec<u8> {
        self.convert_p010_to_rgba(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn device(&self) -> Arc<wgpu::Device> {
        klipp_effects::p010_gpu::GpuP010Converter::device(self)
    }
    fn queue(&self) -> Arc<wgpu::Queue> {
        klipp_effects::p010_gpu::GpuP010Converter::queue(self)
    }
}

impl SemiPlanarGpu for klipp_effects::nv12_gpu::GpuNv12Converter {
    fn on_gpu(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> klipp_effects::p010_gpu::P010GpuOutput {
        self.convert_nv12_on_gpu(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn hdr_codes(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Option<Vec<u32>> {
        self.convert_nv12_hdr_codes(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn to_rgba(&mut self, f: &SemiPlanar, out: (u32, u32), rotation: u32, ids: [u32; 4], pm: [[f32; 4]; 3]) -> Vec<u8> {
        self.convert_nv12_to_rgba(f.y, f.uv, f.width, f.height, f.y_stride, f.uv_stride, out.0, out.1, rotation, ids, pm)
    }
    fn device(&self) -> Arc<wgpu::Device> {
        klipp_effects::nv12_gpu::GpuNv12Converter::device(self)
    }
    fn queue(&self) -> Arc<wgpu::Queue> {
        klipp_effects::nv12_gpu::GpuNv12Converter::queue(self)
    }
}

/// Convert `f` on the GPU to `out`'s size, turned by `rotation`, coloured
/// as `source_cs` says: the frame's CPU pixels, its GPU picture and its
/// HDR codes. The shader picks PQ/HLG vs BT.709 transfer, BT.2020 vs
/// BT.709 matrix/primaries, and limited vs full range from the source's
/// IDs — no ad-hoc HDR sniffing in the host code. PQ/HLG and camera-log
/// sources also get packed 10-bit code values, which become the grade's
/// input (see `crate::types::HdrInput`); `data` stays display-referred
/// for every other consumer.
pub(crate) fn semi_planar_frame(
    converter: &mut dyn SemiPlanarGpu,
    f: &SemiPlanar,
    out: (u32, u32),
    rotation: i32,
    source_cs: klipp_core::SourceColorspace,
    linear_us: &std::sync::atomic::AtomicU64,
) -> (Vec<f32>, Option<Arc<GpuFrame>>, Option<crate::types::HdrInput>) {
    use std::sync::atomic::Ordering;
    let (out_w, out_h) = out;
    let rotation = rotation as u32;
    let color_ids = klipp_core::color_ids::ids(source_cs);
    let hdr_transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(color_ids[2]);
    let (source_to_working, pm) = primaries_matrix_for_source(source_cs);
    if should_attach_gpu_source() {
        // GPU-resident handoff. The shader runs; the buffers stay on the
        // GPU and CPU `data` stays empty — consumers that need it call
        // `LinearFrame::ensure_cpu_data`, which reads the u8 output (for
        // HDR, the tone-mapped preview).
        let gpu = converter.on_gpu(f, out, rotation, color_ids, pm);
        let device = converter.device();
        let queue = converter.queue();
        let hdr = match (hdr_transfer, gpu.hdr_codes) {
            (Some(transfer), Some(buffer)) => Some(crate::types::HdrInput {
                codes: crate::types::HdrCodes::Gpu { buffer, device: Arc::clone(&device), queue: Arc::clone(&queue) },
                transfer,
                source_to_working,
                width: out_w,
                height: out_h,
            }),
            _ => None,
        };
        let attach = Arc::new(GpuFrame::new(device, queue, gpu.rgba, out_w, out_h));
        (Vec::new(), Some(attach), hdr)
    } else if let Some(transfer) = hdr_transfer {
        // HDR on the readback path: read back the codes — the same 4
        // bytes a pixel the u8 frame cost — and derive the
        // display-referred `data` from them on the CPU. The grade
        // re-derives scene-linear from the same codes, so the two cannot
        // drift.
        match converter.hdr_codes(f, out, rotation, color_ids, pm) {
            Some(codes) => {
                let t_linear = std::time::Instant::now();
                let mut display = klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(&codes, transfer, source_to_working);
                klipp_color::hdr::apply_hdr_display_tonemap(&mut display, transfer);
                linear_us.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);
                let hdr = crate::types::HdrInput {
                    codes: crate::types::HdrCodes::Cpu(Arc::new(klipp_color::hdr::SceneCodes::Packed10(codes))),
                    transfer,
                    source_to_working,
                    width: out_w,
                    height: out_h,
                };
                (display, None, Some(hdr))
            }
            None => {
                // Readback failed: fall back to the preview, graded as
                // display-referred. Wrong for grading, but a picture
                // rather than a hole.
                let bytes = converter.to_rgba(f, out, rotation, color_ids, pm);
                (rgba_u8_to_linear(&bytes), None, None)
            }
        }
    } else {
        let bytes = converter.to_rgba(f, out, rotation, color_ids, pm);
        let t_linear = std::time::Instant::now();
        let lin = rgba_u8_to_linear(&bytes);
        linear_us.fetch_add(t_linear.elapsed().as_micros() as u64, Ordering::Relaxed);
        (lin, None, None)
    }
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
    let source_cs = apply_log_override(klipp_core::SourceColorspace::resolve_with_baseline(
        (*sw_frame).color_range as i32,
        (*sw_frame).colorspace as i32,
        (*sw_frame).color_primaries as i32,
        (*sw_frame).color_trc as i32,
        cs_baseline,
    ));
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
            let (linear, gpu_source, hdr) = semi_planar_frame(converter, &SemiPlanar { y: y_data, uv: uv_data, width: src_w, height: src_h, y_stride, uv_stride }, (out_w, out_h), source_rotation, source_cs, &PERF_LINEAR_ENCODE_US);
            // Reuse the NV12 perf bucket — the meaning is identical
            // (GPU YUV→RGBA encode time per frame).
            PERF_NV12_CONV_US.fetch_add(t_p010.elapsed().as_micros() as u64, Ordering::Relaxed);

            av_frame_free(&mut (sw_frame as *mut _));

            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            PERF_GPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);
            log_first_resident_delivery(gpu_source.is_some(), "p010_cuvid");
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
                hdr,
                seek_epoch: 0,
            });
        }
    }

    // The 8-bit branch. P010 was taken on the GPU above, so what falls
    // through here is a 10-bit format that is *not* P010 — those go to the
    // CPU swscale path, which knows about RGBA64LE. Said explicitly
    // because the earlier wording ("the GPU fast path is 8-bit-only")
    // read as though no 10-bit source ever reached the GPU, which has not
    // been true since the P010 converter landed above.
    if !is_hi_bit_depth && sw_fmt == AVPixelFormat::AV_PIX_FMT_NV12 {
        if let Some(converter) = gpu_converter {
            let y_stride = (*sw_frame).linesize[0] as u32;
            let uv_stride = (*sw_frame).linesize[1] as u32;
            let y_data_len = (y_stride * src_h) as usize;
            let uv_data_len = (uv_stride * (src_h / 2)) as usize;

            let y_data = std::slice::from_raw_parts((*sw_frame).data[0], y_data_len);
            let uv_data = std::slice::from_raw_parts((*sw_frame).data[1], uv_data_len);

            let t_nv12 = std::time::Instant::now();
            let (linear, gpu_source, hdr) = semi_planar_frame(converter, &SemiPlanar { y: y_data, uv: uv_data, width: src_w, height: src_h, y_stride, uv_stride }, (out_w, out_h), source_rotation, source_cs, &PERF_LINEAR_ENCODE_US);
            PERF_NV12_CONV_US.fetch_add(t_nv12.elapsed().as_micros() as u64, Ordering::Relaxed);

            av_frame_free(&mut (sw_frame as *mut _));

            let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            PERF_GPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);
            log_first_resident_delivery(gpu_source.is_some(), "nv12_cuvid");
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
                hdr,
                seek_epoch: 0,
            });
        }
    }
    // Reaching here = CPU fallback path
    PERF_CPU_PATH_COUNT.fetch_add(1, Ordering::Relaxed);

    // Scene-referred frames with no GPU converter: the CPU twin of the
    // shaders' codes route rather than swscale's display-referred RGB.
    if let Some(frame) = scene_referred_linear_frame(sw_frame, source_cs, source_rotation) {
        av_frame_free(&mut (sw_frame as *mut _));
        log_first_resident_delivery(false, "scene_referred_cpu");
        return Some(frame);
    }

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
    log_first_resident_delivery(false, "swscale_cpu");
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
        hdr: None,
        seek_epoch: 0,
    })
}

/// One-shot info log per delivery mode so the operator can confirm
/// whether the decoder actually delivered a GPU-resident `gpu_source`
/// or fell through to CPU `data`. Helps validate
/// the GPU-resident decode path end-to-end without sifting per-frame
/// trace.
/// `mode` is a short tag distinguishing which path produced the
/// frame (e.g. `p010_cuvid`, `nv12_cuvid`, `swscale_cpu`).
fn log_first_resident_delivery(gpu_resident: bool, mode: &'static str) {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    static GPU_LOGGED: AtomicBool = AtomicBool::new(false);
    static CPU_LOGGED: AtomicBool = AtomicBool::new(false);
    if gpu_resident {
        if !GPU_LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "decode delivery: GPU-resident frame delivered (mode={mode}; \
                 gpu_source attached, CPU readback deferred until consumer asks)"
            );
        }
    } else if !CPU_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::info!(
            "decode delivery: CPU frame delivered (mode={mode}; \
             gpu_source absent, full CPU data populated up front)"
        );
    }
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
    if !count.is_multiple_of(60) {
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
    last_decode_us: Arc<std::sync::atomic::AtomicU32>,
    last_decoded_pts: Arc<std::sync::atomic::AtomicU64>,
    lease_id: u64,
    decoding: crate::decode_budget::Decoding,
    source_fps: f64,
) {
    DECODE_PATH.with(|p| *p.borrow_mut() = Some(path.clone()));
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
    // A still image is one frame at time 0 (#978). Seeking into it as
    // into a video lands past that frame and finds nothing, so a clip of
    // it was black wherever its source time was not 0. Every seek reads
    // the image again from the start and stamps it with the time asked.
    let still = klipp_core::media_extensions::is_still_image_demuxer(format_ctx.format().name());
    // The time the next frame is stamped with, for a still.
    let mut still_at: Option<f64> = None;

    // Decoder selection ladder (each tier is attempted; if its
    // `decoder.video()` open fails — codec / profile not supported,
    // hwaccel init failed, etc. — we drop the half-built context and
    // try the next tier:
    //
    //   1. cuvid (`*_cuvid`)   — NVIDIA's dedicated NVDEC decoders.
    //                            Native CUDA hwframes; needs a
    //                            `*_mp4toannexb` BSF in front for
    //                            H.264 / HEVC stored in MP4.
    //                            Limits: H.264 Hi10P (10-bit) is not
    //                            supported in NVDEC hardware on any
    //                            consumer NVIDIA GPU — `h264_cuvid`
    //                            errors with "Bit depth 10 with this
    //                            chroma format is not supported".
    //                            HEVC Main 10 (10-bit HEVC) IS
    //                            supported and is the path we want
    //                            for Sony / iPhone / etc. footage.
    //   2. CUDA hwaccel        — standard codec + cuda hwaccel +
    //                            hw_frames_ctx. ffmpeg CLI uses this
    //                            shape; we observed silent SW fallback
    //                            in our setup but it's harmless.
    //   3. VAAPI hwaccel       — H.264 / HEVC on Intel iGPU.
    //                            `nvidia-vaapi-driver` is broken for
    //                            AV1 / VP9 (all-zero frames).
    //   4. Software            — libdav1d / libvpx multi-threaded.
    //
    // Cuvid is now the default HW path. Codec matrix verified end-
    // to-end: H.264 8-bit (1080p + 4K), HEVC Main 10 (Sony A6700,
    // 4K), AV1 8-bit + 10-bit, VP9 Profile 2 — all open via cuvid
    // when supported, and unsupported profiles (notably H.264 Hi10P,
    // which no consumer NVIDIA GPU can handle) drop cleanly to the
    // standard codec path via the avcodec_open2-failure branch in
    // open_video_decoder. `KLIPP_USE_CUVID=0` disables cuvid and
    // forces the standard CUDA hwaccel / VAAPI / SW ladder for
    // diagnosis. When cuvid succeeds the BSF (if any) lives
    // alongside the codec context for the lifetime of this decode
    // thread; the decode loop pipes every packet through the BSF
    // before sending it to the decoder.
    // A stream moved off a full GPU decodes in software on the threads
    // it was given (#2116).
    let sw_threads = match decoding {
        crate::decode_budget::Decoding::Software { threads } => Some(threads),
        crate::decode_budget::Decoding::Gpu => None,
    };
    let hw_enabled = sw_threads.is_none() && HARDWARE_DECODE_PREF.load(std::sync::atomic::Ordering::Relaxed) && !hw_shunned();
    let try_cuvid = hw_enabled && std::env::var("KLIPP_USE_CUVID").as_deref() != Ok("0");

    // The GPU open, and its first frame, within a time limit; software
    // when it runs out (#2394).
    let hw_opened = if hw_enabled && !still { open_hardware_in_time(&path, try_cuvid) } else { None };
    // A still opens as it always did; a GPU open out of time, or no GPU
    // wanted, goes on in software.
    let opened = match hw_opened.unwrap_or_else(|| open_video_decoder(&video_stream, hw_enabled && still, try_cuvid && still, sw_threads)) {
        Ok(o) => o,
        Err(e) => {
            warn!("decode_thread: video decoder error: {e}");
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(LinearFrame::eof());
            return;
        }
    };
    let mut decoder = opened.decoder;
    // What a software decoder for this stream is made from, should the
    // GPU refuse its frames (#2394).
    let stream_parameters = video_stream.parameters();
    let mut bsf_ctx = opened.bsf;
    let mut hw_accel = opened.hw_accel;
    // Hardware frames the GPU refused in a row, and the time of the
    // first of them (#2394).
    let mut hw_refused: u32 = 0;
    let mut first_refused_secs = 0.0;

    if hw_accel != HwAccel::Software {
        hw_active_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    } else if sw_threads.is_none() {
        crate::decode_budget::opened_in_software(lease_id);
    }
    // What this thread decodes and waits is the GPU's measure (#2116).
    let mut meter = crate::decode_budget::Meter::new(hw_accel != HwAccel::Software, sw_threads);

    // Capture the source's colourspace from the AVCodecParameters
    // before the decoder starts producing frames. NVDEC's
    // `av_hwframe_transfer_data` typically strips per-frame colour
    // tags during download (we've seen `color_range=pc` files arrive
    // here as `unspecified`), so the codec-level values are the
    // authoritative baseline for everything downstream. Each frame
    // overrides individual fields where its own AVFrame tags are
    // actually specified.
    let (cs_baseline_from_codecpar, source_pixels): (klipp_core::SourceColorspace, u64) = unsafe {
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
        (klipp_core::SourceColorspace::resolve(range_r, space_r, pri_r, trc_r, w, h), w as u64 * h as u64)
    };
    crate::decode_budget::set_rate(lease_id, source_pixels as f64 * source_fps);

    // KLIPP_FORCE_LOG_FORMAT is the test/dev override before the
    // per-clip UI exists. Setting it to one of the recognised tokens
    // replaces the baseline's transfer + primaries with the matching
    // camera-native pair (e.g. S-Log3 + S-Gamut3.Cine), which the
    // shaders and the CPU swscale path then dispatch on. Container
    // metadata never tags these — every NLE asks the user.
    let cs_baseline = cs_baseline_from_codecpar;
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
    // Software frames repacked for the GPU's YUV converters (#2116).
    let mut sw_repack: Option<SwRepack> = None;
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
    // The presentation time of the last frame this thread decoded, to
    // tell a seek just ahead of it from one that needs the container.
    let mut last_pts_secs: Option<f64> = None;
    // The epoch every frame produced from here on is stamped with. Advanced
    // when a seek is applied, not when it is received, so a frame converted
    // before the seek keeps the older epoch and consumers can drop it.
    let mut seek_epoch: u64 = 0;
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

        // Drain the entire pending queue first, coalescing
        // SeekTo to the last one. A scrub-drag can pile dozens
        // of seeks onto this thread; processing each one means a
        // full avformat seek + decode preroll (~50 ms on 4K
        // HEVC), pinning the CPU and stalling the visible
        // playhead behind the cursor by hundreds of ms. Only the
        // newest scrub target matters.
        let mut latest_seek: Option<(f64, u64, bool)> = None;
        let mut got_stop = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecodeCommand::SeekTo(secs, epoch) => {
                    // Coalesced: the newest seek wins, and its epoch with it.
                    latest_seek = Some((secs, epoch, false));
                }
                DecodeCommand::SeekOn(secs, epoch) => {
                    latest_seek = Some((secs, epoch, true));
                }
                DecodeCommand::Pause => paused = true,
                DecodeCommand::Resume => {
                    paused = false;
                    eof_sent = false;
                }
                DecodeCommand::Stop => {
                    got_stop = true;
                    break;
                }
            }
        }
        if got_stop {
            debug!("decode_thread: stop");
            return;
        }
        if let Some((secs, epoch, decode_on)) = latest_seek {
            seek_epoch = epoch;
            let (secs, decode_on) = still_seek(still, secs, decode_on, &mut still_at);
            if decode_on {
                // Keep decoding forward and skip up to the target: a
                // scrub steps a frame or two at a time, and a container
                // seek would decode the whole group of pictures again
                // from its keyframe for every step. The requester kept
                // the queued frames past the target; frames decoded
                // since then are restamped here.
                let kept = frame_queue.lock().unwrap_or_else(|e| e.into_inner()).keep_from(secs - SEEK_PTS_SLACK, epoch);
                debug!("decode_thread: seek to {secs:.3}s by decoding on from {:.3}s, {kept} queued (epoch {epoch})", last_pts_secs.unwrap_or(0.0));
                seek_target = Some(secs);
                eof_sent = false;
            } else {
                let ts = (secs * 1_000_000.0) as i64;
                debug!("decode_thread: seek to {secs:.3}s (ts={ts} us, epoch {epoch})");
                drop(packets);
                let _ = format_ctx.seek(ts, ..ts);
                packets = format_ctx.packets();
                decoder.flush();
                if let Some(bsf) = bsf_ctx.as_ref() {
                    bsf.flush();
                }
                frame_queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
                seek_target = Some(secs);
                last_pts_secs = None;
                publish_pts(&last_decoded_pts, None);
                eof_sent = false;
                eof_from_demuxer = false;
                pending_packet = None;
            }
        }

        if paused {
            std::thread::sleep(Duration::from_millis(5));
            meter.idle(Duration::from_millis(5));
            continue;
        }

        if eof_sent {
            std::thread::sleep(Duration::from_millis(5));
            meter.idle(Duration::from_millis(5));
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
                meter.idle(Duration::from_millis(1));
                // Re-check commands (seek/stop) while waiting.
                // Drain the whole queue and coalesce SeekTo to the
                // latest, same reasoning as the top-of-loop drain.
                let mut latest_inner_seek: Option<(f64, u64, bool)> = None;
                let mut inner_break = false;
                let mut inner_return = false;
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        DecodeCommand::SeekTo(secs, epoch) => {
                            latest_inner_seek = Some((secs, epoch, false));
                        }
                        DecodeCommand::SeekOn(secs, epoch) => {
                            latest_inner_seek = Some((secs, epoch, true));
                        }
                        DecodeCommand::Pause => { paused = true; inner_break = true; break; }
                        DecodeCommand::Resume => { paused = false; eof_sent = false; }
                        DecodeCommand::Stop => { inner_return = true; break; }
                    }
                }
                if inner_return { return; }
                if let Some((secs, epoch, decode_on)) = latest_inner_seek {
                    seek_epoch = epoch;
                    let (secs, decode_on) = still_seek(still, secs, decode_on, &mut still_at);
                    if decode_on {
                        let kept = frame_queue.lock().unwrap_or_else(|e| e.into_inner()).keep_from(secs - SEEK_PTS_SLACK, epoch);
                        debug!("decode_thread: seek to {secs:.3}s by decoding on, {kept} queued (epoch {epoch}) (in drain)");
                        seek_target = Some(secs);
                        eof_sent = false;
                        continue; // the queue has room again; keep draining
                    }
                    let ts = (secs * 1_000_000.0) as i64;
                    debug!("decode_thread: seek to {secs:.3}s (ts={ts} us, epoch {epoch}) (in drain)");
                    drop(packets);
                    let _ = format_ctx.seek(ts, ..ts);
                    packets = format_ctx.packets();
                    decoder.flush();
                    if let Some(bsf) = bsf_ctx.as_ref() {
                        bsf.flush();
                    }
                    frame_queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    seek_target = Some(secs);
                    last_pts_secs = None;
                    publish_pts(&last_decoded_pts, None);
                    eof_sent = false;
                    eof_from_demuxer = false;
                    pending_packet = None;
                    break; // restart outer loop
                }
                if inner_break { break; }
                continue; // retry drain
            }

            let t_decode = std::time::Instant::now();
            let got_frame = decoder.receive_frame(&mut received_frame).is_ok();
            let this_decode_us = t_decode.elapsed().as_micros() as u64;
            perf_decode_us += this_decode_us;

            if !got_frame {
                // No more buffered frames — need to feed more packets
                break;
            }
            meter.frame(received_frame.width() as u64 * received_frame.height() as u64);

            let pts_secs = received_frame
                .pts()
                .map(|pts| pts as f64 * tb_f64)
                .unwrap_or(0.0);

            // Skip pre-seek frames
            last_pts_secs = Some(pts_secs);
            publish_pts(&last_decoded_pts, last_pts_secs);
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
            // One-shot per-process: log the actual format the master
            // decoder delivered + which path we're routing to. The
            // `get_format` callback can succeed at init but later
            // worker probes still authorise SW auto-download — this
            // tells us what actually arrives instead of guessing.
            {
                use std::sync::atomic::AtomicBool;
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let fmt_name = unsafe {
                        let f = std::mem::transmute::<i32, ffmpeg_next::ffi::AVPixelFormat>((*received_frame.as_ptr()).format);
                        pix_fmt_name(f)
                    };
                    info!(
                        "decode_thread: first delivered frame fmt={fmt_name} → routing to {}",
                        if is_hw_frame { "download_hw_frame (GPU shader path)" }
                        else { "convert_sw_frame (CPU path)" },
                    );
                }
            }

            let t_transfer = std::time::Instant::now();
            let frame_result = if is_hw_frame {
                hw_active_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                let refused = REFUSE_HW_FRAMES.fetch_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |n| n.checked_sub(1)).is_ok();
                if refused { None } else { unsafe { download_hw_frame(&received_frame, &mut hw_scaler, gpu_nv12.as_mut(), &mut gpu_p010, source_rotation, cs_baseline) } }
            } else {
                sw_frame_on_gpu(&received_frame, &mut sw_repack, &mut gpu_nv12, &mut gpu_p010, source_rotation, cs_baseline)
                    .or_else(|| convert_sw_frame(&received_frame, &mut sw_scaler, &mut perf_copy_us, source_rotation, cs_baseline))
            };

            let mut linear_frame = match frame_result {
                Some(result) => {
                    hw_refused = 0;
                    result
                }
                None => {
                    warn!("decode_thread: frame conversion failed, skipping");
                    // The GPU refusing a hardware frame again and again
                    // (a driver out of channels or memory): the stream goes
                    // on in software from the frame it refused (#2394).
                    hw_refused = if is_hw_frame { hw_refused + 1 } else { 0 };
                    if hw_refused == 1 {
                        first_refused_secs = pts_secs;
                    }
                    if hw_refused >= HW_REFUSALS_TO_SOFTWARE {
                        hw_refused = 0;
                        match open_software_decoder(stream_parameters.clone(), sw_threads) {
                            Ok(software) => {
                                warn!("decode_thread: the GPU refused {HW_REFUSALS_TO_SOFTWARE} frames in a row; decoding in software from {first_refused_secs:.3}s");
                                decoder = software.decoder;
                                bsf_ctx = software.bsf;
                                hw_accel = software.hw_accel;
                                hw_active_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                                STREAMS_TO_SOFTWARE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let ts = (first_refused_secs * 1_000_000.0) as i64;
                                let _ = format_ctx.seek(ts, ..ts);
                                packets = format_ctx.packets();
                                seek_target = Some(first_refused_secs);
                                last_pts_secs = None;
                                publish_pts(&last_decoded_pts, None);
                                eof_sent = false;
                                eof_from_demuxer = false;
                                pending_packet = None;
                            }
                            Err(e) => warn!("decode_thread: the GPU refused its frames and software decode would not open: {e}"),
                        }
                    }
                    continue; // skip this frame but keep draining
                }
            };

            let this_transfer_us = t_transfer.elapsed().as_micros() as u64;
            perf_transfer_us += this_transfer_us;
            // Publish this frame's decode + transfer cost for the perf
            // overlay. receive_frame alone under-reports on async HW
            // decoders, so include the download/scale step too.
            last_decode_us.store(
                (this_decode_us + this_transfer_us).min(u32::MAX as u64) as u32,
                std::sync::atomic::Ordering::Relaxed,
            );

            // Stamp pts on the decoder-produced linear frame, then push
            // it straight into the queue — the queue is linear-typed
            // as of Phase 10a. Consumers on the legacy u8 path call
            // [`FrameQueue::pop_u8`] which does the conversion on their
            // side; linear consumers read it directly.
            linear_frame.pts = RationalTime::from_secs_f64(still_at.take().unwrap_or(pts_secs), 90000);
            // Which seek this frame belongs to. Stamped here, at the push,
            // because this is the only place that knows: a frame already in
            // flight when a seek arrives was converted under the old epoch.
            linear_frame.seek_epoch = seek_epoch;
            let (frame_w, frame_h) = (linear_frame.width, linear_frame.height);

            let t_queue = std::time::Instant::now();
            let delay = DECODE_DELAY_FOR_TESTS_MS.load(std::sync::atomic::Ordering::Relaxed);
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
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
        //
        // When `bsf_ctx` is `Some`, packets go through the bitstream
        // filter first — `bsf_route_packet_to_decoder` handles both
        // sending the input packet to the BSF and forwarding all
        // resulting filtered packets to the decoder. This is
        // required for cuvid decoders consuming H.264/HEVC out of
        // MP4 containers (avcc/hvcc → annex B).

        // Retry pending packet first (was EAGAIN last time)
        if let Some(mut pkt) = pending_packet.take() {
            let send_result = send_to_decoder(bsf_ctx.as_ref(), &mut decoder, &mut pkt);
            match send_result {
                Ok(()) => {}
                Err(e) => {
                    warn!("decode_thread: send_packet retry failed: {e}");
                    pending_packet = Some(pkt);
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
            }
        } else if !eof_from_demuxer {
            // Read next packet from demuxer
            match packets.next() {
                Some((stream, mut packet)) => {
                    if stream.index() == stream_index {
                        let send_result = send_to_decoder(bsf_ctx.as_ref(), &mut decoder, &mut packet);
                        if send_result.is_err() {
                            // EAGAIN: decoder buffer full. Save packet and drain first.
                            pending_packet = Some(packet);
                        }
                    }
                    // Non-video packets: just skip, loop will continue to drain+feed
                }
                None => {
                    if let Some(bsf) = bsf_ctx.as_ref() {
                        unsafe { bsf_flush_to_decoder(bsf, &mut decoder) };
                    }
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        } else if !drained_any {
            // EOF from demuxer and no frames drained — we're done
            debug!("decode_thread: EOF reached");
            // The end of the stream as it stands after this seek — a newer
            // seek's consumer must not take it for its own.
            let mut eof = LinearFrame::eof();
            eof.seek_epoch = seek_epoch;
            frame_queue.lock().unwrap_or_else(|e| e.into_inner()).push(eof);
            eof_sent = true;
        }
    }
}

/// A seek as the decode thread carries it out (#978): for a still image,
/// a container seek to its one frame at 0, remembering in `still_at` the
/// time that frame is to be stamped with; anything else as asked.
fn still_seek(still: bool, secs: f64, decode_on: bool, still_at: &mut Option<f64>) -> (f64, bool) {
    if still {
        *still_at = Some(secs);
        (0.0, false)
    } else {
        (secs, decode_on)
    }
}

/// A software frame's planes repacked as the GPU's converters read them:
/// 4:2:0 planar to semi-planar, at the source's size, by swscale's
/// unscaled copy, which changes no sample.
struct SwRepack {
    scaler: ffmpeg_next::software::scaling::Context,
    out: ffmpeg_next::frame::Video,
    key: (ffmpeg_next::format::Pixel, u32, u32),
}

/// Whether software frames go to the GPU (#2116); `KLIPP_SW_FRAMES_ON_GPU=0`
/// keeps them on the CPU path, to compare.
fn sw_frames_on_gpu() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KLIPP_SW_FRAMES_ON_GPU").as_deref() != Ok("0"))
}

/// A software-decoded 4:2:0 frame (8-bit or 10-bit) converted on the GPU,
/// as a frame downloaded from the hardware decoder is (#2116): its planes
/// go up at the source's size and the shader scales, turns and colours
/// them, and the picture stays on the GPU. On the CPU path, the frame was
/// scaled and made linear f32 on the decoder's thread, 16 ms a 4K frame,
/// then sent up as 33 MB of floats a layer a frame: four layers decoded in
/// software played at a third of the rate the GPU's decoder gave them.
/// `None` for any other format, or with no GPU: the CPU path takes it.
fn sw_frame_on_gpu(
    frame: &ffmpeg_next::frame::Video,
    repack: &mut Option<SwRepack>,
    gpu_nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
    gpu_p010: &mut Option<klipp_effects::p010_gpu::GpuP010Converter>,
    source_rotation: i32,
    cs_baseline: klipp_core::SourceColorspace,
) -> Option<LinearFrame> {
    use ffmpeg_next::format::Pixel;
    static LINEAR_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if !sw_frames_on_gpu() {
        return None;
    }
    let (fmt, (w, h)) = (frame.format(), (frame.width(), frame.height()));
    let packed = match fmt {
        Pixel::YUV420P => Pixel::NV12,
        Pixel::YUV420P10LE => Pixel::P010LE,
        _ => return None,
    };
    let converter: &mut dyn SemiPlanarGpu = if packed == Pixel::NV12 {
        if gpu_nv12.is_none() {
            *gpu_nv12 = klipp_effects::nv12_gpu::GpuNv12Converter::new();
        }
        gpu_nv12.as_mut()?
    } else {
        if gpu_p010.is_none() {
            *gpu_p010 = klipp_effects::p010_gpu::GpuP010Converter::new();
        }
        gpu_p010.as_mut()?
    };
    if repack.as_ref().is_none_or(|r| r.key != (fmt, w, h)) {
        let scaler = ffmpeg_next::software::scaling::Context::get(fmt, w, h, packed, w, h, ffmpeg_next::software::scaling::Flags::POINT)
            .map_err(|e| warn!("sw_frame_on_gpu: no repack from {fmt:?}: {e}"))
            .ok()?;
        *repack = Some(SwRepack { scaler, out: ffmpeg_next::frame::Video::empty(), key: (fmt, w, h) });
    }
    let r = repack.as_mut()?;
    r.scaler.run(frame, &mut r.out).map_err(|e| warn!("sw_frame_on_gpu: repack failed: {e}")).ok()?;
    let source_cs = frame_colorspace(frame, cs_baseline);
    let out = playback_dimensions(w, h, source_rotation);
    let planes = SemiPlanar { y: r.out.data(0), uv: r.out.data(1), width: w, height: h, y_stride: r.out.stride(0) as u32, uv_stride: r.out.stride(1) as u32 };
    let (data, gpu_source, hdr) = semi_planar_frame(converter, &planes, out, source_rotation, source_cs, &LINEAR_US);
    log_first_resident_delivery(gpu_source.is_some(), if packed == Pixel::NV12 { "nv12_software" } else { "p010_software" });
    Some(LinearFrame {
        pts: RationalTime::zero(),
        width: out.0,
        height: out.1,
        data,
        is_eof: false,
        gpu_source,
        colorspace: source_cs,
        hdr,
        seek_epoch: 0,
    })
}

/// A software frame's colourspace: its own tags where it has them, else
/// the stream's (`cs_baseline`, read once at decoder init), the same
/// merge as the hardware path's, read from the frame's raw fields.
fn frame_colorspace(frame: &ffmpeg_next::frame::Video, cs_baseline: klipp_core::SourceColorspace) -> klipp_core::SourceColorspace {
    unsafe {
        let avf = frame.as_ptr();
        apply_log_override(klipp_core::SourceColorspace::resolve_with_baseline(
            (*avf).color_range as i32,
            (*avf).colorspace as i32,
            (*avf).color_primaries as i32,
            (*avf).color_trc as i32,
            cs_baseline,
        ))
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
    // Scene-referred sources (PQ, HLG, camera log) take the GPU
    // converters' route on the CPU: 10-bit codes for the grade plus the
    // tone-mapped preview, identical to what NVDEC + the shader produce.
    // This is where ProRes log lands, and where every format NVDEC cannot
    // decode lands. Display-referred sources keep the paths below.
    {
        let source_cs = frame_colorspace(received_frame, cs_baseline);
        if let Some(frame) = unsafe {
            scene_referred_linear_frame(received_frame.as_ptr(), source_cs, source_rotation)
        } {
            return Some(frame);
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
        let source_cs = frame_colorspace(received_frame, cs_baseline);
        let src_w = received_frame.width();
        let src_h = received_frame.height();
        let (dst_w, dst_h) = playback_dimensions(src_w, src_h, 0);
        let y_stride = received_frame.stride(0);
        let u_stride = received_frame.stride(1);
        let v_stride = received_frame.stride(2);
        let y_data = received_frame.data(0);
        let u_data = received_frame.data(1);
        let v_data = received_frame.data(2);
        let linear = klipp_color::camera_log::yuv420p10le_to_linear(
            y_data, u_data, v_data,
            y_stride, u_stride, v_stride,
            src_w, src_h, dst_w, dst_h,
            source_cs,
            working_colorspace(),
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
            hdr: None,
            seek_epoch: 0,
        });
    }

    // Every other YUV format: converted here rather than by swscale, so
    // that subsampled chroma is reconstructed at the same siting the
    // decode shaders use. swscale reconstructs it at a different phase
    // and cannot be told otherwise (see the flags below), so a clip used
    // to change appearance depending on whether NVDEC took it.
    //
    // `read_yuv_samples` reads the planes at native depth using the pixel
    // format's own descriptor, so this covers 4:2:0, 4:2:2 ProRes, 4:4:4,
    // 8 through 16 bits, planar and semi-planar alike. What still falls
    // through to swscale below is what it returns `None` for: the
    // RGB-family, palette, bitstream and hardware formats, greyscale
    // (fewer than three components), and the exotic case of a YUV format
    // whose planes disagree on bit depth or exceed 16 bits. Only that
    // last one could carry subsampled chroma, and nothing Klipp decodes
    // in practice hits it.
    {
        let source_cs = frame_colorspace(received_frame, cs_baseline);
        if let Some(yuv) = unsafe { klipp_media::yuv_samples::read_yuv_samples(received_frame.as_ptr()) } {
            let rot = source_rotation.rem_euclid(360) as u32;
            let (dst_w, dst_h) = playback_dimensions(
                received_frame.width(),
                received_frame.height(),
                source_rotation,
            );
            // `playback_dimensions` already returns the rotated extent,
            // and the converter rotates as it samples, so the frame it
            // hands back is the right way up at the right size.
            let linear = klipp_color::camera_log::yuv_to_linear_display(
                &yuv.samples(),
                dst_w,
                dst_h,
                rot,
                source_cs,
                working_colorspace(),
            );
            return Some(LinearFrame {
                pts: RationalTime::zero(),
                width: dst_w,
                height: dst_h,
                data: linear,
                is_eof: false,
                gpu_source: None,
                colorspace: source_cs,
                hdr: None,
                seek_epoch: 0,
            });
        }
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
            // Accurate rounding and full chroma interpolation: swscale's
            // default fixed-point YUV→RGB reads colours up to four levels
            // low (a saturated blue 180 as 176), which export — converting
            // accurately — does not.
            //
            // Known difference, and don't try to configure it away: the
            // decode shaders and `YuvSamples::chroma_at` reconstruct 4:2:0
            // chroma at H.264's `AVCHROMA_LOC_LEFT` siting (co-sited
            // horizontally, half a luma sample down), so on a hard chroma
            // edge they ramp across the odd luma column while swscale ramps
            // one sample later. Telling swscale the siting does not fix it.
            // `src_h_chr_pos` / `src_v_chr_pos` feed the *scaling* filters
            // only, so at 1:1 — which `playback_dimensions` returns for any
            // source inside the playback budget — they are ignored
            // outright: measured, xpos=0/ypos=128 and xpos=255/ypos=0 give
            // byte-identical output, and setting them before
            // `sws_init_context` rather than after changes nothing.
            //
            // Closing it means doing the YUV→RGB here instead of in
            // swscale, against `chroma_at`, the way
            // `camera_log::yuv420p10le_to_linear` already bypasses swscale
            // for YUV420P10LE. Scene-referred sources and 10-bit 4:2:0
            // already take that route, so what remains on this path is
            // display-referred 8-bit.
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR
                | ffmpeg_next::software::scaling::Flags::ACCURATE_RND
                | ffmpeg_next::software::scaling::Flags::FULL_CHR_H_INT,
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
                            1, // RGB out is full range
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
    let source_cs = frame_colorspace(received_frame, cs_baseline);

    // Rotation still operates on sRGB u8 via rotate_rgba. For hi-bit-
    // depth sources we'd lose precision rotating the u16 buffer
    // through the u8 helper, so: rotate *after* linearisation. For
    // 8-bit we rotate the u8 then linearise — cheaper and bit-
    // identical to the old output. Either way, the linear-decode
    // step uses the colourspace-aware path so log curves and
    // wide-gamut sources reach the project's working space
    // correctly.
    let ws = working_colorspace();
    let linear_data = if is_hi_bit_depth {
        let linear = klipp_color::camera_log::rgba64_le_to_linear(&raw_data, source_cs, ws);
        if source_rotation != 0 {
            rotate_linear_rgba(&linear, width, height, source_rotation).0
        } else {
            linear
        }
    } else if source_rotation != 0 {
        let (rotated, _rw, _rh) = crate::engine::rotate_rgba(&raw_data, width, height, source_rotation);
        klipp_color::camera_log::rgba_u8_to_linear(&rotated, source_cs, ws)
    } else {
        klipp_color::camera_log::rgba_u8_to_linear(&raw_data, source_cs, ws)
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
        hdr: None,
        seek_epoch: 0,
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

#[cfg(test)]
mod decode_thread_barrier_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// These tests read and write a process-global counter, so they
    /// cannot run alongside each other: one holding a token makes
    /// another's reading wrong. It caught itself — the panicking-thread
    /// test failed in a full-suite run with `left: 1` while its sibling
    /// held a token.
    static SERIALISE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// With nothing decoding, the barrier is a no-op.
    #[test]
    fn it_returns_at_once_when_nothing_is_decoding() {
        let _guard = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        // Other tests in this binary may hold tokens; take a reading
        // rather than assuming zero.
        let held = LIVE_DECODE_THREADS.load(Ordering::Acquire);
        if held == 0 {
            let t0 = std::time::Instant::now();
            assert!(wait_for_decode_threads(Duration::from_secs(5)));
            assert!(t0.elapsed() < Duration::from_millis(200), "it waited when it need not");
        }
    }

    /// A held token keeps the barrier waiting, and releasing it lets the
    /// barrier through. This is the whole mechanism: the token is held
    /// for the decode thread's entire life, its `Drop` included, so the
    /// barrier cannot return while a thread is still inside ffmpeg's
    /// teardown — which is where it was crashing when the process
    /// exited underneath it.
    #[test]
    fn a_live_thread_holds_the_barrier_and_releases_it() {
        let _guard = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        let token = DecodeThreadToken::new();
        let before = LIVE_DECODE_THREADS.load(Ordering::Acquire);
        assert!(before >= 1, "the token did not register");

        // Times out while held.
        let t0 = std::time::Instant::now();
        assert!(
            !wait_for_decode_threads(Duration::from_millis(50)),
            "the barrier returned while a thread was registered",
        );
        assert!(t0.elapsed() >= Duration::from_millis(50), "it did not actually wait");

        // Released on drop, wherever that happens — including a thread
        // unwinding out of a panic, which is why it is a guard and not a
        // decrement at the end of the function body.
        // Checked on a count of its own: the shared one also moves with
        // every decoder the other tests in this binary open and close.
        drop(token);
        static OWN: AtomicUsize = AtomicUsize::new(0);
        let token = DecodeThreadToken::counted_in(&OWN);
        assert_eq!(OWN.load(Ordering::Acquire), 1, "the token did not register");
        drop(token);
        assert_eq!(
            OWN.load(Ordering::Acquire),
            0,
            "dropping the token did not deregister the thread",
        );
    }

    /// Dropped on a panicking thread too, or a decoder that panics
    /// during teardown would wedge every later shutdown for the full
    /// timeout.
    #[test]
    fn a_panicking_thread_still_deregisters() {
        let _guard = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        static OWN: AtomicUsize = AtomicUsize::new(0);
        let h = std::thread::spawn(|| {
            let _token = DecodeThreadToken::counted_in(&OWN);
            panic!("as a decode thread might");
        });
        assert!(h.join().is_err(), "the fixture was supposed to panic");
        assert_eq!(
            OWN.load(Ordering::Acquire),
            0,
            "a panicking thread left itself registered",
        );
    }
}

/// Whether a seek to `target` is better served by decoding on than by a
/// container seek. The decoder's last frame was at `last_pts`; the
/// oldest frame still queued, not yet taken by the engine, is at
/// `queued_from`. The target is reachable without the container when it
/// is no earlier than what is still queued (those frames are kept) and
/// no further past the last decoded frame than a group of pictures
/// typically is. Decoding on costs one frame per frame skipped; a
/// container seek costs every frame from the previous keyframe up to
/// the target — for a one-frame step, the whole group again.
fn seek_is_just_ahead(last_pts: Option<f64>, queued_from: Option<f64>, target: f64) -> bool {
    const JUST_AHEAD_SECS: f64 = 1.0;
    let Some(last) = last_pts else { return false };
    if target > last + JUST_AHEAD_SECS {
        return false;
    }
    match queued_from {
        // Still queued from `from`: anything from there on is at hand. A
        // target one frame before it has gone by — the engine took that
        // frame already — and decoding on can never produce it again, so
        // the tolerance is float noise, not a frame.
        Some(from) => target >= from - SEEK_PTS_SLACK,
        // Nothing queued: only what is still to be decoded is ahead.
        None => target > last,
    }
}

#[cfg(test)]
mod seek_shortcut_tests {
    use super::seek_is_just_ahead;

    #[test]
    fn a_step_forward_decodes_on_and_anything_else_seeks() {
        assert!(seek_is_just_ahead(Some(10.0), None, 10.04), "one frame on, nothing queued");
        assert!(seek_is_just_ahead(Some(10.0), None, 10.9), "within a second");
        assert!(!seek_is_just_ahead(Some(10.0), None, 10.0), "the same time with nothing queued: the frame has gone by");
        assert!(!seek_is_just_ahead(Some(10.0), None, 9.96), "one frame back");
        assert!(!seek_is_just_ahead(Some(10.0), None, 11.5), "too far to decode on");
        assert!(!seek_is_just_ahead(None, None, 5.0), "nothing decoded yet");
        // The decoder read ahead to 10.6 s; frames from 10.2 s are queued.
        assert!(seek_is_just_ahead(Some(10.6), Some(10.2), 10.24), "a step into what is queued");
        assert!(seek_is_just_ahead(Some(10.6), Some(10.2), 10.8), "a step past the queue, still ahead");
        assert!(!seek_is_just_ahead(Some(10.6), Some(10.2), 10.0), "before the queue: gone by");
        // Paused right after playing: the frame at the playhead was the
        // last one played and is no longer queued. One frame before the
        // queue is gone by, however close.
        assert!(!seek_is_just_ahead(Some(4.92), Some(4.32), 4.28), "the frame just played is gone by");
        assert!(seek_is_just_ahead(Some(4.92), Some(4.32), 4.32 - 1e-4), "the queued frame itself, to timestamp rounding");
    }
}

#[cfg(test)]
mod sw_frames_on_gpu_tests {
    use super::*;

    /// The first frame of a 320×180 colour test pattern in `pix_fmt`,
    /// decoded in software (FFV1). `None` without ffmpeg.
    fn first_frame(pix_fmt: &str) -> Option<ffmpeg_next::frame::Video> {
        let out = std::env::temp_dir().join(format!("klipp-sw-on-gpu-{pix_fmt}.mkv"));
        if !out.exists() {
            let partial = out.with_extension(format!("{}.partial.mkv", std::process::id()));
            let ok = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:d=1", "-pix_fmt", pix_fmt, "-c:v", "ffv1"])
                .arg(&partial)
                .status()
                .ok()?
                .success();
            if !ok {
                return None;
            }
            std::fs::rename(&partial, &out).ok()?;
        }
        ffmpeg_next::init().ok()?;
        let mut input = ffmpeg_next::format::input(&out).ok()?;
        let stream = input.streams().best(ffmpeg_next::media::Type::Video)?;
        let index = stream.index();
        let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters()).ok()?.decoder().video().ok()?;
        let mut frame = ffmpeg_next::frame::Video::empty();
        for (s, packet) in input.packets() {
            if s.index() != index {
                continue;
            }
            decoder.send_packet(&packet).ok()?;
            if decoder.receive_frame(&mut frame).is_ok() {
                return Some(frame);
            }
        }
        decoder.send_eof().ok()?;
        decoder.receive_frame(&mut frame).ok().map(|_| frame)
    }

    /// #2116: a software frame converted on the GPU, as a hardware frame
    /// is, shows what the CPU path showed: same size, same picture.
    #[test]
    fn a_software_frame_on_the_gpu_matches_the_cpu_path() {
        for pix_fmt in ["yuv420p", "yuv420p10le"] {
            let Some(frame) = first_frame(pix_fmt) else {
                eprintln!("skipping: no ffmpeg");
                return;
            };
            // Untagged, as the decoder's baseline is for these files: range,
            // matrix, primaries and transfer all ffmpeg's "unspecified".
            let baseline = klipp_core::SourceColorspace::resolve(0, 2, 2, 2, frame.width(), frame.height());
            let (mut repack, mut nv12, mut p010) = (None, None, None);
            let Some(mut gpu) = sw_frame_on_gpu(&frame, &mut repack, &mut nv12, &mut p010, 0, baseline) else {
                eprintln!("skipping: no GPU");
                return;
            };
            assert_eq!((nv12.is_some(), p010.is_some()), (pix_fmt == "yuv420p", pix_fmt == "yuv420p10le"), "{pix_fmt}: the converter for its depth");
            gpu.ensure_cpu_data();
            let cpu = convert_sw_frame(&frame, &mut None, &mut 0, 0, baseline).expect("the CPU path converts it");
            assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height), "{pix_fmt}");
            assert_eq!(gpu.data.len(), cpu.data.len(), "{pix_fmt}");
            let diffs: Vec<f32> = gpu.data.iter().zip(&cpu.data).map(|(a, b)| (a - b).abs()).collect();
            let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
            let worst = diffs.iter().copied().fold(0.0, f32::max);
            eprintln!("{pix_fmt}: mean {mean:.5}, worst {worst:.5}");
            assert!(mean < 0.003 && worst < 0.02, "{pix_fmt}: the GPU's picture differs from the CPU's: mean {mean}, worst {worst}");
        }
    }
}
