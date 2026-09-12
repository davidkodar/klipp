use crate::error::RenderError;
use std::path::Path;
use tracing::{debug, info, warn};

pub use klipp_core::GpuVendor;

/// What one decoded export frame looks like when it leaves the decoder.
///
/// Two shapes, because a hardware-decoded frame never needs to be on the
/// CPU at all. NVDEC hands back NV12 on the GPU; the grade runs on the
/// GPU; so the only reason the old path pulled a frame down, converted
/// it with swscale and pushed 66 MB back up was that there was nowhere
/// else to put it.
pub(crate) enum DecodedPixels {
    /// sRGB-encoded RGBA64 on the CPU. Empty for a scene-referred frame,
    /// which travels as codes via `take_scene_referred` instead.
    Rgba64(Vec<u16>),
    /// Linear working-space f32, already on the GPU, with rotation and
    /// the source primaries already applied. Ready to be a grade chain's
    /// input slot with nothing but a device-local copy.
    GpuLinear(std::sync::Arc<wgpu::Buffer>),
}

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
/// Lanczos when the frame is being resized, bilinear when it is not.
///
/// Lanczos is the right filter for a real resize and worth its cost
/// there. At 1:1 it resamples nothing — every kernel lands on its own
/// pixel — so it buys no quality at all, and swscale still walks the
/// slow filter path for the format conversion. That cost was most of the
/// export's decode stage: 101 ms a frame on 4K vertical, where the
/// ffmpeg CLI does the same decode and conversion in 14. A phone clip is
/// the common case for hitting it, because the timeline resolution comes
/// from the first clip and so matches the source exactly.
fn scaler_flags(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> ffmpeg_next::software::scaling::Flags {
    use ffmpeg_next::software::scaling::Flags;
    if src_w == dst_w && src_h == dst_h {
        Flags::BILINEAR
    } else {
        Flags::LANCZOS
    }
}

pub struct RenderDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::codec::decoder::Video,
    video_stream_idx: usize,
    /// Where a frame's time goes inside this decoder: the download and
    /// colour conversion, and the rotation. Read by the export's
    /// per-frame breakdown.
    t_convert: std::time::Duration,
    t_rotate: std::time::Duration,
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
    log_format: Option<klipp_core::LogFormat>,
    working: klipp_core::WorkingColorspace,
    /// Set for scene-referred sources (PQ, HLG, camera log): their frames
    /// go through the viewer's codes route, not swscale RGB.
    scene: Option<SceneSource>,
    /// The codes of the frame `next_frame` last returned, for the grade.
    last_scene: Option<SceneReferredFrame>,
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

/// A scene-referred source frame (PQ, HLG, camera log) as the viewer's
/// grade reads it: packed 10-bit code values in the source primaries.
#[derive(Clone)]
pub struct SceneReferredFrame {
    pub codes: std::sync::Arc<Vec<u32>>,
    pub transfer: klipp_color::hdr::HdrTransfer,
    /// Source primaries → the grade's working space.
    pub source_to_working: [[f32; 3]; 3],
    pub width: u32,
    pub height: u32,
}

/// How a scene-referred source's frames are decoded.
#[derive(Clone, Copy)]
struct SceneSource {
    /// `[range, matrix, transfer, primaries]` as `klipp_core::color_ids`.
    ids: [u32; 4],
    transfer: klipp_color::hdr::HdrTransfer,
    /// Source primaries → the project's working space.
    to_working: [[f32; 3]; 3],
}

impl SceneSource {
    fn for_colourspace(cs: klipp_core::SourceColorspace, ws: klipp_core::WorkingColorspace) -> Option<Self> {
        let ids = klipp_core::color_ids::ids(cs);
        let transfer = klipp_color::hdr::HdrTransfer::from_transfer_id(ids[2])?;
        let to_working = klipp_color::camera_log::primaries_to_working_matrix(cs.primaries, ws);
        Some(Self { ids, transfer, to_working })
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
            ffmpeg_next::format::Pixel::RGBA64LE,
            scale_w,
            scale_h,
            scaler_flags(decoder.width(), decoder.height(), scale_w, scale_h),
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
            t_convert: std::time::Duration::ZERO,
            t_rotate: std::time::Duration::ZERO,
            scaler,
            hw_scaler: None,
            yuv_matrix,
            yuv_range,
            scene: SceneSource::for_colourspace(source_cs, klipp_core::WorkingColorspace::Rec709Linear),
            source_cs,
            log_format: None,
            working: klipp_core::WorkingColorspace::Rec709Linear,
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
        self.log_format = log_format;
        self.resolve_scene();
        self
    }

    /// The project's working colourspace, which scene-referred codes are
    /// converted into (default BT.709 linear).
    pub fn with_working_colorspace(mut self, ws: klipp_core::WorkingColorspace) -> Self {
        self.working = ws;
        self.resolve_scene();
        self
    }

    fn resolve_scene(&mut self) {
        let cs = match self.log_format {
            Some(f) => f.apply_to(self.source_cs),
            None => self.source_cs,
        };
        self.scene = SceneSource::for_colourspace(cs, self.working);
    }

    /// The source's primaries (for converting display-referred frames into
    /// the working space).
    pub fn source_primaries(&self) -> klipp_core::ColorPrimaries {
        self.source_cs.primaries
    }

    /// Whether frames decode through the scene-referred route.
    pub fn is_scene_referred(&self) -> bool {
        self.scene.is_some()
    }

    /// The codes of the frame `next_frame` last returned, when the source
    /// is scene-referred. Taken, so a stale frame's codes can never be
    /// graded in place of a later frame's.
    pub fn take_scene_referred(&mut self) -> Option<SceneReferredFrame> {
        self.last_scene.take()
    }

    /// Decode the next frame. Returns `Ok(None)` when the clip is exhausted.
    /// The next frame as sRGB RGBA8 (empty for a scene-referred source,
    /// whose frames travel as codes — see [`Self::take_scene_referred`]).
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, RenderError> {
        // No converter, so the frame comes back on the CPU: this is the
        // convenience accessor for smoke tests and probes, not the
        // export's path, and handing it a GPU buffer it would only have
        // to read back again buys nothing.
        let Some((_, pixels)) = self.next_frame_with_pts(&mut None)? else { return Ok(None) };
        Ok(Some(match pixels {
            DecodedPixels::Rgba64(rgba) => rgba.iter().map(|v| (v >> 8) as u8).collect(),
            DecodedPixels::GpuLinear(_) => {
                unreachable!("no converter was offered, so no GPU frame can come back")
            }
        }))
    }

    /// The next frame as sRGB-encoded RGBA64 — the source's full precision
    /// for 10- and 12-bit video — with its source time in seconds.
    pub(crate) fn next_frame_with_pts(
        &mut self,
        nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
    ) -> Result<Option<(f64, DecodedPixels)>, RenderError> {
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
                    let rgba = self.frame_to_rgba(&decoded, nv12)?;
                    self.frames_decoded += 1;
                    return Ok(Some((frame_pts_secs, rgba)));
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
    /// Where a decoded frame's time goes: the GPU→CPU download plus the
    /// colour conversion, and the rotation. Totals, logged by the export.
    pub(crate) fn stage_times(&self) -> (std::time::Duration, std::time::Duration) {
        (self.t_convert, self.t_rotate)
    }

    fn frame_to_rgba(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
        nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
    ) -> Result<DecodedPixels, RenderError> {
        // For 90/270 rotation, scale to swapped dimensions first.
        let (scale_w, scale_h) = if self.source_rotation == 90 || self.source_rotation == 270 {
            (self.target_height, self.target_width)
        } else {
            (self.target_width, self.target_height)
        };

        if let Some(scene) = self.scene {
            if unsafe { self.scene_referred_frame(decoded, scene) } {
                // The frame travels as codes (`take_scene_referred`).
                return Ok(DecodedPixels::Rgba64(Vec::new()));
            }
        }

        // The whole conversion on the GPU, for a hardware-decoded
        // display-referred frame: NV12 down from the decoder, linear
        // working-space f32 straight into a buffer the grade chain can
        // take. Skips swscale and the rotation pass both, and uploads
        // 12 MB of planes instead of 66 MB of RGBA64.
        let t_gpu = std::time::Instant::now();
        if let Some(pixels) = unsafe { self.nv12_frame_on_gpu(decoded, nv12) } {
            self.t_convert += t_gpu.elapsed();
            return Ok(pixels);
        }

        let t0 = std::time::Instant::now();
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

        self.t_convert += t0.elapsed();

        // Apply source rotation if needed (phone/drone videos with container rotation metadata)
        if self.source_rotation != 0 {
            let t1 = std::time::Instant::now();
            let (rotated, _rw, _rh) = rotate_rgba(&rgba, scale_w, scale_h, self.source_rotation);
            self.t_rotate += t1.elapsed();
            Ok(DecodedPixels::Rgba64(rotated))
        } else {
            Ok(DecodedPixels::Rgba64(rgba))
        }
    }

    /// A hardware-decoded NV12 frame converted to linear working-space
    /// f32 on the GPU, resized and rotated in the same pass.
    ///
    /// `None` whenever this is not the right route, and the caller falls
    /// back to downloading and converting with swscale: no converter (no
    /// usable GPU, or software decode), a frame that does not download as
    /// NV12, a resize (see below), or a scene-referred source — camera
    /// log and PQ/HLG have their own tested path through
    /// `scene_referred_frame` and packed codes, and this shader's code
    /// output is not wired up here.
    ///
    /// **This changes the export's chroma upsampling.** The shader takes
    /// 4:2:0 chroma nearest-neighbour; swscale interpolated it. On the
    /// user's footage that is a median difference of 0 levels and a worst
    /// of 21, on the 0.01% of samples that sit on a sharp saturated edge.
    /// It is a deliberate trade and arguably the right way round: the
    /// viewer has always shown this shader's nearest-neighbour chroma, so
    /// before this the viewer and the export disagreed, and now they do
    /// not. Proper bilinear chroma would be better than either, but it
    /// belongs in the shader *and* its CPU twin together —
    /// `sw_decode_parity` holds them to one code of each other — so it is
    /// a change to the viewer's picture, not to the export's, and it is
    /// not this function's to make.
    ///
    /// SAFETY: `decoded` must be a valid decoded frame.
    unsafe fn nv12_frame_on_gpu(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
        nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
    ) -> Option<DecodedPixels> {
        use ffmpeg_next::ffi::*;

        // Software decode has the frame on the CPU already, so converting
        // it on the GPU would mean uploading YUV to save a conversion.
        if nv12.is_none()
            || self.hw_accel == HwAccel::Software
            || self.scene.is_some()
            || self.log_format.is_some()
        {
            return None;
        }
        let is_hw = decoded.format() == ffmpeg_next::format::Pixel::CUDA
            || decoded.format() == ffmpeg_next::format::Pixel::VAAPI;
        if !is_hw {
            return None;
        }

        let sw_frame = av_frame_alloc();
        if sw_frame.is_null() {
            return None;
        }
        if av_hwframe_transfer_data(sw_frame, decoded.as_ptr(), 0) < 0 {
            av_frame_free(&mut (sw_frame as *mut _));
            return None;
        }
        let fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);
        if fmt != AVPixelFormat::AV_PIX_FMT_NV12 {
            // P010 and friends: 10-bit, and this shader reads bytes.
            av_frame_free(&mut (sw_frame as *mut _));
            return None;
        }

        let (src_w, src_h) = ((*sw_frame).width as u32, (*sw_frame).height as u32);

        // Only at 1:1. The shader maps each output pixel back to a single
        // source pixel, which is exact for a straight conversion and for
        // a 90/180/270 rotation — both are permutations — but is
        // nearest-neighbour for a real resize, and that aliases. A
        // timeline whose resolution differs from the source keeps
        // swscale's Lanczos, which is the same line `scaler_flags` draws.
        let (want_w, want_h) = if self.source_rotation == 90 || self.source_rotation == 270 {
            (self.target_height, self.target_width)
        } else {
            (self.target_width, self.target_height)
        };
        if (src_w, src_h) != (want_w, want_h) {
            av_frame_free(&mut (sw_frame as *mut _));
            return None;
        }

        let (y_stride, uv_stride) =
            ((*sw_frame).linesize[0] as u32, (*sw_frame).linesize[1] as u32);
        // The planes as they came down, strides and all — the shader
        // indexes by stride, so there is nothing to repack.
        let y = std::slice::from_raw_parts(
            (*sw_frame).data[0],
            (y_stride as usize) * (src_h as usize),
        );
        let uv = std::slice::from_raw_parts(
            (*sw_frame).data[1],
            (uv_stride as usize) * (src_h as usize / 2),
        );

        let ids = klipp_core::color_ids::ids(self.source_cs);
        let primaries = klipp_effects::color_wgsl::pack_primaries_matrix(
            klipp_color::camera_log::primaries_to_working_matrix(
                self.source_cs.primaries,
                self.working,
            ),
        );
        let buf = nv12.as_mut()?.convert_nv12_to_linear_on_gpu(
            y,
            uv,
            src_w,
            src_h,
            y_stride,
            uv_stride,
            // Post-rotation dimensions: the shader maps each output pixel
            // back through the rotation itself, so this is the frame the
            // renderer asked for and no separate pass is needed.
            self.target_width,
            self.target_height,
            self.source_rotation as u32,
            ids,
            primaries,
        );
        av_frame_free(&mut (sw_frame as *mut _));
        Some(DecodedPixels::GpuLinear(buf))
    }

    /// A scene-referred frame the way the viewer decodes it: 10-bit codes
    /// (`klipp_color::hdr::yuv_to_hdr_codes`, bit-identical to the
    /// viewer's decoder shaders), kept for `take_scene_referred`. Resized
    /// nearest-neighbour and rotated as the viewer's decoders do. `false`
    /// if the frame's samples cannot be read, in which case the caller
    /// falls back to swscale.
    ///
    /// SAFETY: `decoded` must be a valid decoded frame.
    unsafe fn scene_referred_frame(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
        scene: SceneSource,
    ) -> bool {
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
                return false;
            }
            sw
        } else {
            decoded.as_ptr()
        };
        let yuv = klipp_media::yuv_samples::read_yuv_samples(src);
        if !sw.is_null() {
            av_frame_free(&mut sw);
        }
        let Some(yuv) = yuv else { return false };
        let (w, h) = (self.target_width, self.target_height);
        let codes = klipp_color::hdr::yuv_to_hdr_codes(
            &yuv.samples(),
            w,
            h,
            self.source_rotation.rem_euclid(360) as u32,
            scene.ids,
        );
        self.last_scene = Some(SceneReferredFrame {
            codes: std::sync::Arc::new(codes),
            transfer: scene.transfer,
            source_to_working: scene.to_working,
            width: w,
            height: h,
        });
        true
    }

    /// Scale a decoded frame using the software scaler.
    fn scale_frame_sw(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
    ) -> Result<Vec<u16>, RenderError> {
        let mut rgba_frame = ffmpeg_next::util::frame::Video::empty();
        self.scaler
            .run(decoded, &mut rgba_frame)
            .map_err(RenderError::Decode)?;
        let (w, h) = (rgba_frame.width() as usize, rgba_frame.height() as usize);
        Ok(rgba64_rows(rgba_frame.data(0), rgba_frame.stride(0), w, h))
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
) -> Option<Vec<u16>> {
    use ffmpeg_next::ffi::*;

    let t_start = std::time::Instant::now();
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
    let t_transfer = t_start.elapsed();

    let src_w = (*sw_frame).width as u32;
    let src_h = (*sw_frame).height as u32;
    let sw_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);

    // Lanczos when the frame is actually being resized, and a cheap flag
    // when it is not — see `scaler_flags`.
    let sws = sws_getCachedContext(
        sws_ctx.unwrap_or(std::ptr::null_mut()),
        src_w as i32,
        src_h as i32,
        sw_fmt,
        target_w as i32,
        target_h as i32,
        AVPixelFormat::AV_PIX_FMT_RGBA64LE,
        if src_w == target_w && src_h == target_h { 0x2 } else { 0x200 },
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

    // The scaler writes straight into the buffer this returns: no
    // AVFrame to allocate, and no second pass copying rows out of one.
    //
    // Both of those cost more than they look. A 4K RGBA64 frame is
    // 66 MB, so the old shape allocated 66 MB, had the scaler fill it,
    // then allocated another 66 MB and copied — and because the second
    // allocation was fresh every frame, most of its 18.6 ms was the
    // kernel faulting in pages rather than any work on pixels.
    //
    // The capacity is handed over uninitialised for the same reason:
    // `vec![0u16; n]` would write 66 MB of zeroes only for the scaler to
    // overwrite every one of them. `sws_scale` fills the whole plane —
    // `target_w * 8` bytes on each of `target_h` rows, which is exactly
    // this buffer with no padding — so every element is initialised
    // before `set_len` publishes it.
    let samples = target_w as usize * target_h as usize * 4;
    let mut out: Vec<u16> = Vec::with_capacity(samples);
    let dst: [*mut u8; 4] = [
        out.as_mut_ptr() as *mut u8,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ];
    let dst_stride: [i32; 4] = [(target_w * 8) as i32, 0, 0, 0];

    let rows = sws_scale(
        sws,
        (*sw_frame).data.as_ptr() as *const *const u8,
        (*sw_frame).linesize.as_ptr(),
        0,
        src_h as i32,
        dst.as_ptr(),
        dst_stride.as_ptr(),
    );
    av_frame_free(&mut (sw_frame as *mut _));
    if rows < target_h as i32 {
        // Short of a full frame means part of the buffer is still
        // uninitialised; there is nothing safe to hand back.
        warn!("HW download: sws_scale wrote {rows} of {target_h} rows");
        return None;
    }
    out.set_len(samples);

    tracing::debug!(
        "hw download: transfer {:.1}ms, sws {:?}→RGBA64 {:.1}ms",
        t_transfer.as_secs_f64() * 1000.0,
        sw_fmt,
        (t_start.elapsed() - t_transfer).as_secs_f64() * 1000.0,
    );
    Some(out)
}

/// RGBA64LE rows (`stride` bytes apart) → packed `u16` samples.
///
/// A 4K frame is 33 million samples, so how this loop is written is
/// worth a few milliseconds. Unpadded rows — the common case, since the
/// scaler allocates its own frame at the target width — are one
/// contiguous block and copy in a single pass. A padded frame is copied
/// a row at a time, in parallel: the rows are independent, and doing
/// them one after another through a byte-pair iterator left a whole core
/// converting while seven idled.
///
/// `pod_collect_to_vec` rather than a cast because an AVFrame's buffer
/// carries no `u16` alignment guarantee.
fn rgba64_rows(data: &[u8], stride: usize, w: usize, h: usize) -> Vec<u16> {
    use rayon::prelude::*;
    let row_bytes = w * 8;
    if stride == row_bytes {
        return bytemuck::pod_collect_to_vec(&data[..row_bytes * h]);
    }
    let mut out = vec![0u16; w * h * 4];
    out.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(row, dst)| {
            let src = &data[row * stride..row * stride + row_bytes];
            for (d, b) in dst.iter_mut().zip(src.chunks_exact(2)) {
                *d = u16::from_le_bytes([b[0], b[1]]);
            }
        });
    out
}

/// Rotate an RGBA frame by 0/90/180/270 degrees clockwise.
/// Returns (rotated_data, new_width, new_height).
/// Rotate an interleaved RGBA frame by a multiple of 90°.
///
/// Written as one rayon pass per output row, gathering from the source.
/// It used to be a single-threaded double loop *writing* to the rotated
/// positions, which is the worst of both worlds at 4K: 8.3 million
/// eight-byte writes, each landing 17 KB from the last, on one core. It
/// was 70 ms a frame of the export's 240 — more than the decode and the
/// colour conversion put together, for a memory shuffle. Gathering
/// instead of scattering keeps the writes sequential, and the rows are
/// independent so they parallelise.
fn rotate_rgba<T: Copy + Default + Send + Sync>(
    rgba: &[T],
    w: u32,
    h: u32,
    degrees: i32,
) -> (Vec<T>, u32, u32) {
    use rayon::prelude::*;
    let (nw, nh) = match degrees {
        90 | 270 => (h, w),
        180 => (w, h),
        _ => return (rgba.to_vec(), w, h),
    };
    let mut out = vec![T::default(); (nw * nh * 4) as usize];
    let (wi, hi) = (w as usize, h as usize);
    let nwi = nw as usize;
    out.par_chunks_exact_mut(nwi * 4)
        .enumerate()
        .for_each(|(dy, row)| {
            for dx in 0..nwi {
                // Where this output pixel came from.
                let (sx, sy) = match degrees {
                    90 => (dy, hi - 1 - dx),
                    270 => (wi - 1 - dy, dx),
                    _ => (wi - 1 - dx, hi - 1 - dy),
                };
                let src = (sy * wi + sx) * 4;
                row[dx * 4..dx * 4 + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        });
    (out, nw, nh)
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
