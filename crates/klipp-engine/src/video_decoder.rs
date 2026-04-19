use crate::types::{FrameQueue, LinearFrame, SharedFrameQueue};
use crossbeam_channel::{Receiver, Sender};
use klipp_color::transfer::srgb_to_linear;
use klipp_core::RationalTime;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

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
    source_rotation: i32,
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

    // Try GPU NV12→RGBA path: only for NV12 format with an available converter
    // Temporary diagnostic — log first frame only
    {
        use std::sync::atomic::AtomicBool;
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!("download_hw_frame: format={:?}, src={}x{}, out={}x{}, rotation={}, gpu_available={}, hi_bit_depth={}", sw_fmt, src_w, src_h, out_w, out_h, source_rotation, gpu_converter.is_some(), is_hi_bit_depth);
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
            let rgba_data = converter.convert_nv12_to_rgba(
                y_data, uv_data, src_w, src_h, y_stride, uv_stride, out_w, out_h, source_rotation as u32,
            );
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
                unsafe { download_hw_frame(&received_frame, &mut hw_scaler, gpu_nv12.as_mut(), source_rotation) }
            } else {
                convert_sw_frame(&received_frame, &mut sw_scaler, &mut perf_copy_us, source_rotation)
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
) -> Option<LinearFrame> {
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
            Ok(s) => {
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

    // Rotation still operates on sRGB u8 via rotate_rgba. For hi-bit-depth
    // sources we'd lose precision if we rotated the u16 buffer through the
    // u8 helper, so: rotate *after* linearisation. For 8-bit we rotate the
    // u8 then linearise — cheaper and bit-identical to the old output.
    let linear_data = if is_hi_bit_depth {
        let linear = rgba64_le_to_linear(&raw_data);
        if source_rotation != 0 {
            rotate_linear_rgba(&linear, width, height, source_rotation).0
        } else {
            linear
        }
    } else if source_rotation != 0 {
        let (rotated, _rw, _rh) = crate::engine::rotate_rgba(&raw_data, width, height, source_rotation);
        rgba_u8_to_linear(&rotated)
    } else {
        rgba_u8_to_linear(&raw_data)
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
}
