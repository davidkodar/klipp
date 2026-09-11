use crate::error::RenderError;
use std::path::Path;
use tracing::{debug, info, warn};

pub use klipp_core::GpuVendor;

/// Probe for the primary GPU vendor at startup (sysfs-based, no FFmpeg or
/// Vulkan calls). Thin wrapper kept for backward compatibility with external
/// callers — the underlying implementation is `klipp_core::GpuVendor::detect()`.
pub fn detect_gpu() -> GpuVendor {
    let v = GpuVendor::detect();
    match v {
        GpuVendor::Nvidia => info!("GPU detected: NVIDIA"),
        GpuVendor::Amd => info!("GPU detected: AMD"),
        GpuVendor::Intel => info!("GPU detected: Intel"),
        GpuVendor::Unknown => info!("No recognised GPU vendor — will use software decode"),
    }
    v
}

/// Hardware acceleration mode for the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HwAccel {
    Cuda,
    Vaapi,
    Software,
}

/// Synchronous, frame-accurate decoder for a single clip during export.
///
/// Tries hardware decode (CUDA NVDEC → VAAPI → software fallback).
/// Uses Lanczos scaling for maximum export quality.
pub struct RenderDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::codec::decoder::Video,
    video_stream_idx: usize,
    /// Software scaler for SW decode path or as fallback.
    scaler: ffmpeg_next::software::scaling::Context,
    /// HW download scaler (raw sws context for NV12→RGBA after GPU download).
    hw_scaler: Option<*mut ffmpeg_next::ffi::SwsContext>,
    /// The source's Y'CbCr matrix and range, resolved from its stream tags
    /// the same way the viewer resolves them (untagged HD is BT.709).
    yuv_matrix: klipp_core::ColorMatrix,
    yuv_range: klipp_core::ColorRange,
    /// The source's resolved colourspace, before any camera log format.
    source_cs: klipp_core::SourceColorspace,
    /// Set for scene-referred sources (PQ, HLG, camera log): their frames
    /// go through the viewer's codes route, not swscale RGB.
    scene: Option<SceneSource>,
    /// The codes of the frame `next_frame` last returned, for the grade.
    last_scene: Option<crate::stage::SceneReferredFrame>,
    /// Active hardware acceleration mode.
    hw_accel: HwAccel,
    /// Target output dimensions.
    target_width: u32,
    target_height: u32,
    /// Stream time_base as f64 ratio for PTS→seconds conversion.
    time_base_secs: f64,
    /// Source time (seconds) below which frames are discarded (keyframe→target skip).
    skip_until_secs: f64,
    /// Source time (seconds) at which this clip ends.
    source_end_secs: f64,
    /// Source stream's average frame rate — used for the half-frame
    /// tolerance in skip / EOF logic. R-9: this was previously the
    /// SEQUENCE fps, which produced an off-by-half tolerance whenever
    /// a 60 fps source rendered into a 24 fps sequence (or the reverse)
    /// — the tolerance window was wrong by a factor of up to 2.5× and
    /// caused sporadic duplicate / missing source frames near clip
    /// boundaries. The tolerance compares against source-time PTS so
    /// the natural unit is source fps.
    fps: f64,
    /// Source rotation in degrees (0, 90, 180, 270) — applied after decode.
    source_rotation: i32,
    demuxer_eof: bool,
    decoder_eof: bool,
    eof_sent: bool,
    frames_decoded: u64,
    frames_skipped: u64,
}

/// How a scene-referred source's frames are decoded.
#[derive(Clone, Copy)]
struct SceneSource {
    /// `[range, matrix, transfer, primaries]` as `klipp_core::color_ids`.
    ids: [u32; 4],
    transfer: klipp_color::hdr::HdrTransfer,
    /// Source primaries → BT.709 linear, export's working space (its
    /// grade executor is never given another).
    to_display: [[f32; 3]; 3],
}

impl SceneSource {
    fn for_colourspace(cs: klipp_core::SourceColorspace) -> Option<Self> {
        let ids = klipp_core::color_ids::ids(cs);
        let transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(ids[2])?;
        let to_display = klipp_color::camera_log::primaries_to_working_matrix(
            cs.primaries,
            klipp_core::WorkingColorspace::Rec709Linear,
        );
        Some(Self { ids, transfer, to_display })
    }
}

impl RenderDecoder {
    /// Open a decoder for a media file, seeking to `source_start_secs`.
    /// Tries CUDA NVDEC, then VAAPI, then software decode.
    /// `source_rotation`: degrees (0, 90, 180, 270) to rotate frames after decode.
    pub fn open(
        path: &Path,
        source_start_secs: f64,
        source_end_secs: f64,
        target_width: u32,
        target_height: u32,
        fps: f64,
        source_rotation: i32,
    ) -> Result<Self, RenderError> {
        let mut format_ctx =
            ffmpeg_next::format::input(path).map_err(RenderError::Decode)?;

        let video_stream = format_ctx
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .ok_or_else(|| RenderError::NoVideoStream(path.to_path_buf()))?;

        let video_stream_idx = video_stream.index();
        let time_base = video_stream.time_base();
        let time_base_secs =
            time_base.numerator() as f64 / time_base.denominator() as f64;

        // R-9: extract the SOURCE stream's avg_frame_rate for the
        // half-frame tolerance. Falls back to the caller-supplied fps
        // (which is the sequence fps) if the stream doesn't declare a
        // valid rate — better an approximate tolerance than zero.
        let source_fps = {
            let r = video_stream.avg_frame_rate();
            if r.numerator() > 0 && r.denominator() > 0 {
                r.numerator() as f64 / r.denominator() as f64
            } else {
                fps
            }
        };

        let mut codec_ctx =
            ffmpeg_next::codec::context::Context::from_parameters(video_stream.parameters())
                .map_err(RenderError::Decode)?;

        // Try hardware acceleration: CUDA → VAAPI → Software
        let hw_accel = unsafe {
            if try_init_cuda(codec_ctx.as_mut_ptr()) {
                HwAccel::Cuda
            } else if try_init_vaapi(codec_ctx.as_mut_ptr()) {
                HwAccel::Vaapi
            } else {
                HwAccel::Software
            }
        };

        // Multi-threaded decode (helps SW, ignored by HW)
        unsafe {
            (*codec_ctx.as_mut_ptr()).thread_count = 0;
        }

        let decoder = codec_ctx.decoder().video().map_err(RenderError::Decode)?;

        // For 90/270 rotation, swap the scale target so that after rotation
        // the output matches (target_width, target_height).
        let (scale_w, scale_h) = if source_rotation == 90 || source_rotation == 270 {
            (target_height, target_width)
        } else {
            (target_width, target_height)
        };

        // Software scaler: source format → RGBA at target resolution.
        // For HW decode, this is only used as fallback; the main path uses
        // download_hw_frame with raw sws for NV12→RGBA.
        let mut scaler = ffmpeg_next::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg_next::format::Pixel::RGBA,
            scale_w,
            scale_h,
            ffmpeg_next::software::scaling::Flags::LANCZOS,
        )
        .map_err(RenderError::Decode)?;
        let source_cs = unsafe {
            let par = (*video_stream.as_ptr()).codecpar;
            klipp_core::SourceColorspace::resolve(
                (*par).color_range as i32,
                (*par).color_space as i32,
                (*par).color_primaries as i32,
                (*par).color_trc as i32,
                (*par).width.max(1) as u32,
                (*par).height.max(1) as u32,
            )
        };
        let (yuv_matrix, yuv_range) = (source_cs.matrix, source_cs.range);
        unsafe {
            crate::sws_colour::set_yuv_to_rgb(scaler.as_mut_ptr() as *mut _, yuv_matrix, yuv_range);
        }

        if source_start_secs > 0.0 {
            let ts = (source_start_secs * 1_000_000.0) as i64;
            let _ = format_ctx.seek(ts, ..ts);
        }

        let accel_str = match hw_accel {
            HwAccel::Cuda => "CUDA/NVDEC",
            HwAccel::Vaapi => "VAAPI",
            HwAccel::Software => "Software",
        };
        debug!(
            "RenderDecoder::open {:?} [{:.3}s..{:.3}s] → {}×{} Lanczos, decode={}",
            path.file_name().unwrap_or_default(),
            source_start_secs,
            source_end_secs,
            target_width,
            target_height,
            accel_str,
        );

        Ok(Self {
            format_ctx,
            decoder,
            video_stream_idx,
            scaler,
            hw_scaler: None,
            yuv_matrix,
            yuv_range,
            scene: SceneSource::for_colourspace(source_cs),
            source_cs,
            last_scene: None,
            hw_accel,
            target_width,
            target_height,
            time_base_secs,
            skip_until_secs: source_start_secs,
            source_end_secs,
            fps: source_fps,
            source_rotation,
            demuxer_eof: false,
            decoder_eof: false,
            eof_sent: false,
            frames_decoded: 0,
            frames_skipped: 0,
        })
    }

    /// Decode as this camera log format — the media's effective format,
    /// which the file's own tags never state. `None` leaves the file's tags
    /// in charge (PQ and HLG files say what they are).
    pub fn with_log_format(mut self, log_format: Option<klipp_core::LogFormat>) -> Self {
        let cs = match log_format {
            Some(f) => f.apply_to(self.source_cs),
            None => self.source_cs,
        };
        self.scene = SceneSource::for_colourspace(cs);
        self
    }

    /// Whether frames decode through the scene-referred route.
    pub fn is_scene_referred(&self) -> bool {
        self.scene.is_some()
    }

    /// The codes of the frame `next_frame` last returned, when the source
    /// is scene-referred. Taken, so a stale frame's codes can never be
    /// graded in place of a later frame's.
    pub fn take_scene_referred(&mut self) -> Option<crate::stage::SceneReferredFrame> {
        self.last_scene.take()
    }

    /// Decode the next frame. Returns `Ok(None)` when the clip is exhausted.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, RenderError> {
        if self.decoder_eof {
            return Ok(None);
        }

        loop {
            // Phase 1: Try to drain a frame from the decoder
            let mut decoded = ffmpeg_next::util::frame::Video::empty();
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    let frame_pts_secs = decoded
                        .pts()
                        .map(|pts| pts as f64 * self.time_base_secs)
                        .unwrap_or(0.0);

                    // Gap 5: Skip frames before exact start point
                    let half_frame = 0.5 / self.fps;
                    if self.skip_until_secs > 0.0
                        && frame_pts_secs < self.skip_until_secs - half_frame
                    {
                        self.frames_skipped += 1;
                        continue;
                    }
                    if self.skip_until_secs > 0.0 {
                        debug!(
                            "RenderDecoder: skipped {} frames to reach {:.3}s (landed at {:.3}s)",
                            self.frames_skipped, self.skip_until_secs, frame_pts_secs
                        );
                        self.skip_until_secs = 0.0;
                    }

                    // Check source end
                    if frame_pts_secs >= self.source_end_secs + half_frame {
                        self.decoder_eof = true;
                        return Ok(None);
                    }

                    // Convert to RGBA
                    let rgba = self.frame_to_rgba(&decoded)?;
                    self.frames_decoded += 1;
                    return Ok(Some(rgba));
                }
                Err(ffmpeg_next::Error::Other { errno })
                    if errno == ffmpeg_next::error::EAGAIN =>
                {
                    // Need more input
                }
                Err(ffmpeg_next::Error::Eof) => {
                    self.decoder_eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(RenderError::Decode(e)),
            }

            // Phase 2: Feed the next video packet
            if self.demuxer_eof {
                if !self.eof_sent {
                    self.eof_sent = true;
                    let _ = self.decoder.send_eof();
                }
                continue;
            }

            loop {
                let read_result = read_next_packet(&mut self.format_ctx);
                match read_result {
                    Ok(packet) => {
                        if packet.stream() == self.video_stream_idx {
                            self.decoder
                                .send_packet(&packet)
                                .map_err(RenderError::Decode)?;
                            break;
                        }
                    }
                    Err(ffmpeg_next::Error::Eof) => {
                        self.demuxer_eof = true;
                        if !self.eof_sent {
                            self.eof_sent = true;
                            let _ = self.decoder.send_eof();
                        }
                        break;
                    }
                    Err(e) => return Err(RenderError::Decode(e)),
                }
            }
        }
    }

    /// Convert a decoded frame to RGBA. Handles both HW and SW frames.
    /// Applies source rotation (90/180/270) after decode when needed.
    fn frame_to_rgba(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
    ) -> Result<Vec<u8>, RenderError> {
        // For 90/270 rotation, scale to swapped dimensions first.
        let (scale_w, scale_h) = if self.source_rotation == 90 || self.source_rotation == 270 {
            (self.target_height, self.target_width)
        } else {
            (self.target_width, self.target_height)
        };

        if let Some(scene) = self.scene {
            if let Some(rgba) = unsafe { self.scene_referred_frame(decoded, scene) } {
                return Ok(rgba);
            }
        }
        let rgba = if self.hw_accel != HwAccel::Software {
            // Try HW download path
            if let Some(rgba) = unsafe {
                download_hw_frame(
                    decoded,
                    &mut self.hw_scaler,
                    scale_w,
                    scale_h,
                    self.yuv_matrix,
                    self.yuv_range,
                )
            } {
                rgba
            } else {
                // HW download failed — fall through to SW scaler
                self.scale_frame_sw(decoded)?
            }
        } else {
            // Software path: use pre-created Lanczos scaler
            self.scale_frame_sw(decoded)?
        };

        // Apply source rotation if needed (phone/drone videos with container rotation metadata)
        if self.source_rotation != 0 {
            let (rotated, _rw, _rh) = rotate_rgba(&rgba, scale_w, scale_h, self.source_rotation);
            Ok(rotated)
        } else {
            Ok(rgba)
        }
    }

    /// A scene-referred frame the way the viewer decodes it: 10-bit codes
    /// (`klipp_color::hdr::yuv_to_hdr_codes`, bit-identical to the
    /// viewer's decoder shaders) kept for the grade, and the tone-mapped
    /// preview returned as the 8-bit frame. Resized nearest-neighbour and
    /// rotated as the viewer's decoders do. `None` if the frame's samples
    /// cannot be read, in which case the caller falls back to swscale.
    ///
    /// SAFETY: `decoded` must be a valid decoded frame.
    unsafe fn scene_referred_frame(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
        scene: SceneSource,
    ) -> Option<Vec<u8>> {
        use ffmpeg_next::ffi::*;
        // Hardware frames are downloaded first; tags come from the source,
        // resolved at open, so the download dropping them does not matter.
        let is_hw = decoded.format() == ffmpeg_next::format::Pixel::CUDA
            || decoded.format() == ffmpeg_next::format::Pixel::VAAPI;
        let mut sw = std::ptr::null_mut::<AVFrame>();
        let src: *const AVFrame = if is_hw {
            sw = av_frame_alloc();
            if sw.is_null() || av_hwframe_transfer_data(sw, decoded.as_ptr(), 0) < 0 {
                av_frame_free(&mut sw);
                return None;
            }
            sw
        } else {
            decoded.as_ptr()
        };
        let yuv = klipp_media::yuv_samples::read_yuv_samples(src);
        if !sw.is_null() {
            av_frame_free(&mut sw);
        }
        let yuv = yuv?;
        let (w, h) = (self.target_width, self.target_height);
        let codes = klipp_color::hdr::yuv_to_hdr_codes(
            &yuv.samples(),
            w,
            h,
            self.source_rotation.rem_euclid(360) as u32,
            scene.ids,
        );
        let preview = klipp_color::hdr::hdr10_codes_to_display_srgb8(&codes, scene.transfer, scene.to_display);
        self.last_scene = Some(crate::stage::SceneReferredFrame {
            codes,
            transfer: scene.transfer,
            source_to_working: scene.to_display,
            width: w,
            height: h,
        });
        Some(preview)
    }

    /// Scale a decoded frame using the software scaler.
    fn scale_frame_sw(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
    ) -> Result<Vec<u8>, RenderError> {
        let mut rgba_frame = ffmpeg_next::util::frame::Video::empty();
        self.scaler
            .run(decoded, &mut rgba_frame)
            .map_err(RenderError::Decode)?;

        let width = rgba_frame.width() as usize;
        let height = rgba_frame.height() as usize;
        let linesize = rgba_frame.stride(0);
        let expected_stride = width * 4;

        if linesize == expected_stride {
            Ok(rgba_frame.data(0)[..expected_stride * height].to_vec())
        } else {
            let mut out = vec![0u8; expected_stride * height];
            let src = rgba_frame.data(0);
            for y in 0..height {
                let src_offset = y * linesize;
                let dst_offset = y * expected_stride;
                out[dst_offset..dst_offset + expected_stride]
                    .copy_from_slice(&src[src_offset..src_offset + expected_stride]);
            }
            Ok(out)
        }
    }

    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    pub fn frames_skipped(&self) -> u64 {
        self.frames_skipped
    }

    pub fn hw_accel_name(&self) -> &'static str {
        match self.hw_accel {
            HwAccel::Cuda => "CUDA/NVDEC",
            HwAccel::Vaapi => "VAAPI",
            HwAccel::Software => "Software",
        }
    }
}

impl Drop for RenderDecoder {
    fn drop(&mut self) {
        if self.frames_decoded > 0 || self.frames_skipped > 0 {
            debug!(
                "RenderDecoder: {} decoded, {} skipped ({})",
                self.frames_decoded,
                self.frames_skipped,
                self.hw_accel_name(),
            );
        }
        // Clean up raw sws context
        if let Some(sws) = self.hw_scaler.take() {
            unsafe {
                ffmpeg_next::ffi::sws_freeContext(sws);
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Hardware acceleration init (ported from klipp-engine/video_decoder.rs)
// ═══════════════════════════════════════════════════════════════════

unsafe extern "C" fn cuda_get_format(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;
    let mut p = pix_fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == AVPixelFormat::AV_PIX_FMT_CUDA {
            return AVPixelFormat::AV_PIX_FMT_CUDA;
        }
        p = p.add(1);
    }
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
            return AVPixelFormat::AV_PIX_FMT_VAAPI;
        }
        p = p.add(1);
    }
    *pix_fmts
}

unsafe fn try_init_cuda(codec_ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext) -> bool {
    use ffmpeg_next::ffi::*;

    let device_type = av_hwdevice_find_type_by_name(c"cuda".as_ptr());
    if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        return false;
    }

    let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
    let ret = av_hwdevice_ctx_create(
        &mut hw_device_ctx,
        device_type,
        std::ptr::null(),
        std::ptr::null_mut(),
        0,
    );
    if ret < 0 {
        return false;
    }

    (*codec_ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ctx);
    (*codec_ctx_ptr).get_format = Some(cuda_get_format);
    av_buffer_unref(&mut hw_device_ctx);
    true
}

unsafe fn try_init_vaapi(codec_ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext) -> bool {
    use ffmpeg_next::ffi::*;

    let device_type = av_hwdevice_find_type_by_name(c"vaapi".as_ptr());
    if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        return false;
    }

    let device_paths: &[&[u8]] = &[
        b"/dev/dri/renderD128\0",
        b"/dev/dri/renderD129\0",
        b"\0",
    ];
    for path in device_paths {
        let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            device_type,
            path.as_ptr() as *const i8,
            std::ptr::null_mut(),
            0,
        );
        if ret >= 0 {
            (*codec_ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ctx);
            (*codec_ctx_ptr).get_format = Some(vaapi_get_format);
            av_buffer_unref(&mut hw_device_ctx);
            return true;
        }
    }
    false
}

/// Download a hardware frame (GPU→CPU), convert NV12→RGBA, scale to target dimensions.
/// Uses raw FFI sws for the conversion since HW frames have non-standard pixel formats.
unsafe fn download_hw_frame(
    hw_frame: &ffmpeg_next::frame::Video,
    sws_ctx: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
    target_w: u32,
    target_h: u32,
    yuv_matrix: klipp_core::ColorMatrix,
    yuv_range: klipp_core::ColorRange,
) -> Option<Vec<u8>> {
    use ffmpeg_next::ffi::*;

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return None;
    }

    // Transfer from GPU to CPU
    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    if ret < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    let src_w = (*sw_frame).width as u32;
    let src_h = (*sw_frame).height as u32;
    let sw_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);

    // Create or reuse sws context — use Lanczos for export quality
    let sws = sws_getCachedContext(
        sws_ctx.unwrap_or(std::ptr::null_mut()),
        src_w as i32,
        src_h as i32,
        sw_fmt,
        target_w as i32,
        target_h as i32,
        AVPixelFormat::AV_PIX_FMT_RGBA,
        0x200, // SWS_LANCZOS, matching the software path
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
    );
    if sws.is_null() {
        warn!("HW download: sws_getCachedContext failed for fmt {:?}", sw_fmt);
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    // A new context starts on swscale's BT.601 default.
    if *sws_ctx != Some(sws) {
        crate::sws_colour::set_yuv_to_rgb(sws, yuv_matrix, yuv_range);
    }
    *sws_ctx = Some(sws);

    // Allocate output RGBA frame
    let rgba_frame = av_frame_alloc();
    if rgba_frame.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    (*rgba_frame).width = target_w as i32;
    (*rgba_frame).height = target_h as i32;
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

    // Extract RGBA data (handle stride padding)
    let stride = (*rgba_frame).linesize[0] as usize;
    let expected_stride = (target_w * 4) as usize;
    let data_slice =
        std::slice::from_raw_parts((*rgba_frame).data[0], stride * target_h as usize);

    let rgba_data = if stride == expected_stride {
        data_slice.to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * target_h as usize);
        for row in 0..target_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&data_slice[start..start + expected_stride]);
        }
        buf
    };

    av_frame_free(&mut (rgba_frame as *mut _));
    Some(rgba_data)
}

/// Rotate an RGBA frame by 0/90/180/270 degrees clockwise.
/// Returns (rotated_data, new_width, new_height).
fn rotate_rgba(rgba: &[u8], w: u32, h: u32, degrees: i32) -> (Vec<u8>, u32, u32) {
    match degrees {
        90 => {
            let nw = h;
            let nh = w;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let src = ((y * w + x) * 4) as usize;
                    let dx = h - 1 - y;
                    let dy = x;
                    let dst = ((dy * nw + dx) * 4) as usize;
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
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
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
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
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            (out, nw, nh)
        }
        _ => (rgba.to_vec(), w, h),
    }
}

/// Decode a single frame from a media file at the given time, scaled to `max_width`.
/// Used for RSC motion estimation (decodes adjacent frames at low resolution).
/// Returns (rgba_data, width, height) or None on failure.
pub(crate) fn decode_frame_at_size(
    path: &Path,
    seek_secs: f64,
    max_width: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let mut format_ctx = ffmpeg_next::format::input(path).ok()?;

    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)?;
    let stream_index = video_stream.index();

    let codec_ctx =
        ffmpeg_next::codec::context::Context::from_parameters(video_stream.parameters()).ok()?;

    let mut decoder = codec_ctx.decoder().video().ok()?;
    unsafe {
        (*decoder.as_mut_ptr()).thread_count = 2;
    }

    if seek_secs > 0.1 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        let _ = format_ctx.seek(ts, ..ts);
    }

    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let mut got_frame = false;

    // Read packets until we get a video frame
    loop {
        let read_result = read_next_packet(&mut format_ctx);
        match read_result {
            Ok(packet) => {
                if packet.stream() == stream_index {
                    let _ = decoder.send_packet(&packet);
                    if decoder.receive_frame(&mut decoded_frame).is_ok() {
                        got_frame = true;
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }

    if !got_frame {
        return None;
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();
    let scale = (max_width as f64 / src_w as f64).min(1.0);
    let dst_w = ((src_w as f64 * scale) as u32).max(1);
    let dst_h = ((src_h as f64 * scale) as u32).max(1);

    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        decoded_frame.format(),
        src_w,
        src_h,
        ffmpeg_next::format::Pixel::RGBA,
        dst_w,
        dst_h,
        ffmpeg_next::software::scaling::Flags::BILINEAR,
    )
    .ok()?;

    let mut rgba_frame = ffmpeg_next::util::frame::Video::empty();
    scaler.run(&decoded_frame, &mut rgba_frame).ok()?;

    let width = rgba_frame.width() as usize;
    let height = rgba_frame.height() as usize;
    let linesize = rgba_frame.stride(0);
    let expected_stride = width * 4;

    let rgba = if linesize == expected_stride {
        rgba_frame.data(0)[..expected_stride * height].to_vec()
    } else {
        let mut out = vec![0u8; expected_stride * height];
        let src = rgba_frame.data(0);
        for y in 0..height {
            let src_offset = y * linesize;
            let dst_offset = y * expected_stride;
            out[dst_offset..dst_offset + expected_stride]
                .copy_from_slice(&src[src_offset..src_offset + expected_stride]);
        }
        out
    };

    Some((rgba, dst_w, dst_h))
}

/// Read the next packet from the demuxer using raw FFI.
fn read_next_packet(
    format_ctx: &mut ffmpeg_next::format::context::Input,
) -> Result<ffmpeg_next::Packet, ffmpeg_next::Error> {
    use ffmpeg_next::codec::packet::Mut as PacketMut;
    unsafe {
        let mut pkt = ffmpeg_next::Packet::empty();
        match ffmpeg_next::ffi::av_read_frame(format_ctx.as_mut_ptr(), pkt.as_mut_ptr()) {
            0 => Ok(pkt),
            e => Err(ffmpeg_next::Error::from(e)),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_media_path() -> Option<PathBuf> {
        std::env::var("TEST_MEDIA_FILE").ok().map(PathBuf::from)
    }

    #[test]
    fn gpu_detection_runs() {
        let gpu = detect_gpu();
        println!("Detected GPU: {:?}", gpu);
        // Just verify it doesn't crash
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_produces_frames() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        let fps = 30.0;
        let duration_secs = 2.0;
        let mut dec =
            RenderDecoder::open(&path, 0.0, duration_secs, 1920, 1080, fps, 0).unwrap();

        println!("Decode mode: {}", dec.hw_accel_name());

        let mut count = 0u64;
        while let Ok(Some(frame)) = dec.next_frame() {
            assert_eq!(frame.len(), 1920 * 1080 * 4);
            count += 1;
        }

        assert!(count > 0, "expected at least one frame");
        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s, got {count}"
        );
        println!(
            "decoded {count} frames, skipped {}, mode={}",
            dec.frames_skipped(),
            dec.hw_accel_name()
        );
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_seeks_correctly() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        let fps = 30.0;
        let duration_secs = 2.0;
        let mut dec =
            RenderDecoder::open(&path, 1.0, 1.0 + duration_secs, 1280, 720, fps, 0).unwrap();

        let mut count = 0u64;
        while let Ok(Some(_)) = dec.next_frame() {
            count += 1;
        }

        assert!(count > 0, "expected frames after seek");
        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s from seek, got {count}"
        );
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_respects_source_end() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        let fps = 30.0;
        let duration_secs = 0.5;
        let mut dec =
            RenderDecoder::open(&path, 0.0, duration_secs, 640, 480, fps, 0).unwrap();

        let mut count = 0u64;
        while let Ok(Some(_)) = dec.next_frame() {
            count += 1;
        }

        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s, got {count}"
        );
    }
}
