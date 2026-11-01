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

/// The GPU converters a frame source owns, one of each, shared by every
/// decoder it drives.
///
/// One struct rather than two threaded arguments: they travel together
/// through `pull` and `next_frame_with_pts`, and a second `&mut Option`
/// alongside the first is two chances to pass the wrong one. Building a
/// converter per decoder is also what exhausted the shared device's
/// descriptor pool once, which arrives as SIGSEGV rather than an error.
#[derive(Default)]
pub struct GpuConverters {
    /// Counted in [`live_render_decoders`] while it lives.
    _live: Live,
    /// Eight-bit 4:2:0 (NV12), what an SDR hardware decode hands back.
    pub nv12: Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
    /// Ten-bit 4:2:0 (P010), what a 10-bit one does. Without this a
    /// 10-bit frame is scaled to RGBA64 by swscale, at 42 ms a 4K frame.
    pub p010: Option<klipp_effects::p010_gpu::GpuP010Converter>,
}

impl GpuConverters {
    /// Both converters on the process-wide context — the same one the
    /// grade chain uses, which is required for the chain to be able to
    /// bind the buffers they return.
    pub fn shared() -> Self {
        let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
            return Self::default();
        };
        Self {
            _live: Live::new(),
            nv12: klipp_effects::nv12_gpu::GpuNv12Converter::with_context(ctx),
            p010: klipp_effects::p010_gpu::GpuP010Converter::with_context(ctx),
        }
    }
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
    /// Counted in [`hardware_decoders_open`] while it decodes on the GPU.
    _on_gpu: Option<OnGpu>,
    /// Counted in [`live_render_decoders`] while it lives.
    _live: Live,
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
    /// A still image (#978): opened at its one frame whatever the in point.
    still: bool,
}

/// A scene-referred source frame (PQ, HLG, camera log) as the viewer's
/// grade reads it: code values in the source primaries, 10-bit or, for
/// a deeper source, 16-bit (#1226).
#[derive(Clone)]
pub struct SceneReferredFrame {
    pub codes: std::sync::Arc<klipp_color::hdr::SceneCodes>,
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
            klipp_media::open_input(path).map_err(RenderError::Decode)?;

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

        // A still image is one frame at time 0 (#978): from any in point
        // it is that frame, which a seek and a skip to the in point as
        // into a video would pass by, leaving the clip black.
        let still = klipp_core::media_extensions::is_still_image_demuxer(format_ctx.format().name());
        let source_start_secs = if still { 0.0 } else { source_start_secs };
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
            scene: SceneSource::for_colourspace(source_cs, klipp_core::WorkingColorspace::Rec709Linear),
            source_cs,
            log_format: None,
            working: klipp_core::WorkingColorspace::Rec709Linear,
            last_scene: None,
            hw_accel,
            _on_gpu: (hw_accel != HwAccel::Software).then(OnGpu::new),
            _live: Live::new(),
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
            still,
        })
    }

    /// Whether the file is a still image: its one frame is the clip's
    /// picture throughout, not a file that ended early.
    pub fn is_still(&self) -> bool {
        self.still
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
        let Some((_, pixels)) = self.next_frame_with_pts(&mut GpuConverters::default())? else {
            return Ok(None);
        };
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
        gpu: &mut GpuConverters,
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
                    let rgba = self.frame_to_rgba(&decoded, gpu)?;
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
        gpu: &mut GpuConverters,
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
        if let Some(pixels) = unsafe { self.hw_frame_on_gpu(decoded, gpu) } {
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
                    self.source_cs,
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

    /// A hardware-decoded frame converted to linear working-space f32 on
    /// the GPU, resized and rotated in the same pass.
    ///
    /// `None` whenever this is not the right route, and the caller falls
    /// back to downloading and converting on the CPU: no converter (no
    /// usable GPU, or software decode), a frame that does not download as
    /// NV12 or P010, or a scene-referred source — camera
    /// log and PQ/HLG have their own tested path through
    /// `scene_referred_frame` and packed codes, and this shader's code
    /// output is not wired up here.
    ///
    /// Chroma is reconstructed as the viewer's decoders reconstruct it:
    /// this is the viewer's own shader, bilinear at
    /// `klipp_color::hdr::chroma_taps`' siting, and the software path's
    /// converter (`klipp_rgba64`) uses the same taps (#3266).
    ///
    /// SAFETY: `decoded` must be a valid decoded frame.
    unsafe fn hw_frame_on_gpu(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
        gpu: &mut GpuConverters,
    ) -> Option<DecodedPixels> {
        use ffmpeg_next::ffi::*;

        // Software decode has the frame on the CPU already, so converting
        // it on the GPU would mean uploading YUV to save a conversion.
        if self.hw_accel == HwAccel::Software
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
        // Eight bits or ten; the two shaders differ only in how they read
        // a sample. Anything else falls back to swscale.
        let fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);
        let ten_bit = match fmt {
            AVPixelFormat::AV_PIX_FMT_NV12 => false,
            AVPixelFormat::AV_PIX_FMT_P010LE => true,
            _ => {
                av_frame_free(&mut (sw_frame as *mut _));
                return None;
            }
        };
        if (ten_bit && gpu.p010.is_none()) || (!ten_bit && gpu.nv12.is_none()) {
            av_frame_free(&mut (sw_frame as *mut _));
            return None;
        }

        let (src_w, src_h) = ((*sw_frame).width as u32, (*sw_frame).height as u32);

        // At any size. A straight conversion and a 90/180/270 rotation are
        // permutations, exact one source pixel per output pixel; a
        // downscale averages each output pixel's footprint in linear light
        // (#744), as the viewer's decode now does too; an upscale is
        // Lanczos-3 (#2557), the kernel swscale used here, on the encoded
        // values as it filtered them, so a 4K export keeps its look.
        // Each used to be refused, and went down to the CPU through
        // swscale and back up: a 1080p source in a 4K export took 44 ms a
        // frame there, against a millisecond on the GPU.

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
        // Post-rotation dimensions: the shader maps each output pixel back
        // through the rotation itself, so this is the frame the renderer
        // asked for and no separate pass is needed.
        let buf = if ten_bit {
            gpu.p010.as_mut()?.convert_p010_to_linear_on_gpu(
                y,
                uv,
                src_w,
                src_h,
                y_stride,
                uv_stride,
                self.target_width,
                self.target_height,
                self.source_rotation as u32,
                ids,
                primaries,
            )
        } else {
            gpu.nv12.as_mut()?.convert_nv12_to_linear_on_gpu(
                y,
                uv,
                src_w,
                src_h,
                y_stride,
                uv_stride,
                self.target_width,
                self.target_height,
                self.source_rotation as u32,
                ids,
                primaries,
            )
        };
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

    /// A software-decoded frame as RGBA64 at the scaled size: converted by
    /// Klipp's own Y'CbCr converter, as the viewer's software decode does,
    /// and only resized by swscale (#3266). swscale converting the colour
    /// too reconstructed 4:2:0 and 4:2:2 chroma at its own phase, so at a
    /// hard colour edge the export differed from the viewer by up to 255
    /// levels, and so did a kept or frozen frame. What Klipp's reader
    /// does not take (RGB, palette, grey) still goes through swscale.
    fn scale_frame_sw(
        &mut self,
        decoded: &ffmpeg_next::util::frame::Video,
    ) -> Result<Vec<u16>, RenderError> {
        let (scale_w, scale_h) = (self.scaler.output().width, self.scaler.output().height);
        // SAFETY: `decoded` is a valid decoded frame.
        if let Some(rgba) = unsafe { klipp_rgba64(decoded.as_ptr(), Some(decoded), self.source_cs, scale_w, scale_h) } {
            return Ok(rgba);
        }
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

    /// The source stream's frame rate: the unit for "half a frame" of
    /// seek tolerance. Half a *sequence* frame is 1.25 source frames
    /// for 60 fps footage in a 24 fps sequence, and a tolerance that
    /// wide picks the frame after the one asked for.
    pub(crate) fn source_fps(&self) -> f64 {
        self.fps
    }

    pub fn hw_accel_name(&self) -> &'static str {
        match self.hw_accel {
            HwAccel::Cuda => "CUDA/NVDEC",
            HwAccel::Vaapi => "VAAPI",
            HwAccel::Software => "Software",
        }
    }
}

/// Render decoders open on the GPU's decoder now.
static ON_GPU: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many render decoders (export, background renders) decode on the
/// GPU now. The viewer's decode budget measures what the GPU's decoder
/// gives from its own decoders' frames; while these share it, that
/// measure is only the viewer's part, and is not taken (#2116).
pub fn hardware_decoders_open() -> usize {
    ON_GPU.load(std::sync::atomic::Ordering::Relaxed)
}

/// One render decoder's place in [`hardware_decoders_open`].
struct OnGpu;

impl OnGpu {
    fn new() -> Self {
        ON_GPU.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self
    }
}

impl Drop for OnGpu {
    fn drop(&mut self) {
        ON_GPU.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Render decoders and GPU frame converters alive now, export's and
/// background renders'.
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many render decoders and GPU frame converters are alive. A quit
/// waits for them to be dropped before the process exits: one still in
/// ffmpeg or the GPU driver as the driver tears itself down at exit
/// crashes the process, as a viewer decoder did (#2261).
pub fn live_render_decoders() -> usize {
    LIVE.load(std::sync::atomic::Ordering::Acquire)
}

/// One decoder's or converter's place in [`live_render_decoders`].
struct Live;

impl Live {
    fn new() -> Self {
        LIVE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self
    }
}

// `GpuConverters::default()`, the converters a render without a GPU
// has, is counted as the shared ones are.
impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
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

    // Never NVIDIA's node: its VAAPI driver can deadlock the process
    // against a CUDA decoder opening on another thread.
    for node in GpuVendor::vaapi_nodes() {
        let Ok(path) = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(node.as_os_str())) else {
            continue;
        };
        let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            device_type,
            path.as_ptr(),
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
    source_cs: klipp_core::SourceColorspace,
) -> Option<Vec<u16>> {
    let (yuv_matrix, yuv_range) = (source_cs.matrix, source_cs.range);
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

    // The downloaded NV12 or P010 through Klipp's converter, as the
    // software decode's frames go (#3266); swscale below only for what
    // it does not read.
    if let Some(rgba) = klipp_rgba64(sw_frame, None, source_cs, target_w, target_h) {
        av_frame_free(&mut (sw_frame as *mut _));
        return Some(rgba);
    }

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

/// A software Y'CbCr frame as RGBA64 at `target_w`×`target_h` by Klipp's
/// converter (`klipp_color::camera_log::yuv_to_display_rgba64`), the
/// chroma reconstruction the viewer's decoders use, then shrunk or grown
/// by the CPU twins of the decode shaders' resize, so the export resizes
/// a software-decoded file as it does a hardware-decoded one. `alpha_from` carries a ProRes 4444 or FFV1 alpha plane
/// (#3283). `None` when Klipp's reader does not take the format.
///
/// SAFETY: `frame` must be a valid software frame.
unsafe fn klipp_rgba64(
    frame: *const ffmpeg_next::ffi::AVFrame,
    alpha_from: Option<&ffmpeg_next::util::frame::Video>,
    source_cs: klipp_core::SourceColorspace,
    target_w: u32,
    target_h: u32,
) -> Option<Vec<u16>> {
    let yuv = klipp_media::yuv_samples::read_yuv_samples(frame)?;
    let samples = yuv.samples();
    let alpha = alpha_from.and_then(klipp_media::yuv_samples::alpha_plane_u16);
    let from = (samples.width.max(1), samples.height.max(1));
    // Smaller on a side: the decode shaders' footprint average, so the
    // export shrinks a software-decoded file as it shrinks a hardware-
    // decoded one and as the viewer does.
    if klipp_color::camera_log::resize_averages(from, (target_w, target_h)) {
        return Some(klipp_color::camera_log::yuv_to_display_rgba64_downscaled(&samples, source_cs, alpha.as_deref(), target_w, target_h));
    }
    let mut rgba = klipp_color::camera_log::yuv_to_display_rgba64(&samples, source_cs);
    if let Some(alpha) = alpha {
        for (px, a) in rgba.chunks_exact_mut(4).zip(alpha) {
            px[3] = a;
        }
    }
    if from == (target_w, target_h) {
        return Some(rgba);
    }
    // Larger: the shaders' Lanczos-3 on the encoded values. swscale
    // growing RGBA64 put a wobble of a few codes into flat areas.
    Some(klipp_color::camera_log::lanczos_upscale_rgba64(&rgba, from, (target_w, target_h)))
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

/// The picture's size upright: the video stream's coded size, turned for a
/// 90° or 270° `rotation`. `None` when the file cannot be read.
pub(crate) fn display_size(path: &Path, rotation: i32) -> Option<(u32, u32)> {
    let input = klipp_media::open_input(path).ok()?;
    let stream = input.streams().best(ffmpeg_next::media::Type::Video)?;
    let (w, h) = unsafe {
        let par = (*stream.as_ptr()).codecpar;
        ((*par).width, (*par).height)
    };
    let (w, h) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    Some(if rotation.rem_euclid(180) == 90 { (h, w) } else { (w, h) })
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
    // The tests that open a RenderDecoder try CUDA first. Run with
    // `--ignored`, the first cuInit of the process could meet a VAAPI test
    // loading nvidia-vaapi-driver, the deadlock #568 found with NVENC, so
    // they take the same lock as the encoder and VAAPI tests.
    use serial_test::serial;

    /// A CUDA decoder and a VAAPI one opening at the same moment, each
    /// the first of its process, never deadlock. NVIDIA's VAAPI driver
    /// initialises CUDA from its constructor under the loader's lock, and
    /// a thread in its first cuInit waits for that lock: the VAAPI side
    /// must never open NVIDIA's node. Each round is a fresh process, as
    /// only a process's first initialisation can meet the other.
    #[test]
    fn cuda_and_vaapi_opening_together_never_deadlock() {
        let _watch = crate::test_watchdog::Watchdog::start();
        const CHILD: &str = "KLIPP_HWDEVICE_RACE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            ffmpeg_next::init().unwrap();
            let start = std::sync::Arc::new(std::sync::Barrier::new(2));
            let open = |cuda: bool| {
                let start = start.clone();
                std::thread::spawn(move || {
                    let mut ctx = ffmpeg_next::codec::context::Context::new();
                    start.wait();
                    // SAFETY: `ctx` is a fresh, unopened codec context
                    // this thread owns; each call only gives it a device.
                    unsafe { if cuda { try_init_cuda(ctx.as_mut_ptr()) } else { try_init_vaapi(ctx.as_mut_ptr()) } }
                })
            };
            let (cuda, vaapi) = (open(true), open(false));
            cuda.join().unwrap();
            vaapi.join().unwrap();
            return;
        }
        if std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none() {
            eprintln!("skipped: set KLIPP_TEST_REQUIRE_GPU=1 to open the GPU's devices");
            return;
        }
        let rounds: usize = std::env::var("KLIPP_HWDEVICE_RACE_ROUNDS").ok().and_then(|n| n.parse().ok()).unwrap_or(20);
        let exe = std::env::current_exe().unwrap();
        let mut stuck = 0;
        for _ in 0..rounds {
            let mut child = std::process::Command::new(&exe)
                .args(["--exact", "clip_decoder::tests::cuda_and_vaapi_opening_together_never_deadlock", "--test-threads=1"])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let began = std::time::Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "a round failed: {status}");
                    break;
                }
                if began.elapsed() > std::time::Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    stuck += 1;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        assert_eq!(stuck, 0, "{stuck} of {rounds} rounds deadlocked opening CUDA and VAAPI together");
    }

    fn test_media_path() -> Option<PathBuf> {
        std::env::var("TEST_MEDIA_FILE").ok().map(PathBuf::from)
    }

    #[test]
    fn gpu_detection_runs() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let gpu = detect_gpu();
        println!("Detected GPU: {:?}", gpu);
        // Just verify it doesn't crash
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    #[serial]
    fn decoder_produces_frames() {
        let _watch = crate::test_watchdog::Watchdog::start();
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
    #[serial]
    fn decoder_seeks_correctly() {
        let _watch = crate::test_watchdog::Watchdog::start();
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
    #[serial]
    fn decoder_respects_source_end() {
        let _watch = crate::test_watchdog::Watchdog::start();
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

    /// What a 10-bit source costs on the way in, before and after the GPU
    /// conversion.
    ///
    /// A probe, not a pass/fail test:
    /// `KLIPP_TENBIT=<file.mp4> cargo test --release -p klipp-render \
    ///     clip_decoder::tests::tenbit_decode_cost -- --ignored --nocapture`
    ///
    /// It also proves the path is *taken*, which a timing number alone
    /// would not: with the converters present the decoder answers
    /// `GpuLinear`, and without them `Rgba64`. A wrong format gate would
    /// return the same variant both times and the timings would match.
    #[test]
    #[ignore = "perf probe: needs KLIPP_TENBIT"]
    #[serial]
    fn tenbit_decode_cost() {
        let _watch = crate::test_watchdog::Watchdog::start();
        let Ok(path) = std::env::var("KLIPP_TENBIT") else {
            eprintln!("skipping: set KLIPP_TENBIT to a 10-bit file");
            return;
        };
        let path = std::path::PathBuf::from(path);
        for (label, mut gpu) in [
            ("swscale (no converters)", GpuConverters::default()),
            ("GPU", GpuConverters::shared()),
        ] {
            let mut dec = match RenderDecoder::open(&path, 0.0, 3.0, 3840, 2160, 30.0, 0) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("could not open: {e:?}");
                    return;
                }
            };
            let mut resident = 0;
            let mut on_cpu = 0;
            let t = std::time::Instant::now();
            let mut frames = 0;
            while frames < 20 {
                match dec.next_frame_with_pts(&mut gpu) {
                    Ok(Some((_, DecodedPixels::GpuLinear(_)))) => resident += 1,
                    Ok(Some((_, DecodedPixels::Rgba64(_)))) => on_cpu += 1,
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("decode error: {e:?}");
                        break;
                    }
                }
                frames += 1;
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "{label} ({}): {frames} frames, {:.1} ms/frame \
                 ({resident} GPU-resident, {on_cpu} on the CPU)",
                dec.hw_accel_name(),
                ms / frames.max(1) as f64,
            );
        }
    }
}

/// #2557: a source smaller than the export decodes on the GPU, upscaled
/// there with the Lanczos-3 swscale used, instead of going down to the CPU
/// to be scaled at 44 ms a frame for a 1080p source in a 4K export.
#[cfg(test)]
mod upscale_tests {
    use super::{DecodedPixels, GpuConverters, RenderDecoder};

    /// Six seconds of a 1080p test chart held still and in grey, H.264:
    /// any frame two decoders land on is the same picture, and with no
    /// colour their only difference is the resize, not how each rebuilds
    /// the half-size chroma (which differ on saturated edges at 1:1 too).
    /// `None` without ffmpeg.
    fn chart() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-upscale-grey-chart-1080p.mp4");
        if out.exists() {
            return Some(out);
        }
        let part = out.with_extension("part.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=25:d=6", "-vf", "loop=loop=149:size=1:start=0,hue=s=0", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25"])
            .arg(&part)
            .status()
            .is_ok_and(|s| s.success());
        ok.then(|| std::fs::rename(&part, &out).ok()).flatten().map(|_| out)
    }

    /// The same chart in 10-bit HEVC, decoded as P010; `None` without an
    /// ffmpeg with x265.
    fn chart_10bit() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-upscale-grey-chart-1080p-10bit.mp4");
        if out.exists() {
            return Some(out);
        }
        let part = out.with_extension("part.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=25:d=6", "-vf", "loop=loop=149:size=1:start=0,hue=s=0,format=yuv420p10le"])
            .args(["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "keyint=25:log-level=error"])
            .arg(&part)
            .status()
            .is_ok_and(|s| s.success());
        ok.then(|| std::fs::rename(&part, &out).ok()).flatten().map(|_| out)
    }

    /// The milliseconds a frame takes over `n` frames at `w` x `h`, and
    /// whether every one stayed on the GPU.
    fn timed(path: &std::path::Path, w: u32, h: u32, gpu: &mut GpuConverters, n: usize) -> (f64, bool) {
        let mut d = RenderDecoder::open(path, 0.0, f64::INFINITY, w, h, 25.0, 0).unwrap();
        let _ = d.next_frame_with_pts(gpu).unwrap();
        let (t, mut on_gpu) = (std::time::Instant::now(), true);
        for _ in 0..n {
            let (_, px) = d.next_frame_with_pts(gpu).unwrap().expect("a frame");
            on_gpu &= matches!(px, DecodedPixels::GpuLinear(_));
        }
        (t.elapsed().as_secs_f64() * 1000.0 / n as f64, on_gpu)
    }

    /// The working-space linear RGBA of a frame on the GPU, read back.
    fn read_back(gpu: &GpuConverters, buf: &wgpu::Buffer, pixels: usize) -> Vec<f32> {
        let conv = gpu.nv12.as_ref().expect("a converter");
        let (device, queue) = (conv.device(), conv.queue());
        let size = (pixels * 16) as u64;
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("upscale_test_read"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, size);
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let out = bytemuck_floats(&staging.slice(..).get_mapped_range());
        staging.unmap();
        out
    }

    fn bytemuck_floats(bytes: &[u8]) -> Vec<f32> {
        bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    }

    /// Linear light to an 8-bit sRGB level, where the difference is read.
    fn srgb8(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        255.0 * if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    }

    /// The same frame decoded on the GPU and through swscale, at `w` x
    /// `h`, as the compositor gets each: the largest and the mean
    /// difference, in 8-bit sRGB levels.
    fn against_swscale(path: &std::path::Path, w: u32, h: u32, gpu: &mut GpuConverters) -> (f32, f32) {
        let frame = |gpu: &mut GpuConverters| {
            let mut d = RenderDecoder::open(path, 1.0, f64::INFINITY, w, h, 25.0, 0).unwrap();
            d.next_frame_with_pts(gpu).unwrap().expect("a frame").1
        };
        let DecodedPixels::GpuLinear(buf) = frame(gpu) else { panic!("not on the GPU at {w}x{h}") };
        let on_gpu = read_back(gpu, &buf, (w * h) as usize);
        let DecodedPixels::Rgba64(rgba) = frame(&mut GpuConverters::default()) else { panic!("not on the CPU") };
        let scaled = crate::frame_source::rgba64_to_linear(&rgba, None);
        let (mut worst, mut sum) = (0.0f32, 0.0f64);
        for (a, b) in on_gpu.iter().zip(&scaled).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, p)| p) {
            let d = (srgb8(*a) - srgb8(*b)).abs();
            worst = worst.max(d);
            sum += d as f64;
        }
        let mut ds: Vec<f32> = on_gpu.iter().zip(&scaled).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (srgb8(*a) - srgb8(*b)).abs()).collect();
        ds.sort_by(f32::total_cmp);
        let pct = |p: f64| ds[((ds.len() - 1) as f64 * p) as usize];
        eprintln!("{w}x{h} against swscale: p99 {:.1}, p99.9 {:.1} levels", pct(0.99), pct(0.999));
        (worst, (sum / (w * h * 3) as f64) as f32)
    }

    #[test]
    fn a_1080p_source_in_a_4k_export_stays_on_the_gpu() {
        let Some(path) = chart() else { return eprintln!("skipping: no ffmpeg") };
        let mut gpu = GpuConverters::shared();
        if gpu.nv12.is_none() || RenderDecoder::open(&path, 0.0, 1.0, 64, 36, 25.0, 0).unwrap().hw_accel_name() == "Software" {
            return eprintln!("skipping: no hardware decode and GPU converter here");
        }
        let (hd, hd_gpu) = timed(&path, 1920, 1080, &mut gpu, 60);
        let (uhd, uhd_gpu) = timed(&path, 3840, 2160, &mut gpu, 60);
        eprintln!("decode: {hd:.1} ms a frame at 1080p, {uhd:.1} ms at 4K");
        assert!(hd_gpu && uhd_gpu, "a frame went down to the CPU");
        // The same order: it was 44 ms against 1.8.
        assert!(uhd < hd * 6.0 + 4.0, "4K takes {uhd:.1} ms a frame against {hd:.1} at 1080p");
        // Ten bits (P010) take the same road.
        if let Some(ten) = chart_10bit() {
            let (hd, hd_gpu) = timed(&ten, 1920, 1080, &mut gpu, 30);
            let (uhd, uhd_gpu) = timed(&ten, 3840, 2160, &mut gpu, 30);
            eprintln!("10-bit decode: {hd:.1} ms a frame at 1080p, {uhd:.1} ms at 4K");
            assert!(hd_gpu && uhd_gpu, "a 10-bit frame went down to the CPU");
            assert!(uhd < hd * 6.0 + 4.0, "10-bit 4K takes {uhd:.1} ms a frame against {hd:.1} at 1080p");
        }
    }

    #[test]
    fn the_gpu_upscale_matches_swscales_lanczos() {
        let Some(path) = chart() else { return eprintln!("skipping: no ffmpeg") };
        let mut gpu = GpuConverters::shared();
        if gpu.nv12.is_none() || RenderDecoder::open(&path, 0.0, 1.0, 64, 36, 25.0, 0).unwrap().hw_accel_name() == "Software" {
            return eprintln!("skipping: no hardware decode and GPU converter here");
        }
        let (base_max, base_mean) = against_swscale(&path, 1920, 1080, &mut gpu);
        let (max, mean) = against_swscale(&path, 3840, 2160, &mut gpu);
        eprintln!("GPU against swscale, levels: at 1:1 max {base_max:.1} mean {base_mean:.3}; 2x upscale max {max:.1} mean {mean:.3}");
        // Swscale's export, to a level, as at 1:1: a 4K export keeps its look.
        assert!(mean <= base_mean + 0.1, "the upscale adds {:.3} levels on average to what 1:1 differs by", mean - base_mean);
        assert!(max <= base_max + 1.0, "the upscale is {max:.1} levels off at worst, against {base_max:.1} at 1:1");
    }
}
