use crate::error::RenderError;
use crate::snapshot::SequenceSnapshot;
use klipp_core::{ExportSettings, ExportVideoCodec};
use std::path::Path;
use tracing::{debug, info};

/// In-process encoder using ffmpeg-next (replaces the ffmpeg CLI subprocess).
///
/// Gap 8 fix: correct setup order:
///   1. Find codec
///   2. Create codec context, configure parameters
///   3. Open codec (open_as_with with options dict)
///   4. Add stream to output, copy parameters from opened encoder
///   5. Write header
///
/// Gap 9 fix: RGBA→YUV scaler created once in open(), reused per frame.
pub struct Encoder {
    output_ctx: ffmpeg_next::format::context::Output,
    video_encoder: ffmpeg_next::codec::encoder::video::Encoder,
    audio_encoder: Option<ffmpeg_next::codec::encoder::audio::Encoder>,
    video_stream_idx: usize,
    audio_stream_idx: Option<usize>,
    /// Monotonically increasing PTS counter for video.
    video_pts: i64,
    /// Monotonically increasing PTS counter for audio (in audio time_base units).
    audio_pts: i64,
    /// Video time_base (for packet rescaling).
    video_time_base: ffmpeg_next::Rational,
    /// Audio time_base (for packet rescaling).
    audio_time_base: Option<ffmpeg_next::Rational>,
    /// Pre-created RGBA→YUV scaler (Gap 9: created once, reused per frame).
    rgba_to_yuv: ffmpeg_next::software::scaling::Context,
    /// Audio samples per encoder frame (codec-specific).
    audio_frame_size: Option<usize>,
    /// Pending audio samples (accumulate until we have a full encoder frame).
    audio_buffer: Vec<f32>,
    /// Pre-allocated audio frame (reused per encode to avoid allocation issues).
    audio_frame: Option<ffmpeg_next::frame::Audio>,
    /// Name of the video encoder in use (for reporting).
    video_codec_name: &'static str,
    /// Audio resampler (f32 packed stereo → encoder's required format).
    /// Reserved for future use when format conversion is needed beyond FLTP.
    #[allow(dead_code)]
    audio_resampler: Option<ffmpeg_next::software::resampling::Context>,
    /// The encoder's actual sample format.
    audio_sample_format: Option<ffmpeg_next::format::Sample>,
}

/// Pick the best available video encoder for the given codec.
/// Returns (encoder_name, is_hw_accelerated).
/// Tries NVENC (NVIDIA) → AMF (AMD) → software fallback.
/// NVENC requires minimum 128x128 resolution.
fn pick_video_encoder(codec: &ExportVideoCodec, width: u32, height: u32) -> (&'static str, bool) {
    // NVENC/AMF have minimum resolution requirements (~128x128)
    let hw_eligible = width >= 128 && height >= 128;

    match codec {
        ExportVideoCodec::H264 => {
            if hw_eligible {
                if ffmpeg_next::encoder::find_by_name("h264_nvenc").is_some() {
                    return ("h264_nvenc", true);
                }
                if ffmpeg_next::encoder::find_by_name("h264_amf").is_some() {
                    return ("h264_amf", true);
                }
            }
            ("libx264", false)
        }
        ExportVideoCodec::H265 => {
            if hw_eligible {
                if ffmpeg_next::encoder::find_by_name("hevc_nvenc").is_some() {
                    return ("hevc_nvenc", true);
                }
                if ffmpeg_next::encoder::find_by_name("hevc_amf").is_some() {
                    return ("hevc_amf", true);
                }
            }
            ("libx265", false)
        }
        ExportVideoCodec::ProRes422
        | ExportVideoCodec::ProRes422HQ
        | ExportVideoCodec::ProRes4444 => ("prores_ks", false),
        ExportVideoCodec::DNxHD115 | ExportVideoCodec::DNxHD145 => ("dnxhd", false),
    }
}

impl Encoder {
    /// Open the encoder for the given output path and settings.
    pub fn open(
        output_path: &Path,
        settings: &ExportSettings,
        snapshot: &SequenceSnapshot,
    ) -> Result<Self, RenderError> {
        // Force the correct container format based on codec.
        // ProRes and DNxHD require MOV; H.264/H.265 work in MP4/MKV/MOV.
        let forced_format = match settings.video_codec {
            ExportVideoCodec::ProRes422
            | ExportVideoCodec::ProRes422HQ
            | ExportVideoCodec::ProRes4444
            | ExportVideoCodec::DNxHD115
            | ExportVideoCodec::DNxHD145 => Some("mov"),
            _ => None,
        };

        let mut output_ctx = if let Some(fmt) = forced_format {
            ffmpeg_next::format::output_as(output_path, fmt)
                .map_err(|e| RenderError::Encode(format!("output context (forced {fmt}): {e}")))?
        } else {
            ffmpeg_next::format::output(output_path)
                .map_err(|e| RenderError::Encode(format!("output context: {e}")))?
        };

        // ═══════════════════════════════════════════════════════════
        // VIDEO ENCODER
        // ═══════════════════════════════════════════════════════════

        // Try GPU-accelerated encoder first (NVENC for NVIDIA, AMF for AMD),
        // fall back to software (libx264/libx265).
        let (video_codec_name, is_hw_encoder) =
            pick_video_encoder(&settings.video_codec, snapshot.width, snapshot.height);
        let video_codec = ffmpeg_next::encoder::find_by_name(video_codec_name)
            .ok_or_else(|| RenderError::Encode(format!("codec not found: {video_codec_name}")))?;

        info!("Video encoder: {} (hw={})", video_codec_name, is_hw_encoder);

        // Create encoder context
        let mut video_ctx = ffmpeg_next::codec::context::Context::new_with_codec(video_codec)
            .encoder()
            .video()
            .map_err(|e| RenderError::Encode(format!("video encoder context: {e}")))?;

        video_ctx.set_width(snapshot.width);
        video_ctx.set_height(snapshot.height);
        video_ctx.set_frame_rate(Some(ffmpeg_next::Rational::new(
            snapshot.fps_num as i32,
            snapshot.fps_den as i32,
        )));
        video_ctx.set_time_base(ffmpeg_next::Rational::new(
            snapshot.fps_den as i32,
            snapshot.fps_num as i32,
        ));

        // Set pixel format based on codec + HW/SW
        let pix_fmt = match settings.video_codec {
            ExportVideoCodec::ProRes4444 => ffmpeg_next::format::Pixel::YUV444P10LE,
            ExportVideoCodec::ProRes422 | ExportVideoCodec::ProRes422HQ => {
                ffmpeg_next::format::Pixel::YUV422P10LE
            }
            ExportVideoCodec::DNxHD115 | ExportVideoCodec::DNxHD145 => {
                ffmpeg_next::format::Pixel::YUV422P
            }
            ExportVideoCodec::H264 | ExportVideoCodec::H265 => {
                if is_hw_encoder {
                    // NVENC/AMF prefer NV12 or P010
                    ffmpeg_next::format::Pixel::NV12
                } else {
                    ffmpeg_next::format::Pixel::YUV420P
                }
            }
        };
        video_ctx.set_format(pix_fmt);

        // Codec-specific options
        let mut video_opts = ffmpeg_next::Dictionary::new();
        match settings.video_codec {
            ExportVideoCodec::H264 if is_hw_encoder => {
                // NVENC: use VBR with constant quality (cq) — equivalent to CRF
                video_opts.set("rc", "vbr");
                video_opts.set("cq", &settings.crf.to_string());
                // Set a high bitrate cap so VBR has room (0 = uncapped)
                video_ctx.set_bit_rate(0);
                video_opts.set("b:v", "0");
                // NVENC preset: p1(fastest)..p7(best quality)
                let nvenc_preset = match settings.preset {
                    klipp_core::EncoderPreset::Fast => "p4",
                    klipp_core::EncoderPreset::Medium => "p5",
                    klipp_core::EncoderPreset::Slow => "p6",
                    klipp_core::EncoderPreset::VerySlow => "p7",
                };
                video_opts.set("preset", nvenc_preset);
                // Rec.709 colour space tagging
                unsafe {
                    (*video_ctx.as_mut_ptr()).color_primaries =
                        ffmpeg_next::ffi::AVColorPrimaries::AVCOL_PRI_BT709;
                    (*video_ctx.as_mut_ptr()).color_trc =
                        ffmpeg_next::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
                    (*video_ctx.as_mut_ptr()).colorspace =
                        ffmpeg_next::ffi::AVColorSpace::AVCOL_SPC_BT709;
                }
            }
            ExportVideoCodec::H264 => {
                video_ctx.set_bit_rate(0); // CRF mode
                video_opts.set("crf", &settings.crf.to_string());
                video_opts.set("preset", settings.preset.ffmpeg_value());
                unsafe {
                    (*video_ctx.as_mut_ptr()).color_primaries =
                        ffmpeg_next::ffi::AVColorPrimaries::AVCOL_PRI_BT709;
                    (*video_ctx.as_mut_ptr()).color_trc =
                        ffmpeg_next::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
                    (*video_ctx.as_mut_ptr()).colorspace =
                        ffmpeg_next::ffi::AVColorSpace::AVCOL_SPC_BT709;
                }
            }
            ExportVideoCodec::H265 if is_hw_encoder => {
                video_opts.set("rc", "vbr");
                video_opts.set("cq", &settings.crf.to_string());
                video_ctx.set_bit_rate(0);
                video_opts.set("b:v", "0");
                let nvenc_preset = match settings.preset {
                    klipp_core::EncoderPreset::Fast => "p4",
                    klipp_core::EncoderPreset::Medium => "p5",
                    klipp_core::EncoderPreset::Slow => "p6",
                    klipp_core::EncoderPreset::VerySlow => "p7",
                };
                video_opts.set("preset", nvenc_preset);
                unsafe {
                    (*video_ctx.as_mut_ptr()).color_primaries =
                        ffmpeg_next::ffi::AVColorPrimaries::AVCOL_PRI_BT709;
                    (*video_ctx.as_mut_ptr()).color_trc =
                        ffmpeg_next::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
                    (*video_ctx.as_mut_ptr()).colorspace =
                        ffmpeg_next::ffi::AVColorSpace::AVCOL_SPC_BT709;
                }
            }
            ExportVideoCodec::H265 => {
                video_ctx.set_bit_rate(0);
                video_opts.set("crf", &settings.crf.to_string());
                video_opts.set("preset", settings.preset.ffmpeg_value());
                video_opts.set(
                    "x265-params",
                    "colorprim=bt709:transfer=bt709:colormatrix=bt709",
                );
            }
            ExportVideoCodec::ProRes422 => {
                video_opts.set("profile:v", "2");
            }
            ExportVideoCodec::ProRes422HQ => {
                video_opts.set("profile:v", "3");
            }
            ExportVideoCodec::ProRes4444 => {
                video_opts.set("profile:v", "4");
            }
            ExportVideoCodec::DNxHD115 => {
                video_ctx.set_bit_rate(115_000_000);
            }
            ExportVideoCodec::DNxHD145 => {
                video_ctx.set_bit_rate(145_000_000);
            }
        }

        // Global header flag (needed for MP4/MOV containers)
        if output_ctx
            .format()
            .flags()
            .contains(ffmpeg_next::format::flag::Flags::GLOBAL_HEADER)
        {
            unsafe {
                (*video_ctx.as_mut_ptr()).flags |=
                    ffmpeg_next::ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
            }
        }

        // Open the video encoder
        let video_encoder = video_ctx
            .open_as_with(video_codec, video_opts)
            .map_err(|e| RenderError::Encode(format!("open video encoder: {e}")))?;

        let video_time_base = video_encoder.time_base();

        // Add video stream and copy parameters from opened encoder
        let video_stream_idx = unsafe {
            let stream_ptr =
                ffmpeg_next::ffi::avformat_new_stream(output_ctx.as_mut_ptr(), std::ptr::null());
            if stream_ptr.is_null() {
                return Err(RenderError::Encode("failed to add video stream".into()));
            }
            ffmpeg_next::ffi::avcodec_parameters_from_context(
                (*stream_ptr).codecpar,
                video_encoder.as_ptr(),
            );
            (*stream_ptr).time_base = video_time_base.into();
            ((*output_ctx.as_ptr()).nb_streams - 1) as usize
        };

        // Create RGBA→YUV scaler (Gap 9: created once, reused per frame)
        let rgba_to_yuv = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::RGBA,
            snapshot.width,
            snapshot.height,
            pix_fmt,
            snapshot.width,
            snapshot.height,
            ffmpeg_next::software::scaling::Flags::BICUBIC,
        )
        .map_err(|e| RenderError::Encode(format!("RGBA→YUV scaler: {e}")))?;

        // ═══════════════════════════════════════════════════════════
        // AUDIO ENCODER (optional)
        // ═══════════════════════════════════════════════════════════

        let mut audio_encoder = None;
        let mut audio_stream_idx = None;
        let mut audio_time_base = None;
        let mut audio_frame_size = None;
        let mut audio_resampler = None;
        let mut audio_sample_fmt = None;

        // Skip audio encoder for "Copy" codec (doesn't make sense for in-process encode)
        let audio_codec_name = settings.audio_codec.ffmpeg_codec();
        let skip_audio = matches!(settings.audio_codec, klipp_core::ExportAudioCodec::Copy);
        let found_audio_codec = if skip_audio {
            None
        } else {
            ffmpeg_next::encoder::find_by_name(audio_codec_name)
        };

        if let Some(audio_codec) = found_audio_codec {
            let mut audio_ctx = ffmpeg_next::codec::context::Context::new_with_codec(audio_codec)
                .encoder()
                .audio()
                .map_err(|e| RenderError::Encode(format!("audio encoder context: {e}")))?;

            audio_ctx.set_rate(snapshot.sample_rate as i32);
            audio_ctx.set_channel_layout(ffmpeg_next::ChannelLayout::STEREO);
            // Use the codec's preferred sample format (not always f32)
            let sample_fmt = unsafe {
                let codec_ptr = audio_codec.as_ptr();
                let fmts = (*codec_ptr).sample_fmts;
                if fmts.is_null() {
                    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed)
                } else {
                    // Use first supported format
                    ffmpeg_next::format::Sample::from(*fmts)
                }
            };
            audio_ctx.set_format(sample_fmt);
            audio_ctx.set_time_base(ffmpeg_next::Rational::new(1, snapshot.sample_rate as i32));

            if let Some(bitrate) = settings.audio_bitrate_kbps {
                audio_ctx.set_bit_rate(bitrate as usize * 1000);
            }

            // Global header flag
            if output_ctx
                .format()
                .flags()
                .contains(ffmpeg_next::format::flag::Flags::GLOBAL_HEADER)
            {
                unsafe {
                    (*audio_ctx.as_mut_ptr()).flags |=
                        ffmpeg_next::ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
                }
            }

            let enc = audio_ctx
                .open_as(audio_codec)
                .map_err(|e| RenderError::Encode(format!("open audio encoder: {e}")))?;

            let tb = enc.time_base();
            let frame_size = unsafe { (*enc.as_ptr()).frame_size as usize };

            let a_idx = unsafe {
                let stream_ptr = ffmpeg_next::ffi::avformat_new_stream(
                    output_ctx.as_mut_ptr(),
                    std::ptr::null(),
                );
                if stream_ptr.is_null() {
                    return Err(RenderError::Encode("failed to add audio stream".into()));
                }
                ffmpeg_next::ffi::avcodec_parameters_from_context(
                    (*stream_ptr).codecpar,
                    enc.as_ptr(),
                );
                (*stream_ptr).time_base = tb.into();
                ((*output_ctx.as_ptr()).nb_streams - 1) as usize
            };

            // Create resampler from f32 packed → encoder's format (if different)
            let needs_resample = sample_fmt
                != ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed);
            let a_resampler = if needs_resample {
                Some(
                    ffmpeg_next::software::resampling::Context::get(
                        ffmpeg_next::format::Sample::F32(
                            ffmpeg_next::format::sample::Type::Packed,
                        ),
                        ffmpeg_next::ChannelLayout::STEREO,
                        snapshot.sample_rate,
                        sample_fmt,
                        ffmpeg_next::ChannelLayout::STEREO,
                        snapshot.sample_rate,
                    )
                    .map_err(|e| {
                        RenderError::Encode(format!("audio resampler: {e}"))
                    })?,
                )
            } else {
                None
            };

            audio_stream_idx = Some(a_idx);
            audio_time_base = Some(tb);
            audio_frame_size = if frame_size > 0 {
                Some(frame_size)
            } else {
                Some(1024)
            };
            audio_encoder = Some(enc);
            audio_resampler = a_resampler;
            audio_sample_fmt = Some(sample_fmt);
        }

        // ═══════════════════════════════════════════════════════════
        // WRITE HEADER
        // ═══════════════════════════════════════════════════════════

        output_ctx
            .write_header()
            .map_err(|e| RenderError::Encode(format!(
                "write header: {e} (codec={video_codec_name}, {}x{} @{}/{}fps, path={:?})",
                snapshot.width, snapshot.height, snapshot.fps_num, snapshot.fps_den, output_path
            )))?;

        debug!(
            "Encoder: {} {}x{} @{:.1}fps → {:?}",
            video_codec_name,
            snapshot.width,
            snapshot.height,
            snapshot.fps,
            output_path
        );

        Ok(Self {
            output_ctx,
            video_encoder,
            audio_encoder,
            video_stream_idx,
            audio_stream_idx,
            video_pts: 0,
            audio_pts: 0,
            video_time_base,
            audio_time_base,
            rgba_to_yuv,
            audio_frame_size,
            audio_buffer: Vec::new(),
            audio_resampler,
            audio_sample_format: audio_sample_fmt,
            audio_frame: if audio_frame_size.is_some() && audio_sample_fmt.is_some() {
                Some(ffmpeg_next::frame::Audio::new(
                    audio_sample_fmt.unwrap(),
                    audio_frame_size.unwrap(),
                    ffmpeg_next::ChannelLayout::STEREO,
                ))
            } else {
                None
            },
            video_codec_name,
        })
    }

    /// Human-readable hardware info for the video encoder.
    pub fn hw_info(&self) -> &'static str {
        if self.video_codec_name.contains("nvenc") {
            "NVENC (NVIDIA)"
        } else if self.video_codec_name.contains("amf") {
            "AMF (AMD)"
        } else if self.video_codec_name.contains("prores") {
            "ProRes (CPU)"
        } else if self.video_codec_name.contains("dnxhd") {
            "DNxHD (CPU)"
        } else {
            "Software (CPU)"
        }
    }

    /// Encode one RGBA video frame.
    pub fn encode_video_frame(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), RenderError> {
        // Build RGBA source frame
        let mut rgba_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::RGBA,
            width,
            height,
        );
        // Copy RGBA data into the frame's data plane
        let stride = rgba_frame.stride(0);
        let row_bytes = width as usize * 4;
        {
            let dst = rgba_frame.data_mut(0);
            for y in 0..height as usize {
                let src_off = y * row_bytes;
                let dst_off = y * stride;
                dst[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&rgba[src_off..src_off + row_bytes]);
            }
        }

        // Convert RGBA → YUV using pre-created scaler (Gap 9)
        let mut yuv_frame = ffmpeg_next::frame::Video::new(
            self.video_encoder.format(),
            width,
            height,
        );
        self.rgba_to_yuv
            .run(&rgba_frame, &mut yuv_frame)
            .map_err(|e| RenderError::Encode(format!("RGBA→YUV scale: {e}")))?;

        yuv_frame.set_pts(Some(self.video_pts));
        self.video_pts += 1;

        // Send to encoder
        self.video_encoder
            .send_frame(&yuv_frame)
            .map_err(|e| RenderError::Encode(format!("send video frame: {e}")))?;

        self.drain_video_packets()?;
        Ok(())
    }

    /// Feed audio samples for one video frame (interleaved stereo f32).
    /// Buffers internally and encodes when enough samples accumulate.
    pub fn encode_audio_samples(&mut self, samples: &[f32]) -> Result<(), RenderError> {
        if self.audio_encoder.is_none() {
            return Ok(());
        }

        let frame_size = self.audio_frame_size.unwrap_or(1024);
        let channels = 2usize; // stereo
        let floats_per_frame = frame_size * channels;

        self.audio_buffer.extend_from_slice(samples);

        // Encode complete frames using pre-allocated frame
        let is_planar = matches!(
            self.audio_sample_format,
            Some(ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar))
        );

        while self.audio_buffer.len() >= floats_per_frame {
            // Fill the pre-allocated frame
            if let Some(ref mut frame) = self.audio_frame {
                if is_planar {
                    let plane0 = frame.data_mut(0);
                    for i in 0..frame_size {
                        let val = self.audio_buffer[i * 2];
                        let off = i * 4;
                        if off + 4 <= plane0.len() {
                            plane0[off..off + 4].copy_from_slice(&val.to_le_bytes());
                        }
                    }
                    let plane1 = frame.data_mut(1);
                    for i in 0..frame_size {
                        let val = self.audio_buffer[i * 2 + 1];
                        let off = i * 4;
                        if off + 4 <= plane1.len() {
                            plane1[off..off + 4].copy_from_slice(&val.to_le_bytes());
                        }
                    }
                } else {
                    let dst = frame.data_mut(0);
                    let src_bytes: &[u8] =
                        bytemuck::cast_slice(&self.audio_buffer[..floats_per_frame]);
                    let copy_len = src_bytes.len().min(dst.len());
                    dst[..copy_len].copy_from_slice(&src_bytes[..copy_len]);
                }
                frame.set_pts(Some(self.audio_pts));
                unsafe {
                    let rate = self.audio_encoder.as_ref().unwrap().rate() as i32;
                    (*frame.as_mut_ptr()).time_base = self.audio_time_base.unwrap().into();
                    (*frame.as_mut_ptr()).sample_rate = rate;
                }
            }

            self.audio_buffer.drain(..floats_per_frame);
            self.audio_pts += frame_size as i64;

            // Get raw pointers before borrowing self for send/drain
            let (frame_ptr, enc_ptr) = unsafe {
                let fp = match &self.audio_frame {
                    Some(f) => f.as_ptr(),
                    None => continue,
                };
                let ep = self.audio_encoder.as_mut().unwrap().as_mut_ptr();
                (fp, ep)
            };

            // Send frame via raw FFI (avoids borrow conflicts with drain)
            let send_result =
                unsafe { ffmpeg_next::ffi::avcodec_send_frame(enc_ptr, frame_ptr) };
            match send_result {
                0 => {}
                e if e == ffmpeg_next::error::EAGAIN => {
                    self.drain_audio_packets()?;
                    let ep2 = unsafe { self.audio_encoder.as_mut().unwrap().as_mut_ptr() };
                    unsafe {
                        ffmpeg_next::ffi::avcodec_send_frame(ep2, frame_ptr);
                    }
                }
                e => {
                    if self.audio_pts < (frame_size * 5) as i64 {
                        tracing::warn!("audio send_frame error={e} at pts={}", self.audio_pts);
                    }
                }
            }

            self.drain_audio_packets()?;
        }

        Ok(())
    }

    /// Finalise the output file. Must be called after all frames are encoded.
    pub fn finalise(mut self) -> Result<(), RenderError> {
        // Flush video encoder
        self.video_encoder
            .send_eof()
            .map_err(|e| RenderError::Encode(format!("video send_eof: {e}")))?;
        self.drain_video_packets()?;

        // Flush remaining audio buffer + encoder
        if self.audio_encoder.is_some() {
            // Encode any remaining partial audio frame
            let frame_size = self.audio_frame_size.unwrap_or(1024);
            let channels = 2usize;
            if !self.audio_buffer.is_empty() {
                let floats_per_frame = frame_size * channels;
                self.audio_buffer.resize(floats_per_frame, 0.0);
                // Re-use encode_audio_samples logic by draining buffer through it
                let remaining: Vec<f32> = self.audio_buffer.drain(..).collect();
                self.audio_buffer.extend_from_slice(&remaining);
                // Encode the padded frame
                let enc_fmt = self.audio_sample_format.unwrap_or(
                    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                );
                let mut frame = ffmpeg_next::frame::Audio::new(
                    enc_fmt,
                    frame_size,
                    ffmpeg_next::ChannelLayout::STEREO,
                );
                match enc_fmt {
                    ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar) => {
                        let plane0 = frame.data_mut(0);
                        for i in 0..frame_size {
                            let val = self.audio_buffer.get(i * 2).copied().unwrap_or(0.0);
                            let off = i * 4;
                            if off + 4 <= plane0.len() {
                                plane0[off..off + 4].copy_from_slice(&val.to_le_bytes());
                            }
                        }
                        let plane1 = frame.data_mut(1);
                        for i in 0..frame_size {
                            let val = self.audio_buffer.get(i * 2 + 1).copied().unwrap_or(0.0);
                            let off = i * 4;
                            if off + 4 <= plane1.len() {
                                plane1[off..off + 4].copy_from_slice(&val.to_le_bytes());
                            }
                        }
                    }
                    _ => {
                        let dst = frame.data_mut(0);
                        let src_bytes: &[u8] = bytemuck::cast_slice(&self.audio_buffer);
                        let copy_len = src_bytes.len().min(dst.len());
                        dst[..copy_len].copy_from_slice(&src_bytes[..copy_len]);
                    }
                }
                frame.set_pts(Some(self.audio_pts));
                let _ = self.audio_encoder.as_mut().unwrap().send_frame(&frame);
                self.drain_audio_packets()?;
            }

            // send_eof may fail if encoder is in a bad state — don't hard-fail
            let _ = self.audio_encoder.as_mut().unwrap().send_eof();
            self.drain_audio_packets()?;
        }

        // Write trailer
        self.output_ctx
            .write_trailer()
            .map_err(|e| RenderError::Encode(format!("write trailer: {e}")))?;

        debug!("Encoder: finalised");
        Ok(())
    }

    fn drain_video_packets(&mut self) -> Result<(), RenderError> {
        let mut packet = ffmpeg_next::Packet::empty();
        while self.video_encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.video_stream_idx);
            packet.rescale_ts(
                self.video_time_base,
                self.output_ctx
                    .stream(self.video_stream_idx)
                    .unwrap()
                    .time_base(),
            );
            packet
                .write_interleaved(&mut self.output_ctx)
                .map_err(|e| RenderError::Encode(format!("write video packet: {e}")))?;
        }
        Ok(())
    }

    fn drain_audio_packets(&mut self) -> Result<(), RenderError> {
        let a_idx = match self.audio_stream_idx {
            Some(i) => i,
            None => return Ok(()),
        };
        let a_tb = self.audio_time_base.unwrap();
        let encoder = match &mut self.audio_encoder {
            Some(e) => e,
            None => return Ok(()),
        };

        let mut packet = ffmpeg_next::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(a_idx);
            packet.rescale_ts(a_tb, self.output_ctx.stream(a_idx).unwrap().time_base());
            packet
                .write_interleaved(&mut self.output_ctx)
                .map_err(|e| RenderError::Encode(format!("write audio packet: {e}")))?;
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::SequenceSnapshot;
    use klipp_core::{
        EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec,
    };

    fn make_settings(codec: ExportVideoCodec, path: &str) -> ExportSettings {
        ExportSettings {
            output_path: std::path::PathBuf::from(path),
            format: ExportFormat::Mp4,
            video_codec: codec,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::Fast,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(128),
        }
    }

    fn make_snapshot() -> SequenceSnapshot {
        SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: 320,
            height: 240,
            sample_rate: 48000,
            duration_frames: 10,
            export_range: None,
            video_tracks: vec![],
            audio_tracks: vec![],
            global_lut_path: None,
        }
    }

    #[test]
    fn encode_h264_10_frames() {
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h264.mp4");

        let mut settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        // Skip audio in this test — audio encoding tested separately
        settings.audio_codec = ExportAudioCodec::Copy;
        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        // Encode 10 red frames
        let rgba = vec![255u8, 0, 0, 255].repeat(320 * 240);
        for _ in 0..10 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }

        enc.finalise().unwrap();

        // Verify file exists and has non-zero size
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 100, "output file should be non-trivial size");

        // Verify with ffprobe
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v", "error",
                "-select_streams", "v:0",
                "-show_entries", "stream=codec_name,width,height",
                "-of", "csv=p=0",
            ])
            .arg(&path)
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("h264"), "expected h264 codec, got: {stdout}");
            assert!(stdout.contains("320"), "expected width 320");
            assert!(stdout.contains("240"), "expected height 240");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn encode_h264_with_aac_audio() {
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h264_aac.mp4");

        let settings = make_settings(ExportVideoCodec::H264, path.to_str().unwrap());
        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        let rgba = vec![128u8, 128, 128, 255].repeat(320 * 240);
        for _ in 0..10 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }

        // Feed enough audio to trigger at least one AAC frame (1024 samples)
        let silence = vec![0.0f32; 2000 * 2];
        for _ in 0..10 {
            enc.encode_audio_samples(&silence).unwrap();
        }

        enc.finalise().unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 100);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn encode_h265_video_only() {
        let dir = std::env::temp_dir().join("klipp_render_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_h265.mp4");

        let mut settings = make_settings(ExportVideoCodec::H265, path.to_str().unwrap());
        settings.audio_codec = ExportAudioCodec::Copy; // no audio

        let snapshot = make_snapshot();

        let mut enc = Encoder::open(&path, &settings, &snapshot).unwrap();

        let rgba = vec![0u8, 255, 0, 255].repeat(320 * 240); // green
        for _ in 0..5 {
            enc.encode_video_frame(&rgba, 320, 240).unwrap();
        }

        enc.finalise().unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 100);

        let _ = std::fs::remove_file(&path);
    }
}
